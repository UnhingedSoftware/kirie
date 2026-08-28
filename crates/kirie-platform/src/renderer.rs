#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

pub struct RenderTarget<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub format: wgpu::TextureFormat,
    pub output_name: &'a str,
    pub size: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedrawHint {
    Unknown,
    Static,
    After(std::time::Duration),
}

pub trait Renderer {
    fn render(&mut self, view: &wgpu::TextureView, size: SurfaceSize, dt: f32);

    fn is_passive(&self) -> bool {
        false
    }

    fn poll(&mut self) {}

    fn set_property(&mut self, _key: &str, _value: &str) -> PropertyImpact {
        PropertyImpact::Live
    }

    fn set_pointer(&mut self, _x: f32, _y: f32) {}

    fn redraw_hint(&self) -> RedrawHint {
        RedrawHint::Unknown
    }

    fn set_pointer_buttons(&mut self, _left_down: bool) {}

    fn snapshot(&mut self) -> Option<RendererSnapshot> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFormat {
    Bgra8,
    Rgba8,
}

impl SnapshotFormat {
    #[must_use]
    pub fn wgpu_srgb(self) -> wgpu::TextureFormat {
        match self {
            SnapshotFormat::Bgra8 => wgpu::TextureFormat::Bgra8UnormSrgb,
            SnapshotFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }
}

#[derive(Debug)]
pub struct RendererSnapshot {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: SnapshotFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyImpact {
    Live,
    NeedsRebuild,
}

pub type RendererFactory = Box<dyn FnMut(&RenderTarget<'_>) -> Box<dyn Renderer>>;

pub type BuildFn = Box<
    dyn FnOnce(&wgpu::Device, &wgpu::Queue, wgpu::TextureFormat, &str, (u32, u32)) -> Box<dyn Renderer + Send>
        + Send,
>;

pub type InitialBuildFn = Box<dyn FnMut(&str) -> Option<BuildFn>>;

pub type BuildLocalFn = Box<
    dyn FnOnce(&wgpu::Device, &wgpu::Queue, wgpu::TextureFormat, &str, (u32, u32)) -> Box<dyn Renderer>
        + Send,
>;

pub type CaptureFn =
    Box<dyn FnOnce(&wgpu::Device, &wgpu::Queue, &mut dyn Renderer, SurfaceSize, wgpu::TextureFormat) + Send>;

pub type CommandSender = smithay_client_toolkit::reexports::calloop::channel::Sender<RenderCommand>;

pub enum RenderCommand {
    Build {
        screen: String,
        stash: Option<String>,
        build: BuildFn,
    },
    Swap {
        screen: String,
        key: String,
        build: BuildFn,
    },
    Install {
        screen: String,
        stash: Option<String>,
        renderer: Box<dyn Renderer + Send>,
    },
    Screenshot {
        screen: String,
        capture: CaptureFn,
    },
    SwapLocal {
        screen: String,
        build_local: BuildLocalFn,
    },
    SetProperty {
        screen: String,
        key: String,
        value: String,
        structural: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    SetFps(Option<u32>),
    SetSpeed(f32),
}
