#![deny(unsafe_code)]

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod backend;
// Pure, so it is built for tests everywhere; only Windows uses it.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod desktop_tree;
mod error;
mod gpu;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{DesktopSurface, open_desktop, pump_desktop_events, set_battery_fps};
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
#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::set_battery_fps;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use backend::{Backend, Platform, PresentOptions};
pub use error::PlatformError;
pub use gpu::{
    attach_pipeline_cache, persist_pipeline_cache, pipeline_cache, pipeline_cache_feature, power_preference,
};
#[cfg(target_os = "linux")]
pub use renderer::CommandSender;
#[cfg(any(target_os = "macos", windows))]
pub use renderer::MakeViewFn;
#[cfg(windows)]
pub use renderer::PageView;
pub use renderer::{BuildFn, BuildLocalFn, InitialBuildFn, RenderCommand, RendererFactory};
pub use renderer::{
    CaptureFn, PropertyImpact, RedrawHint, RenderTarget, Renderer, RendererSnapshot, SnapshotFormat,
    SurfaceSize,
};
pub use test_pattern::TestPattern;
#[cfg(target_os = "linux")]
pub use x11::X11Mode;
