use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use kirie_audio::AudioCapture;
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_render::{ImageContent, ImageOptions, ImageRenderer};
use kirie_video::{VideoOptions, VideoPlayer};

use crate::compat::args::{ClampMode, ScalingMode};
use crate::compat::resolve::Wallpaper;

const DEFAULT_CAPTURE_SIZE: SurfaceSize = SurfaceSize {
    width: 1280,
    height: 720,
};

const CAPTURE_MAX_EDGE: u32 = 1280;

fn parse_size(raw: &str) -> Option<SurfaceSize> {
    let (w, h) = raw.trim().split_once(['x', 'X'])?;
    let width: u32 = w.trim().parse().ok()?;
    let height: u32 = h.trim().parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(SurfaceSize { width, height })
}

fn size_override() -> Option<SurfaceSize> {
    let raw = std::env::var("KIRIE_SCREENSHOT_SIZE").ok()?;
    match parse_size(&raw) {
        Some(sz) => Some(sz),
        None => {
            tracing::warn!(value = %raw, "ignoring malformed KIRIE_SCREENSHOT_SIZE (want WxH, e.g. 634x692)");
            None
        }
    }
}

fn scene_projection_dims(scene_json: &[u8]) -> Option<(u32, u32)> {
    let root: serde_json::Value = serde_json::from_slice(scene_json).ok()?;
    let op = root.get("general")?.get("orthogonalprojection")?;
    if op.is_null() || op.get("auto").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let dim = |key: &str| -> Option<u32> {
        let v = op.get(key)?;
        let n = v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))?;
        u32::try_from(n).ok().filter(|n| *n > 0)
    };
    Some((dim("width")?, dim("height")?))
}

pub(crate) fn scene_projection(dir: &Path) -> Option<(u32, u32)> {
    let pkg = kirie_formats::pkg::OwnedPkg::from_path(dir.join("scene.pkg")).ok()?;
    let bytes = pkg.read_name(b"scene.json").ok()?;
    scene_projection_dims(bytes)
}

pub(crate) fn fit_aspect(w: u32, h: u32, max_edge: u32) -> SurfaceSize {
    let max_edge = max_edge.max(1);
    let longest = w.max(h);
    if longest <= max_edge {
        return SurfaceSize {
            width: w.max(1),
            height: h.max(1),
        };
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    let round = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);
    SurfaceSize {
        width: round(w),
        height: round(h),
    }
}

fn resolve_capture_size(wallpaper: &Wallpaper) -> SurfaceSize {
    resolve_capture_size_with(size_override(), wallpaper)
}

fn resolve_capture_size_with(override_size: Option<SurfaceSize>, wallpaper: &Wallpaper) -> SurfaceSize {
    if let Some(sz) = override_size {
        return sz;
    }
    if let Wallpaper::Scene { dir } = wallpaper
        && let Some((w, h)) = scene_projection(dir)
    {
        return fit_aspect(w, h, CAPTURE_MAX_EDGE);
    }
    DEFAULT_CAPTURE_SIZE
}

const ROW_ALIGN: u32 = 256;

const SCENE_CONTENT_FLOOR: f64 = 0.005;

const SETTLE_LIT_EPS_ABS: f64 = 0.002;
const SETTLE_LIT_EPS_REL: f64 = 0.05;

const SETTLE_STREAK: u32 = 3;

const SETTLE_MIN_EXTRA: u32 = 8;

const SETTLE_MAX_EXTRA: u32 = 150;

struct SceneSettle {
    prev_lit: Option<f64>,
    stable_streak: u32,
    extra: u32,
}

impl SceneSettle {
    fn new() -> Self {
        Self {
            prev_lit: None,
            stable_streak: 0,
            extra: 0,
        }
    }

    fn observe(&mut self, lit: f64) -> bool {
        self.extra = self.extra.saturating_add(1);
        let stable = self.prev_lit.is_some_and(|prev| {
            let tol = SETTLE_LIT_EPS_ABS.max(prev * SETTLE_LIT_EPS_REL);
            (lit - prev).abs() <= tol
        });
        if stable {
            self.stable_streak += 1;
        } else {
            self.stable_streak = 0;
        }
        self.prev_lit = Some(lit);
        (self.stable_streak >= SETTLE_STREAK && self.extra >= SETTLE_MIN_EXTRA)
            || self.extra >= SETTLE_MAX_EXTRA
    }
}

