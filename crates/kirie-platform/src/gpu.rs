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

/// One adapter wgpu offered on Windows, as much of it as choosing needs.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) backend: wgpu::Backend,
    /// PCI ids, which are the same whichever backend reports the GPU, and so
    /// what tells one GPU's Vulkan and DX12 adapters apart from another's.
    pub(crate) vendor: u32,
    pub(crate) device: u32,
    pub(crate) software: bool,
}

/// Which GPU `preference` put first, and why.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Aim {
    /// The one `--gpu` or `KIRIE_GPU` named.
    Asked,
    /// `--gpu` named a GPU this machine does not have, so the screen's.
    AskedButAbsent,
    /// The GPU driving the screen.
    Screen,
    /// No DX12 adapter to say which GPU drives the screen, which happens when
    /// `WGPU_BACKEND` leaves DX12 out, so wgpu's own order.
    Anything,
}

/// The order to try `offers` in, best first, for a Windows desktop.
///
/// The GPU to use is the one driving the screen, not the most frugal one: a
/// wallpaper drawn anywhere else has to be copied across to it every frame,
/// and on a desktop with the monitor on the graphics card and the integrated
/// GPU still enabled, that copy is where frames went missing. DXGI lists that
/// GPU first -- it is also where Windows' own per-app graphics setting moves
/// whichever GPU the user picked -- and wgpu keeps DXGI's order for its DX12
/// adapters, so the first DX12 adapter names it. `--gpu` overrides it with a
/// word from `kirie gpus`.
///
/// On the chosen GPU Vulkan comes before DX12, because Vulkan is what kirie's
/// shaders are written and tested against; DX12 is there for a GPU without a
/// Vulkan driver. Every other adapter follows as a fallback, software last.
#[cfg(any(windows, test))]
pub(crate) fn preference(offers: &[Offer], wanted: Option<&str>) -> (Vec<usize>, Aim) {
    let key = |offer: &Offer| (offer.vendor, offer.device);
    let screen = offers
        .iter()
        .find(|offer| offer.backend == wgpu::Backend::Dx12 && !offer.software)
        .map(key);

    let wanted = wanted
        .map(str::trim)
        .filter(|word| !word.is_empty() && !word.eq_ignore_ascii_case("auto"));
    let asked = wanted.and_then(|word| {
        let named: Vec<&Offer> = offers.iter().filter(|offer| names(word, offer)).collect();
        // Of two GPUs from one vendor, the one on the screen, or else the one
        // DXGI puts first.
        named
            .iter()
            .find(|offer| Some(key(offer)) == screen)
            .or_else(|| named.iter().find(|offer| offer.backend == wgpu::Backend::Dx12))
            .or_else(|| named.first())
            .map(|offer| key(offer))
    });
    let (target, aim) = match (asked, wanted, screen) {
        (Some(asked), _, _) => (Some(asked), Aim::Asked),
        (None, Some(_), _) => (screen, Aim::AskedButAbsent),
        (None, None, Some(screen)) => (Some(screen), Aim::Screen),
        (None, None, None) => (None, Aim::Anything),
    };

    let mut order: Vec<usize> = (0..offers.len()).collect();
    order.sort_by_key(|at| {
        let Some(offer) = offers.get(*at) else {
            return (u8::MAX, u8::MAX, *at);
        };
        let tier = if target.is_some_and(|target| key(offer) == target) {
            0
        } else if offer.software {
            2
        } else {
            1
        };
        let backend = match offer.backend {
            wgpu::Backend::Vulkan => 0,
            wgpu::Backend::Dx12 => 1,
            _ => 2,
        };
        (tier, backend, *at)
    });
    (order, aim)
}

