//! The webkit2gtk web-wallpaper backend.
//!
//! See the module docs ([`super`]) for the rendering model. This file owns the
//! live `WebKitWebView` widget, drives the WE JS bridge, and forwards pointer
//! input as synthetic DOM events. webkit itself is reached through
//! [`super::webkit_sys`], which `dlopen`s it at run time so one binary serves
//! both `webkit2gtk-4.1` and `webkit2gtk-4.0`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::glib::translate::{ToGlibPtr as _, from_glib_none};
use gtk::prelude::*;

use crate::backend::{PointerState, WebError, WebFrameRef, WebSize};
use crate::shim;

use super::webkit_sys::WebKit;

/// webkit2gtk exposes no pixel read-back and no OSR path, so muting is done
/// in JavaScript. This init script installs a bridge that (1) keeps every
/// `<audio>`/`<video>` element's `muted` flag in sync with `window.__wpMuteState`
/// (existing and future, via a `MutationObserver`), and (2) routes every
/// `AudioContext.destination` through a gain node so Web Audio output (used by
/// visualiser wallpapers, which bypass media-element muting) can be silenced
/// too. The literal `MUTESTATE` token is replaced with the initial bool.
const MUTE_INIT: &str = r#"(function(){
  if (window.__wpMuteInit) { window.__wpMuteState = MUTESTATE; return; }
  window.__wpMuteInit = true;
  window.__wpMuteState = MUTESTATE;
  function applyEl(el){ try { el.muted = window.__wpMuteState; } catch(e){} }
  window.__wpSweepMute = function(){
    var m = document.querySelectorAll('audio,video');
    for (var i = 0; i < m.length; i++) { applyEl(m[i]); }
  };
  document.addEventListener('DOMContentLoaded', window.__wpSweepMute);
  try {
    new MutationObserver(window.__wpSweepMute)
      .observe(document.documentElement, { childList: true, subtree: true });
  } catch(e) {}
  var AC = window.AudioContext || window.webkitAudioContext;
  if (AC && !AC.__wpPatched) {
    AC.__wpPatched = true;
    var desc = Object.getOwnPropertyDescriptor(AC.prototype, 'destination');
    if (desc && desc.get) {
      Object.defineProperty(AC.prototype, 'destination', {
        configurable: true,
        get: function () {
          if (!this.__wpMuteGain) {
            var real = desc.get.call(this);
            var g = this.createGain();
            g.connect(real);
            this.__wpMuteGain = g;
          }
          this.__wpMuteGain.gain.value = window.__wpMuteState ? 0 : 1;
          return this.__wpMuteGain;
        }
      });
    }
  }
})();"#;

/// How many scripts may pile up while the first page load is still in flight.
///
/// wry queued them *unboundedly*. Here the queue is fed by [`send_pointer`]
/// at whatever rate the engine samples the pointer, so a page that never
/// commits (dead URL, webkit crash loop) would grow it without limit. 64 is
/// far more than the handful of `props`/`mute` lines that legitimately arrive
/// before first paint; past that, further scripts are dropped rather than
/// accumulated.
///
/// [`send_pointer`]: WebviewBackend::send_pointer
const MAX_PENDING: usize = 64;

/// Build the combined initialization script: WE bridge + mute bridge.
fn init_script(muted: bool) -> String {
    let mute = MUTE_INIT.replace("MUTESTATE", if muted { "true" } else { "false" });
    format!("{}\n{}", shim::BRIDGE_INIT, mute)
}

/// A live web wallpaper rendered by webkit2gtk into a native background surface.
///
/// This type is intentionally **not** the object-safe, `Send` `WebBackend`
/// trait from [`crate::backend`]: a webkit2gtk object is `!Send` (it must live
/// on the GTK main thread) and produces no CPU frame, so the off-screen
/// `WebBackend` contract does not apply. The method set below mirrors that
/// trait as closely as the native-surface model allows.
pub struct WebviewBackend {
    /// The live `WebKitWebView`, held as the `gtk::Widget` handle that owns a
    /// strong GObject reference to it. `None` after [`Self::shutdown`].
    view: Option<gtk::Widget>,
    /// The process-wide `dlopen`ed webkit entry points.
    webkit: &'static WebKit,
    size: WebSize,
    muted: bool,
    last_pointer: PointerState,
    /// Scripts held back until the first load commits; `None` once flushed.
    /// Shared with the `load-changed` handler installed by
    /// [`flush_pending_on_commit`].
    pending: Rc<RefCell<Option<Vec<String>>>>,
}

