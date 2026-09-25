//! Counters for what one frame asks of the GPU.
//!
//! Frame time on its own hides the work that costs a wallpaper the most power:
//! render passes it opens, full-screen textures it copies, buffers it re-uploads
//! with bytes that did not change. Those are counted here so a change can be
//! judged by the work it removed rather than by a stopwatch on one machine.
//!
//! Counting is off unless it is switched on, and costs an unsynchronised atomic
//! load per site when it is off.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

static RENDER_PASSES: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);
static TEXTURE_COPIES: AtomicU64 = AtomicU64::new(0);
static COPY_BYTES: AtomicU64 = AtomicU64::new(0);
static BUFFER_WRITES: AtomicU64 = AtomicU64::new(0);
static BUFFER_BYTES: AtomicU64 = AtomicU64::new(0);
static WRITES_SKIPPED: AtomicU64 = AtomicU64::new(0);
static SNAPSHOTS_SKIPPED: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// How often the running totals are reported while counting.
const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

static SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Whether the renderer should count what it submits.
#[inline]
pub fn counting() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Start counting. Called for `--render-debug=frame-cost`.
pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

/// A render pass opened on the scene encoder.
#[inline]
pub(crate) fn render_pass() {
    if counting() {
        RENDER_PASSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Draws recorded into a render pass.
#[inline]
pub(crate) fn draw(n: u64) {
    if counting() {
        DRAWS.fetch_add(n, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn texture_copy(bytes: u64) {
    if counting() {
        TEXTURE_COPIES.fetch_add(1, Ordering::Relaxed);
        COPY_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn buffer_write(bytes: u64) {
    if counting() {
        BUFFER_WRITES.fetch_add(1, Ordering::Relaxed);
        BUFFER_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// A scene snapshot skipped because nothing had been drawn since the last one.
#[inline]
pub(crate) fn snapshot_skipped() {
    if counting() {
        SNAPSHOTS_SKIPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// A buffer write skipped because the bytes matched what the buffer holds.
#[inline]
pub(crate) fn buffer_write_skipped() {
    if counting() {
        WRITES_SKIPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Ends a frame. While counting, the running totals are logged every few
/// seconds and then cleared, so a wallpaper left running reports what it is
/// costing now rather than an average since it started.
pub fn end_frame() {
    if !counting() {
        return;
    }
    FRAMES.fetch_add(1, Ordering::Relaxed);
    let Ok(mut since) = SINCE.lock() else { return };
    let started = *since.get_or_insert_with(std::time::Instant::now);
    let elapsed = started.elapsed();
    if elapsed < REPORT_EVERY {
        return;
    }
    let frames = FRAMES.load(Ordering::Relaxed);
    let cost = per_frame(frames);
    tracing::info!(
        frames,
        fps = frames as f64 / elapsed.as_secs_f64(),
        render_passes = cost.render_passes,
        draws = cost.draws,
        texture_copies = cost.texture_copies,
        copy_mib = cost.copy_mib,
        buffer_writes = cost.buffer_writes,
        buffer_kib = cost.buffer_kib,
        writes_skipped = cost.buffer_writes_skipped,
        copies_skipped = cost.snapshots_skipped,
        "frame cost"
    );
    reset();
    *since = Some(std::time::Instant::now());
}

/// What one frame cost, averaged over `frames`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameCost {
    pub render_passes: f64,
    pub draws: f64,
    pub texture_copies: f64,
    pub copy_mib: f64,
    pub buffer_writes: f64,
    pub buffer_kib: f64,
    pub buffer_writes_skipped: f64,
    pub snapshots_skipped: f64,
}

/// Zero the counters and start counting.
pub fn reset() {
    enable();
    for counter in [
        &RENDER_PASSES,
        &DRAWS,
        &TEXTURE_COPIES,
        &COPY_BYTES,
        &BUFFER_WRITES,
        &BUFFER_BYTES,
        &WRITES_SKIPPED,
        &SNAPSHOTS_SKIPPED,
        &FRAMES,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

/// The counters so far, divided by how many frames were drawn.
#[must_use]
pub fn per_frame(frames: u64) -> FrameCost {
    let n = frames.max(1) as f64;
    let read = |c: &AtomicU64| c.load(Ordering::Relaxed) as f64 / n;
    FrameCost {
        render_passes: read(&RENDER_PASSES),
        draws: read(&DRAWS),
        texture_copies: read(&TEXTURE_COPIES),
        copy_mib: read(&COPY_BYTES) / (1024.0 * 1024.0),
        buffer_writes: read(&BUFFER_WRITES),
        buffer_kib: read(&BUFFER_BYTES) / 1024.0,
        buffer_writes_skipped: read(&WRITES_SKIPPED),
        snapshots_skipped: read(&SNAPSHOTS_SKIPPED),
    }
}
