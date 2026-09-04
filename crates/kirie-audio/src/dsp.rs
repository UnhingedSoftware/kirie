use rustfft::num_complex::Complex;

pub const WAVE_BUFFER_SIZE: usize = 1024;

pub const SAMPLE_RATE: u32 = 44100;

pub const FFT_BINS: usize = WAVE_BUFFER_SIZE / 2 + 1;

pub const SMOOTH_ATTACK: f32 = 0.55;
pub const SMOOTH_RELEASE: f32 = 0.18;
pub const SMOOTH_RATE: f32 = 0.3;

pub const DEFAULT_GATE: f32 = 1.0;

pub const BANDS_16: usize = 16;
pub const BANDS_32: usize = 32;
pub const BANDS_64: usize = 64;

pub const DEFAULT_LEVEL: f32 = 1.0;

#[must_use]
#[inline]
pub fn move_towards(current: f32, target: f32, delta: f32) -> f32 {
    let diff = target - current;
    if diff.abs() <= delta {
        target
    } else {
        current + diff.signum() * delta
    }
}

#[must_use]
#[inline]
pub fn boost(x: f32) -> f32 {
    2.0 - ((1.0 - x) - 0.5).exp()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandTargets {
    pub b16: [f32; BANDS_16],
    pub b32: [f32; BANDS_32],
    pub b64: [f32; BANDS_64],
}

impl BandTargets {
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            b16: [0.0; BANDS_16],
            b32: [0.0; BANDS_32],
            b64: [0.0; BANDS_64],
        }
    }
}

#[must_use]
#[inline]
pub fn normalize_sample(sample: u8) -> f32 {
    (f32::from(sample) - 128.0) / 128.0
}

#[must_use]
pub fn gate_rms(samples: &[u8; WAVE_BUFFER_SIZE]) -> f32 {
    let mut acc = 0.0f64;
    for &s in samples.iter() {
        let x = f64::from(s) - 128.0;
        acc += x * x;
    }
    (acc / WAVE_BUFFER_SIZE as f64).sqrt() as f32
}

#[must_use]
pub fn analyze_frame(
    fft: &dyn rustfft::Fft<f32>,
    samples: &[u8; WAVE_BUFFER_SIZE],
    gate: f32,
    ref_db: f32,
) -> (BandTargets, Option<f32>) {
    debug_assert_eq!(fft.len(), WAVE_BUFFER_SIZE);

    if gate > 0.0 && gate_rms(samples) < gate {
        return (BandTargets::zero(), None);
    }

    let mut buf: Vec<Complex<f32>> = samples
        .iter()
        .map(|&s| Complex::new(normalize_sample(s), 0.0))
        .collect();
    fft.process(&mut buf);

    let spectrum = &buf[..FFT_BINS];
    (bands_from_spectrum(spectrum, ref_db), frame_peak_db(spectrum))
}

pub const RANGE_DB: f32 = 50.0;
pub const REF_DB_MAX: f32 = 54.2;
pub const REF_DB_MIN: f32 = 10.0;
pub const REF_DECAY_DB: f32 = 0.06;

