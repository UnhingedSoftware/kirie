use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("failed to connect to the wayland display: {0}")]
    Connect(#[from] wayland_client::ConnectError),

    #[error("failed to enumerate wayland globals: {0}")]
    Globals(#[from] wayland_client::globals::GlobalError),

    #[error("required wayland global unavailable: {0}")]
    Bind(#[from] wayland_client::globals::BindError),

    #[error("wl_display pointer is null; the libwayland client backend is required")]
    NullDisplayPointer,

    #[error("wl_surface pointer is null; the surface was already destroyed")]
    NullSurfacePointer,

    #[error("failed to create wgpu surface: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),

    #[error("no compatible wgpu adapter (tried Vulkan, then all backends): {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),

    #[error("failed to create wgpu device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),

    #[error("adapter reports no supported configuration for output {output:?}")]
    UnsupportedSurface { output: String },

    #[error("event loop error: {0}")]
    EventLoop(#[from] smithay_client_toolkit::reexports::calloop::Error),

    #[error("failed to register wayland source in the event loop: {0}")]
    EventLoopRegister(String),

    #[error("failed to connect to the X display: {0}")]
    X11Connect(String),

    #[error("xcb_connection_t pointer is null; the libxcb (xcb_ffi) backend is required")]
    NullXcbConnection,

    #[error("X11 protocol error: {0}")]
    X11Protocol(String),

    #[error("no active RANDR CRTC found; nothing to render on")]
    NoCrtcs,
}
