#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Rng {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[inline]
    pub fn unit(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }

    #[inline]
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    #[inline]
    pub fn sign(&mut self) -> f32 {
        if self.next_u64() & 1 == 0 { 1.0 } else { -1.0 }
    }
}

#[must_use]
pub fn derived(seed: u32, salt: u32) -> Rng {
    Rng::new(((u64::from(seed)) << 32) | u64::from(salt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_is_in_range_and_deterministic() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..10_000 {
            let x = a.unit();
            assert!((0.0..1.0).contains(&x));
            assert_eq!(x, b.unit());
        }
    }

    #[test]
    fn distinct_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.unit(), b.unit());
    }

    #[test]
    fn mean_is_near_half() {
        let mut r = Rng::new(7);
        let n = 100_000;
        let sum: f64 = (0..n).map(|_| f64::from(r.unit())).sum();
        let mean = sum / f64::from(n);
        assert!((mean - 0.5).abs() < 0.01, "mean={mean}");
    }
}
