use std::time::Duration;

use crate::error::PlatformError;
use crate::platform::WaylandPlatform;
use crate::renderer::RendererFactory;
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Wayland,
    X11,
}

impl Backend {
    #[must_use]
    pub fn from_env() -> Self {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Backend::Wayland
        } else if std::env::var_os("DISPLAY").is_some() {
            Backend::X11
        } else {
            Backend::Wayland
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub enum Platform {
    Wayland(WaylandPlatform),
    X11(X11Platform),
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
            Backend::Wayland => Ok(Self::Wayland(WaylandPlatform::connect_with(
                make_renderer,
                options,
            )?)),
            Backend::X11 => Self::connect_x11(X11Mode::Desktop, make_renderer),
        }
    }

    pub fn connect_x11(mode: X11Mode, make_renderer: RendererFactory) -> Result<Self, PlatformError> {
        Ok(Self::X11(X11Platform::connect(mode, make_renderer)?))
    }

    #[must_use]
    pub fn output_count(&self) -> usize {
        match self {
            Self::Wayland(p) => p.output_count(),
            Self::X11(p) => p.output_count(),
        }
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        match self {
            Self::Wayland(p) => p.surface_count(),
            Self::X11(p) => p.surface_count(),
        }
    }

    #[must_use]
    pub fn command_sender(&self) -> Option<crate::renderer::CommandSender> {
        match self {
            Self::Wayland(p) => Some(p.command_sender()),
            Self::X11(_) => None,
        }
    }

    pub fn set_initial_build(&mut self, f: crate::renderer::InitialBuildFn) {
        match self {
            Self::Wayland(p) => p.set_initial_build(f),
            Self::X11(_) => {}
        }
    }

    pub fn run(&mut self, duration: Option<Duration>) -> Result<(), PlatformError> {
        match self {
            Self::Wayland(p) => p.run(duration),
            Self::X11(p) => p.run(duration),
        }
    }
}
