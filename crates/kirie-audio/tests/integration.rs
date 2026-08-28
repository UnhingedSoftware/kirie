use std::time::{Duration, Instant};

use kirie_audio::{AudioCapture, AudioConfig, AutoMute, CaptureStatus};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

#[test]
fn disabled_is_silent() {
    let cap = AudioCapture::disabled();
    assert_eq!(cap.status(), CaptureStatus::Disabled);
    let spec = cap.latest_spectrum();
    assert!(spec.audio16.iter().all(|&x| x == 0.0));
    assert!(spec.audio32.iter().all(|&x| x == 0.0));
    assert!(spec.audio64.iter().all(|&x| x == 0.0));
}

#[test]
fn device_name_api() {
    let cap = AudioCapture::start(AudioConfig {
        enabled: false,
        device: Some("some.source".into()),
        ..AudioConfig::default()
    });
    assert_eq!(cap.device(), Some("some.source"));

    let cfg = AudioConfig::with_device(Some(String::new()));
    assert!(cfg.device.is_none(), "empty device string normalizes to None");
}

#[test]
fn ring_overflow_and_underflow() {
    let (mut prod, mut cons) = HeapRb::<u8>::new(8).split();

    let mut buf = [0u8; 16];
    assert_eq!(cons.pop_slice(&mut buf), 0);

    let pushed = prod.push_slice(&[1u8; 32]);
    assert_eq!(pushed, 8);

    let got = cons.pop_slice(&mut buf);
    assert_eq!(got, 8);
    assert!(buf[..8].iter().all(|&b| b == 1));
    assert_eq!(cons.pop_slice(&mut buf), 0);
}

#[test]
fn automute_disabled_never_plays() {
    let am = AutoMute::disabled();
    assert!(!am.enabled(), "disabled detector reports itself off");
    assert!(!am.is_playing(), "disabled detector never mutes the wallpaper");
    drop(am);
}

#[test]
fn automute_enabled_starts_and_stops() {
    let am = AutoMute::start(true);
    assert!(am.enabled(), "enabled detector reports itself on");
    let _ = am.is_playing();
    drop(am);
}

#[test]
fn live_capture_smoke() {
    if std::env::var("KIRIE_AUDIO_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping live capture (set KIRIE_AUDIO_LIVE=1 to enable)");
        return;
    }

    let cap = AudioCapture::start(AudioConfig::default());

    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        match cap.status() {
            CaptureStatus::Running | CaptureStatus::Failed => break,
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let mut peak = 0.0f32;
    let obs_end = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < obs_end {
        let spec = cap.latest_spectrum();
        for &v in spec.audio64.iter() {
            assert!(v.is_finite(), "spectrum must stay finite");
            peak = peak.max(v);
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    eprintln!("live status = {:?}, peak64 = {peak}", cap.status());

    if std::env::var("KIRIE_AUDIO_PLAYING").as_deref() == Ok("1") {
        assert_eq!(cap.status(), CaptureStatus::Running, "expected a live stream");
        assert!(peak > 0.0, "audio playing but spectrum stayed silent");
    }
}
