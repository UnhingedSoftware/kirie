use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use kirie_platform::{RenderTarget, SurfaceSize};

use crate::compat::args::{ClampMode, ScalingMode};
use crate::compat::resolve::{Wallpaper, classify};
use crate::compat::screenshot::{Headless, build_offscreen_renderer};

#[derive(Debug, Clone, Copy)]
pub struct SoakReport {
    pub iters: usize,
    pub fails: usize,
    pub rss_start_kb: u64,
    pub rss_warm_kb: u64,
    pub rss_end_kb: u64,
    pub rss_peak_kb: u64,
    pub fd_start: usize,
    pub fd_end: usize,
    pub fd_peak: usize,
}

pub fn bench_from_env() -> ExitCode {
    match bench() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bench: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn bench() -> Result<()> {
    let dir = PathBuf::from(std::env::var_os("KIRIE_BENCH").unwrap_or_default());
    let frames = env_usize("KIRIE_BENCH_FRAMES", 120);
    let (w, h) = std::env::var("KIRIE_BENCH_SIZE")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once(['x', 'X'])?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .unwrap_or((1920u32, 1080u32));

    if let Some(s) = std::env::var("KIRIE_BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        crate::compat::common::set_render_scale(s);
    }

    if std::env::var_os("KIRIE_BENCH_FIT").is_some() {
        crate::compat::common::set_fit_render_to_output(true);
    }

    let wp = classify(&dir.to_string_lossy()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let gpu = Headless::new()?;
    let info = gpu.adapter.get_info();
    let size = SurfaceSize {
        width: w.max(1),
        height: h.max(1),
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-bench-target"),
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
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let rt = RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format,
        output_name: "bench",
        size: (size.width, size.height),
    };

    let build_start = Instant::now();
    let mut renderer =
        build_offscreen_renderer(&rt, &wp, ScalingMode::Default, ClampMode::Clamp, size, None, &[])?;
    let build_ms = build_start.elapsed().as_secs_f64() * 1e3;

    for _ in 0..15 {
        renderer.render(&view, size, 1.0 / 60.0);
    }
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

    let mut times = Vec::with_capacity(frames);
    let mut cpu_times = Vec::with_capacity(frames);
    for _ in 0..frames {
        let t = Instant::now();
        renderer.render(&view, size, 1.0 / 60.0);
        cpu_times.push(t.elapsed().as_secs_f64() * 1e3);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let stats = |v: &mut Vec<f64>| -> (f64, f64, f64) {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        (
            v.iter().sum::<f64>() / v.len() as f64,
            v[v.len() / 2],
            v[v.len() * 99 / 100],
        )
    };
    let (cpu_mean, cpu_p50, cpu_p99) = stats(&mut cpu_times);
    let (mean, p50, p99) = stats(&mut times);

    eprintln!(
        "bench: adapter={} ({:?}) {}x{} | build={build_ms:.0}ms | cpu mean={cpu_mean:.3}ms \
         p50={cpu_p50:.3}ms p99={cpu_p99:.3}ms | frame mean={mean:.2}ms \
         p50={p50:.2}ms p99={p99:.2}ms | sustainable={:.0} fps | budget@30fps={:.0}%",
        info.name,
        info.device_type,
        size.width,
        size.height,
        1e3 / mean.max(1e-6),
        mean / (1e3 / 30.0) * 100.0,
    );
    Ok(())
}

pub fn run_from_env() -> ExitCode {
    match soak_from_env() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("soak: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn default_corpus_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/share/Steam/steamapps/workshop/content/431960")
}

fn soak_from_env() -> Result<SoakReport> {
    let iters = env_usize("KIRIE_SOAK_ITERS", 500);
    let frames = env_usize("KIRIE_SOAK_FRAMES", 8);
    let sample_every = env_usize("KIRIE_SOAK_SAMPLE", 10);
    let dir = std::env::var_os("KIRIE_SOAK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_corpus_dir);
    soak(&dir, iters, frames, sample_every)
}

fn is_soakable(wp: &Wallpaper) -> bool {
    matches!(
        wp,
        Wallpaper::Scene { .. } | Wallpaper::Image { .. } | Wallpaper::Video { .. }
    )
}

pub fn soak(
    corpus_dir: &Path,
    iters: usize,
    frames_per_iter: usize,
    sample_every: usize,
) -> Result<SoakReport> {
    kirie_bake::limit_malloc_arenas(2);

    let mut wallpapers: Vec<(String, Wallpaper)> = Vec::new();
    for entry in std::fs::read_dir(corpus_dir)
        .with_context(|| format!("reading corpus dir {}", corpus_dir.display()))?
    {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(wp) = classify(&path.to_string_lossy())
            && is_soakable(&wp)
        {
            wallpapers.push((id, wp));
        }
    }
    ensure!(
        !wallpapers.is_empty(),
        "no renderable (scene/image/video) wallpapers under {}",
        corpus_dir.display()
    );
    wallpapers.sort_by(|a, b| a.0.cmp(&b.0));

    let gpu = Headless::new()?;
    let info = gpu.adapter.get_info();
    eprintln!(
        "soak: adapter={} ({:?}) | {} wallpapers | {iters} iters × {frames_per_iter} frames | sample/{sample_every}",
        info.name,
        info.device_type,
        wallpapers.len(),
    );

    let size = SurfaceSize {
        width: 1920,
        height: 1080,
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-soak-target"),
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
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let render_target = RenderTarget {
        device: &gpu.device,
        queue: &gpu.queue,
        format,
        output_name: "soak",
        size: (size.width, size.height),
    };

    let rss_start = rss_kb().unwrap_or(0);
    let fd_start = fd_count().unwrap_or(0);
    let warm_at = wallpapers.len().min(iters).saturating_sub(1);
    let mut rss_warm = rss_start;
    let mut rss_peak = rss_start;
    let mut fd_peak = fd_start;
    let start = Instant::now();
    let mut fails = 0usize;

    for i in 0..iters {
        let (id, wp) = &wallpapers[i % wallpapers.len()];
        match build_offscreen_renderer(
            &render_target,
            wp,
            ScalingMode::Default,
            ClampMode::Clamp,
            size,
            None,
            &[],
        ) {
            Ok(mut renderer) => {
                for _ in 0..frames_per_iter {
                    renderer.render(&view, size, 1.0 / 60.0);
                }
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                drop(renderer);
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                kirie_bake::trim_heap();
            }
            Err(e) => {
                fails += 1;
                if fails <= 5 {
                    eprintln!("soak: build failed for {id}: {e:#}");
                }
            }
        }

        if i == warm_at {
            rss_warm = rss_kb().unwrap_or(rss_warm);
        }
        if i % sample_every == 0 || i + 1 == iters {
            let rss = rss_kb().unwrap_or(0);
            let fds = fd_count().unwrap_or(0);
            rss_peak = rss_peak.max(rss);
            fd_peak = fd_peak.max(fds);
            eprintln!(
                "soak: iter={i:>5}/{iters} wp={id:<12} rss={rss}KB (Δ{:+}KB) fds={fds} t={:.1}s",
                rss as i64 - rss_start as i64,
                start.elapsed().as_secs_f32(),
            );
        }
    }

    let rss_end = rss_kb().unwrap_or(0);
    let fd_end = fd_count().unwrap_or(0);
    rss_peak = rss_peak.max(rss_end);
    let per = if iters > 0 {
        (rss_end as f64 - rss_warm as f64) / iters as f64
    } else {
        0.0
    };
    eprintln!(
        "soak: DONE iters={iters} fails={fails} rss start={rss_start}KB warm={rss_warm}KB \
         end={rss_end}KB peak={rss_peak}KB post-warm-slope={per:+.2}KB/iter fds {fd_start}->{fd_end} \
         over {:.1}s",
        start.elapsed().as_secs_f32(),
    );

    Ok(SoakReport {
        iters,
        fails,
        rss_start_kb: rss_start,
        rss_warm_kb: rss_warm,
        rss_end_kb: rss_end,
        rss_peak_kb: rss_peak,
        fd_start,
        fd_end,
        fd_peak,
    })
}

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn fd_count() -> Option<usize> {
    Some(std::fs::read_dir("/proc/self/fd").ok()?.count())
}
