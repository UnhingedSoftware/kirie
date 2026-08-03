//! Release-hardening leak/stability soak.
//!
//! Drives the offscreen build→render→drop cycle over the installed corpus on
//! ONE shared wgpu device — the same device the live engine keeps across `bg`
//! swaps — for many iterations, sampling resident memory and open-fd count. An
//! unbounded climb across iterations is a leak; a rise-then-plateau (caches
//! warming over the first full cycle, then flat once every page freed on drop
//! is returned to the OS via [`kirie_bake::trim_heap`]) is clean.
//!
//! Triggered out-of-band with `KIRIE_SOAK=1` so it never touches the compat CLI
//! surface. The [`soak`] fn is also called by the ignored `tests/soak.rs`
//! release gate, which asserts on the returned [`SoakReport`].
//!
//! Env knobs (all optional):
//! - `KIRIE_SOAK_ITERS`  total build/render/drop iterations (default 500)
//! - `KIRIE_SOAK_FRAMES` frames rendered per iteration       (default 8)
//! - `KIRIE_SOAK_SAMPLE` sample RSS/fds every N iterations    (default 10)
//! - `KIRIE_SOAK_DIR`    corpus root (default: Steam workshop 431960)

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use kirie_platform::{RenderTarget, SurfaceSize};

use crate::compat::args::{ClampMode, ScalingMode};
use crate::compat::resolve::{Wallpaper, classify};
use crate::compat::screenshot::{Headless, build_offscreen_renderer};

/// Outcome of a soak run — enough for the release gate to assert leak-freedom.
#[derive(Debug, Clone, Copy)]
pub struct SoakReport {
    pub iters: usize,
    /// Builds that failed (a renderable item that errored); expected 0.
    pub fails: usize,
    pub rss_start_kb: u64,
    /// RSS right after the first full corpus cycle — the warm baseline every
    /// later iteration should return to. Leak ⇒ `rss_end_kb` ≫ this.
    pub rss_warm_kb: u64,
    pub rss_end_kb: u64,
    pub rss_peak_kb: u64,
    pub fd_start: usize,
    pub fd_end: usize,
    pub fd_peak: usize,
}

/// Entry point for `KIRIE_SOAK=1 kirie …` (wired in [`crate::run`]).
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

/// Only Scene/Image/Video build offscreen in a default (no-web) build.
fn is_soakable(wp: &Wallpaper) -> bool {
    matches!(
        wp,
        Wallpaper::Scene { .. } | Wallpaper::Image { .. } | Wallpaper::Video { .. }
    )
}

/// Cycle every renderable item in `corpus_dir` build→render(`frames_per_iter`)
/// →drop for `iters` total iterations on one shared device, logging RSS/fds
/// every `sample_every` iterations. Returns a [`SoakReport`] for assertions.
pub fn soak(
    corpus_dir: &Path,
    iters: usize,
    frames_per_iter: usize,
    sample_every: usize,
) -> Result<SoakReport> {
    // Mirror the live engine's allocator policy (compat::run): cap glibc arenas
    // so per-iteration worker threads reuse them, and `trim_heap` after each
    // drop actually returns the pages. Without this the soak measures arena
    // retention, not real footprint.
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
    // Warm baseline = RSS after one full corpus cycle (all caches primed). If
    // fewer iters than the corpus size, fall back to the start.
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
                // Block until the GPU is idle so each iteration's resources are
                // actually reclaimed on drop (not lazily deferred) — a real
                // leak then shows as monotonic growth, not free-timing noise.
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                drop(renderer);
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                // Return freed pages to the OS, exactly as the live swap path
                // does — otherwise RSS reflects glibc arena caching, not usage.
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

/// Resident set size in KB from `/proc/self/status` (`VmRSS`).
fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Open file-descriptor count (leak canary for sockets / mmaps / device fds).
fn fd_count() -> Option<usize> {
    Some(std::fs::read_dir("/proc/self/fd").ok()?.count())
}
