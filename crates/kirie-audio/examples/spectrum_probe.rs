//! Probe: print low/mid/high group peaks.
use kirie_audio::{AudioCapture, AudioConfig};
fn main() {
    let cap = AudioCapture::start(AudioConfig::with_device(std::env::args().nth(1)));
    for i in 0..18 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let s = cap.latest_spectrum();
        let g = |r: std::ops::Range<usize>| s.audio64[r].iter().cloned().fold(0.0f32, f32::max);
        println!(
            "t={} low={:.2} mid={:.2} high={:.2}",
            i + 1,
            g(0..16),
            g(16..40),
            g(40..64)
        );
    }
}
