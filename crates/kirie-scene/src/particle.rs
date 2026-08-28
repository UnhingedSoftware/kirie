use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::user::{UserSetting, read_user, user_f32};
use crate::value::{DynamicValue, Vec2, Vec3, coerce_f64, coerce_i64, coerce_u32, parse_vec};

pub fn parse_bvec3(value: Option<&Value>, default: Vec3) -> Vec3 {
    match value {
        Some(Value::String(s)) => parse_vec::<3>(s).unwrap_or(default),
        Some(Value::Array(a)) => {
            let mut out = default;
            for (i, slot) in out.iter_mut().enumerate() {
                if let Some(v) = a.get(i).and_then(coerce_f64) {
                    *slot = v as f32;
                }
            }
            out
        }
        Some(Value::Number(_)) => {
            let n = coerce_f64(value.unwrap()).unwrap_or(0.0) as f32;
            [n, n, n]
        }
        _ => default,
    }
}

fn parse_ivec3(value: Option<&Value>, default: [i32; 3]) -> [i32; 3] {
    match value {
        Some(Value::Array(a)) => {
            let mut out = default;
            for (i, slot) in out.iter_mut().enumerate() {
                if let Some(v) = a.get(i).and_then(coerce_i64) {
                    *slot = v as i32;
                }
            }
            out
        }
        _ => default,
    }
}

