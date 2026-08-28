use std::time::Instant;

#[derive(Debug, Clone)]
pub(crate) struct WallClock {
    base_media: f64,
    anchor: Instant,
    speed: f64,
    paused: bool,
}

impl WallClock {
    pub fn new(now: Instant, paused: bool) -> Self {
        Self {
            base_media: 0.0,
            anchor: now,
            speed: 1.0,
            paused,
        }
    }

    pub fn now(&self, now: Instant) -> f64 {
        if self.paused {
            self.base_media
        } else {
            self.base_media + now.saturating_duration_since(self.anchor).as_secs_f64() * self.speed
        }
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        if self.paused == paused {
            return;
        }
        self.base_media = self.now(now);
        self.anchor = now;
        self.paused = paused;
    }

    pub fn set_speed(&mut self, speed: f64, now: Instant) {
        self.base_media = self.now(now);
        self.anchor = now;
        self.speed = if speed > 0.0 && speed.is_finite() {
            speed
        } else {
            1.0
        };
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProducerSnap {
    pub pushed: u64,
    pub head: f64,
    pub speed: f64,
}

impl Default for ProducerSnap {
    fn default() -> Self {
        Self {
            pushed: 0,
            head: 0.0,
            speed: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConsumerSnap {
    pub consumed: u64,
    pub at: Instant,
    pub paused: bool,
}

impl ConsumerSnap {
    pub fn initial(now: Instant) -> Self {
        Self {
            consumed: 0,
            at: now,
            paused: false,
        }
    }
}

pub(crate) fn audio_position(
    prod: &ProducerSnap,
    cons: &ConsumerSnap,
    sample_rate: u32,
    now: Instant,
) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    let buffered = prod.pushed.saturating_sub(cons.consumed) as f64 / f64::from(sample_rate) * prod.speed;
    let mut pos = prod.head - buffered;
    if !cons.paused {
        pos += now.saturating_duration_since(cons.at).as_secs_f64() * prod.speed;
    }
    pos.clamp(0.0, prod.head.max(0.0))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ConsumerSnap, ProducerSnap, WallClock, audio_position};

    #[test]
    fn wall_clock_advances_with_time() {
        let t0 = Instant::now();
        let clock = WallClock::new(t0, false);
        assert_eq!(clock.now(t0), 0.0);
        assert!((clock.now(t0 + Duration::from_secs(2)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn wall_clock_pause_freezes_and_resume_continues() {
        let t0 = Instant::now();
        let mut clock = WallClock::new(t0, false);
        let t1 = t0 + Duration::from_secs(1);
        clock.set_paused(true, t1);
        assert!((clock.now(t1 + Duration::from_secs(5)) - 1.0).abs() < 1e-9);
        let t2 = t1 + Duration::from_secs(5);
        clock.set_paused(false, t2);
        assert!((clock.now(t2 + Duration::from_secs(1)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn wall_clock_starts_paused_when_asked() {
        let t0 = Instant::now();
        let clock = WallClock::new(t0, true);
        assert_eq!(clock.now(t0 + Duration::from_secs(3)), 0.0);
    }

    #[test]
    fn wall_clock_speed_scales_time() {
        let t0 = Instant::now();
        let mut clock = WallClock::new(t0, false);
        let t1 = t0 + Duration::from_secs(1);
        clock.set_speed(2.0, t1);
        assert!((clock.now(t1 + Duration::from_secs(2)) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn wall_clock_coerces_nonpositive_speed_to_one() {
        let t0 = Instant::now();
        let mut clock = WallClock::new(t0, false);
        clock.set_speed(0.0, t0);
        assert!((clock.now(t0 + Duration::from_secs(1)) - 1.0).abs() < 1e-9);
        clock.set_speed(-3.0, t0);
        assert!((clock.now(t0 + Duration::from_secs(1)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn audio_position_accounts_for_ring_backlog() {
        let now = Instant::now();
        let prod = ProducerSnap {
            pushed: 48_000,
            head: 2.0,
            speed: 1.0,
        };
        let cons = ConsumerSnap {
            consumed: 24_000,
            at: now,
            paused: true,
        };
        assert!((audio_position(&prod, &cons, 48_000, now) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn audio_position_extrapolates_while_playing() {
        let at = Instant::now();
        let prod = ProducerSnap {
            pushed: 48_000,
            head: 2.0,
            speed: 1.0,
        };
        let cons = ConsumerSnap {
            consumed: 24_000,
            at,
            paused: false,
        };
        let pos = audio_position(&prod, &cons, 48_000, at + Duration::from_millis(100));
        assert!((pos - 1.6).abs() < 1e-9);
    }

    #[test]
    fn audio_position_clamped_to_decoded_head() {
        let at = Instant::now();
        let prod = ProducerSnap::default();
        let cons = ConsumerSnap {
            consumed: 0,
            at,
            paused: false,
        };
        assert_eq!(
            audio_position(&prod, &cons, 48_000, at + Duration::from_secs(1)),
            0.0
        );
    }

    #[test]
    fn audio_position_zero_rate_is_safe() {
        let now = Instant::now();
        assert_eq!(
            audio_position(&ProducerSnap::default(), &ConsumerSnap::initial(now), 0, now),
            0.0
        );
    }
}
