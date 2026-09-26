use std::time::Duration;

use crate::error::PlatformError;
#[cfg(target_os = "linux")]
use crate::platform::WaylandPlatform;
use crate::renderer::RendererFactory;
#[cfg(windows)]
use crate::windows::WindowsPlatform;
#[cfg(target_os = "linux")]
use crate::x11::{X11Mode, X11Platform};

#[derive(Debug, Clone)]
pub struct PresentOptions {
    pub layer_namespace: String,
    pub screen_roots: Vec<String>,
    pub fps: Option<u32>,
    pub playback_speed: f64,
    pub fullscreen_pause: bool,
    pub fullscreen_pause_only_active: bool,
    pub fullscreen_pause_ignore_appids: Vec<String>,
    pub release_hidden_after: Option<Duration>,
    pub activity_paused: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub pointer: bool,
    pub take_clicks: bool,
    /// The GPU to draw on, as a word from `kirie gpus`. Only Windows reads it;
    /// elsewhere `--gpu` has already pinned the Vulkan driver by the time the
    /// platform starts.
    pub gpu: Option<String>,
}

impl Default for PresentOptions {
    fn default() -> Self {
        Self {
            layer_namespace: "linux-wallpaperengine".to_string(),
            screen_roots: Vec::new(),
            fps: None,
            playback_speed: 1.0,
            fullscreen_pause: true,
            fullscreen_pause_only_active: false,
            fullscreen_pause_ignore_appids: Vec::new(),
            release_hidden_after: None,
            activity_paused: None,
            pointer: true,
            take_clicks: false,
            gpu: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    #[cfg(target_os = "linux")]
    Wayland,
    #[cfg(target_os = "linux")]
    X11,
    #[cfg(target_os = "macos")]
    Mac,
    #[cfg(windows)]
    Windows,
}

impl Backend {
    #[must_use]
    pub fn from_env() -> Self {
        #[cfg(windows)]
        {
            Backend::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Backend::Mac
        }
        #[cfg(target_os = "linux")]
        {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                Backend::Wayland
            } else if std::env::var_os("DISPLAY").is_some() {
                Backend::X11
            } else {
                Backend::Wayland
            }
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub enum Platform {
    #[cfg(target_os = "linux")]
    Wayland(WaylandPlatform),
    #[cfg(target_os = "linux")]
    X11(X11Platform),
    #[cfg(target_os = "macos")]
    Mac(crate::macos::MacPlatform),
    #[cfg(windows)]
    Windows(WindowsPlatform),
}

impl Platform {
    pub fn connect(make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Self::connect_backend(Backend::from_env(), make_renderer)
    }

    pub fn connect_backend(backend: Backend, make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Self::connect_with(backend, PresentOptions::default(), make_renderer)
    }

    pub fn connect_with(
        backend: Backend,
        options: PresentOptions,
        make_renderer: RendererFactory,
    ) -> Result<Self, PlatformError> {
        match backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland => Ok(Self::Wayland(WaylandPlatform::connect_with(
                make_renderer,
                options,
            )?)),
            #[cfg(target_os = "linux")]
            Backend::X11 => Self::connect_x11(X11Mode::Desktop, make_renderer),
            #[cfg(target_os = "macos")]
            Backend::Mac => Ok(Self::Mac(crate::macos::MacPlatform::connect_with(
                make_renderer,
                options,
            )?)),
            #[cfg(windows)]
            Backend::Windows => Ok(Self::Windows(WindowsPlatform::connect_with(
                make_renderer,
                options,
            )?)),
        }
    }

    #[cfg(target_os = "linux")]
    pub fn connect_x11(mode: X11Mode, make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Ok(Self::X11(X11Platform::connect(mode, make_renderer)?))
    }

    #[must_use]
    pub fn output_count(&self) -> usize {
        match self {
            #[cfg(target_os = "linux")]
            Self::Wayland(p) => p.output_count(),
            #[cfg(target_os = "linux")]
            Self::X11(p) => p.output_count(),
            #[cfg(target_os = "macos")]
            Self::Mac(p) => p.output_count(),
            #[cfg(windows)]
            Self::Windows(p) => p.output_count(),
        }
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        match self {
            #[cfg(target_os = "linux")]
            Self::Wayland(p) => p.surface_count(),
            #[cfg(target_os = "linux")]
            Self::X11(p) => p.surface_count(),
            #[cfg(target_os = "macos")]
            Self::Mac(p) => p.surface_count(),
            #[cfg(windows)]
            Self::Windows(p) => p.surface_count(),
        }
    }

    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn command_sender(&self) -> Option<crate::renderer::CommandSender> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Wayland(p) => Some(p.command_sender()),
            #[cfg(target_os = "linux")]
            Self::X11(_) => None,
            #[cfg(target_os = "macos")]
            Self::Mac(_) => None,
            #[cfg(windows)]
            Self::Windows(_) => None,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn set_initial_build(&mut self, f: crate::renderer::InitialBuildFn) {
        match self {
            #[cfg(target_os = "linux")]
            Self::Wayland(p) => p.set_initial_build(f),
            #[cfg(target_os = "linux")]
            Self::X11(_) => {}
            #[cfg(target_os = "macos")]
            Self::Mac(_) => {}
            #[cfg(windows)]
            Self::Windows(_) => {}
        }
    }

    /// Where to send commands on the backends that have no event loop of their
    /// own to wake -- everything but Wayland, which has calloop.
    #[cfg(any(target_os = "macos", windows))]
    #[must_use]
    pub fn orders(&self) -> std::sync::mpsc::Sender<crate::renderer::RenderCommand> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Mac(p) => p.orders(),
            #[cfg(windows)]
            Self::Windows(p) => p.orders(),
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    #[must_use]
    pub fn screen_names(&self) -> Vec<String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Mac(p) => p.screen_names(),
            #[cfg(windows)]
            Self::Windows(p) => p.screen_names(),
        }
    }

    pub fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Wayland(p) => p.run(duration),
            #[cfg(target_os = "linux")]
            Self::X11(p) => p.run(duration),
            #[cfg(target_os = "macos")]
            Self::Mac(p) => p.run(duration),
            #[cfg(windows)]
            Self::Windows(p) => p.run(duration),
        }
    }
}
