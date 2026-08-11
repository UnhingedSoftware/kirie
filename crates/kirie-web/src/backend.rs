//! Backend-neutral web-wallpaper contract.
//!
//! A web wallpaper is a headless browser rendered off-screen: the browser
//! paints into a CPU pixel buffer that the GPU side uploads to a texture and
//! blits fullscreen (docs/subsystems-misc.md §3, "offscreen (windowless)
//! rendering"). The Chromium Embedded Framework OSR backend ([`crate::cef`],
//! feature `cef`) produces those buffers behind the single [`WebBackend`]
//! trait defined here, so the presentation layer never learns which browser
//! engine is in use. It is the **only** implementor: the `webview`
//! (wry/webkit2gtk) backend cannot satisfy this contract — webkit2gtk has no
//! off-screen/pixel-readback path and its views are `!Send` (upstream
//! limitation; see the `webview` module docs) — so it lives outside the trait
//! as a native-surface fallback.
//!
//! Threading (SPEC §V4 "render never blocks"): the browser runs its own
//! message loop on a dedicated thread and publishes finished frames through a
//! lock-free [`arc_swap`] slot. The render thread only ever *reads* the latest
//! published frame — it never waits on the browser.

use std::sync::Arc;

use arc_swap::ArcSwapOption;

/// Pixel layout of a published [`FrameBuffer`].
///
/// CEF's `OnPaint` hands back a top-left-origin, tightly packed 32-bit buffer
/// in **BGRA** order (docs/subsystems-misc.md §3.5: `GL_BGRA_EXT`), so the CEF
/// backend always reports [`PixelFormat::Bgra8`]. The enum exists so a future
/// RGBA-producing backend (some `wry` paths) can be uploaded with the correct
/// swizzle without a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Byte order B, G, R, A — the CEF OSR native format.
    Bgra8,
    /// Byte order R, G, B, A.
    Rgba8,
}

impl PixelFormat {
    /// The wgpu texture format that samples this layout as linear colour.
    ///
    /// Web content emits sRGB-encoded bytes; sampling through the matching
    /// `*UnormSrgb` format lets the GPU linearise on read so the fullscreen
    /// blit is colour-correct.
    #[must_use]
    pub fn wgpu_srgb(self) -> wgpu::TextureFormat {
        match self {
            PixelFormat::Bgra8 => wgpu::TextureFormat::Bgra8UnormSrgb,
            PixelFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }
}

/// One finished, immutable browser frame.
///
/// Owns its pixels so it can outlive the browser's paint callback and be
/// handed across the arc-swap slot to the render thread without copying on
/// read.
#[derive(Debug)]
pub struct FrameBuffer {
    /// Tightly packed pixels, `width * height * 4` bytes, top-left origin.
    pub data: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Byte order of [`FrameBuffer::data`].
    pub format: PixelFormat,
}

impl FrameBuffer {
    /// `true` when a byte count is consistent with `width * height * 4`.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.data.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

/// A borrowed view of the most recently published frame.
///
/// Returned by [`WebBackend::latest_frame`]. Carries the dimensions and
/// [`PixelFormat`] alongside the byte slice because an uploader needs all
/// three; the slice alone (the raw `&[u8]` the task sketches) is not enough to
/// build a texture.
#[derive(Debug, Clone, Copy)]
pub struct WebFrameRef<'a> {
    /// The frame pixels.
    pub data: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Byte order of [`WebFrameRef::data`].
    pub format: PixelFormat,
}

/// Physical off-screen size a web wallpaper renders at, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSize {
    /// Width in pixels (min 1).
    pub width: u32,
    /// Height in pixels (min 1).
    pub height: u32,
}

impl WebSize {
    /// Clamp both axes to at least 1 — CEF rejects a zero-area view rect.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }
}

/// A pointer sample forwarded to the page, in browser (top-left origin) pixels.
///
/// Mirrors the C++ `CWeb` input path (docs/subsystems-misc.md §3.5): position
/// every frame, plus discrete left/right button state. Buttons are absolute
/// state (pressed = `true`); the backend derives click/release edges.
#[derive(Debug, Clone, Copy, Default)]
pub struct PointerState {
    /// X in browser pixels (0 = left edge).
    pub x: i32,
    /// Y in browser pixels (0 = top edge).
    pub y: i32,
    /// Left button held.
    pub left: bool,
    /// Right button held.
    pub right: bool,
}

