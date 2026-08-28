#![deny(unsafe_code)]

mod backend;
mod error;
mod gpu;
mod output;
mod platform;
mod pointer;
mod renderer;
mod snapshot;
mod test_pattern;
mod toplevel;
mod x11;

pub use backend::{Backend, Platform, PresentOptions};
pub use error::PlatformError;
pub use gpu::{
    attach_pipeline_cache, persist_pipeline_cache, pipeline_cache, pipeline_cache_feature, power_preference,
};
pub use renderer::{
    BuildFn, BuildLocalFn, CaptureFn, CommandSender, InitialBuildFn, PropertyImpact, RedrawHint,
    RenderCommand, RenderTarget, Renderer, RendererFactory, RendererSnapshot, SnapshotFormat, SurfaceSize,
};
pub use test_pattern::TestPattern;
pub use x11::X11Mode;
