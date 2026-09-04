use crate::model::{
    IDENTITY, PuppetAnimation, PuppetKey, PuppetMesh, PuppetTrack, key_matrix, matrix_invert, matrix_mul,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PuppetLayer {
    pub id: i64,
    pub name: String,
    pub clip: Option<usize>,
    pub rate: f32,
    pub blend: f32,
    pub additive: bool,
    pub visible: bool,
    pub playing: bool,
    pub time: f32,
}

impl PuppetLayer {
    #[must_use]
    pub fn new(id: i64, name: impl Into<String>, clip: Option<usize>) -> Self {
        PuppetLayer {
            id,
            name: name.into(),
            clip,
            rate: 1.0,
            blend: 1.0,
            additive: false,
            visible: true,
            playing: true,
            time: 0.0,
        }
    }

    fn active(&self) -> bool {
        self.visible && self.clip.is_some() && self.blend > 0.0
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PuppetPlayer {
    pub layers: Vec<PuppetLayer>,
}

impl PuppetPlayer {
    #[must_use]
    pub fn from_layers(layers: Vec<PuppetLayer>) -> Self {
        PuppetPlayer { layers }
    }

    pub fn layer_mut(&mut self, id: i64) -> Option<&mut PuppetLayer> {
        self.layers.iter_mut().find(|layer| layer.id == id)
    }

    #[must_use]
    pub fn is_animating(&self, mesh: &PuppetMesh) -> bool {
        self.layers.iter().any(|layer| {
            layer.active()
                && layer.playing
                && layer.rate != 0.0
                && layer
                    .clip
                    .and_then(|index| mesh.animations.get(index))
                    .is_some_and(|clip| clip.frames > 1 && clip.fps > 0.0)
        })
    }

    pub fn advance(&mut self, mesh: &PuppetMesh, dt: f32) -> bool {
        let mut moved = false;
        for layer in &mut self.layers {
            if !layer.active() || !layer.playing || layer.rate == 0.0 {
                continue;
            }
            let Some(clip) = layer.clip.and_then(|index| mesh.animations.get(index)) else {
                continue;
            };
            let duration = clip.duration();
            if duration <= 0.0 {
                continue;
            }
            let next = layer.time + dt * layer.rate;
            layer.time = if clip.loops() {
                next.rem_euclid(duration)
            } else if next >= duration {
                layer.playing = false;
                duration
            } else if next <= 0.0 {
                layer.playing = false;
                0.0
            } else {
                next
            };
            moved = true;
        }
        moved
    }

    #[must_use]
    pub fn bone_world(&self, mesh: &PuppetMesh) -> Vec<[f32; 16]> {
        let mut world: Vec<[f32; 16]> = Vec::with_capacity(mesh.bones.len());
        for (index, bone) in mesh.bones.iter().enumerate() {
            let rest = decompose_affine(bone.transform);
            let mut mixed = rest;
            let mut keyed = false;
            for layer in self.layers.iter().filter(|layer| layer.active()) {
                let Some(clip) = layer.clip.and_then(|at| mesh.animations.get(at)) else {
                    continue;
                };
                let Some(track) = clip.tracks.iter().find(|track| track.bone == index) else {
                    continue;
                };
                let Some(key) = clip.sample(track, layer.time) else {
                    continue;
                };
                let weight = layer.blend.clamp(0.0, 1.0);
                keyed = true;
                if layer.additive {
                    let base = track.keys.first().copied().unwrap_or(rest);
                    mixed = add_scaled(mixed, sub(key, base), weight);
                } else {
                    mixed = lerp(mixed, key, weight);
                }
            }
            let local = if keyed { key_matrix(mixed) } else { bone.transform };
            let up = usize::try_from(bone.parent)
                .ok()
                .filter(|parent| *parent < index)
                .and_then(|parent| world.get(parent).copied());
            world.push(up.map_or(local, |parent| matrix_mul(local, parent)));
        }
        world
    }

    #[must_use]
    pub fn pose(&self, mesh: &PuppetMesh) -> Vec<[f32; 16]> {
        let rest = PuppetPlayer::default().bone_world(mesh);
        let now = self.bone_world(mesh);
        rest.iter()
            .zip(now.iter())
            .map(|(bind, posed)| matrix_invert(*bind).map_or(IDENTITY, |back| matrix_mul(back, *posed)))
            .collect()
    }

    #[must_use]
    pub fn anchor(&self, mesh: &PuppetMesh, name: &str) -> Option<[f32; 3]> {
        let point = mesh.attachment(name)?;
        let local = point.translation();
        let world = self.bone_world(mesh);
        let Some(matrix) = world.get(point.bone) else {
            return Some(local);
        };
        Some(crate::model::puppet_skin_point(local, *matrix))
    }
}

impl PuppetAnimation {
    #[must_use]
    pub fn loops(&self) -> bool {
        self.mode != "single"
    }

    #[must_use]
    pub fn duration(&self) -> f32 {
        if self.fps <= 0.0 {
            0.0
        } else {
            self.frames as f32 / self.fps
        }
    }

    #[must_use]
    pub fn sample(&self, track: &PuppetTrack, time: f32) -> Option<PuppetKey> {
        let keys = track.keys.len();
        if keys == 0 {
            return None;
        }
        if keys == 1 || self.fps <= 0.0 {
            return track.keys.first().copied();
        }
        let last = (keys - 1) as f32;
        let frame = time * self.fps;
        let position = if self.loops() {
            let period = if self.frames >= 1 && (self.frames as f32) <= last {
                self.frames as f32
            } else {
                last
            };
            frame.rem_euclid(period)
        } else {
            frame.clamp(0.0, last)
        };
        let lower = position.floor();
        let blend = position - lower;
        let first = track.keys.get(lower as usize)?;
        let second = track.keys.get(lower as usize + 1).unwrap_or(first);
        Some(lerp(*first, *second, blend))
    }
}

fn lerp(a: PuppetKey, b: PuppetKey, t: f32) -> PuppetKey {
    let mix = |x: [f32; 3], y: [f32; 3]| {
        [
            x[0] + (y[0] - x[0]) * t,
            x[1] + (y[1] - x[1]) * t,
            x[2] + (y[2] - x[2]) * t,
        ]
    };
    PuppetKey {
        translation: mix(a.translation, b.translation),
        rotation: mix(a.rotation, b.rotation),
        scale: mix(a.scale, b.scale),
    }
}

fn sub(a: PuppetKey, b: PuppetKey) -> PuppetKey {
    let diff = |x: [f32; 3], y: [f32; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
    PuppetKey {
        translation: diff(a.translation, b.translation),
        rotation: diff(a.rotation, b.rotation),
        scale: diff(a.scale, b.scale),
    }
}

fn add_scaled(a: PuppetKey, delta: PuppetKey, w: f32) -> PuppetKey {
    let add = |x: [f32; 3], d: [f32; 3]| [x[0] + d[0] * w, x[1] + d[1] * w, x[2] + d[2] * w];
    PuppetKey {
        translation: add(a.translation, delta.translation),
        rotation: add(a.rotation, delta.rotation),
        scale: add(a.scale, delta.scale),
    }
}

fn decompose_affine(m: [f32; 16]) -> PuppetKey {
    let row = |r: usize| [m[r * 4], m[r * 4 + 1], m[r * 4 + 2]];
    let len = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let scale = [len(row(0)), len(row(1)), len(row(2))];
    let unit = |r: usize| {
        let v = row(r);
        let s = scale[r];
        if s > 1e-12 {
            [v[0] / s, v[1] / s, v[2] / s]
        } else {
            [0.0, 0.0, 0.0]
        }
    };
    let (r0, r1, r2) = (unit(0), unit(1), unit(2));
    let sy = (-r0[2]).clamp(-1.0, 1.0);
    let ry = sy.asin();
    let (rx, rz) = if sy.abs() < 0.999_999 {
        (r1[2].atan2(r2[2]), r0[1].atan2(r0[0]))
    } else {
        ((-r2[1]).atan2(r1[1]), 0.0)
    };
    PuppetKey {
        translation: [m[12], m[13], m[14]],
        rotation: [rx, ry, rz],
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PuppetAnimation, PuppetBone, PuppetKey, PuppetMesh, PuppetTrack};

    fn key(t: [f32; 3], rz: f32) -> PuppetKey {
        PuppetKey {
            translation: t,
            rotation: [0.0, 0.0, rz],
            scale: [1.0, 1.0, 1.0],
        }
    }

    fn translation(x: f32, y: f32) -> [f32; 16] {
        let mut m = IDENTITY;
        m[12] = x;
        m[13] = y;
        m
    }

    fn rig() -> PuppetMesh {
        PuppetMesh {
            version: "MDLV0013".into(),
            vertices: vec![],
            indices: vec![],
            bones: vec![
                PuppetBone {
                    name: "root".into(),
                    parent: -1,
                    transform: translation(10.0, 20.0),
                },
                PuppetBone {
                    name: "child".into(),
                    parent: 0,
                    transform: translation(5.0, 0.0),
                },
            ],
            attachments: vec![],
            animations: vec![
                PuppetAnimation {
                    id: 7,
                    name: "walk".into(),
                    mode: "loop".into(),
                    fps: 10.0,
                    frames: 2,
                    tracks: vec![PuppetTrack {
                        bone: 0,
                        keys: vec![
                            key([10.0, 20.0, 0.0], 0.0),
                            key([30.0, 20.0, 0.0], 0.0),
                            key([10.0, 20.0, 0.0], 0.0),
                        ],
                    }],
                    shapes: vec![],
                },
                PuppetAnimation {
                    id: 8,
                    name: "bob".into(),
                    mode: "loop".into(),
                    fps: 10.0,
                    frames: 2,
                    tracks: vec![PuppetTrack {
                        bone: 0,
                        keys: vec![
                            key([10.0, 20.0, 0.0], 0.0),
                            key([10.0, 120.0, 0.0], 0.0),
                            key([10.0, 20.0, 0.0], 0.0),
                        ],
                    }],
                    shapes: vec![],
                },
                PuppetAnimation {
                    id: 9,
                    name: "once".into(),
                    mode: "single".into(),
                    fps: 10.0,
                    frames: 2,
                    tracks: vec![PuppetTrack {
                        bone: 1,
                        keys: vec![key([5.0, 0.0, 0.0], 0.0), key([5.0, 0.0, 0.0], 1.0)],
                    }],
                    shapes: vec![],
                },
            ],
        }
    }

    fn root_xy(world: &[[f32; 16]], bone: usize) -> [f32; 2] {
        [world[bone][12], world[bone][13]]
    }

    #[test]
    fn decompose_roundtrips_rotation_and_scale() {
        let k = PuppetKey {
            translation: [3.0, -4.0, 0.5],
            rotation: [0.3, -0.2, 1.1],
            scale: [2.0, 0.5, 1.5],
        };
        let back = decompose_affine(key_matrix(k));
        for (a, b) in k.translation.iter().zip(back.translation.iter()) {
            assert!((a - b).abs() < 1e-4);
        }
        for (a, b) in k.rotation.iter().zip(back.rotation.iter()) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
        for (a, b) in k.scale.iter().zip(back.scale.iter()) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn loop_clip_wraps_and_child_follows() {
        let mesh = rig();
        let mut player = PuppetPlayer::from_layers(vec![PuppetLayer::new(1, "a", Some(0))]);
        assert!(player.is_animating(&mesh));
        assert!(player.advance(&mesh, 0.1));
        let world = player.bone_world(&mesh);
        assert_eq!(root_xy(&world, 0), [30.0, 20.0]);
        assert_eq!(root_xy(&world, 1), [35.0, 20.0]);
        player.advance(&mesh, 0.1);
        assert!((player.time_of(1) - 0.0).abs() < 1e-5);
        assert_eq!(root_xy(&player.bone_world(&mesh), 0), [10.0, 20.0]);
        player.advance(&mesh, 0.05);
        assert_eq!(root_xy(&player.bone_world(&mesh), 0), [20.0, 20.0]);
    }

    #[test]
    fn single_clip_holds_last_frame_and_stops() {
        let mesh = rig();
        let mut player = PuppetPlayer::from_layers(vec![PuppetLayer::new(1, "a", Some(2))]);
        player.advance(&mesh, 5.0);
        assert!(!player.layers[0].playing);
        assert!((player.layers[0].time - 0.2).abs() < 1e-6);
        let world = player.bone_world(&mesh);
        assert!((world[1][0] - 1.0f32.cos()).abs() < 1e-5);
        assert!(!player.is_animating(&mesh));
    }

    #[test]
    fn additive_layer_stacks_on_base_layer() {
        let mesh = rig();
        let mut bob = PuppetLayer::new(2, "bob", Some(1));
        bob.additive = true;
        let mut player = PuppetPlayer::from_layers(vec![PuppetLayer::new(1, "walk", Some(0)), bob]);
        player.advance(&mesh, 0.1);
        let world = player.bone_world(&mesh);
        assert_eq!(root_xy(&world, 0), [30.0, 120.0]);
    }

    #[test]
    fn blend_weight_mixes_toward_rest() {
        let mesh = rig();
        let mut layer = PuppetLayer::new(1, "walk", Some(0));
        layer.blend = 0.5;
        let mut player = PuppetPlayer::from_layers(vec![layer]);
        player.advance(&mesh, 0.1);
        assert_eq!(root_xy(&player.bone_world(&mesh), 0), [20.0, 20.0]);
    }

    #[test]
    fn hidden_and_paused_layers_do_not_move() {
        let mesh = rig();
        let mut hidden = PuppetLayer::new(1, "walk", Some(0));
        hidden.visible = false;
        let mut paused = PuppetLayer::new(2, "bob", Some(1));
        paused.playing = false;
        let mut player = PuppetPlayer::from_layers(vec![hidden, paused]);
        assert!(!player.is_animating(&mesh));
        assert!(!player.advance(&mesh, 0.1));
        assert_eq!(root_xy(&player.bone_world(&mesh), 0), [10.0, 20.0]);
    }

    #[test]
    fn pose_is_identity_at_rest() {
        let mesh = rig();
        let player = PuppetPlayer::default();
        for m in player.pose(&mesh) {
            for (a, b) in m.iter().zip(IDENTITY.iter()) {
                assert!((a - b).abs() < 1e-6);
            }
        }
    }

    impl PuppetPlayer {
        fn time_of(&self, id: i64) -> f32 {
            self.layers.iter().find(|l| l.id == id).map_or(0.0, |l| l.time)
        }
    }
}