#[must_use]
pub fn bands_from_spectrum(spectrum: &[Complex<f32>], ref_db: f32) -> BandTargets {
    let mut out = BandTargets::zero();
    let mut level = [0.0_f32; BANDS_64];
    for (band, slot) in level.iter_mut().enumerate() {
        let c = spectrum[band * 2];
        let mag2 = c.re * c.re + c.im * c.im;
        *slot = if mag2 > 0.0 {
            ((10.0 * mag2.log10() - (ref_db - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    for (band, peak) in level.iter().enumerate() {
        out.b64[band] = (peak * boost(band as f32 / (BANDS_64 - 1) as f32)).min(1.0);
    }
    for (band, slot) in out.b32.iter_mut().enumerate() {
        let peak = level[band * 2..band * 2 + 2]
            .iter()
            .fold(0.0_f32, |m, v| m.max(*v));
        *slot = (peak * boost(band as f32 / (BANDS_32 - 1) as f32)).min(1.0);
    }
    for (band, slot) in out.b16.iter_mut().enumerate() {
        let peak = level[band * 4..band * 4 + 4]
            .iter()
            .fold(0.0_f32, |m, v| m.max(*v));
        *slot = (peak * boost(band as f32 / (BANDS_16 - 1) as f32)).min(1.0);
    }
    out
}

#[must_use]
pub fn frame_peak_db(spectrum: &[Complex<f32>]) -> Option<f32> {
    let mut peak: Option<f32> = None;
    for band in 0..BANDS_64 {
        let c = spectrum[band * 2];
        let mag2 = c.re * c.re + c.im * c.im;
        if mag2 > 0.0 {
            let db = 10.0 * mag2.log10();
            peak = Some(peak.map_or(db, |p: f32| p.max(db)));
        }
    }
    peak
}

#[derive(Clone, Debug)]
pub struct Smoother {
    pub b16: [f32; BANDS_16],
    pub b32: [f32; BANDS_32],
    pub b64: [f32; BANDS_64],
    targets: BandTargets,
}

impl Default for Smoother {
    fn default() -> Self {
        Self::new()
    }
}

impl Smoother {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            b16: [0.0; BANDS_16],
            b32: [0.0; BANDS_32],
            b64: [0.0; BANDS_64],
            targets: BandTargets::zero(),
        }
    }

    pub fn set_targets(&mut self, mut targets: BandTargets) {
        fn blur(values: &mut [f32]) {
            let n = values.len();
            if n < 3 {
                return;
            }
            let mut prev = values[0];
            for i in 0..n {
                let here = values[i];
                let next = if i + 1 < n { values[i + 1] } else { here };
                values[i] = prev.mul_add(0.25, here.mul_add(0.5, next * 0.25));
                prev = here;
            }
        }
        blur(&mut targets.b16);
        blur(&mut targets.b32);
        blur(&mut targets.b64);
        self.targets = targets;
    }

    #[must_use]
    pub fn is_settled_silent(&self) -> bool {
        const EPS: f32 = 1e-3;
        self.b64.iter().all(|v| *v < EPS)
            && self.targets.b64.iter().all(|v| *v < EPS)
            && self.b32.iter().all(|v| *v < EPS)
            && self.b16.iter().all(|v| *v < EPS)
    }

    pub fn tick(&mut self) {
        fn step(current: f32, target: f32) -> f32 {
            let k = if target > current {
                SMOOTH_ATTACK
            } else {
                SMOOTH_RELEASE
            };
            (target - current).mul_add(k, current)
        }
        for i in 0..BANDS_16 {
            self.b16[i] = step(self.b16[i], self.targets.b16[i]);
        }
        for i in 0..BANDS_32 {
            self.b32[i] = step(self.b32[i], self.targets.b32[i]);
        }
        for i in 0..BANDS_64 {
            self.b64[i] = step(self.b64[i], self.targets.b64[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::FftPlanner;

    #[test]
    fn move_towards_exact() {
        assert_eq!(move_towards(0.5, 0.6, 0.3), 0.6);
        assert_eq!(move_towards(0.6, 0.5, 0.3), 0.5);
        assert_eq!(move_towards(0.0, 1.0, 0.3), 0.3);
        assert!((move_towards(1.0, 0.0, 0.3) - 0.7).abs() < 1e-6);
        assert!((move_towards(0.0, -1.0, 0.3) + 0.3).abs() < 1e-6);
    }

    #[test]
    fn normalize_exact() {
        assert_eq!(normalize_sample(128), 0.0);
        assert_eq!(normalize_sample(0), -1.0);
        assert_eq!(normalize_sample(255), 127.0 / 128.0);
    }

    #[test]
    fn boost_endpoints() {
        assert!((boost(0.0) - (2.0 - 0.5f32.exp())).abs() < 1e-6);
        assert!((boost(1.0) - (2.0 - (-0.5f32).exp())).abs() < 1e-6);
        assert!((boost(0.0) - 0.3513).abs() < 1e-3);
        assert!((boost(1.0) - 1.3935).abs() < 1e-3);
    }

    #[test]
    fn gate_rms_behavior() {
        let silent = [128u8; WAVE_BUFFER_SIZE];
        assert_eq!(gate_rms(&silent), 0.0);
        assert!(gate_rms(&silent) < DEFAULT_GATE);

        let mut loud = [0u8; WAVE_BUFFER_SIZE];
        for (i, s) in loud.iter_mut().enumerate() {
            let v = 128.0 + 100.0 * (std::f32::consts::TAU * 40.0 * i as f32 / 1024.0).sin();
            *s = v.round().clamp(0.0, 255.0) as u8;
        }
        assert!(gate_rms(&loud) > DEFAULT_GATE);
    }

    #[test]
    fn bands_formula_exact() {
        let mut spec = vec![Complex::new(0.0f32, 0.0); FFT_BINS];
        spec[2] = Complex::new(10.0, 0.0);
        let out = bands_from_spectrum(&spec, REF_DB_MAX);
        let f1 = (20.0 - (REF_DB_MAX - RANGE_DB)) / RANGE_DB;
        let expected = f1 * boost(1.0 / 63.0);
        assert!((out.b64[1] - expected).abs() < 1e-5, "got {}", out.b64[1]);
        assert_eq!(out.b64[0], 0.0);
        let mut spec = vec![Complex::new(0.0f32, 0.0); FFT_BINS];
        spec[126] = Complex::new(512.0, 0.0);
        let out = bands_from_spectrum(&spec, REF_DB_MAX);
        assert!(
            (out.b64[63] - boost(1.0).min(1.0f32)).abs() < 0.02,
            "got {}",
            out.b64[63]
        );
        let mut spec = vec![Complex::new(0.0f32, 0.0); FFT_BINS];
        spec[2] = Complex::new(0.4, 0.0);
        let out = bands_from_spectrum(&spec, REF_DB_MAX);
        assert_eq!(out.b64[1], 0.0);
    }

    #[test]
    fn coarse_bands_keep_the_loudest_of_their_range() {
        let mut spec = vec![Complex::new(0.0f32, 0.0); FFT_BINS];
        spec[2] = Complex::new(10.0, 0.0);
        let out = bands_from_spectrum(&spec, REF_DB_MAX);
        let f1 = (20.0 - (REF_DB_MAX - RANGE_DB)) / RANGE_DB;

        assert!(
            (out.b64[1] - f1 * boost(1.0 / 63.0)).abs() < 1e-5,
            "got {}",
            out.b64[1]
        );
        assert!((out.b32[0] - f1 * boost(0.0)).abs() < 1e-5, "got {}", out.b32[0]);
        assert!((out.b16[0] - f1 * boost(0.0)).abs() < 1e-5, "got {}", out.b16[0]);
    }

    #[test]
    fn a_peak_anywhere_in_a_group_reaches_the_16_band_spectrum() {
        for band in 0..4 {
            let mut spec = vec![Complex::new(0.0f32, 0.0); FFT_BINS];
            spec[band * 2] = Complex::new(10.0, 0.0);
            let out = bands_from_spectrum(&spec, REF_DB_MAX);
            assert!(
                out.b16[0] > 0.0,
                "energy in 64-band {band} vanished from the 16-band spectrum"
            );
        }
    }

    #[test]
    fn fft_sine_peaks_at_expected_band() {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(WAVE_BUFFER_SIZE);

        let mut samples = [0u8; WAVE_BUFFER_SIZE];
        for (i, s) in samples.iter_mut().enumerate() {
            let v = 128.0 + 100.0 * (std::f32::consts::TAU * 40.0 * i as f32 / 1024.0).sin();
            *s = v.round().clamp(0.0, 255.0) as u8;
        }
        let (out, _) = analyze_frame(fft.as_ref(), &samples, DEFAULT_GATE, REF_DB_MAX);
        assert!(out.b64[20] > 0.5, "band 20 = {}", out.b64[20]);
        assert!(out.b64[0] <= 0.0, "DC band 0 = {}", out.b64[0]);
        let max = out.b64.iter().cloned().fold(f32::MIN, f32::max);
        assert!((out.b64[20] - max).abs() < 1e-6);

        let silent = [128u8; WAVE_BUFFER_SIZE];
        let (zero, _) = analyze_frame(fft.as_ref(), &silent, DEFAULT_GATE, REF_DB_MAX);
        assert_eq!(zero, BandTargets::zero());
    }

    #[test]
    fn gate_disabled_processes_frame() {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(WAVE_BUFFER_SIZE);
        let mut samples = [128u8; WAVE_BUFFER_SIZE];
        for (i, s) in samples.iter_mut().enumerate() {
            if i % 2 == 0 {
                *s = 129;
            }
        }
        let (gated, _) = analyze_frame(fft.as_ref(), &samples, DEFAULT_GATE, REF_DB_MAX);
        assert_eq!(gated, BandTargets::zero());
        let (ungated, _) = analyze_frame(fft.as_ref(), &samples, 0.0, REF_DB_MAX);
        assert!(ungated != BandTargets::zero());
    }

    #[test]
    fn smoother_slew_and_decay() {
        let mut sm = Smoother::new();
        let mut target = BandTargets::zero();
        target.b64[1] = 1.0;
        sm.set_targets(target);
        assert!(
            (sm.targets.b64[1] - 0.5).abs() < 1e-6,
            "blurred {}",
            sm.targets.b64[1]
        );
        assert!((sm.targets.b64[0] - 0.25).abs() < 1e-6);
        assert!((sm.targets.b64[2] - 0.25).abs() < 1e-6);
        sm.tick();
        assert!((sm.b64[1] - 0.5 * SMOOTH_ATTACK).abs() < 1e-6);
        let after_one = sm.b64[1];
        sm.tick();
        let expected = after_one + (0.5 - after_one) * SMOOTH_ATTACK;
        assert!((sm.b64[1] - expected).abs() < 1e-6);
        let peak = sm.b64[1];
        sm.set_targets(BandTargets::zero());
        sm.tick();
        assert!((sm.b64[1] - peak * (1.0 - SMOOTH_RELEASE)).abs() < 1e-6);
        assert!(sm.b64[1] < peak);
    }
}
