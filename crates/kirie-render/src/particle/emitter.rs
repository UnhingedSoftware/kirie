use kirie_scene::particle::Emitter;
use kirie_scene::value::Vec3;

use super::math;
use super::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    BoxRandom,
    SphereRandom,
    Unsupported,
}

const FLAG_ONE_PER_FRAME: u32 = 0x2;
const FLAG_PERIODIC: u32 = 0x4;

#[derive(Clone, Debug)]
pub struct CompiledEmitter {
    shape: Shape,
    directions: Vec3,
    distancemin: Vec3,
    distancemax: Vec3,
    origin: Vec3,
    sign: [i32; 3],
    instantaneous: bool,
    speedmin: f32,
    speedmax: f32,
    rate: f32,
    controlpoint: usize,
    flags: u32,
    delay: f32,
    duration: f32,
    minperiodicdelay: f32,
    maxperiodicdelay: f32,
    minperiodicduration: f32,
    maxperiodicduration: f32,

    accumulator: f32,
    active: bool,
    burst_fired: bool,
    next_toggle: f32,
    primed: bool,
}

impl CompiledEmitter {
    #[must_use]
    pub fn compile(e: &Emitter) -> Self {
        let shape = match e.name.as_str() {
            "boxrandom" => Shape::BoxRandom,
            "sphererandom" => Shape::SphereRandom,
            _ => Shape::Unsupported,
        };
        CompiledEmitter {
            shape,
            directions: math::flip_y(e.directions),
            distancemin: e.distancemin,
            distancemax: e.distancemax,
            origin: math::flip_y(e.origin),
            sign: e.sign,
            instantaneous: e.instantaneous != 0,
            speedmin: e.speedmin,
            speedmax: e.speedmax,
            rate: e.rate,
            controlpoint: e.controlpoint.max(0) as usize,
            flags: e.flags,
            delay: e.delay,
            duration: e.duration,
            minperiodicdelay: e.minperiodicdelay,
            maxperiodicdelay: e.maxperiodicdelay,
            minperiodicduration: e.minperiodicduration,
            maxperiodicduration: e.maxperiodicduration,
            accumulator: 0.0,
            active: false,
            burst_fired: false,
            next_toggle: 0.0,
            primed: false,
        }
    }

    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.shape != Shape::Unsupported
    }

    pub fn tick(&mut self, dt: f32, time: f32, rate_override: f32, audio_factor: f32, rng: &mut Rng) -> u32 {
        if self.shape == Shape::Unsupported {
            return 0;
        }

        let was_active = self.active;
        self.active = self.compute_active(time, rng);
        if self.active && !was_active {
            self.burst_fired = false;
        }
        if !self.active {
            return 0;
        }

        if self.instantaneous {
            if self.burst_fired {
                return 0;
            }
            self.burst_fired = true;
            return (self.rate * rate_override).round().max(0.0) as u32;
        }

        self.accumulator += dt * self.rate * rate_override * audio_factor;
        let mut n = self.accumulator.floor().max(0.0);
        self.accumulator -= n;
        if self.flags & FLAG_ONE_PER_FRAME != 0 {
            n = n.min(1.0);
        }
        n as u32
    }

    fn compute_active(&mut self, time: f32, rng: &mut Rng) -> bool {
        if self.flags & FLAG_PERIODIC != 0 {
            if !self.primed {
                self.primed = true;
                self.active = false;
                self.next_toggle = self.delay + rng.range(self.minperiodicdelay, self.maxperiodicdelay);
                return false;
            }
            if time >= self.next_toggle {
                let now_active = !self.active;
                let span = if now_active {
                    rng.range(self.minperiodicduration, self.maxperiodicduration)
                } else {
                    rng.range(self.minperiodicdelay, self.maxperiodicdelay)
                };
                self.next_toggle = time + span.max(0.0);
                return now_active;
            }
            return self.active;
        }
        if time < self.delay {
            return false;
        }
        self.duration <= 0.0 || time <= self.delay + self.duration
    }

    #[must_use]
    pub fn spawn(&self, control_points: &[Vec3], perspective: bool, rng: &mut Rng) -> (Vec3, Vec3) {
        let base = math::add(
            self.origin,
            control_points.get(self.controlpoint).copied().unwrap_or([0.0; 3]),
        );
        match self.shape {
            Shape::BoxRandom => {
                let off = [
                    rng.range(self.distancemin[0], self.distancemax[0]) * rng.sign() * self.directions[0],
                    rng.range(self.distancemin[1], self.distancemax[1]) * rng.sign() * self.directions[1],
                    rng.range(self.distancemin[2], self.distancemax[2]) * rng.sign() * self.directions[2],
                ];
                (math::add(base, off), [0.0, 0.0, 0.0])
            }
            Shape::SphereRandom => {
                let rmin = self.distancemin[0];
                let rmax = self.distancemax[0];
                let u = rng.unit();
                let mut dir;
                let radius;
                if perspective {
                    radius = (lerp(rmin.powi(3), rmax.powi(3), u)).cbrt();
                    let z = rng.range(-1.0, 1.0);
                    let phi = rng.range(0.0, std::f32::consts::TAU);
                    let s = (1.0 - z * z).max(0.0).sqrt();
                    dir = [s * phi.cos(), s * phi.sin(), z];
                } else {
                    radius = lerp(rmin * rmin, rmax * rmax, u).sqrt();
                    let theta = rng.range(0.0, std::f32::consts::TAU);
                    dir = [theta.cos(), theta.sin(), 0.0];
                }
                for (d, &s) in dir.iter_mut().zip(self.sign.iter()) {
                    if s != 0 {
                        *d = d.abs() * (s.signum() as f32);
                    }
                }
                let offset = [
                    dir[0] * radius * self.directions[0],
                    dir[1] * radius * self.directions[1],
                    dir[2] * radius * self.directions[2],
                ];
                let pos = math::add(base, offset);
                let vel = math::mul(dir, rng.range(self.speedmin, self.speedmax));
                (pos, vel)
            }
            Shape::Unsupported => (base, [0.0, 0.0, 0.0]),
        }
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
