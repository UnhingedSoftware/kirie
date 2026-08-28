use kirie_scene::value::Vec3;

#[inline]
#[must_use]
pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
#[must_use]
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
#[must_use]
pub fn mul(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
#[must_use]
pub fn mul_comp(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}

#[inline]
#[must_use]
pub fn mul_add(a: Vec3, b: Vec3, s: f32) -> Vec3 {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

#[inline]
#[must_use]
pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
#[must_use]
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
#[must_use]
pub fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

#[inline]
#[must_use]
pub fn normalize_or(a: Vec3, fallback: Vec3) -> Vec3 {
    let len = length(a);
    if len > 1e-8 { mul(a, 1.0 / len) } else { fallback }
}

#[inline]
#[must_use]
pub fn flip_y(a: Vec3) -> Vec3 {
    [a[0], -a[1], a[2]]
}

#[inline]
#[must_use]
pub fn wrap_pi(a: Vec3) -> Vec3 {
    let w = |x: f32| {
        let two_pi = std::f32::consts::TAU;
        let mut v = (x + std::f32::consts::PI).rem_euclid(two_pi) - std::f32::consts::PI;
        if v <= -std::f32::consts::PI {
            v += two_pi;
        }
        v
    };
    [w(a[0]), w(a[1]), w(a[2])]
}