fn parse_bvec2(value: Option<&Value>, default: Vec2) -> Vec2 {
    match value {
        Some(Value::String(s)) => parse_vec::<2>(s).unwrap_or(default),
        Some(Value::Array(a)) => {
            let mut out = default;
            for (i, slot) in out.iter_mut().enumerate() {
                if let Some(v) = a.get(i).and_then(coerce_f64) {
                    *slot = v as f32;
                }
            }
            out
        }
        _ => default,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Emitter {
    pub id: i64,
    pub name: String,
    pub directions: Vec3,
    pub distancemin: Vec3,
    pub distancemax: Vec3,
    pub origin: Vec3,
    pub sign: [i32; 3],
    pub instantaneous: u32,
    pub speedmin: f32,
    pub speedmax: f32,
    pub rate: f32,
    pub controlpoint: i64,
    pub flags: u32,
    pub cone: f32,
    pub delay: f32,
    pub duration: f32,
    pub audioprocessingbounds: Vec2,
    pub audioprocessingexponent: i64,
    pub audioprocessingfrequencystart: i64,
    pub audioprocessingfrequencyend: i64,
    pub audioprocessingmode: i64,
    pub minperiodicdelay: f32,
    pub maxperiodicdelay: f32,
    pub minperiodicduration: f32,
    pub maxperiodicduration: f32,
}

impl Emitter {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        let f = |k: &str, d: f32| obj.get(k).and_then(coerce_f64).map_or(d, |v| v as f32);
        let i = |k: &str, d: i64| obj.get(k).and_then(coerce_i64).unwrap_or(d);
        let u = |k: &str, d: u32| obj.get(k).and_then(coerce_u32).unwrap_or(d);
        Emitter {
            id: i("id", -1),
            name: obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            directions: parse_bvec3(obj.get("directions"), [1.0, 1.0, 0.0]),
            distancemin: parse_bvec3(obj.get("distancemin"), [0.0, 0.0, 0.0]),
            distancemax: parse_bvec3(obj.get("distancemax"), [256.0, 256.0, 0.0]),
            origin: parse_bvec3(obj.get("origin"), [0.0, 0.0, 0.0]),
            sign: parse_ivec3(obj.get("sign"), [0, 0, 0]),
            instantaneous: u("instantaneous", 0),
            speedmin: f("speedmin", 0.0),
            speedmax: f("speedmax", 0.0),
            rate: f("rate", 10.0),
            controlpoint: i("controlpoint", 0),
            flags: u("flags", 0),
            cone: f("cone", 0.0),
            delay: f("delay", 0.0),
            duration: f("duration", 0.0),
            audioprocessingbounds: parse_bvec2(obj.get("audioprocessingbounds"), [0.8, 1.0]),
            audioprocessingexponent: i("audioprocessingexponent", 2),
            audioprocessingfrequencystart: i("audioprocessingfrequencystart", 0),
            audioprocessingfrequencyend: i("audioprocessingfrequencyend", 1),
            audioprocessingmode: i("audioprocessingmode", 0),
            minperiodicdelay: f("minperiodicdelay", 1.0),
            maxperiodicdelay: f("maxperiodicdelay", 2.0),
            minperiodicduration: f("minperiodicduration", 2.0),
            maxperiodicduration: f("maxperiodicduration", 3.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedStage {
    pub id: i64,
    pub name: String,
    pub params: std::collections::BTreeMap<String, UserSetting<DynamicValue>>,
}

impl NamedStage {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        let mut params = std::collections::BTreeMap::new();
        for (k, v) in obj {
            if k == "id" || k == "name" {
                continue;
            }
            params.insert(
                k.clone(),
                read_user(obj, k, DynamicValue::Null, |val| {
                    Some(DynamicValue::decode(val, false))
                }),
            );
            let _ = v;
        }
        NamedStage {
            id: obj.get("id").and_then(coerce_i64).unwrap_or(-1),
            name: obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            params,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Renderer {
    pub name: String,
    pub length: f32,
    pub maxlength: f32,
    pub minlength: f32,
    pub subdivision: f32,
    pub segments: f32,
    pub uvscale: f32,
    pub uvscrolling: bool,
    pub uvsmoothing: bool,
    pub fadealpha: bool,
    pub fadesize: bool,
}

impl Renderer {
    pub fn default_sprite() -> Self {
        Renderer {
            name: "sprite".to_owned(),
            length: 0.05,
            maxlength: 10.0,
            minlength: 0.0,
            subdivision: 1.0,
            segments: 4.0,
            uvscale: 1.0,
            uvscrolling: false,
            uvsmoothing: true,
            fadealpha: false,
            fadesize: false,
        }
    }

    pub fn parse(obj: &Map<String, Value>) -> Self {
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("sprite")
            .to_owned();
        let length_default = if name == "ropetrail" { 1.0 } else { 0.05 };
        let subdivision_default = if name == "rope" { 4.0 } else { 1.0 };
        let f = |k: &str, d: f32| obj.get(k).and_then(coerce_f64).map_or(d, |v| v as f32);
        let b = |k: &str, d: bool| obj.get(k).and_then(crate::value::coerce_bool).unwrap_or(d);
        Renderer {
            length: f("length", length_default),
            maxlength: f("maxlength", 10.0),
            minlength: f("minlength", 0.0),
            subdivision: f("subdivision", subdivision_default),
            segments: f("segments", 4.0),
            uvscale: f("uvscale", 1.0),
            uvscrolling: b("uvscrolling", false),
            uvsmoothing: b("uvsmoothing", true),
            fadealpha: b("fadealpha", false),
            fadesize: b("fadesize", false),
            name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    pub id: i64,
    pub flags: u32,
    pub offset: Vec3,
    pub locktopointer: bool,
}

impl ControlPoint {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        ControlPoint {
            id: obj.get("id").and_then(coerce_i64).unwrap_or(-1),
            flags: obj.get("flags").and_then(coerce_u32).unwrap_or(0),
            offset: obj
                .get("offset")
                .and_then(Value::as_str)
                .and_then(|s| parse_vec::<3>(s).ok())
                .unwrap_or([0.0, 0.0, 0.0]),
            locktopointer: obj
                .get("locktopointer")
                .and_then(crate::value::coerce_bool)
                .unwrap_or(false)
                || obj.get("flags").and_then(coerce_u32).unwrap_or(0) & 1 != 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChildSystem {
    pub kind: String,
    pub name: String,
    pub maxcount: u32,
    pub controlpointstartindex: i64,
    pub probability: f32,
    pub angles: Vec3,
    pub origin: Vec3,
    pub scale: Vec3,
    pub particle: Option<String>,
}

impl ChildSystem {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        ChildSystem {
            kind: obj
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("static")
                .to_owned(),
            name: obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            maxcount: obj.get("maxcount").and_then(coerce_u32).unwrap_or(20),
            controlpointstartindex: obj
                .get("controlpointstartindex")
                .and_then(coerce_i64)
                .unwrap_or(0),
            probability: obj
                .get("probability")
                .and_then(coerce_f64)
                .map_or(1.0, |v| v as f32),
            angles: parse_bvec3(obj.get("angles"), [0.0, 0.0, 0.0]),
            origin: parse_bvec3(obj.get("origin"), [0.0, 0.0, 0.0]),
            scale: parse_bvec3(obj.get("scale"), [1.0, 1.0, 1.0]),
            particle: obj.get("particle").and_then(Value::as_str).map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ParticleSystem {
    pub material: Option<String>,
    pub resolved_material: Option<crate::material::Material>,
    pub animationmode: String,
    pub sequencemultiplier: f32,
    pub maxcount: u32,
    pub starttime: u32,
    pub flags: u32,
    pub emitters: Vec<Emitter>,
    pub initializers: Vec<NamedStage>,
    pub operators: Vec<NamedStage>,
    pub renderers: Vec<Renderer>,
    pub controlpoints: Vec<ControlPoint>,
    pub children: Vec<ChildSystem>,
}

impl ParticleSystem {
    pub fn from_value(value: &Value) -> Self {
        let obj = value.as_object().cloned().unwrap_or_default();
        let arr = |k: &str| -> Vec<Map<String, Value>> {
            match obj.get(k) {
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_object).cloned().collect(),
                _ => Vec::new(),
            }
        };
        let mut renderers: Vec<Renderer> = arr("renderer").iter().map(Renderer::parse).collect();
        if renderers.is_empty() {
            renderers.push(Renderer::default_sprite());
        }
        ParticleSystem {
            material: obj.get("material").and_then(Value::as_str).map(str::to_owned),
            resolved_material: None,
            animationmode: obj
                .get("animationmode")
                .and_then(Value::as_str)
                .unwrap_or("sequence")
                .to_owned(),
            sequencemultiplier: obj
                .get("sequencemultiplier")
                .and_then(coerce_f64)
                .map_or(1.0, |v| v as f32),
            maxcount: obj.get("maxcount").and_then(coerce_u32).unwrap_or(100),
            starttime: obj.get("starttime").and_then(coerce_u32).unwrap_or(0),
            flags: obj.get("flags").and_then(coerce_u32).unwrap_or(0),
            emitters: arr("emitter").iter().map(Emitter::parse).collect(),
            initializers: arr("initializer").iter().map(NamedStage::parse).collect(),
            operators: arr("operator").iter().map(NamedStage::parse).collect(),
            renderers,
            controlpoints: arr("controlpoint").iter().map(ControlPoint::parse).collect(),
            children: arr("children").iter().map(ChildSystem::parse).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstanceOverride {
    pub enabled: UserSetting<bool>,
    pub alpha: UserSetting<f32>,
    pub size: UserSetting<f32>,
    pub lifetime: UserSetting<f32>,
    pub rate: UserSetting<f32>,
    pub speed: UserSetting<f32>,
    pub count: UserSetting<f32>,
    pub color: UserSetting<Vec3>,
    pub colorn: UserSetting<Vec3>,
}

impl Default for InstanceOverride {
    fn default() -> Self {
        InstanceOverride {
            enabled: UserSetting::literal(true),
            alpha: UserSetting::literal(1.0),
            size: UserSetting::literal(1.0),
            lifetime: UserSetting::literal(1.0),
            rate: UserSetting::literal(1.0),
            speed: UserSetting::literal(1.0),
            count: UserSetting::literal(1.0),
            color: UserSetting::literal([1.0, 1.0, 1.0]),
            colorn: UserSetting::literal([1.0, 1.0, 1.0]),
        }
    }
}

impl InstanceOverride {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        use crate::user::{user_bool, user_vec3};
        InstanceOverride {
            enabled: user_bool(obj, "enabled", true),
            alpha: user_f32(obj, "alpha", 1.0),
            size: user_f32(obj, "size", 1.0),
            lifetime: user_f32(obj, "lifetime", 1.0),
            rate: user_f32(obj, "rate", 1.0),
            speed: user_f32(obj, "speed", 1.0),
            count: user_f32(obj, "count", 1.0),
            color: user_vec3(obj, "color", [1.0, 1.0, 1.0]),
            colorn: user_vec3(obj, "colorn", [1.0, 1.0, 1.0]),
        }
    }
}
