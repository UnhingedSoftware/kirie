pub trait Timed {
    fn play_pts(&self) -> f64;
}

#[derive(Debug, Clone)]
pub struct LoopTimeline {
    base: f64,
    end: f64,
    nominal: Option<f64>,
}

impl LoopTimeline {
    #[must_use]
    pub fn new(nominal: Option<f64>) -> Self {
        Self {
            base: 0.0,
            end: 0.0,
            nominal: nominal.filter(|d| d.is_finite() && *d > 0.0),
        }
    }

    pub fn map(&mut self, raw_pts: f64, duration: f64) -> f64 {
        let play = self.base + raw_pts;
        let dur = if duration.is_finite() && duration > 0.0 {
            duration
        } else {
            0.0
        };
        self.end = self.end.max(play + dur);
        play
    }

    pub fn wrap(&mut self) {
        let mut next = self.end;
        if let Some(nominal) = self.nominal {
            next = next.max(self.base + nominal);
        }
        self.base = next;
        self.end = next;
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PacerStats {
    pub presented: u64,
    pub dropped: u64,
}

#[derive(Debug)]
pub struct Pacer<T> {
    pending: Option<T>,
    stats: PacerStats,
}

impl<T: Timed> Pacer<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: None,
            stats: PacerStats::default(),
        }
    }

    #[must_use]
    pub fn stats(&self) -> PacerStats {
        self.stats
    }

    pub fn select(
        &mut self,
        now: f64,
        mut pull: impl FnMut() -> Option<T>,
        mut recycle: impl FnMut(T),
    ) -> Option<T> {
        let mut due: Option<T> = None;
        loop {
            let candidate = match self.pending.take() {
                Some(frame) => frame,
                None => match pull() {
                    Some(frame) => frame,
                    None => break,
                },
            };
            if candidate.play_pts() <= now {
                if let Some(superseded) = due.replace(candidate) {
                    self.stats.dropped += 1;
                    recycle(superseded);
                }
            } else {
                self.pending = Some(candidate);
                break;
            }
        }
        if due.is_some() {
            self.stats.presented += 1;
        }
        due
    }
}

impl<T: Timed> Default for Pacer<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{LoopTimeline, Pacer, Timed};

    #[derive(Debug, PartialEq)]
    struct F(f64);

    impl Timed for F {
        fn play_pts(&self) -> f64 {
            self.0
        }
    }

    fn pull_from(frames: &mut Vec<F>) -> impl FnMut() -> Option<F> {
        frames.reverse();
        let mut v = std::mem::take(frames);
        move || v.pop()
    }

    #[test]
    fn selects_newest_due_frame_and_drops_older_ones() {
        let mut pacer = Pacer::new();
        let mut dropped = Vec::new();
        let mut queue = vec![F(0.0), F(0.1), F(0.2), F(0.3)];
        let picked = pacer.select(0.25, pull_from(&mut queue), |f| dropped.push(f.0));
        assert_eq!(picked, Some(F(0.2)));
        assert_eq!(dropped, vec![0.0, 0.1]);
        assert_eq!(pacer.stats().dropped, 2);
        assert_eq!(pacer.stats().presented, 1);
    }

    #[test]
    fn future_frame_is_held_not_dropped() {
        let mut pacer = Pacer::new();
        let mut queue = vec![F(1.0)];
        assert_eq!(pacer.select(0.5, pull_from(&mut queue), |_| {}), None);
        assert_eq!(pacer.select(1.0, || None, |_| {}), Some(F(1.0)));
        assert_eq!(pacer.stats().dropped, 0);
        assert_eq!(pacer.stats().presented, 1);
    }

    #[test]
    fn empty_source_yields_none() {
        let mut pacer: Pacer<F> = Pacer::new();
        assert_eq!(pacer.select(10.0, || None, |_| {}), None);
        assert_eq!(pacer.stats().presented, 0);
    }

    #[test]
    fn exactly_due_frame_is_presented() {
        let mut pacer = Pacer::new();
        let mut queue = vec![F(0.5)];
        assert_eq!(pacer.select(0.5, pull_from(&mut queue), |_| {}), Some(F(0.5)));
    }

    #[test]
    fn timeline_is_monotonic_across_loop_wrap() {
        let mut tl = LoopTimeline::new(None);
        assert_eq!(tl.map(0.00, 0.04), 0.00);
        assert_eq!(tl.map(0.04, 0.04), 0.04);
        assert_eq!(tl.map(0.08, 0.04), 0.08);
        tl.wrap();
        let second = tl.map(0.00, 0.04);
        assert!(
            (second - 0.12).abs() < 1e-9,
            "second iteration starts at {second}"
        );
        assert!(tl.map(0.04, 0.04) > second);
    }

    #[test]
    fn timeline_wrap_uses_nominal_duration_when_longer() {
        let mut tl = LoopTimeline::new(Some(1.0));
        tl.map(0.92, 0.04);
        tl.wrap();
        assert_eq!(tl.map(0.0, 0.04), 1.0);
    }

    #[test]
    fn timeline_wrap_uses_observed_end_when_past_nominal() {
        let mut tl = LoopTimeline::new(Some(1.0));
        tl.map(1.06, 0.04);
        tl.wrap();
        assert!((tl.map(0.0, 0.04) - 1.1).abs() < 1e-9);
    }

    #[test]
    fn timeline_ignores_bogus_durations() {
        let mut tl = LoopTimeline::new(Some(f64::NAN));
        tl.map(0.5, f64::INFINITY);
        tl.wrap();
        assert_eq!(tl.map(0.0, 0.04), 0.5);
    }
}
