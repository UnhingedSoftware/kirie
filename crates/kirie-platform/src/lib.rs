#![deny(unsafe_code)]

#[cfg(target_os = "linux")]
mod backend;
mod error;
mod gpu;
#[cfg(target_os = "linux")]
mod output;
#[cfg(target_os = "linux")]
mod platform;
#[cfg(target_os = "linux")]
mod pointer;
mod renderer;
mod snapshot;
mod test_pattern;
#[cfg(target_os = "linux")]
mod toplevel;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub use backend::{Backend, Platform, PresentOptions};
pub use error::PlatformError;
pub use gpu::{
    attach_pipeline_cache, persist_pipeline_cache, pipeline_cache, pipeline_cache_feature, power_preference,
};
#[cfg(target_os = "linux")]
pub use renderer::{BuildFn, BuildLocalFn, CommandSender, InitialBuildFn, RenderCommand, RendererFactory};
pub use renderer::{
    CaptureFn, PropertyImpact, RedrawHint, RenderTarget, Renderer, RendererSnapshot, SnapshotFormat,
    SurfaceSize,
};
pub use test_pattern::TestPattern;
#[cfg(target_os = "linux")]
pub use x11::X11Mode;