impl WebviewBackend {
    /// Always [`None`]: the webview renders directly into its surface and never
    /// produces a CPU frame (native-surface model, see [`super`]).
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn latest_frame(&self) -> Option<WebFrameRef<'_>> {
        None
    }

    /// Advance one presentation step. `dt` is unused: webkit2gtk renders on the
    /// host's GTK/event loop, not on a tick we pump. Kept for interface parity.
    #[allow(clippy::unused_self)]
    pub fn tick(&mut self, _dt: f32) {}

    /// Record a new surface size.
    ///
    /// Nothing is pushed to webkit: the view is a GTK child of the host's
    /// layer-shell window, so GTK reallocates it whenever the compositor
    /// resizes that surface. (wry needed an explicit `set_bounds` only for its
    /// X11 *child-window* attachment path, which went away with the raw-handle
    /// constructor.)
    pub fn resize(&mut self, size: WebSize) {
        let size = size.clamped();
        if size != self.size {
            tracing::debug!(
                width = size.width,
                height = size.height,
                "webview surface resized; GTK reallocates the view"
            );
            self.size = size;
        }
    }

    /// Push one audio frame to the page's registered audio listeners.
    ///
    /// Transient: a spectrum frame that arrives before the page exists is
    /// dropped rather than queued. It has to be — audio arrives ~30 times a
    /// second and would otherwise fill [`MAX_PENDING`] within two seconds of a
    /// slow first load, evicting the `props`/media batches that genuinely
    /// cannot be re-derived. A dropped spectrum frame costs nothing: the next
    /// one is 33 ms away.
    pub fn push_audio(&mut self, bands: &[f32]) {
        self.evaluate_transient(&shim::audio_call(bands));
    }

    /// Push one now-playing update to the page's registered media listeners.
    ///
    /// Queued, never transient — **including** the timeline, which looks like a
    /// continuously moving value but is not delivered like one. The engine
    /// sends every media channel on *change* only, so a paused player (or one
    /// that simply reports the same position twice) produces exactly one
    /// timeline event and then silence; dropping it because the page had not
    /// committed yet would leave a permanently blank progress bar. That is not
    /// hypothetical — it is what the probe page caught. Audio is the only
    /// stream where "the next one is 33 ms away" holds, and it is the only one
    /// allowed to be dropped.
    pub fn push_media(&mut self, channel: crate::feed::MediaChannel, json: &str) {
        self.evaluate(&channel.call(json));
    }

    /// Apply user properties (`{name: {value: ...}}` JSON) to the page.
    pub fn apply_user_properties(&mut self, json: &str) {
        self.evaluate(&shim::apply_user_properties_call(json));
    }

    /// Apply general/engine properties to the page.
    pub fn apply_general_properties(&mut self, json: &str) {
        self.evaluate(&shim::apply_general_properties_call(json));
    }

    /// Mute or unmute page audio. Flips `window.__wpMuteState` and re-sweeps
    /// media elements; Web Audio gains pick the new value up on next access.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        let state = if muted { "true" } else { "false" };
        self.evaluate(&format!(
            "window.__wpMuteState={state};if(window.__wpSweepMute){{window.__wpSweepMute();}}"
        ));
    }

    /// Forward a pointer sample as synthetic DOM events.
    ///
    /// A background layer/desktop window rarely holds pointer focus, so real
    /// webkit pointer events usually never arrive; dispatching synthetic
    /// `MouseEvent`s lets pages that listen on `document`/`window` still react
    /// (docs/subsystems-misc.md §3.5: position every frame, click on state
    /// change — no keyboard, no scroll).
    pub fn send_pointer(&mut self, pointer: PointerState) {
        self.evaluate(&mouse_move_call(pointer.x, pointer.y));
        if pointer.left != self.last_pointer.left {
            self.evaluate(&mouse_button_call(pointer.x, pointer.y, 0, pointer.left));
        }
        if pointer.right != self.last_pointer.right {
            self.evaluate(&mouse_button_call(pointer.x, pointer.y, 2, pointer.right));
        }
        self.last_pointer = pointer;
    }

    /// Launch a web wallpaper on `url`, rendering into a live GTK container
    /// (the `kirie-webviewhost` path: the container sits in a gtk-layer-shell
    /// window on the compositor's background layer, so webkit presents the
    /// wallpaper natively — no off-screen buffer involved).
    ///
    /// `url` should be a `file://` URL to the entry page (see
    /// [`file_url`](super::file_url)) so the page's relative asset references
    /// resolve against its own directory. `size` is the initial surface size;
    /// `muted` sets the starting audio state (honouring `--silent`).
    ///
    /// # Errors
    ///
    /// Returns [`WebError::BrowserCreation`] if no WebKitGTK could be
    /// `dlopen`ed (neither 4.1 nor 4.0 present, or one is present but missing
    /// a required entry point) or if webkit refused to construct the view.
    /// That variant carries no context, so the real reason is logged at
    /// `ERROR` first.
    pub fn with_gtk_container(
        url: &str,
        size: WebSize,
        container: &impl gtk::prelude::IsA<gtk::Container>,
        muted: bool,
    ) -> Result<Self, WebError> {
        let size = size.clamped();
        // Make the model unmistakable at runtime: this backend paints its own
        // native surface and can never composite through the wgpu presentation
        // layer (webkit2gtk has no off-screen path — see the module docs;
        // won't-fix upstream). The CEF backend is the composited one.
        tracing::warn!(
            url,
            "webview (webkit2gtk) backend: native-surface fallback only; it cannot render \
             off-screen (upstream webkit2gtk limitation) — build with the `cef` feature \
             (kirie: --features web-cef) for composited web wallpapers"
        );

        let webkit = WebKit::load().map_err(|e| {
            tracing::error!(error = %e, url, "could not dlopen WebKitGTK");
            WebError::BrowserCreation
        })?;

        // Before the first view exists: the cache model is sampled when the
        // web process starts, so this has to precede `new_web_view`.
        webkit.minimize_caches();

        let raw = webkit.new_web_view(Some(&init_script(muted)));
        if raw.is_null() {
            tracing::error!(url, soname = webkit.soname(), "webkit returned no web view");
            return Err(WebError::BrowserCreation);
        }
        // SAFETY: `raw` is a non-null, freshly constructed `WebKitWebView` —
        // a `GtkWidget` subclass, so it still carries the *floating* reference
        // every `GInitiallyUnowned` is born with. glib's object `from_glib_none`
        // ref-sinks, which is precisely the ownership transfer wanted here: the
        // `gtk::Widget` becomes the owner of that initial reference, and adding
        // it to a container below adds the container's own.
        let view: gtk::Widget = unsafe { from_glib_none(raw) };

        // A wallpaper is opaque; an opaque black background avoids the
        // compositor showing through before first paint. This is the same
        // colour wry produced from `.with_transparent(false)` +
        // `.with_background_color((0, 0, 0, 255))`, which it forwarded to
        // `webkit_web_view_set_background_color` as 0/0/0/1.0.
        webkit.set_background_color(&view, [0.0, 0.0, 0.0, 1.0]);
        // WE pages start audio/video with no user gesture
        // (docs/subsystems-misc.md §3.3: `--autoplay-policy=no-user-gesture-required`).
        webkit.set_autoplay(&view, true);

        // Fill the container. A `GtkBox` needs the explicit
        // `pack_start(child, expand, fill, 0)` wry also special-cased (0.55.1
        // `add_to_container`), because box packing is a *child property* that
        // `gtk_container_add` fills in with its own defaults. The expand flags
        // cover the generic `IsA<Container>` case as well, where all we can do
        // is `add` and ask the child to grow with its parent.
        view.set_hexpand(true);
        view.set_vexpand(true);
        if let Some(gtk_box) = container.as_ref().downcast_ref::<gtk::Box>() {
            gtk_box.pack_start(&view, true, true, 0);
        } else {
            container.as_ref().add(&view);
        }
        // The host realizes and `show_all()`s its window before building the
        // backend, so a child added afterwards must be shown explicitly or it
        // stays unmapped.
        view.show_all();

        let pending = Rc::new(RefCell::new(Some(Vec::new())));
        // Connect before the load starts so the commit cannot be missed.
        flush_pending_on_commit(&view, webkit, &pending);
        webkit.load_uri(&view, url);

        Ok(Self {
            view: Some(view),
            webkit,
            size,
            muted,
            last_pointer: PointerState::default(),
            pending,
        })
    }

    /// Tear the web view down. Idempotent.
    /// Draw the live page into `path` as raw BGRA (premultiplied), returning
    /// `WxH` on success.
    ///
    /// webkit has no off-screen *rendering* path, but the widget still paints
    /// through GTK, and the host already forces webkit's shared-memory
    /// renderer (`WEBKIT_DISABLE_DMABUF_RENDERER`, the explicit-sync
    /// workaround), so `gtk_widget_draw` can pull the composited page out into
    /// a cairo surface. That gives the engine a still of a web wallpaper it
    /// otherwise cannot see, to stand in while the wallpaper is released.
    ///
    /// Raw rather than PNG: the consumer uploads it straight to a texture, so
    /// encoding would only cost time and pull in cairo's png feature.
    pub fn snapshot_raw(&self, path: &str) -> Option<(i32, i32)> {
        let view = self.view.as_ref()?;
        let (w, h) = (view.allocated_width(), view.allocated_height());
        if w <= 0 || h <= 0 {
            return None;
        }
        let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h).ok()?;
        {
            let cr = gtk::cairo::Context::new(&surface).ok()?;
            view.draw(&cr);
        }
        surface.flush();
        let stride = surface.stride();
        let mut surface = surface;
        let data = surface.data().ok()?;
        // Drop cairo's row padding so the consumer gets a tight w*h*4 buffer.
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * stride) as usize;
            out.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        std::fs::write(path, &out).ok()?;
        Some((w, h))
    }

    /// Destroy the web view and drop this backend's reference to it.
    ///
    /// Idempotent: a second call finds no view and returns. Afterwards
    /// [`Self::latest_frame`] stays `None` and every script call is a no-op.
    pub fn shutdown(&mut self) {
        let Some(view) = self.view.take() else {
            return;
        };
        // SAFETY: `gtk_widget_destroy` is `unsafe` in gtk-rs because it
        // invalidates the widget while other handles may still point at it.
        // Ours was just consumed by `take`, the only other reference is the
        // container's (which `destroy` itself removes), and we are on the GTK
        // main thread that created the widget. This mirrors wry's own
        // `impl Drop for InnerWebView`, which does exactly this call.
        unsafe { view.destroy() };
    }

    /// The initial (or last-set) audio mute state.
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Evaluate `js` in the page, or queue it if the page does not exist yet.
    ///
    /// Never propagates a failure: a broken page or a torn-down view must not
    /// take the wallpaper down (SPEC V9). webkit's JS entry points are
    /// fire-and-forget, so a syntax error surfaces in the page console rather
    /// than here — the same as on the wry path, whose `Result` was only ever
    /// `Ok`.
    fn evaluate(&self, js: &str) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        // Before the first load commits there is no document to run JS in
        // (see `flush_pending_on_commit`).
        let mut pending = self.pending.borrow_mut();
        if let Some(queue) = pending.as_mut() {
            if queue.len() < MAX_PENDING {
                queue.push(js.to_owned());
            } else {
                tracing::debug!("webview script queue full before first paint; script dropped");
            }
            return;
        }
        drop(pending);
        self.webkit.eval(view, js);
    }

    /// Evaluate `js` only if the page already exists; drop it otherwise.
    ///
    /// The counterpart to [`Self::evaluate`]'s buffering, and used by exactly
    /// one caller: the audio spectrum. The rule for qualifying is strict — the
    /// value must be re-sent unconditionally on a short fixed period, so that
    /// dropping one costs a single frame and nothing else. Audio is the only
    /// such stream ([`Self::push_audio`], every 33 ms); everything else the
    /// engine pushes is change-driven and must be buffered instead, or a value
    /// that never changes again would be lost for the wallpaper's lifetime.
    ///
    /// Without this exemption a slow first load would fill the [`MAX_PENDING`]
    /// queue with spectrum frames that are stale by the time it drains, and
    /// evict the one-shot batches that cannot be reproduced.
    fn evaluate_transient(&self, js: &str) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        if self.pending.borrow().is_some() {
            return; // no document yet — this frame is simply skipped
        }
        self.webkit.eval(view, js);
    }
}

