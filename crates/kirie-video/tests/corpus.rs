use std::path::PathBuf;
use std::time::Duration;

use kirie_video::{VideoOptions, VideoPlayer};

const CORPUS_ITEM: &str = ".steam/steam/steamapps/workshop/content/431960/3600453929";

fn corpus_video() -> Option<PathBuf> {
    let dir = std::env::home_dir()?.join(CORPUS_ITEM);
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4")))
}

#[test]
fn corpus_video_first_30_frames_decode() {
    let Some(path) = corpus_video() else {
        eprintln!("corpus video not installed; skipping");
        return;
    };

    let options = VideoOptions {
        enable_audio: false,
        ..VideoOptions::default()
    };
    let (player, _control) = VideoPlayer::open(&path, options).expect("corpus video must open");

    let info = player.info();
    assert!(info.width > 0 && info.height > 0, "probed geometry: {info:?}");
    assert!(
        info.frame_rate > 0.0,
        "corpus mp4 advertises a frame rate: {info:?}"
    );
    assert!(info.duration > 0.0, "corpus mp4 advertises a duration: {info:?}");

    let mut last_pts = f64::NEG_INFINITY;
    for i in 0..30 {
        let frame = player
            .recv_frame_timeout(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("frame {i} not decoded within 10s"));
        assert_eq!(
            frame.data.len(),
            frame.width as usize * frame.height as usize * 4,
            "frame {i}: tightly packed RGBA"
        );
        assert_eq!(
            (frame.width, frame.height),
            (info.width, info.height),
            "frame {i} geometry"
        );
        assert!(
            frame.play_pts > last_pts || (i == 0 && frame.play_pts >= 0.0),
            "frame {i}: pts {} not monotonic after {last_pts}",
            frame.play_pts
        );
        last_pts = frame.play_pts;
        player.recycle_buffer(frame.data);
    }
}

#[test]
fn missing_file_is_a_typed_error() {
    let err = VideoPlayer::open("/nonexistent/kirie-video-test.mp4", VideoOptions::default());
    assert!(err.is_err());
}