/// Whether a `--gpu` word, as `kirie gpus` lists them, names this adapter.
#[cfg(any(windows, test))]
fn names(word: &str, offer: &Offer) -> bool {
    let word = word.to_ascii_lowercase();
    match word.as_str() {
        "lvp" | "software" | "cpu" => offer.software,
        _ if offer.software => false,
        "nvidia" => offer.vendor == 0x10DE,
        "amd" => matches!(offer.vendor, 0x1002 | 0x1022),
        "intel" => offer.vendor == 0x8086,
        "virtio" => offer.vendor == 0x1AF4,
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::Backend::{Dx12, Gl, Vulkan};

    const NVIDIA: (u32, u32) = (0x10DE, 0x2484);
    const INTEL: (u32, u32) = (0x8086, 0x4680);
    const WARP: (u32, u32) = (0x1414, 0x008C);

    fn offer(backend: wgpu::Backend, (vendor, device): (u32, u32)) -> Offer {
        Offer {
            backend,
            vendor,
            device,
            software: (vendor, device) == WARP,
        }
    }

    /// A desktop with its monitor on the graphics card and the integrated GPU
    /// still enabled, listed the way wgpu lists them: Vulkan, then DX12 in
    /// DXGI's order.
    fn desktop() -> Vec<Offer> {
        vec![
            offer(Vulkan, INTEL),
            offer(Vulkan, NVIDIA),
            offer(Dx12, NVIDIA),
            offer(Dx12, INTEL),
            offer(Dx12, WARP),
        ]
    }

    #[test]
    fn the_gpu_driving_the_screen_comes_first_on_vulkan() {
        let (order, aim) = preference(&desktop(), None);
        assert_eq!(order, vec![1, 2, 0, 3, 4]);
        assert_eq!(aim, Aim::Screen);
    }

    #[test]
    fn a_laptop_panel_on_the_integrated_gpu_keeps_it() {
        let laptop = vec![
            offer(Vulkan, INTEL),
            offer(Vulkan, NVIDIA),
            offer(Dx12, INTEL),
            offer(Dx12, NVIDIA),
            offer(Dx12, WARP),
        ];
        assert_eq!(preference(&laptop, None).0, vec![0, 2, 1, 3, 4]);
    }

    #[test]
    fn gpu_names_the_vendor_whichever_gpu_drives_the_screen() {
        let (order, aim) = preference(&desktop(), Some("intel"));
        assert_eq!(order, vec![0, 3, 1, 2, 4]);
        assert_eq!(aim, Aim::Asked);
        assert_eq!(preference(&desktop(), Some(" NVIDIA ")).0.first(), Some(&1));
    }

    #[test]
    fn auto_and_nothing_mean_the_screen() {
        for word in [None, Some(""), Some("auto"), Some("Auto")] {
            assert_eq!(
                preference(&desktop(), word),
                preference(&desktop(), None),
                "{word:?}"
            );
        }
    }

    #[test]
    fn a_gpu_this_machine_lacks_falls_back_to_the_screen() {
        for word in ["amd", "banana"] {
            let (order, aim) = preference(&desktop(), Some(word));
            assert_eq!(order.first(), Some(&1), "{word}");
            assert_eq!(aim, Aim::AskedButAbsent, "{word}");
        }
    }

    #[test]
    fn software_is_last_unless_asked_for() {
        assert_eq!(preference(&desktop(), None).0.last(), Some(&4));
        assert_eq!(preference(&desktop(), Some("lvp")).0.first(), Some(&4));
    }

    #[test]
    fn without_dx12_the_order_is_wgpus_own() {
        let vulkan_only = vec![offer(Vulkan, NVIDIA), offer(Vulkan, INTEL), offer(Gl, NVIDIA)];
        let (order, aim) = preference(&vulkan_only, None);
        assert_eq!(order, vec![0, 1, 2]);
        assert_eq!(aim, Aim::Anything);
    }

    #[test]
    fn a_gpu_without_vulkan_still_gets_dx12() {
        let no_vulkan = vec![offer(Vulkan, INTEL), offer(Dx12, NVIDIA), offer(Dx12, INTEL)];
        assert_eq!(preference(&no_vulkan, None).0, vec![1, 0, 2]);
    }
}
