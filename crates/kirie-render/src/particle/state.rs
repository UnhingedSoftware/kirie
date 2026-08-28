use kirie_scene::particle::InstanceOverride;
use kirie_scene::value::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Initial {
    pub color: Vec3,
    pub alpha: f32,
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    pub position: Vec3,
    pub velocity: Vec3,
    pub acceleration: Vec3,
    pub rotation: Vec3,
    pub angular_velocity: Vec3,
    pub color: Vec3,
    pub alpha: f32,
    pub size: f32,
    pub lifetime: f32,
    pub age: f32,
    pub frame: f32,
    pub initial: Initial,
    pub seed: u32,
}

impl Particle {
    #[inline]
    #[must_use]
    pub fn life_pos(&self) -> f32 {
        if self.lifetime > 0.0 {
            (self.age / self.lifetime).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overrides {
    pub enabled: bool,
    pub alpha: f32,
    pub size: f32,
    pub lifetime: f32,
    pub rate: f32,
    pub speed: f32,
    pub count: f32,
    pub color: Vec3,
    pub colorn: Vec3,
}

impl Overrides {
    #[must_use]
    pub fn from_scene(o: &InstanceOverride) -> Self {
        Overrides {
            enabled: o.enabled.value,
            alpha: o.alpha.value,
            size: o.size.value,
            lifetime: o.lifetime.value,
            rate: o.rate.value,
            speed: o.speed.value,
            count: o.count.value,
            color: o.color.value,
            colorn: o.colorn.value,
        }
    }
}

impl Default for Overrides {
    fn default() -> Self {
        Overrides {
            enabled: true,
            alpha: 1.0,
            size: 1.0,
            lifetime: 1.0,
            rate: 1.0,
            speed: 1.0,
            count: 1.0,
            color: [1.0, 1.0, 1.0],
            colorn: [1.0, 1.0, 1.0],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpriteInstance {
    pub position_size: [f32; 4],
    pub color: [f32; 4],
    pub rotation_frame: [f32; 4],
    pub velocity: [f32; 4],
}