impl Drop for WebviewBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Replay scripts queued before the page existed, once its load commits.
///
/// wry buffered every `eval()` in `pending_scripts` and drained the queue from
/// a `load-changed` handler on `LoadEvent::Committed` (0.55.1
/// `src/webkitgtk/mod.rs`). That buffering is load-bearing for this host
/// rather than an optimisation: `kirie-webviewhost` prints `ready` as soon as
/// the view is built and the engine answers immediately with the initial
/// `props` line — long before webkit has a document to run JavaScript in.
/// Dropping those scripts would bring every wallpaper up on default user
/// properties.
///
/// The signal is connected *by name* instead of through a bound `webkit_*`
/// symbol: `g_signal_*` lives in libgobject, which this binary links normally,
/// so the dlopen surface stays at the nine entry points in
/// [`super::webkit_sys`].
fn flush_pending_on_commit(
    view: &gtk::Widget,
    webkit: &'static WebKit,
    pending: &Rc<RefCell<Option<Vec<String>>>>,
) {
    /// `WEBKIT_LOAD_FINISHED`, NOT `COMMITTED`: webkit injects user scripts
    /// (the `__wpApplyProps` bridge) at document-start, which begins *after*
    /// `load-changed` reports COMMITTED — a flush there can race ahead of the
    /// bridge and the queued call dies in a page that has no
    /// `window.__wpApplyProps` yet. Silently, because eval is fire-and-forget.
    /// Pages that BLOCK init on `applyUserProperties` (playlist/music
    /// wallpapers) then sat on their loading screen forever. At FINISHED both
    /// the bridge and the page's own scripts have run, and the bridge handles
    /// either order of listener-assignment vs. property arrival.
    const WEBKIT_LOAD_FINISHED: i32 = 3;

    let Some(signal) = glib::subclass::signal::SignalId::lookup("load-changed", view.type_()) else {
        // Unreachable on a real `WebKitWebView`, but a wallpaper process must
        // not panic on a surprise (SPEC V9) and must not queue forever either:
        // give up on buffering and let scripts run as they come.
        tracing::warn!("webkit view exposes no `load-changed` signal; not buffering scripts");
        pending.borrow_mut().take();
        return;
    };

    let pending = pending.clone();
    let _handler = view.connect_local_id(signal, None, false, move |values| {
        // `load-changed` is `(WebKitWebView *, WebKitLoadEvent)`.
        let event = values.get(1)?;
        if !event.type_().is_a(glib::Type::ENUM) {
            return None;
        }
        // SAFETY: the `GValue` belongs to the emitting signal frame, so it is
        // live for this call, and it was just checked to hold an enum — which
        // is exactly `g_value_get_enum`'s precondition.
        let event = unsafe { glib::gobject_ffi::g_value_get_enum(event.to_glib_none().0) };
        if event != WEBKIT_LOAD_FINISHED {
            return None;
        }
        let view = values.first()?.get::<gtk::Widget>().ok()?;
        // Take the queue out before evaluating: `WebKit::eval` re-enters
        // webkit, and holding the `RefCell` borrow across that would turn any
        // future re-entrant `evaluate` into a panic.
        let queued = pending.borrow_mut().take();
        for js in queued.unwrap_or_default() {
            webkit.eval(&view, &js);
        }
        None
    });
}

