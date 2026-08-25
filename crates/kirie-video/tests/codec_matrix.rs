//! Every container/codec pair a `portable` build claims to support, decoded.
//!
//! The release links a *trimmed* static ffmpeg (`scripts/build-ffmpeg.sh`): the
//! stock one ships ~500 decoders and every muxer, which is most of the release
//! binary, so it is configured down to what kirie can actually be asked to
//! open. That trim is only safe if something proves the surviving list still
//! decodes — and the Steam corpus contains exactly one video wallpaper, which
//! is nowhere near coverage.
//!
//! So this generates its own fixtures with the *system* ffmpeg and decodes them
//! through kirie-video. A codec dropped from the allow-list by mistake fails
//! here instead of in a user's wallpaper.
//!
//! Skips (passing) when the `ffmpeg` CLI is absent, since the fixtures cannot
//! be produced without it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use kirie_video::{VideoOptions, VideoPlayer};

/// Container + video codec pairs kirie must keep decoding.
///
/// Mirrors `VIDEO_EXTS` in `crates/kirie/src/compat/resolve.rs` crossed with
/// the decoders `scripts/build-ffmpeg.sh` enables. `mov`/`m4v` share the mov
/// demuxer with `mp4`, so one entry covers all three.
const CASES: &[(&str, &str, &[&str])] = &[
    ("mp4", "libx264", &["-pix_fmt", "yuv420p"]),
    ("mp4", "libx265", &["-pix_fmt", "yuv420p", "-tag:v", "hvc1"]),
    ("mp4", "mpeg4", &[]),
    ("mkv", "libx264", &["-pix_fmt", "yuv420p"]),
    ("mkv", "libvpx-vp9", &["-pix_fmt", "yuv420p"]),
    ("webm", "libvpx", &["-pix_fmt", "yuv420p"]),
    ("webm", "libvpx-vp9", &["-pix_fmt", "yuv420p"]),
    ("avi", "mpeg4", &[]),
    ("mov", "libx264", &["-pix_fmt", "yuv420p"]),
];

fn have_system_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Encode a 12-frame colour-bar clip, returning `None` when this system's
/// ffmpeg lacks the encoder (the decoder can still be present in kirie's
/// build — an encoder gap is a fixture problem, not a kirie failure).
fn make_fixture(dir: &Path, container: &str, codec: &str, extra: &[&str]) -> Option<PathBuf> {
    let path = dir.join(format!("{codec}.{container}"));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "testsrc=size=128x96:rate=12:duration=1"])
        .args(["-c:v", codec])
        .args(extra)
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let ok = cmd.status().is_ok_and(|s| s.success());
    (ok && path.is_file()).then_some(path)
}

#[test]
fn every_supported_container_and_codec_decodes() {
    if !have_system_ffmpeg() {
        eprintln!("system ffmpeg not installed; skipping codec matrix");
        return;
    }

    let dir = std::env::temp_dir().join("kirie-codec-matrix");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fixture dir");

    let mut decoded = 0usize;
    let mut skipped: Vec<String> = Vec::new();

    for (container, codec, extra) in CASES {
        let Some(path) = make_fixture(&dir, container, codec, extra) else {
            skipped.push(format!("{codec}/{container}"));
            continue;
        };

        // Audio off: tests must not open the audio device.
        let options = VideoOptions {
            enable_audio: false,
            ..VideoOptions::default()
        };
        let (player, _control) = VideoPlayer::open(&path, options).unwrap_or_else(|err| {
            panic!("{codec}/{container}: kirie-video could not open the fixture: {err}")
        });

        let info = player.info();
        assert!(
            info.width > 0 && info.height > 0,
            "{codec}/{container}: probed degenerate geometry: {info:?}"
        );

        // One decoded frame is the whole assertion: the demuxer, the parser and
        // the decoder all had to work to produce it.
        let frame = player
            .recv_frame_timeout(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("{codec}/{container}: opened but decoded no frame"));
        assert!(
            !frame.data.is_empty(),
            "{codec}/{container}: decoded an empty frame"
        );
        decoded += 1;
    }

    let _ = std::fs::remove_dir_all(&dir);

    // Always report the count: a matrix that quietly skipped everything would
    // otherwise pass while proving nothing.
    eprintln!(
        "codec matrix: {decoded}/{} decoded{}",
        CASES.len(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(
                ", {} skipped (no system encoder): {}",
                skipped.len(),
                skipped.join(", ")
            )
        }
    );
    assert!(
        decoded >= 4,
        "only {decoded} of {} fixtures could be produced; this system's ffmpeg is too limited \
         for the matrix to mean anything",
        CASES.len()
    );
}
