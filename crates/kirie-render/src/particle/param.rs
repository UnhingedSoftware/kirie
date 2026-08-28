use kirie_scene::particle::NamedStage;
use kirie_scene::value::{DynamicValue, Vec3};

pub struct Params<'a>(pub &'a NamedStage);

impl<'a> Params<'a> {
    #[must_use]
    pub fn new(stage: &'a NamedStage) -> Self {
        Params(stage)
    }

    fn get(&self, key: &str) -> Option<&DynamicValue> {
        self.0.params.get(key).map(|us| &us.value)
    }

    #[must_use]
    pub fn f32(&self, key: &str, default: f32) -> f32 {
        match self.get(key) {
            Some(DynamicValue::Null) | None => default,
            Some(v) => v.as_f32(),
        }
    }

    #[must_use]
    pub fn i64(&self, key: &str, default: i64) -> i64 {
        match self.get(key) {
            Some(DynamicValue::Null) | None => default,
            Some(v) => v.as_f32() as i64,
        }
    }

    #[must_use]
    pub fn vec3(&self, key: &str, default: Vec3) -> Vec3 {
        match self.get(key) {
            Some(DynamicValue::Null) | None => default,
            Some(DynamicValue::Vec(v)) => {
                let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
                [c(0), c(1), c(2)]
            }
            Some(DynamicValue::Color(c)) => [c[0], c[1], c[2]],
            Some(other) => {
                let s = other.as_f32();
                [s, s, s]
            }
        }
    }

    #[must_use]
    pub fn color3(&self, key: &str, default: Vec3) -> Vec3 {
        match self.get(key) {
            Some(DynamicValue::Null) | None => default,
            Some(DynamicValue::Color(c)) => [c[0], c[1], c[2]],
            Some(DynamicValue::Vec(v)) => {
                let c = |i: usize| v.get(i).copied().unwrap_or(0.0);
                let raw = [c(0), c(1), c(2)];
                if raw.iter().any(|&x| x > 1.0) {
                    [raw[0] / 255.0, raw[1] / 255.0, raw[2] / 255.0]
                } else {
                    raw
                }
            }
            Some(other) => {
                let s = other.as_f32();
                let s = if s > 1.0 { s / 255.0 } else { s };
                [s, s, s]
            }
        }
    }
}