/// The shared lock-free slot a browser thread publishes frames into.
///
/// `None` until the first `OnPaint`. Producers store a fresh `Arc`; consumers
/// `load_full()` a cheap ref-counted handle.
pub type FrameSlot = Arc<ArcSwapOption<FrameBuffer>>;

/// The one contract every browser backend implements.
///
/// Object-safe on purpose: the presentation layer holds a
/// `Box<dyn WebBackend>` and never names the concrete engine. `new` is the
/// sole non-object-safe method and is therefore a free associated function
/// with a `Self: Sized` bound.
pub trait WebBackend: Send {
    /// Whether this backend hands the engine frames to composite.
    ///
    /// `true` for CEF (off-screen BGRA buffers). `false` for the webview host,
    /// where webkit paints its own layer-shell window and `latest_frame` is
    /// always `None` — the engine's surface has nothing to show, so it must
    /// stop presenting entirely (see `Renderer::is_passive`).
    fn produces_frames(&self) -> bool {
        true
    }

    /// Launch a browser on `url` rendering off-screen at `size`.
    ///
    /// `url` should be a `file://` URL to the wallpaper's entry page so the
    /// page's relative `css`/`js`/`img` references resolve against its own
    /// directory (docs/subsystems-misc.md §3.4).
    fn new(url: &str, size: WebSize) -> Result<Self, WebError>
    where
        Self: Sized;

    /// Advance one presentation step. `dt` is seconds since the previous tick.
    ///
    /// Cheap and non-blocking: it refreshes the backend's cached handle on the
    /// latest published frame and pumps any backend-internal bookkeeping. It
    /// does **not** drive the browser message loop (that runs on the browser's
    /// own thread) and never waits on it (SPEC §V4).
    fn tick(&mut self, dt: f32);

    /// Borrow the most recently published frame, or `None` before first paint.
    fn latest_frame(&self) -> Option<WebFrameRef<'_>>;

    /// Resize the off-screen surface; takes effect on the next browser paint.
    fn resize(&mut self, size: WebSize);

    /// Forward a pointer sample to the page.
    fn send_pointer(&mut self, pointer: PointerState);

    /// Mute or unmute the page's audio.
    fn set_muted(&mut self, muted: bool);

    /// On-battery hint: a composited backend may drop its internal paint
    /// rate (CEF's windowless frame rate); a native-surface backend has
    /// nothing to throttle (the compositor paces the page) and ignores it.
    fn set_power_save(&mut self, _on: bool) {}

    /// Deliver a user-properties batch to the page as a `__wpApplyProps({...})`
    /// call (the shim forwards to `wallpaperPropertyListener.applyUserProperties`,
    /// docs §3.5; reference `CWeb.cpp` sends the full set once on the first
    /// frame, then singles on live changes). `json` is the `{name:{value:..}}`
    /// object literal. Default: ignored (backends without script injection).
    fn apply_properties(&mut self, _json: &str) {}

    /// Push one audio-spectrum frame to the page's registered audio listeners
    /// (`shim::audio_call` → `window.__wpAudio`).
    ///
    /// `bands` is the mono FFT band array; a 64-entry slice is mirrored into
    /// the 128 floats (identical left+right channels) the reference delivers
    /// (docs/subsystems-misc.md §1.3, §3.5). Called from the render thread at
    /// [`crate::feed::AUDIO_INTERVAL`], so an implementation must not block.
    ///
    /// Default: ignored — correct for a backend with no script injection.
    fn push_audio(&mut self, _bands: &[f32]) {}

    /// Push one now-playing update to the page's registered media listeners.
    ///
    /// `json` is a single-line JSON object literal built by
    /// [`crate::feed`], spliced verbatim into the `window.__wpMedia*` call
    /// `channel` selects. Split by channel (rather than one fat event) because
    /// that is what lets the thumbnail — a base64 cover image, orders of
    /// magnitude larger than everything else — be sent only when the cover
    /// actually changes.
    ///
    /// Default: ignored.
    fn push_media(&mut self, _channel: crate::feed::MediaChannel, _json: &str) {}

    /// Draw a one-off still of the live page, for the engine to leave on the
    /// output when this wallpaper is released (`--release-hidden-after`).
    ///
    /// Default `None`, which is correct for every *composited* backend: the
    /// engine already holds their pixels, and the surface keeps its last
    /// committed buffer once we stop drawing, so there is nothing to stand in
    /// for. Only the native-surface webview backend needs this — webkit paints
    /// its own layer-shell window over the engine's (black) surface, so killing
    /// the host uncovers black for the whole release unless a still was left
    /// behind first.
    ///
    /// Called on the RENDER thread. An implementation that has to ask another
    /// process must bound its wait and return `None` on timeout; blocking here
    /// stalls every output, not just this one.
    fn snapshot(&mut self) -> Option<FrameBuffer> {
        None
    }