pub(crate) struct Headless {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) adapter: wgpu::Adapter,
}

impl Headless {
    pub(crate) fn new() -> Result<Self> {
        let mut last: Option<anyhow::Error> = None;
        for backends in [wgpu::Backends::VULKAN, wgpu::Backends::all()] {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
                Ok(adapter) => {
                    let info = adapter.get_info();
                    tracing::info!(backend = %info.backend, adapter = %info.name, "screenshot gpu");
                    let (device, queue) =
                        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                            label: Some("kirie-screenshot"),
                            required_features: kirie_platform::pipeline_cache_feature(&adapter),
                            ..wgpu::DeviceDescriptor::default()
                        }))
                        .context("request headless wgpu device")?;
                    kirie_platform::attach_pipeline_cache(&device, &adapter);
                    return Ok(Self {
                        device,
                        queue,
                        adapter,
                    });
                }
                Err(err) => last = Some(anyhow!("no adapter on {backends:?}: {err}")),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("no wgpu adapter for screenshot")))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn capture(
    wallpaper: &Wallpaper,
    scaling: ScalingMode,
    clamp: ClampMode,
    delay: u32,
    out_path: &Path,
    audio: Option<Arc<AudioCapture>>,
    properties: &[(String, String)],
) -> Result<()> {
    let capture_size = resolve_capture_size(wallpaper);

    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    if let Wallpaper::Web { dir, file } = wallpaper
        && capture_web_host(
            dir,
            file,
            capture_size,
            properties,
            out_path,
            capture_budget(wallpaper),
        )?
    {
        return Ok(());
    }

    let gpu = Headless::new()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    tracing::info!(
        width = capture_size.width,
        height = capture_size.height,
        "screenshot canvas"
    );

    let target_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-screenshot-target"),
        size: wgpu::Extent3d {
            width: capture_size.width,
            height: capture_size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let render_target = RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format,
        output_name: "screenshot",
        size: (capture_size.width, capture_size.height),
        position: (0, 0),
    };

    let mut renderer = build_offscreen_renderer(
        &render_target,
        wallpaper,
        scaling,
        clamp,
        audio,
        properties,
    )?;

    let deadline = Instant::now() + capture_budget(wallpaper);
    let dt = 1.0 / 60.0;
    let min_frames = delay.max(1);
    let settle_scene = matches!(wallpaper, Wallpaper::Scene { .. });
    let content_floor = if settle_scene { SCENE_CONTENT_FLOOR } else { 0.05 };
    let mut pixels = vec![0u8; (capture_size.width * capture_size.height * 4) as usize];
    let mut frame: u32 = 0;
    let mut captured_nonblack = false;
    let mut settle = SceneSettle::new();
    loop {
        renderer.render(&view, capture_size, dt);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("gpu poll after render: {e}"))?;
        frame += 1;

        let timed_out = Instant::now() >= deadline;

        if frame >= min_frames || timed_out {
            pixels = readback(&gpu.device, &gpu.queue, &target_tex, capture_size)?;
            let lit = lit_fraction(&pixels);
            if lit > content_floor {
                captured_nonblack = true;
                if !settle_scene {
                    break;
                }
                if settle.observe(lit) {
                    break;
                }
            }
        }

        if timed_out {
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    kirie_platform::persist_pipeline_cache(&gpu.adapter);
    write_image(out_path, capture_size.width, capture_size.height, &pixels)?;
    if !captured_nonblack {
        tracing::warn!(
            path = %out_path.display(),
            "screenshot frame was all black (wallpaper produced no visible frame in time)"
        );
    }
    Ok(())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct Sound {
    pub volume: i64,
    pub silent: bool,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn build_presented_renderer(
    render_target: &RenderTarget,
    wallpaper: &Wallpaper,
    scaling: ScalingMode,
    clamp: ClampMode,
    properties: &[(String, String)],
    sound: Sound,
) -> Result<Box<dyn Renderer>> {
    if let Wallpaper::Video { media } = wallpaper {
        let options = VideoOptions {
            volume: sound.volume as f64 * 100.0 / 128.0,
            mute: false,
            silent: sound.silent,
            paused: false,
            scaling: super::common::to_video_scaling(scaling),
            nv12: false,
            enable_audio: true,
        };
        let (player, _control) = VideoPlayer::open(media, options)
            .with_context(|| format!("opening video {}", media.display()))?;
        return Ok(Box::new(kirie_video::VideoRenderer::new(render_target, player)));
    }
    build_offscreen_renderer(render_target, wallpaper, scaling, clamp, None, properties)
}

#[cfg_attr(not(feature = "web-cef"), allow(unused_variables))]
fn wallpaper_path(wallpaper: &Wallpaper) -> Option<&std::path::Path> {
    match wallpaper {
        Wallpaper::Scene { dir } => Some(dir),
        Wallpaper::Video { media } => Some(media),
        Wallpaper::Image { file } => Some(file),
        Wallpaper::Web { dir, .. } => Some(dir),
        Wallpaper::Unsupported { .. } | Wallpaper::Asset => None,
    }
}

pub(crate) fn build_offscreen_renderer(
    render_target: &RenderTarget,
    wallpaper: &Wallpaper,
    scaling: ScalingMode,
    clamp: ClampMode,
    audio: Option<Arc<AudioCapture>>,
    properties: &[(String, String)],
) -> Result<Box<dyn Renderer>> {
    let saved = wallpaper_path(wallpaper).map(|bg| super::saved_props::with_saved(bg, properties));
    let properties: &[(String, String)] = saved.as_deref().unwrap_or(properties);
    let renderer: Box<dyn Renderer> = match wallpaper {
        Wallpaper::Video { media } => {
            let options = VideoOptions {
                scaling: super::common::to_video_scaling(scaling),
                enable_audio: false,
                ..VideoOptions::default()
            };
            let (player, _control) = VideoPlayer::open(media, options)
                .with_context(|| format!("opening video {}", media.display()))?;
            Box::new(kirie_video::VideoRenderer::new(render_target, player))
        }
        Wallpaper::Image { file } => {
            let content =
                ImageContent::from_path(file).with_context(|| format!("loading image {}", file.display()))?;
            let options = ImageOptions {
                scaling: super::common::to_render_scaling(scaling),
                clamp: super::common::to_render_clamp(clamp),
            };
            Box::new(ImageRenderer::new(render_target, &content, options).context("building image renderer")?)
        }
        Wallpaper::Scene { dir } => {
            let options = kirie_render::SceneOptions {
                render_scale: super::common::render_scale(),
                scaling: super::common::to_render_scaling(scaling),
                clamp: super::common::to_render_clamp(clamp),
                disable_parallax: false,
                fit_render_to_output: super::common::fit_render_to_output(),
                only_objects: super::common::object_filter().0,
                skip_objects: super::common::object_filter().1,
            };
            kirie_render::load_workshop_scene(
                render_target,
                dir,
                super::resolve::we_assets_dir_or_warn().as_deref(),
                options,
                audio,
                properties,
            )
            .with_context(|| format!("building scene renderer for {}", dir.display()))?
        }
        #[cfg(any(feature = "web-cef", all(feature = "web-webview", not(target_os = "macos"))))]
        Wallpaper::Web { dir, file } => {
            use kirie_web::{WebBackend, WebRenderer, WebSize};
            let url = super::resolve::web_entry_url(dir, file);
            let size = WebSize {
                width: render_target.size.0,
                height: render_target.size.1,
            };
            let mut backend =
                <LiveWebBackend as WebBackend>::new_on_output(
                    &url,
                    size,
                    Some(render_target.output_name),
                    Some(render_target.position),
                )
                .map_err(|e| anyhow!("starting web backend for {url}: {e}"))?;

            let props = super::common::web_props_json(dir, properties);
            if props != "{}" {
                backend.apply_properties(&props);
            }
            let mut renderer = WebRenderer::new(render_target, Box::new(backend));
            let media = Some(Arc::new(kirie_render::MediaSource::start(
                kirie_render::MediaConfig::default(),
            )));
            if let Some(feed) = crate::compat::webfeed::EngineWebFeed::new(audio, media) {
                renderer.set_feed(Box::new(feed));
            }
            Box::new(renderer)
        }
        #[cfg(not(any(feature = "web-cef", all(feature = "web-webview", not(target_os = "macos")))))]
        Wallpaper::Web { .. } => {
            bail!(
                "cannot screenshot a web wallpaper: this build has no web backend \
                 (rebuild with --features web-webview)"
            );
        }
        Wallpaper::Unsupported { kind } => {
            bail!("cannot screenshot a {kind} wallpaper: not yet supported by kirie");
        }
        Wallpaper::Asset => {
            bail!(
                "cannot screenshot this item: it is a Wallpaper Engine asset (effect preset), not a renderable wallpaper"
            );
        }
    };
    Ok(renderer)
}

#[cfg(feature = "web-cef")]
type LiveWebBackend = kirie_web::hosted::HostedBackend;
#[cfg(all(feature = "web-webview", not(feature = "web-cef"), not(target_os = "macos")))]
type LiveWebBackend = kirie_web::viewhost::ViewHostBackend;

#[cfg(feature = "web-cef")]
type OffscreenWebBackend = kirie_web::hosted::HostedBackend;
#[cfg(all(feature = "web-webview", not(feature = "web-cef"), not(target_os = "macos")))]
type OffscreenWebBackend = kirie_web::viewhost::ViewHostBackend;
#[cfg(all(feature = "web-webview", not(feature = "web-cef"), target_os = "macos"))]
type OffscreenWebBackend = kirie_web::wk::WkBackend;

pub fn capture_live(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut dyn Renderer,
    size: SurfaceSize,
    format: wgpu::TextureFormat,
    path: &Path,
) -> Result<()> {
    let size = SurfaceSize {
        width: size.width.max(1),
        height: size.height.max(1),
    };
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-socket-screenshot"),
        size: wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    renderer.render(&view, size, 0.0);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("gpu poll after live-frame render: {e}"))?;

    let mut pixels = readback(device, queue, &target_tex, size)?;
    if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    write_image(path, size.width, size.height, &pixels)
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
fn capture_web_host(
    dir: &Path,
    file: &str,
    size: SurfaceSize,
    properties: &[(String, String)],
    out_path: &Path,
    budget: Duration,
) -> Result<bool> {
    use kirie_web::{OffscreenWeb, PixelFormat, WebSize};

    let url = super::resolve::web_entry_url(dir, file);
    let mut backend = <OffscreenWebBackend as OffscreenWeb>::open(
        &url,
        WebSize {
            width: size.width,
            height: size.height,
        },
    )
    .map_err(|e| anyhow!("starting web backend for {url}: {e}"))?;

    if backend.produces_frames() {
        backend.shutdown();
        return Ok(false);
    }

    let props = super::common::web_props_json(dir, properties);
    if props != "{}" {
        backend.apply_properties(&props);
    }

    let deadline = Instant::now() + budget;
    let mut best: Option<(Vec<u8>, u32, u32)> = None;
    loop {
        backend.tick(1.0 / 60.0);
        if let Some(frame) = backend.snapshot()
            && frame.is_consistent()
        {
            let mut pixels = frame.data;
            if matches!(frame.format, PixelFormat::Bgra8) {
                for px in pixels.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
            }
            let lit = lit_fraction(&pixels);
            best = Some((pixels, frame.width, frame.height));
            if lit > WEB_CONTENT_FLOOR {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    backend.shutdown();

    let Some((pixels, width, height)) = best else {
        return Ok(false);
    };
    if lit_fraction(&pixels) <= f64::EPSILON {
        tracing::warn!(
            path = %out_path.display(),
            "web host returned only black frames (is its surface visible?)"
        );
    }
    write_image(out_path, width, height, &pixels)?;
    Ok(true)
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
const WEB_CONTENT_FLOOR: f64 = 0.02;

fn capture_budget(wallpaper: &Wallpaper) -> Duration {
    let default_secs = match wallpaper {
        Wallpaper::Web { .. } => 45,
        Wallpaper::Scene { .. } => 20,
        _ => 6,
    };
    let secs = std::env::var("KIRIE_SCREENSHOT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(default_secs);
    Duration::from_secs(secs)
}

pub(crate) fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: SurfaceSize,
) -> Result<Vec<u8>> {
    let width = size.width;
    let height = size.height;
    let unpadded = width * 4;
    let padded = unpadded.div_ceil(ROW_ALIGN) * ROW_ALIGN;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kirie-screenshot-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("kirie-screenshot-copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = crossbeam_channel::bounded(1);
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("gpu poll for map: {e}"))?;
    rx.recv()
        .map_err(|_| anyhow!("readback map channel closed"))?
        .map_err(|e| anyhow!("buffer map failed: {e}"))?;

    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("mapping readback buffer: {e}"))?;
    let mut out = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    buffer.unmap();
    Ok(out)
}

fn lit_fraction(rgba: &[u8]) -> f64 {
    let total = rgba.len() / 4;
    if total == 0 {
        return 0.0;
    }
    let mut lit = 0usize;
    let (pixels, _) = rgba.as_chunks::<4>();
    for px in pixels {
        if px[0] > 8 || px[1] > 8 || px[2] > 8 {
            lit += 1;
        }
    }
    lit as f64 / total as f64
}

fn write_image(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    let (pixels, _) = rgba.as_chunks::<4>();
    for px in pixels {
        rgb.extend_from_slice(&px[0..3]);
    }
    let img = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| anyhow!("screenshot buffer size mismatch"))?;
    img.save(path)
        .with_context(|| format!("writing screenshot {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn lit_fraction_counts_lit_pixels_and_floors_empty() {
        assert_eq!(lit_fraction(&[]), 0.0);
        let mut buf = vec![0u8; 1000 * 4];
        for px in buf.chunks_mut(4).take(20) {
            px[0] = 255;
        }
        let frac = lit_fraction(&buf);
        assert!((frac - 0.02).abs() < 1e-9, "expected 2% lit, got {frac}");
        assert!(
            frac > SCENE_CONTENT_FLOOR,
            "dark scene must clear the scene floor"
        );
        assert!(frac < 0.05, "but stays under the image/video 5% gate");
        let mut floor = vec![0u8; 4];
        floor[0] = 8;
        assert_eq!(lit_fraction(&floor), 0.0);
    }

    #[test]
    fn scene_settle_accepts_stable_scene_after_min_extra() {
        let mut s = SceneSettle::new();
        for i in 1..SETTLE_MIN_EXTRA {
            assert!(!s.observe(0.42), "accepted too early at extra={i}");
        }
        assert!(s.observe(0.42), "must accept once min extra reached and stable");
    }

    #[test]
    fn scene_settle_waits_through_fade_in() {
        let mut s = SceneSettle::new();
        let mut lit = 0.10;
        for _ in 0..20 {
            assert!(!s.observe(lit), "must not settle mid fade-in at lit={lit}");
            lit += 0.03;
        }
        let plateau = lit;
        let mut settled = false;
        for _ in 0..SETTLE_STREAK + 1 {
            if s.observe(plateau) {
                settled = true;
                break;
            }
        }
        assert!(settled, "must settle once the composite plateaus");
    }

    #[test]
    fn scene_settle_caps_when_never_stable() {
        let mut s = SceneSettle::new();
        let mut accepted_at = None;
        for i in 1..=SETTLE_MAX_EXTRA {
            let lit = if i % 2 == 0 { 0.20 } else { 0.60 };
            if s.observe(lit) {
                accepted_at = Some(i);
                break;
            }
        }
        assert_eq!(
            accepted_at,
            Some(SETTLE_MAX_EXTRA),
            "unstable composite must accept exactly at the settle cap"
        );
    }

    #[test]
    fn parse_size_accepts_wxh_and_rejects_junk() {
        assert_eq!(
            parse_size("1280x720"),
            Some(SurfaceSize {
                width: 1280,
                height: 720
            })
        );
        assert_eq!(
            parse_size(" 634X692 "),
            Some(SurfaceSize {
                width: 634,
                height: 692
            })
        );
        assert_eq!(parse_size("0x100"), None);
        assert_eq!(parse_size("100x0"), None);
        assert_eq!(parse_size("1280"), None);
        assert_eq!(parse_size("axb"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn fit_aspect_preserves_orientation_and_bounds_long_edge() {
        assert_eq!(
            fit_aspect(1920, 1080, 1280),
            SurfaceSize {
                width: 1280,
                height: 720
            }
        );
        assert_eq!(
            fit_aspect(2560, 1440, 1280),
            SurfaceSize {
                width: 1280,
                height: 720
            }
        );
        let tall = fit_aspect(634, 692, 1280);
        assert!(tall.width < tall.height, "portrait must stay portrait: {tall:?}");
        assert_eq!(
            tall,
            SurfaceSize {
                width: 634,
                height: 692
            }
        );
        let big_tall = fit_aspect(1500, 3000, 1280);
        assert_eq!(
            big_tall,
            SurfaceSize {
                width: 640,
                height: 1280
            }
        );
        assert!(big_tall.width < big_tall.height);
        assert_eq!(
            fit_aspect(2474, 1856, 1280),
            SurfaceSize {
                width: 1280,
                height: 960
            }
        );
    }

    #[test]
    fn scene_projection_dims_reads_orthogonalprojection() {
        let portrait = br#"{"general":{"orthogonalprojection":{"width":634,"height":692}}}"#;
        assert_eq!(scene_projection_dims(portrait), Some((634, 692)));
        let landscape = br#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}}}"#;
        assert_eq!(scene_projection_dims(landscape), Some((1920, 1080)));
        let strnums = br#"{"general":{"orthogonalprojection":{"width":"800","height":"600"}}}"#;
        assert_eq!(scene_projection_dims(strnums), Some((800, 600)));
        assert_eq!(
            scene_projection_dims(br#"{"general":{"orthogonalprojection":{"auto":true}}}"#),
            None
        );
        assert_eq!(
            scene_projection_dims(br#"{"general":{"orthogonalprojection":null}}"#),
            None
        );
        assert_eq!(scene_projection_dims(br#"{"general":{}}"#), None);
        assert_eq!(
            scene_projection_dims(br#"{"general":{"orthogonalprojection":{"width":0,"height":100}}}"#),
            None
        );
        assert_eq!(scene_projection_dims(b"not json"), None);
    }

    #[test]
    fn projection_derived_canvas_is_not_stretched() {
        let portrait =
            scene_projection_dims(br#"{"general":{"orthogonalprojection":{"width":634,"height":692}}}"#)
                .map(|(w, h)| fit_aspect(w, h, CAPTURE_MAX_EDGE))
                .unwrap();
        assert!(
            portrait.width < portrait.height,
            "portrait scene must screenshot portrait, got {portrait:?}"
        );
        assert_ne!(portrait, DEFAULT_CAPTURE_SIZE, "must not be the 1280x720 default");

        let landscape =
            scene_projection_dims(br#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}}}"#)
                .map(|(w, h)| fit_aspect(w, h, CAPTURE_MAX_EDGE))
                .unwrap();
        assert!(landscape.width > landscape.height, "landscape stays landscape");
        assert_eq!(landscape, DEFAULT_CAPTURE_SIZE);
    }

    #[test]
    fn resolve_capture_size_priority() {
        let video = Wallpaper::Video {
            media: PathBuf::from("/nonexistent.mp4"),
        };
        assert_eq!(resolve_capture_size_with(None, &video), DEFAULT_CAPTURE_SIZE);

        let bad_scene = Wallpaper::Scene {
            dir: PathBuf::from("/nonexistent-scene-dir"),
        };
        assert_eq!(resolve_capture_size_with(None, &bad_scene), DEFAULT_CAPTURE_SIZE);

        let over = SurfaceSize {
            width: 500,
            height: 900,
        };
        assert_eq!(resolve_capture_size_with(Some(over), &video), over);

        let corpus = PathBuf::from("/home/aiko/.steam/steam/steamapps/workshop/content/431960/3609007632");
        if corpus.join("scene.pkg").is_file() {
            let sz = resolve_capture_size_with(None, &Wallpaper::Scene { dir: corpus });
            assert_eq!(sz, fit_aspect(2474, 1856, CAPTURE_MAX_EDGE));
            assert_ne!(
                sz, DEFAULT_CAPTURE_SIZE,
                "non-16:9 scene must not use the 1280x720 default"
            );
        }
    }
}
