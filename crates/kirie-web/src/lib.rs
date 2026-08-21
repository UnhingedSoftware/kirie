//! kirie-web — web wallpapers via an off-screen (headless) browser.
//!
//! A "web" wallpaper is an HTML/CSS/JS bundle (`project.json` `"type":"web"`,
//! entry `index.html`) rendered by an embedded browser in **windowless /
//! off-screen** mode: the browser paints into a CPU buffer, which the GPU side
//! uploads to a texture and blits fullscreen. See docs/subsystems-misc.md §3
//! (WebBrowser / CEF) for the reference C++ behaviour this ports.
//!
//! # Layout
//!
//! * [`backend`] — the engine-neutral [`WebBackend`] trait plus its frame /
//!   pointer / error types. Shared by every browser backend.
//! * [`feed`] — the live audio-spectrum + MPRIS now-playing data a page's
//!   `wallpaperRegister*Listener` callbacks expect, plus the rate-limiting,
//!   diffing pump that delivers it and the host wire format it travels on.
//! * [`renderer`] — [`WebRenderer`], a [`kirie_platform::Renderer`] that
//!   presents whatever a [`WebBackend`] paints.
//! * `cef` (feature `cef`) — the Chromium Embedded Framework OSR backend
//!   ([`cef::CefBackend`]) and the JS shim WE web wallpapers expect. Heavy
//!   (downloads + cmake-builds libcef); **compiled only with `--features
//!   cef`** so the default `cargo build --workspace` stays green on machines
//!   without libcef.
//!
//! # Safety (SPEC §V2)
//!
//! The default build carries `#![forbid(unsafe_code)]`. The `cef` module
//! cannot: CEF is a C ABI and every callback/handoff is `unsafe`. That module
//! locally relaxes the ban and `// SAFETY`-annotates each FFI touch; the ban
//! stays in force for the rest of the crate.

#![cfg_attr(not(any(feature = "cef", feature = "webview")), forbid(unsafe_code))]

pub mod backend;
pub mod feed;
pub mod renderer;
pub mod shim;

#[cfg(feature = "cef")]
pub mod cef;

/// Out-of-process web host client (feature `host`): spawns `kirie-webhost`,
/// maps its frame shm, kills it on drop — full browser-runtime reclaim.
#[cfg(feature = "host")]
pub mod hosted;

/// The system-`webkit2gtk` native-surface backend (feature `webview`).
///
/// webkit2gtk has no off-screen/pixel-readback path (upstream won't-fix) and
/// its objects are `!Send` (GTK main-thread-bound), so this backend can never
/// be the composited, frame-publishing [`WebBackend`] — see [`webview`] for
/// the evidence (wry 0.55.1 API survey). Instead it renders **natively**: a
/// separate host process ([`webview::host`]) owns a gtk-layer-shell window on
/// the compositor's background layer and webkit paints straight into it. The
/// engine talks to that process through [`viewhost`]. webkit is reached by
/// `dlopen` rather than by linking, so one binary works against both
/// `webkit2gtk-4.1` and `webkit2gtk-4.0`; that C-ABI boundary is why the
/// crate-level `forbid(unsafe_code)` is relaxed for this feature too.
#[cfg(feature = "webview")]
pub mod webview;

/// Out-of-process webview host client (feature `webview-client`): starts the
/// host (which owns webkit + a background-layer window) and drives it over
/// stdin. This feature alone links no browser or gtk; an engine that also
/// enables `webview` hosts itself by re-executing, so it ships as one file,
/// while `webview-client` on its own spawns a sibling `kirie-webviewhost`.
#[cfg(feature = "webview-client")]
pub mod viewhost;

pub use backend::{FrameBuffer, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize};
pub use feed::{FeedPump, MediaChannel, MediaPalette, MediaSnapshot, WebFeed};
pub use renderer::WebRenderer;