    /// Tear the browser down and stop its thread. Idempotent.
    fn shutdown(&mut self);
}

/// Errors a web backend can surface. Never panics on malformed input
/// (SPEC §V9); every failure path returns one of these.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// The requested backend was compiled out (its feature was disabled).
    #[error("web backend `{0}` is not compiled in (enable its cargo feature)")]
    BackendDisabled(&'static str),

    /// `CefInitialize` failed (missing libcef runtime files, bad flags, …).
    #[error("failed to initialize the CEF browser context: {0}")]
    Init(String),

    /// `CefBrowserHost::CreateBrowserSync` returned no browser.
    #[error("failed to create the CEF browser")]
    BrowserCreation,

    /// The URL could not be built from the requested path.
    #[error("invalid wallpaper url: {0}")]
    Url(String),

    /// The browser thread could not be started.
    #[error("failed to start the browser thread: {0}")]
    Thread(String),
}

/// GL device-selection environment for a web host child, from `KIRIE_GPU`.
///
/// `--gpu` pins kirie's own Vulkan device, but a browser host renders through
/// GL and chooses its device independently — on a hybrid machine that means
/// the default GPU, silently, no matter what the user selected. The engine
/// exports the selection as `KIRIE_GPU` when it pins itself; this translates
/// it into the offload variables the child's GL stack understands.
///
/// Only the NVIDIA PRIME offload variables are emitted today: they are
/// documented, stable, and strictly additive (an all-Mesa machine ignores
/// them). Mesa's `DRI_PRIME` is deliberately *not* set for `amd`/`intel`
/// tokens — it selects the *non-default* GPU, so setting it when the chosen
/// GPU already is the default would flip rendering onto the wrong one.
#[must_use]
pub fn gpu_offload_env() -> Vec<(&'static str, String)> {
    let vendor_id = match std::env::var("KIRIE_GPU").as_deref() {
        Ok("nvidia") => "0x10de",
        Ok("amd") => "0x1002",
        Ok("intel") => "0x8086",
        _ => return Vec::new(),
    };

    let mut env: Vec<(&'static str, String)> = Vec::new();
    if vendor_id == "0x10de" {
        env.push(("__NV_PRIME_RENDER_OFFLOAD", "1".to_owned()));
        env.push(("__GLX_VENDOR_LIBRARY_NAME", "nvidia".to_owned()));
        // EGL picks its vendor through glvnd; point it at the NVIDIA
        // manifest when present (WebKitGTK and CEF's Ozone path are EGL).
        let egl = "/usr/share/glvnd/egl_vendor.d/10_nvidia.json";
        if std::path::Path::new(egl).is_file() {
            env.push(("__EGL_VENDOR_LIBRARY_FILENAMES", egl.to_owned()));
        }
    }

    // WebKitGTK ignores the vendor variables — its DMA-BUF renderer opens a
    // DRM render node it enumerates itself — but honours an explicit node via
    // WEBKIT_WEB_RENDER_DEVICE_FILE. Resolve the selected vendor to its node
    // so the webview host lands on the same GPU the engine renders on.
    if let Some(node) = render_node_for_vendor(vendor_id) {
        env.push(("WEBKIT_WEB_RENDER_DEVICE_FILE", node));
        // Opening the node is not the same as painting on it: Skia falls back
        // to CPU raster on drivers it distrusts and still allocates its
        // buffers from the node, which looks GPU-bound in `lsof` while every
        // pixel is drawn by the CPU. Painting was explicitly requested onto
        // this device, so turn the CPU fallback off with it.
        env.push(("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "0".to_owned()));
    }
    env
}

/// The `/dev/dri/renderD*` node whose device reports `vendor_id`, if any.
fn render_node_for_vendor(vendor_id: &str) -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("renderD") {
            continue;
        }
        let vendor = std::fs::read_to_string(entry.path().join("device/vendor")).ok();
        if vendor.is_some_and(|v| v.trim().eq_ignore_ascii_case(vendor_id)) {
            let node = format!("/dev/dri/{name}");
            if std::path::Path::new(&node).exists() {
                return Some(node);
            }
        }
    }
    None
}
