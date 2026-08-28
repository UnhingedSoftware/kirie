use std::sync::Arc;

use arc_swap::ArcSwapOption;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
}

impl PixelFormat {
    #[must_use]
    pub fn wgpu_srgb(self) -> wgpu::TextureFormat {
        match self {
            PixelFormat::Bgra8 => wgpu::TextureFormat::Bgra8UnormSrgb,
            PixelFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }
}

#[derive(Debug)]
pub struct FrameBuffer {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

impl FrameBuffer {
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.data.len() == (self.width as usize) * (self.height as usize) * 4
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WebFrameRef<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSize {
    pub width: u32,
    pub height: u32,
}

impl WebSize {
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PointerState {
    pub x: i32,
    pub y: i32,
    pub left: bool,
    pub right: bool,
}

pub type FrameSlot = Arc<ArcSwapOption<FrameBuffer>>;

pub trait WebBackend: Send {
    fn produces_frames(&self) -> bool {
        true
    }

    fn new(url: &str, size: WebSize) -> Result<Self, WebError>
    where
        Self: Sized;

    fn tick(&mut self, dt: f32);

    fn latest_frame(&self) -> Option<WebFrameRef<'_>>;

    fn resize(&mut self, size: WebSize);

    fn send_pointer(&mut self, pointer: PointerState);

    fn set_muted(&mut self, muted: bool);

    fn set_power_save(&mut self, _on: bool) {}

    fn apply_properties(&mut self, _json: &str) {}

    fn push_audio(&mut self, _bands: &[f32]) {}

    fn push_media(&mut self, _channel: crate::feed::MediaChannel, _json: &str) {}

    fn snapshot(&mut self) -> Option<FrameBuffer> {
        None
    }

    fn shutdown(&mut self);
}

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("web backend `{0}` is not compiled in (enable its cargo feature)")]
    BackendDisabled(&'static str),

    #[error("failed to initialize the CEF browser context: {0}")]
    Init(String),

    #[error("failed to create the CEF browser")]
    BrowserCreation,

    #[error("invalid wallpaper url: {0}")]
    Url(String),

    #[error("failed to start the browser thread: {0}")]
    Thread(String),
}

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
        let egl = "/usr/share/glvnd/egl_vendor.d/10_nvidia.json";
        if std::path::Path::new(egl).is_file() {
            env.push(("__EGL_VENDOR_LIBRARY_FILENAMES", egl.to_owned()));
        }
    }

    if let Some(node) = render_node_for_vendor(vendor_id) {
        env.push(("WEBKIT_WEB_RENDER_DEVICE_FILE", node));
        env.push(("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "0".to_owned()));
    }
    env
}

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
