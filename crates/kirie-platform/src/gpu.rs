#[cfg(target_os = "linux")]
use std::ptr::NonNull;
use std::sync::OnceLock;

#[cfg(target_os = "linux")]
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
#[cfg(target_os = "linux")]
use wayland_client::protocol::wl_surface::WlSurface;
#[cfg(target_os = "linux")]
use wayland_client::{Connection, Proxy};

#[cfg(target_os = "linux")]
use crate::error::PlatformError;

pub fn power_preference() -> wgpu::PowerPreference {
    if std::env::var_os("KIRIE_GPU").is_some() || std::env::var_os("KIRIE_GPU_PINNED").is_some() {
        wgpu::PowerPreference::None
    } else {
        wgpu::PowerPreference::LowPower
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

static SHARED_PIPELINE_CACHE: OnceLock<wgpu::PipelineCache> = OnceLock::new();

#[must_use]
pub fn pipeline_cache() -> Option<&'static wgpu::PipelineCache> {
    SHARED_PIPELINE_CACHE.get()
}

fn pipeline_cache_file(adapter: &wgpu::Adapter) -> Option<std::path::PathBuf> {
    let info = adapter.get_info();
    let key: String = format!("{}-{}-{}", info.name, info.driver, info.backend)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(
        cache_home()?
            .join("kirie")
            .join("pipelines")
            .join(format!("{key}.bin")),
    )
}

/// The per-user cache directory, under whatever name this platform gives it.
///
/// Windows sets neither `XDG_CACHE_HOME` nor `HOME`, so asking only for those
/// found nothing and the pipeline cache was never written or read there.
#[cfg(windows)]
fn cache_home() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

#[cfg(unix)]
fn cache_home() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| std::path::PathBuf::from(home).join(".cache"))
        })
}

#[must_use]
pub fn pipeline_cache_feature(adapter: &wgpu::Adapter) -> wgpu::Features {
    let opt_out = std::env::var_os("KIRIE_NO_PIPELINE_CACHE").is_some()
        || adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    if opt_out {
        wgpu::Features::empty()
    } else {
        adapter.features() & wgpu::Features::PIPELINE_CACHE
    }
}

#[allow(unsafe_code)]
pub fn attach_pipeline_cache(device: &wgpu::Device, adapter: &wgpu::Adapter) {
    if !device.features().contains(wgpu::Features::PIPELINE_CACHE) || SHARED_PIPELINE_CACHE.get().is_some() {
        return;
    }
    let data = pipeline_cache_file(adapter).and_then(|p| std::fs::read(p).ok());
    // SAFETY: `create_pipeline_cache` is unsafe because a driver given a blob
    // it did not write can do anything with it. This blob is our own previous
    // `get_data()` output, from a file under this user's cache directory whose
    // name is the adapter's own name, driver string and backend, so a cache
    // written by a different GPU or a different driver is never handed back.
    // `fallback: true` covers the rest: the driver checks its own header and
    // starts empty rather than trusting a blob it does not recognise, which is
    // what a truncated or edited file looks like to it.
    let cache = unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("kirie-pipeline-cache"),
            data: data.as_deref(),
            fallback: true,
        })
    };
    let loaded = data.is_some();
    if SHARED_PIPELINE_CACHE.set(cache).is_ok() {
        tracing::info!(warm = loaded, "driver pipeline cache attached");
    }
}

pub fn persist_pipeline_cache(adapter: &wgpu::Adapter) {
    let Some(cache) = SHARED_PIPELINE_CACHE.get() else {
        return;
    };
    let Some(data) = cache.get_data() else { return };
    let Some(path) = pipeline_cache_file(adapter) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(target_os = "linux")]
impl Gpu {
    pub fn new_for_surface(
        conn: &Connection,
        wl_surface: &WlSurface,
    ) -> Result<(Self, wgpu::Surface<'static>), PlatformError> {
        let mut last_err: Option<PlatformError> = None;

        for backends in [wgpu::Backends::VULKAN, wgpu::Backends::all()] {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });

            let surface = match create_wgpu_surface(&instance, conn, wl_surface) {
                Ok(surface) => surface,
                Err(err) => {
                    tracing::warn!(?backends, %err, "surface creation failed on backend set");
                    last_err = Some(err);
                    continue;
                }
            };

            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference: power_preference(),
                ..wgpu::RequestAdapterOptions::default()
            })) {
                Ok(adapter) => {
                    let info = adapter.get_info();
                    tracing::info!(
                        backend = %info.backend,
                        adapter = %info.name,
                        "selected gpu adapter"
                    );
                    let (device, queue) =
                        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                            label: Some("kirie-platform"),
                            required_features: pipeline_cache_feature(&adapter),
                            ..wgpu::DeviceDescriptor::default()
                        }))?;
                    attach_pipeline_cache(&device, &adapter);
                    return Ok((
                        Self {
                            instance,
                            adapter,
                            device,
                            queue,
                        },
                        surface,
                    ));
                }
                Err(err) => {
                    tracing::warn!(?backends, %err, "no adapter for backend set");
                    last_err = Some(err.into());
                }
            }
        }

        Err(last_err.unwrap_or(PlatformError::NullDisplayPointer))
    }

    pub fn create_surface(
        &self,
        conn: &Connection,
        wl_surface: &WlSurface,
    ) -> Result<wgpu::Surface<'static>, PlatformError> {
        create_wgpu_surface(&self.instance, conn, wl_surface)
    }
}

#[allow(unsafe_code)]
#[cfg(target_os = "linux")]
fn create_wgpu_surface(
    instance: &wgpu::Instance,
    conn: &Connection,
    wl_surface: &WlSurface,
) -> Result<wgpu::Surface<'static>, PlatformError> {
    let display =
        NonNull::new(conn.backend().display_ptr().cast()).ok_or(PlatformError::NullDisplayPointer)?;
    let surface = NonNull::new(wl_surface.id().as_ptr().cast()).ok_or(PlatformError::NullSurfacePointer)?;

    let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(surface));

    // SAFETY: `create_surface_unsafe` requires both raw handles to be valid
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }?;

    Ok(surface)
}