/// Build a synthetic `mousemove` dispatch at `(x, y)` browser pixels.
fn mouse_move_call(x: i32, y: i32) -> String {
    format!(
        "(function(){{var t=document.elementFromPoint({x},{y})||document;\
t.dispatchEvent(new MouseEvent('mousemove',\
{{clientX:{x},clientY:{y},bubbles:true,cancelable:true,view:window}}));}})();"
    )
}

/// Build a synthetic mouse button dispatch. `button` is the DOM button index
/// (0 = left, 2 = right); `down` selects `mousedown`+`click` vs `mouseup`.
fn mouse_button_call(x: i32, y: i32, button: i32, down: bool) -> String {
    let mut js = format!(
        "(function(){{var t=document.elementFromPoint({x},{y})||document;\
t.dispatchEvent(new MouseEvent('{ev}',\
{{clientX:{x},clientY:{y},button:{button},bubbles:true,cancelable:true,view:window}}));",
        ev = if down { "mousedown" } else { "mouseup" },
    );
    if !down {
        // A release completes a click.
        js.push_str(&format!(
            "t.dispatchEvent(new MouseEvent('click',\
{{clientX:{x},clientY:{y},button:{button},bubbles:true,cancelable:true,view:window}}));"
        ));
    }
    js.push_str("})();");
    js
}
