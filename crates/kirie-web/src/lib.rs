#![cfg_attr(
    not(any(feature = "cef", feature = "webview", feature = "webview-mac")),
    forbid(unsafe_code)
)]

pub mod backend;
pub mod feed;
pub mod renderer;
pub mod shim;

#[cfg(feature = "cef")]
pub mod cef;

#[cfg(feature = "host")]
pub mod hosted;

#[cfg(feature = "webview")]
pub mod webview;

#[cfg(all(target_os = "macos", feature = "webview-mac"))]
pub mod wk;

#[cfg(feature = "webview-client")]
pub mod viewhost;

pub use backend::{
    FrameBuffer, OffscreenWeb, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize,
};
pub use feed::{FeedPump, MediaChannel, MediaPalette, MediaSnapshot, WebFeed};
pub use renderer::WebRenderer;
