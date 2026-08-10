//! Live probe: capture like the engine does, print band peaks.
use kirie_audio::{AudioCapture, AudioConfig};
fn main() {
    let dev = std::env::args().nth(1);
    let cap = AudioCapture::start(AudioConfig::with_device(dev));
    for i in 0..6 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let s = cap.latest_spectrum();
        let peak = s.audio64.iter().cloned().fold(f32::MIN, f32::max);
        let min = s.audio64.iter().cloned().fold(f32::MAX, f32::min);
        println!(
            "t={}s status={:?} min={:.3} peak={:.3}",
            i + 1,
            cap.status(),
            min,
            peak
        );
    }
}
