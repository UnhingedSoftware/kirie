use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::glib::translate::{ToGlibPtr as _, from_glib_none};
use gtk::prelude::*;

use crate::backend::{PointerState, WebError, WebFrameRef, WebSize};
use crate::shim;

use super::webkit_sys::WebKit;

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

const MAX_PENDING: usize = 64;

fn init_script(muted: bool) -> String {
    let mute = MUTE_INIT.replace("MUTESTATE", if muted { "true" } else { "false" });
    format!("{}\n{}", shim::BRIDGE_INIT, mute)
}

pub struct WebviewBackend {
    view: Option<gtk::Widget>,
    webkit: &'static WebKit,
    size: WebSize,
    muted: bool,
    last_pointer: PointerState,
    pending: Rc<RefCell<Option<Vec<String>>>>,
}

impl WebviewBackend {
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn latest_frame(&self) -> Option<WebFrameRef<'_>> {
        None
    }

    #[allow(clippy::unused_self)]
    pub fn tick(&mut self, _dt: f32) {}

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

    pub fn push_audio(&mut self, bands: &[f32]) {
        self.evaluate_transient(&shim::audio_call(bands));
    }

    pub fn push_media(&mut self, channel: crate::feed::MediaChannel, json: &str) {
        self.evaluate(&channel.call(json));
    }

    pub fn apply_user_properties(&mut self, json: &str) {
        self.evaluate(&shim::apply_user_properties_call(json));
    }

    pub fn apply_general_properties(&mut self, json: &str) {
        self.evaluate(&shim::apply_general_properties_call(json));
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        let state = if muted { "true" } else { "false" };
        self.evaluate(&format!(
            "window.__wpMuteState={state};if(window.__wpSweepMute){{window.__wpSweepMute();}}"
        ));
    }

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

    pub fn with_gtk_container(
        url: &str,
        size: WebSize,
        container: &impl gtk::prelude::IsA<gtk::Container>,
        muted: bool,
    ) -> Result<Self, WebError> {
        let size = size.clamped();
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

        webkit.minimize_caches();

        let raw = webkit.new_web_view(Some(&init_script(muted)));
        if raw.is_null() {
            tracing::error!(url, soname = webkit.soname(), "webkit returned no web view");
            return Err(WebError::BrowserCreation);
        }
        // SAFETY: `raw` is a non-null, freshly constructed `WebKitWebView` —
        let view: gtk::Widget = unsafe { from_glib_none(raw) };

        webkit.set_background_color(&view, [0.0, 0.0, 0.0, 1.0]);
        webkit.set_autoplay(&view, true);
        if std::env::var_os("KIRIE_WEB_CONSOLE").is_some() {
            webkit.write_console_to_stdout(&view);
        }

        view.set_hexpand(true);
        view.set_vexpand(true);
        if let Some(gtk_box) = container.as_ref().downcast_ref::<gtk::Box>() {
            gtk_box.pack_start(&view, true, true, 0);
        } else {
            container.as_ref().add(&view);
        }
        view.show_all();

        let pending = Rc::new(RefCell::new(Some(Vec::new())));
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
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * stride) as usize;
            out.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
        std::fs::write(path, &out).ok()?;
        Some((w, h))
    }

    pub fn shutdown(&mut self) {
        let Some(view) = self.view.take() else {
            return;
        };
        // SAFETY: `gtk_widget_destroy` is `unsafe` in gtk-rs because it
        unsafe { view.destroy() };
    }

    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    fn evaluate(&self, js: &str) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
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

    fn evaluate_transient(&self, js: &str) {
        let Some(view) = self.view.as_ref() else {
            return;
        };
        if self.pending.borrow().is_some() {
            return;
        }
        self.webkit.eval(view, js);
    }
}

impl Drop for WebviewBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn flush_pending_on_commit(
    view: &gtk::Widget,
    webkit: &'static WebKit,
    pending: &Rc<RefCell<Option<Vec<String>>>>,
) {
    const WEBKIT_LOAD_FINISHED: i32 = 3;

    let Some(signal) = glib::subclass::signal::SignalId::lookup("load-changed", view.type_()) else {
        tracing::warn!("webkit view exposes no `load-changed` signal; not buffering scripts");
        pending.borrow_mut().take();
        return;
    };

    let pending = pending.clone();
    let _handler = view.connect_local_id(signal, None, false, move |values| {
        let event = values.get(1)?;
        if !event.type_().is_a(glib::Type::ENUM) {
            return None;
        }
        // SAFETY: the `GValue` belongs to the emitting signal frame, so it is
        let event = unsafe { glib::gobject_ffi::g_value_get_enum(event.to_glib_none().0) };
        if event != WEBKIT_LOAD_FINISHED {
            return None;
        }
        let view = values.first()?.get::<gtk::Widget>().ok()?;
        let queued = pending.borrow_mut().take();
        let n = queued.as_ref().map_or(0, Vec::len);
        tracing::debug!(scripts = n, "load finished; flushing queued scripts");
        for js in queued.unwrap_or_default() {
            webkit.eval(&view, &js);
        }
        None
    });
}

fn mouse_move_call(x: i32, y: i32) -> String {
    format!(
        "(function(){{var t=document.elementFromPoint({x},{y})||document;\
t.dispatchEvent(new MouseEvent('mousemove',\
{{clientX:{x},clientY:{y},bubbles:true,cancelable:true,view:window}}));}})();"
    )
}

fn mouse_button_call(x: i32, y: i32, button: i32, down: bool) -> String {
    let mut js = format!(
        "(function(){{var t=document.elementFromPoint({x},{y})||document;\
t.dispatchEvent(new MouseEvent('{ev}',\
{{clientX:{x},clientY:{y},button:{button},bubbles:true,cancelable:true,view:window}}));",
        ev = if down { "mousedown" } else { "mouseup" },
    );
    if !down {
        js.push_str(&format!(
            "t.dispatchEvent(new MouseEvent('click',\
{{clientX:{x},clientY:{y},button:{button},bubbles:true,cancelable:true,view:window}}));"
        ));
    }
    js.push_str("})();");
    js
}
