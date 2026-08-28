#![cfg_attr(not(any(feature = "cef", feature = "webview")), forbid(unsafe_code))]

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

#[cfg(feature = "webview-client")]
pub mod viewhost;

pub use backend::{FrameBuffer, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize};
pub use feed::{FeedPump, MediaChannel, MediaPalette, MediaSnapshot, WebFeed};
pub use renderer::WebRenderer;
