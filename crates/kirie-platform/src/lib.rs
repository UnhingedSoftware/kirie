#![deny(unsafe_code)]

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod backend;
mod error;
mod gpu;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{DesktopSurface, open_desktop, pump_desktop_events};
#[cfg(target_os = "linux")]
mod output;
#[cfg(target_os = "linux")]
mod platform;
#[cfg(target_os = "linux")]
mod pointer;
mod renderer;
#[cfg(target_os = "linux")]
mod snapshot;
mod test_pattern;
#[cfg(target_os = "linux")]
mod toplevel;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use backend::{Backend, Platform, PresentOptions};
pub use error::PlatformError;
pub use gpu::{
    attach_pipeline_cache, persist_pipeline_cache, pipeline_cache, pipeline_cache_feature, power_preference,
};
pub use renderer::RendererFactory;
#[cfg(target_os = "linux")]
pub use renderer::{BuildFn, BuildLocalFn, CommandSender, InitialBuildFn, RenderCommand};
pub use renderer::{
    CaptureFn, PropertyImpact, RedrawHint, RenderTarget, Renderer, RendererSnapshot, SnapshotFormat,
    SurfaceSize,
};
pub use test_pattern::TestPattern;
#[cfg(target_os = "linux")]
pub use x11::X11Mode;
