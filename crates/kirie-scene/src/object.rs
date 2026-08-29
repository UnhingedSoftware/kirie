use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::material::{Combos, TextureSlots, parse_combos, parse_textures};
use crate::particle::{InstanceOverride, ParticleSystem};
use crate::user::{
    ConstantValues, UserSetting, parse_constant_values, user_bool, user_color, user_f32, user_i64,
    user_string, user_vec2, user_vec3,
};
use crate::value::{Color, Vec2, Vec3, WHITE, coerce_f64, coerce_i64};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub frame: f32,
    pub value: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnimMode {
    Mirror,
    #[default]
    Loop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct AnimationTrack {
    pub c0: Vec<Keyframe>,
    pub c1: Vec<Keyframe>,
    pub c2: Vec<Keyframe>,
    pub fps: f32,
    pub length: f32,
    pub mode: AnimMode,
    pub relative: bool,
}

impl AnimationTrack {
    fn parse(obj: &Map<String, Value>) -> Self {
        let channel = |k: &str| -> Vec<Keyframe> {
            match obj.get(k) {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|kf| {
                        let o = kf.as_object()?;
                        Some(Keyframe {
                            frame: coerce_f64(o.get("frame")?)? as f32,
                            value: coerce_f64(o.get("value")?)? as f32,
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let options = obj.get("options").and_then(Value::as_object);
        let fps = options
            .and_then(|o| o.get("fps"))
            .and_then(coerce_f64)
            .map_or(30.0, |v| v as f32);
        let length = options
            .and_then(|o| o.get("length"))
            .and_then(coerce_f64)
            .map_or(0.0, |v| v as f32);
        let mode = match options.and_then(|o| o.get("mode")).and_then(Value::as_str) {
            Some("mirror") => AnimMode::Mirror,
            _ => AnimMode::Loop,
        };
        let relative = obj
            .get("relative")
            .and_then(crate::value::coerce_bool)
            .unwrap_or(true);
        AnimationTrack {
            c0: channel("c0"),
            c1: channel("c1"),
            c2: channel("c2"),
            fps,
            length,
            mode,
            relative,
        }
    }

    #[must_use]
    pub fn sample(&self, time_secs: f32) -> Option<[f32; 3]> {
        if self.length <= 0.0 || (self.c0.is_empty() && self.c1.is_empty() && self.c2.is_empty()) {
            return None;
        }
        let mut frame = time_secs * self.fps;
        match self.mode {
            AnimMode::Mirror => {
                let period = 2.0 * self.length;
                frame = frame.rem_euclid(period);
                if frame > self.length {
                    frame = period - frame;
                }
            }
            AnimMode::Loop => frame = frame.rem_euclid(self.length),
        }
        Some([
            eval_channel(&self.c0, frame),
            eval_channel(&self.c1, frame),
            eval_channel(&self.c2, frame),
        ])
    }
}

fn eval_channel(keys: &[Keyframe], frame: f32) -> f32 {
    match (keys.first(), keys.last()) {
        (None, _) => 0.0,
        (Some(first), _) if frame <= first.frame => first.value,
        (_, Some(last)) if frame >= last.frame => last.value,
        _ => {
            for w in keys.windows(2) {
                if frame <= w[1].frame {
                    let (a, b) = (w[0], w[1]);
                    let span = b.frame - a.frame;
                    let t = if span > 0.0 { (frame - a.frame) / span } else { 0.0 };
                    return a.value + (b.value - a.value) * t;
                }
            }
            keys.last().map_or(0.0, |k| k.value)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaseObject {
    pub id: i64,
    pub name: String,
    pub sortorder: i64,
    pub dependencies: Vec<i64>,
    pub parent: Option<i64>,
    pub origin: UserSetting<Vec3>,
    pub scale: UserSetting<Vec3>,
    pub angles: UserSetting<Vec3>,
    pub angles_animation: Option<AnimationTrack>,
    pub visible: UserSetting<bool>,
    pub attachment: Option<String>,
}

impl BaseObject {
    fn parse(obj: &Map<String, Value>) -> Self {
        let id = obj.get("id").and_then(coerce_i64).unwrap_or(-1);
        let name = match obj.get("name") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => "unknown".to_owned(),
        };
        let dependencies = match obj.get("dependencies") {
            Some(Value::Array(a)) => a.iter().filter_map(coerce_i64).collect(),
            _ => Vec::new(),
        };
        let angles_animation = obj
            .get("angles")
            .and_then(Value::as_object)
            .and_then(|o| o.get("animation"))
            .and_then(Value::as_object)
            .map(AnimationTrack::parse);
        BaseObject {
            id,
            name,
            sortorder: obj.get("sortorder").and_then(coerce_i64).unwrap_or(0),
            dependencies,
            parent: obj.get("parent").and_then(coerce_i64),
            attachment: obj
                .get("attachment")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|name| !name.is_empty()),
            origin: user_vec3(obj, "origin", [0.0, 0.0, 0.0]),
            scale: user_vec3(obj, "scale", [1.0, 1.0, 1.0]),
            angles: user_vec3(obj, "angles", [0.0, 0.0, 0.0]),
            angles_animation,
            visible: user_bool(obj, "visible", true),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub file: String,
    pub id: i64,
    pub name: String,
    pub visible: UserSetting<bool>,
    pub passes: Vec<PassOverride>,
    pub resolved: Option<crate::material::EffectFile>,
}

impl Effect {
    fn parse(obj: &Map<String, Value>) -> Option<Self> {
        let file = obj.get("file").and_then(Value::as_str)?.to_owned();
        let passes = match obj.get("passes") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_object)
                .map(PassOverride::parse)
                .collect(),
            _ => Vec::new(),
        };
        Some(Effect {
            file,
            id: obj.get("id").and_then(coerce_i64).unwrap_or(-1),
            name: obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Effect without name")
                .to_owned(),
            visible: user_bool(obj, "visible", true),
            passes,
            resolved: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PassOverride {
    pub id: i64,
    pub combos: Combos,
    pub constantshadervalues: ConstantValues,
    pub textures: TextureSlots,
    pub usertextures: TextureSlots,
}

impl PassOverride {
    fn parse(obj: &Map<String, Value>) -> Self {
        PassOverride {
            id: obj.get("id").and_then(coerce_i64).unwrap_or(-1),
            combos: parse_combos(obj.get("combos")),
            constantshadervalues: parse_constant_values(obj.get("constantshadervalues")),
            textures: parse_textures(obj.get("textures")),
            usertextures: parse_textures(obj.get("usertextures")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimationLayer {
    pub id: i64,
    pub rate: UserSetting<f32>,
    pub visible: UserSetting<bool>,
    pub blend: UserSetting<f32>,
    pub animation: UserSetting<i64>,
}

impl AnimationLayer {
    fn parse(obj: &Map<String, Value>) -> Option<Self> {
        let id = obj.get("id").and_then(coerce_i64)?;
        Some(AnimationLayer {
            id,
            rate: user_f32(obj, "rate", 1.0),
            visible: user_bool(obj, "visible", false),
            blend: user_f32(obj, "blend", 1.0),
            animation: user_i64(obj, "animation", 0),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Instance {
    pub textures: TextureSlots,
    pub usertextures: TextureSlots,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageObject {
    pub image: String,
    pub model: Option<crate::material::ModelFile>,
    pub material: Option<crate::material::Material>,
    pub scale: UserSetting<Vec3>,
    pub angles: UserSetting<Vec3>,
    pub visible: UserSetting<bool>,
    pub alpha: UserSetting<f32>,
    pub color: UserSetting<Color>,
    pub alignment: String,
    pub size: Vec2,
    pub parallax_depth: UserSetting<Vec2>,
    pub color_blend_mode: UserSetting<i64>,
    pub brightness: UserSetting<f32>,
    pub effects: Vec<Effect>,
    pub animationlayers: Vec<AnimationLayer>,
    pub instance: Option<Instance>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextObject {
    pub text: UserSetting<String>,
    pub font: String,
    pub pointsize: UserSetting<f32>,
    pub size: Vec2,
    pub scale: UserSetting<Vec3>,
    pub color: UserSetting<Color>,
    pub alpha: UserSetting<f32>,
    pub visible: UserSetting<bool>,
    pub horizontalalign: String,
    pub verticalalign: String,
    pub padding: i64,
    pub limitwidth: bool,
    pub maxwidth: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundObject {
    pub sound: Vec<String>,
    pub playbackmode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelObject {
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ObjectKind {
    Image(Box<ImageObject>),
    Sound(SoundObject),
    Particle(Box<ParticleObject>),
    Text(Box<TextObject>),
    Model(ModelObject),
    Light(Map<String, Value>),
    Shape(Map<String, Value>),
    Group,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParticleObject {
    pub scale: UserSetting<Vec3>,
    pub angles: UserSetting<Vec3>,
    pub visible: UserSetting<bool>,
    pub parallax_depth: UserSetting<Vec2>,
    pub particle_file: Option<String>,
    pub system: ParticleSystem,
    pub instanceoverride: InstanceOverride,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub base: BaseObject,
    pub kind: ObjectKind,
    pub extra: Map<String, Value>,
}

impl Object {
    pub fn parse(value: &Value) -> Option<Object> {
        let obj = value.as_object()?;
        let base = BaseObject::parse(obj);

        let kind = if obj.get("image").is_some_and(Value::is_string) {
            ObjectKind::Image(Box::new(parse_image(obj)))
        } else if obj.get("sound").is_some_and(Value::is_array) {
            parse_sound(obj)
        } else if present(obj, "particle") {
            ObjectKind::Particle(Box::new(parse_particle(obj)))
        } else if present(obj, "text") {
            ObjectKind::Text(Box::new(parse_text(obj)))
        } else if obj.get("model").is_some_and(Value::is_string) {
            ObjectKind::Model(ModelObject {
                model: obj
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        } else if present(obj, "light") {
            ObjectKind::Light(obj.clone())
        } else if present(obj, "shape") {
            ObjectKind::Shape(obj.clone())
        } else {
            ObjectKind::Group
        };

        Some(Object {
            base,
            kind,
            extra: obj.clone(),
        })
    }
}

fn present(obj: &Map<String, Value>, key: &str) -> bool {
    !matches!(obj.get(key), None | Some(Value::Null))
}

fn parse_image(obj: &Map<String, Value>) -> ImageObject {
    let alignment = obj
        .get("horizontalalign")
        .and_then(Value::as_str)
        .or_else(|| obj.get("alignment").and_then(Value::as_str))
        .unwrap_or("center")
        .to_owned();
    let size = user_vec2(obj, "size", [0.0, 0.0]).value;
    let effects = match obj.get("effects") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_object)
            .filter_map(Effect::parse)
            .collect(),
        _ => Vec::new(),
    };
    let animationlayers = match obj.get("animationlayers") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_object)
            .filter_map(AnimationLayer::parse)
            .collect(),
        _ => Vec::new(),
    };
    let instance = obj.get("instance").and_then(Value::as_object).map(|o| Instance {
        textures: parse_textures(o.get("textures")),
        usertextures: parse_textures(o.get("usertextures")),
    });
    ImageObject {
        image: obj
            .get("image")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        model: None,
        material: None,
        scale: user_vec3(obj, "scale", [1.0, 1.0, 1.0]),
        angles: user_vec3(obj, "angles", [0.0, 0.0, 0.0]),
        visible: user_bool(obj, "visible", true),
        alpha: user_f32(obj, "alpha", 1.0),
        color: user_color(obj, "color", WHITE),
        alignment,
        size,
        parallax_depth: user_vec2(obj, "parallaxDepth", [0.0, 0.0]),
        color_blend_mode: user_i64(obj, "colorBlendMode", 0),
        brightness: user_f32(obj, "brightness", 1.0),
        effects,
        animationlayers,
        instance,
    }
}

fn parse_text(obj: &Map<String, Value>) -> TextObject {
    let horizontalalign = obj
        .get("horizontalalign")
        .and_then(Value::as_str)
        .or_else(|| obj.get("alignment").and_then(Value::as_str))
        .unwrap_or("center")
        .to_owned();
    TextObject {
        text: user_string(obj, "text", ""),
        font: obj
            .get("font")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        pointsize: user_f32(obj, "pointsize", 32.0),
        size: user_vec2(obj, "size", [0.0, 0.0]).value,
        scale: user_vec3(obj, "scale", [1.0, 1.0, 1.0]),
        color: user_color(obj, "color", WHITE),
        alpha: user_f32(obj, "alpha", 1.0),
        visible: user_bool(obj, "visible", true),
        horizontalalign,
        verticalalign: obj
            .get("verticalalign")
            .and_then(Value::as_str)
            .unwrap_or("center")
            .to_owned(),
        padding: obj.get("padding").and_then(coerce_i64).unwrap_or(0),
        limitwidth: obj.get("limitwidth").and_then(Value::as_bool).unwrap_or(false),
        maxwidth: obj
            .get("maxwidth")
            .and_then(Value::as_f64)
            .map_or(500.0, |v| v as f32),
    }
}

fn parse_sound(obj: &Map<String, Value>) -> ObjectKind {
    let sound = match obj.get("sound") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    ObjectKind::Sound(SoundObject {
        sound,
        playbackmode: obj.get("playbackmode").and_then(Value::as_str).map(str::to_owned),
    })
}

fn parse_particle(obj: &Map<String, Value>) -> ParticleObject {
    let (particle_file, system) = match obj.get("particle") {
        Some(Value::String(s)) => (Some(s.clone()), ParticleSystem::default()),
        Some(v @ Value::Object(_)) => (None, ParticleSystem::from_value(v)),
        _ => (None, ParticleSystem::default()),
    };
    let instanceoverride = obj
        .get("instanceoverride")
        .and_then(Value::as_object)
        .map_or_else(InstanceOverride::default, InstanceOverride::parse);
    ParticleObject {
        scale: user_vec3(obj, "scale", [1.0, 1.0, 1.0]),
        angles: user_vec3(obj, "angles", [0.0, 0.0, 0.0]),
        visible: user_bool(obj, "visible", true),
        parallax_depth: user_vec2(obj, "parallaxDepth", [0.0, 0.0]),
        particle_file,
        system,
        instanceoverride,
    }
}

#[cfg(test)]
mod anim_tests {
    use super::{AnimMode, AnimationTrack, Keyframe};

    fn track(c0: Vec<Keyframe>, length: f32, mode: AnimMode) -> AnimationTrack {
        AnimationTrack {
            c0,
            c1: vec![],
            c2: vec![],
            fps: 30.0,
            length,
            mode,
            relative: true,
        }
    }
    fn kf(frame: f32, value: f32) -> Keyframe {
        Keyframe { frame, value }
    }

    #[test]
    fn disabled_when_length_zero_or_empty() {
        assert_eq!(track(vec![kf(0.0, 1.0)], 0.0, AnimMode::Loop).sample(1.0), None);
        assert_eq!(track(vec![], 100.0, AnimMode::Loop).sample(1.0), None);
    }

    #[test]
    fn linear_interp_and_hold_outside_range() {
        let t = track(vec![kf(0.0, 0.0), kf(100.0, 10.0)], 1000.0, AnimMode::Loop);
        assert!((t.sample(0.0).unwrap()[0] - 0.0).abs() < 1e-3);
        assert!((t.sample(50.0 / 30.0).unwrap()[0] - 5.0).abs() < 1e-3);
        assert!((t.sample(200.0 / 30.0).unwrap()[0] - 10.0).abs() < 1e-3);
    }

    #[test]
    fn loop_wraps_over_length() {
        let t = track(vec![kf(0.0, 0.0), kf(100.0, 10.0)], 100.0, AnimMode::Loop);
        assert!((t.sample(100.0 / 30.0).unwrap()[0] - 0.0).abs() < 1e-3);
        assert!((t.sample(150.0 / 30.0).unwrap()[0] - 5.0).abs() < 1e-3);
    }

    #[test]
    fn mirror_ping_pongs() {
        let t = track(vec![kf(0.0, 0.0), kf(100.0, 10.0)], 100.0, AnimMode::Mirror);
        assert!((t.sample(150.0 / 30.0).unwrap()[0] - 5.0).abs() < 1e-3);
        assert!((t.sample(100.0 / 30.0).unwrap()[0] - 10.0).abs() < 1e-3);
    }
}
