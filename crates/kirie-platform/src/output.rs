use std::time::Instant;

use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::renderer::{Renderer, SurfaceSize};

pub(crate) struct OutputContext {
    pub wgpu_surface: Option<wgpu::Surface<'static>>,
    pub layer: LayerSurface,
    pub wl_output: WlOutput,
    pub name: String,
    pub scale: u32,
    pub logical_size: (u32, u32),
    pub physical_size: SurfaceSize,
    pub configured: bool,
    pub frame_pending: bool,
    pub timer_armed: bool,
    pub static_content: bool,
    pub paused: bool,
    pub paused_at: Option<std::time::Instant>,
    pub released: bool,
    pub initial_build_pending: bool,
    pub first_frame_presented: bool,
    pub renderer: Option<Box<dyn Renderer>>,
    pub last_frame: Option<Instant>,
    pub format: Option<wgpu::TextureFormat>,
    pub position: (i32, i32),
}

impl OutputContext {
    pub fn wl_surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn update_physical_size(&mut self) {
        self.physical_size = SurfaceSize {
            width: self.logical_size.0.saturating_mul(self.scale).max(1),
            height: self.logical_size.1.saturating_mul(self.scale).max(1),
        };
    }
}
