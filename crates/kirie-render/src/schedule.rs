#[derive(Debug, Clone, PartialEq)]
pub struct FrameSchedule {
    durations: Vec<f32>,
    total: f64,
}

impl FrameSchedule {
    #[must_use]
    pub fn new(durations: Vec<f32>) -> Self {
        let total = durations.iter().map(|&d| f64::from(d)).sum::<f64>();
        Self { durations, total }
    }

    #[must_use]
    pub fn durations(&self) -> &[f32] {
        &self.durations
    }

    #[must_use]
    pub fn total_seconds(&self) -> f64 {
        self.total
    }

    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.durations.len() > 1 && self.total > 0.0
    }

    #[must_use]
    pub fn frame_at(&self, elapsed: f64) -> usize {
        if !self.is_animated() {
            return 0;
        }
        let mut t = elapsed.rem_euclid(self.total);
        for (index, &duration) in self.durations.iter().enumerate() {
            t -= f64::from(duration);
            if t <= 0.0 {
                return index;
            }
        }
        self.durations.len() - 1
    }

    #[must_use]
    pub fn time_until_change(&self, elapsed: f64) -> Option<f64> {
        if !self.is_animated() {
            return None;
        }
        let t = elapsed.rem_euclid(self.total);
        let mut acc = 0.0f64;
        for &duration in &self.durations {
            acc += f64::from(duration);
            if t <= acc {
                return Some(acc - t);
            }
        }
        Some((self.total - t).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_table_walks_in_file_order() {
        let s = FrameSchedule::new(vec![0.1, 0.2, 0.3]);
        assert!(s.is_animated());
        assert_eq!(s.frame_at(0.0), 0);
        assert_eq!(s.frame_at(0.05), 0);
        assert_eq!(s.frame_at(f64::from(0.1f32)), 0);
        assert_eq!(s.frame_at(0.15), 1);
        assert_eq!(s.frame_at(0.31), 2);
        assert_eq!(s.frame_at(0.59), 2);
    }

    #[test]
    fn playback_wraps_with_fmod() {
        let s = FrameSchedule::new(vec![0.25, 0.25]);
        assert_eq!(s.total_seconds(), 0.5);
        assert_eq!(s.frame_at(0.1), 0);
        assert_eq!(s.frame_at(0.3), 1);
        assert_eq!(s.frame_at(0.6), 0);
        assert_eq!(s.frame_at(10.3), 1);
        assert_eq!(s.frame_at(1e6 + 0.3), 1);
    }

    #[test]
    fn zero_frametime_displays_only_as_first_crossing() {
        let leading_zero = FrameSchedule::new(vec![0.0, 0.1]);
        assert_eq!(leading_zero.frame_at(0.0), 0);
        assert_eq!(leading_zero.frame_at(0.05), 1);

        let middle_zero = FrameSchedule::new(vec![0.1, 0.0, 0.2]);
        assert_eq!(middle_zero.frame_at(0.05), 0);
        assert_eq!(middle_zero.frame_at(f64::from(0.1f32)), 0);
        assert_eq!(middle_zero.frame_at(0.15), 2);
    }

    #[test]
    fn static_and_malformed_tables_never_animate() {
        for s in [
            FrameSchedule::new(vec![1.0]),
            FrameSchedule::new(vec![]),
            FrameSchedule::new(vec![0.0, 0.0, 0.0]),
        ] {
            assert!(!s.is_animated());
            assert_eq!(s.frame_at(0.0), 0);
            assert_eq!(s.frame_at(123.4), 0);
            assert_eq!(s.time_until_change(5.0), None);
        }
    }

    #[test]
    fn uniform_39_frame_table_matches_atlas_sample() {
        let dt = 1.0f32 / 39.0;
        let s = FrameSchedule::new(vec![dt; 39]);
        assert!(s.is_animated());
        assert!((s.total_seconds() - 1.0).abs() < 1e-5);
        for k in 0..39usize {
            let midpoint = (k as f64 + 0.5) * f64::from(dt);
            assert_eq!(s.frame_at(midpoint), k, "midpoint of slot {k}");
        }
        assert_eq!(s.frame_at(s.total_seconds() + 0.5 * f64::from(dt)), 0);
    }

    #[test]
    fn time_until_change_counts_down_to_the_boundary() {
        let s = FrameSchedule::new(vec![0.1, 0.2, 0.3]);
        let eps = 1e-9;
        assert!((s.time_until_change(0.0).unwrap() - 0.1).abs() < 1e-6);
        assert!((s.time_until_change(0.05).unwrap() - 0.05).abs() < 1e-6);
        assert!((s.time_until_change(0.15).unwrap() - 0.15).abs() < 1e-6);
        assert!((s.time_until_change(0.6 + 0.05).unwrap() - 0.05).abs() < 1e-6);
        assert!(s.time_until_change(0.1 + eps).unwrap() < 0.2);
    }
}
