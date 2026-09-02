use kirie_scene::particle::{InstanceOverride, ParticleSystem};
use kirie_scene::value::Vec3;

use super::emitter::CompiledEmitter;
use super::initializer::{Initializer, SpawnCtx};
use super::operator::{Operator, StepCtx};
use super::rng::Rng;
use super::state::{Initial, Overrides, Particle, SpriteInstance};

pub const MAX_DT: f32 = 0.1;

const BASE_SIZE: f32 = 20.0;

const MAX_POOL: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FrameMode {
    #[default]
    Loop,
    Once,
    RandomFrame,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteSheet {
    pub frames: u32,
    pub frame_duration: f32,
    pub mode: FrameMode,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SimConfig {
    pub seed: u64,
    pub sheet: Option<SpriteSheet>,
}

pub struct ParticleSim {
    particles: Vec<Particle>,
    capacity: usize,
    emitters: Vec<CompiledEmitter>,
    initializers: Vec<Initializer>,
    operators: Vec<Operator>,
    control_points: Vec<Vec3>,
    cp_pointer_offset: Vec<Option<Vec3>>,
    overrides: Overrides,
    paused: bool,
    stopped: bool,
    perspective: bool,
    sequence_multiplier: f32,
    sheet: Option<SpriteSheet>,
    rng: Rng,
    time: f32,
    next_seed: u32,
    total_spawned: u64,
}

impl ParticleSim {
    #[must_use]
    pub fn new(system: &ParticleSystem, overrides: &InstanceOverride, config: SimConfig) -> Self {
        let ov = Overrides::from_scene(overrides);
        let capacity = pool_size(system.maxcount, ov.count);

        let emitters: Vec<CompiledEmitter> = system.emitters.iter().map(CompiledEmitter::compile).collect();
        let initializers: Vec<Initializer> = system.initializers.iter().map(Initializer::compile).collect();

        let mut rng = Rng::new(config.seed);
        let operators: Vec<Operator> = system
            .operators
            .iter()
            .enumerate()
            .map(|(i, s)| Operator::compile(s, 0x9E37_79B9u32.wrapping_mul(i as u32 + 1), &mut rng))
            .collect();

        let mut control_points: Vec<Vec3> = system.controlpoints.iter().map(|cp| super::math::flip_y(cp.offset)).collect();
        if control_points.is_empty() {
            control_points.push([0.0, 0.0, 0.0]);
        }
        let mut cp_pointer_offset: Vec<Option<Vec3>> = system
            .controlpoints
            .iter()
            .map(|cp| cp.locktopointer.then(|| super::math::flip_y(cp.offset)))
            .collect();
        cp_pointer_offset.resize(control_points.len(), None);

        let perspective = system.flags & 0x4 != 0;

        ParticleSim {
            particles: Vec::with_capacity(capacity),
            capacity,
            emitters,
            initializers,
            operators,
            control_points,
            cp_pointer_offset,
            overrides: ov,
            paused: false,
            stopped: false,
            perspective,
            sequence_multiplier: system.sequencemultiplier,
            sheet: config.sheet,
            rng,
            time: 0.0,
            next_seed: 0x1234_5678,
            total_spawned: 0,
        }
    }

    pub fn update(&mut self, dt: f32) {
        if !self.overrides.enabled || self.paused || self.stopped {
            return;
        }
        let dt = dt.clamp(0.0, MAX_DT);
        self.time += dt;

        self.run_emitters(dt);
        self.age_particles(dt);
        self.run_operators(dt);
        self.compute_frames();
        self.compact();
    }

    pub fn play(&mut self) {
        self.paused = false;
        self.stopped = false;
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.particles.clear();
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.overrides.enabled && !self.paused && !self.stopped
    }

    pub fn emit_burst(&mut self, count: u32) {
        if self.emitters.is_empty() {
            return;
        }
        for i in 0..count as usize {
            if !self.spawn_one(i % self.emitters.len()) {
                break;
            }
        }
    }

    pub fn set_instance_scalar(&mut self, name: &str, v: f32) -> bool {
        match name {
            "alpha" => self.overrides.alpha = v,
            "size" => self.overrides.size = v,
            "count" => self.overrides.count = v,
            "speed" => self.overrides.speed = v,
            "lifetime" => self.overrides.lifetime = v,
            "rate" => self.overrides.rate = v,
            _ => return false,
        }
        true
    }

    pub fn set_instance_colorn(&mut self, v: Vec3) {
        self.overrides.colorn = v;
    }

    pub fn set_control_point(&mut self, idx: usize, pos: Vec3) {
        if idx >= 8 {
            return;
        }
        if idx >= self.control_points.len() {
            self.control_points.resize(idx + 1, [0.0, 0.0, 0.0]);
            self.cp_pointer_offset.resize(idx + 1, None);
        }
        self.control_points[idx] = pos;
    }

    fn run_emitters(&mut self, dt: f32) {
        for e_idx in 0..self.emitters.len() {
            let n = self.emitters[e_idx].tick(dt, self.time, self.overrides.rate, 1.0, &mut self.rng);
            for _ in 0..n {
                if !self.spawn_one(e_idx) {
                    break;
                }
            }
        }
    }

    fn spawn_one(&mut self, e_idx: usize) -> bool {
        if self.particles.len() >= self.capacity {
            return false;
        }
        let (position, velocity) =
            self.emitters[e_idx].spawn(&self.control_points, self.perspective, &mut self.rng);
        let mut pt = self.new_particle(position, velocity);
        let spawn_ctx = SpawnCtx {
            overrides: &self.overrides,
            perspective: self.perspective,
            control_points: &self.control_points,
        };
        for init in &mut self.initializers {
            init.apply(&mut pt, &spawn_ctx, &mut self.rng);
        }
        self.particles.push(pt);
        self.total_spawned += 1;
        true
    }

    fn new_particle(&mut self, position: Vec3, velocity: Vec3) -> Particle {
        let seed = self.next_seed;
        self.next_seed = self.next_seed.wrapping_add(0x9E37_79B9);
        let color = self.overrides.colorn;
        let alpha = self.overrides.alpha;
        let size = BASE_SIZE * self.overrides.size;
        let lifetime = self.overrides.lifetime;
        Particle {
            position,
            velocity,
            acceleration: [0.0; 3],
            rotation: [0.0; 3],
            angular_velocity: [0.0; 3],
            color,
            alpha,
            size,
            lifetime,
            age: 0.0,
            frame: 0.0,
            initial: Initial { color, alpha, size },
            seed,
        }
    }

    fn age_particles(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.age += dt;
        }
    }

    fn run_operators(&mut self, dt: f32) {
        let ctx = StepCtx {
            dt,
            time: self.time,
            overrides: &self.overrides,
            control_points: &self.control_points,
        };
        for op in &self.operators {
            for p in &mut self.particles {
                op.apply(p, &ctx);
            }
        }
    }

    fn compute_frames(&mut self) {
        let Some(sheet) = self.sheet else { return };
        let frames = sheet.frames.max(1);
        if frames == 1 || sheet.frame_duration <= 0.0 {
            return;
        }
        let fps = self.sequence_multiplier / sheet.frame_duration;
        for p in &mut self.particles {
            let raw = p.age * fps;
            p.frame = match sheet.mode {
                FrameMode::RandomFrame => {
                    let mut r = super::rng::derived(p.seed, 0xF00D);
                    (r.unit() * frames as f32).floor().min((frames - 1) as f32)
                }
                FrameMode::Once => raw.floor().min((frames - 1) as f32),
                FrameMode::Loop => raw.floor().rem_euclid(frames as f32),
            };
        }
    }

    fn compact(&mut self) {
        self.particles.retain(|p| p.lifetime > 0.0 && p.age < p.lifetime);
    }

    pub fn set_rate_override(&mut self, rate: f32) {
        if rate.is_finite() && rate >= 0.0 {
            self.overrides.rate = rate;
        }
    }

    pub fn set_pointer_local(&mut self, pos: Vec3) {
        for (point, offset) in self.control_points.iter_mut().zip(&self.cp_pointer_offset) {
            if let Some(off) = offset {
                *point = [pos[0] + off[0], pos[1] + off[1], pos[2] + off[2]];
            }
        }
    }

    #[must_use]
    pub fn follows_pointer(&self) -> bool {
        self.cp_pointer_offset.iter().any(Option::is_some)
    }

    #[must_use]
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    #[must_use]
    pub fn live_count(&self) -> usize {
        self.particles.len()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn total_spawned(&self) -> u64 {
        self.total_spawned
    }

    #[must_use]
    pub fn has_supported_emitter(&self) -> bool {
        self.emitters.iter().any(CompiledEmitter::is_supported)
    }

    #[must_use]
    pub fn time(&self) -> f32 {
        self.time
    }

    pub fn write_sprites(&self, out: &mut Vec<SpriteInstance>) {
        out.clear();
        let frames = self.sheet.map_or(1u32, |s| s.frames.max(1));
        for p in &self.particles {
            let norm_frame = if frames > 1 { p.frame / frames as f32 } else { 0.0 };
            out.push(SpriteInstance {
                position_size: [p.position[0], p.position[1], p.position[2], p.size],
                color: [p.color[0], p.color[1], p.color[2], p.alpha],
                rotation_frame: [p.rotation[0], p.rotation[1], p.rotation[2], norm_frame],
                velocity: [p.velocity[0], p.velocity[1], p.velocity[2], 0.0],
            });
        }
    }
}

#[must_use]
fn pool_size(maxcount: u32, count_override: f32) -> usize {
    let raw = (maxcount as f32 * count_override.max(0.0)).ceil();
    (raw as usize).clamp(1, MAX_POOL)
}
