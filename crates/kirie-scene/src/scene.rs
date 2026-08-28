use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::SceneError;
use crate::object::Object;
use crate::user::{UserSetting, user_bool, user_color, user_f32};
use crate::value::{BLACK, Color, Vec3, WHITE, coerce_bool, coerce_f64, coerce_i64, parse_vec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Projection {
    Orthogonal { width: i64, height: i64 },
    Auto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
    pub nearz: f32,
    pub farz: f32,
    pub fov: UserSetting<f32>,
    pub projection: Projection,
}

impl Camera {
    fn parse(camera: &Map<String, Value>, general: &Map<String, Value>) -> Result<Self, SceneError> {
        let vec = |field: &'static str| -> Result<Vec3, SceneError> {
            let s = camera
                .get(field)
                .and_then(Value::as_str)
                .ok_or(SceneError::MissingCameraField(field))?;
            parse_vec::<3>(s).map_err(|source| SceneError::CameraVec { field, source })
        };
        let eye = vec("eye")?;
        let center = vec("center")?;
        let up = vec("up")?;

        let read_f = |key: &str, default: f32| -> f32 {
            camera
                .get(key)
                .and_then(coerce_f64)
                .or_else(|| general.get(key).and_then(coerce_f64))
                .map_or(default, |v| v as f32)
        };
        let nearz = read_f("nearz", 0.1);
        let farz = read_f("farz", 10000.0);
        let fov = if matches!(general.get("fov"), Some(v) if !v.is_null()) {
            user_f32(general, "fov", 50.0)
        } else {
            user_f32(camera, "fov", 50.0)
        };

        let projection = match general.get("orthogonalprojection") {
            Some(Value::Object(o)) => {
                if o.get("auto").and_then(coerce_bool).unwrap_or(false) {
                    Projection::Auto
                } else {
                    Projection::Orthogonal {
                        width: o.get("width").and_then(coerce_i64).unwrap_or(0),
                        height: o.get("height").and_then(coerce_i64).unwrap_or(0),
                    }
                }
            }
            _ => Projection::Auto,
        };

        Ok(Camera {
            eye,
            center,
            up,
            nearz,
            farz,
            fov,
            projection,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct General {
    pub ambientcolor: UserSetting<Color>,
    pub skylightcolor: UserSetting<Color>,
    pub clearcolor: UserSetting<Color>,
    pub camerafade: UserSetting<bool>,
    pub camerapreview: bool,
    pub bloom: UserSetting<bool>,
    pub bloomstrength: UserSetting<f32>,
    pub bloomthreshold: UserSetting<f32>,
    pub cameraparallax: UserSetting<bool>,
    pub cameraparallaxamount: UserSetting<f32>,
    pub cameraparallaxdelay: UserSetting<f32>,
    pub cameraparallaxmouseinfluence: UserSetting<f32>,
    pub camerashake: UserSetting<bool>,
    pub camerashakeamplitude: UserSetting<f32>,
    pub camerashakeroughness: UserSetting<f32>,
    pub camerashakespeed: UserSetting<f32>,
    pub customsortorder: bool,
    pub extra: Map<String, Value>,
}

impl General {
    fn parse(map: &Map<String, Value>) -> Self {
        General {
            ambientcolor: user_color(map, "ambientcolor", BLACK),
            skylightcolor: user_color(map, "skylightcolor", BLACK),
            clearcolor: user_color(map, "clearcolor", WHITE),
            camerafade: user_bool(map, "camerafade", false),
            camerapreview: map.get("camerapreview").and_then(coerce_bool).unwrap_or(false),
            bloom: user_bool(map, "bloom", false),
            bloomstrength: user_f32(map, "bloomstrength", 0.0),
            bloomthreshold: user_f32(map, "bloomthreshold", 0.0),
            cameraparallax: user_bool(map, "cameraparallax", false),
            cameraparallaxamount: user_f32(map, "cameraparallaxamount", 1.0),
            cameraparallaxdelay: user_f32(map, "cameraparallaxdelay", 0.0),
            cameraparallaxmouseinfluence: user_f32(map, "cameraparallaxmouseinfluence", 1.0),
            camerashake: user_bool(map, "camerashake", false),
            camerashakeamplitude: user_f32(map, "camerashakeamplitude", 0.0),
            camerashakeroughness: user_f32(map, "camerashakeroughness", 0.0),
            camerashakespeed: user_f32(map, "camerashakespeed", 0.0),
            customsortorder: map.get("customsortorder").and_then(coerce_bool).unwrap_or(false),
            extra: map.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub camera: Camera,
    pub general: General,
    pub objects: Vec<Object>,
}

impl Scene {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, SceneError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| SceneError::Json(e.to_string()))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, SceneError> {
        let root = value.as_object().ok_or(SceneError::NotAnObject)?;

        let camera_map = root
            .get("camera")
            .and_then(Value::as_object)
            .ok_or(SceneError::MissingSection("camera"))?;
        let general_map = root
            .get("general")
            .and_then(Value::as_object)
            .ok_or(SceneError::MissingSection("general"))?;
        let objects_arr = root
            .get("objects")
            .and_then(Value::as_array)
            .ok_or(SceneError::MissingSection("objects"))?;

        let camera = Camera::parse(camera_map, general_map)?;
        let general = General::parse(general_map);
        let objects = objects_arr.iter().filter_map(Object::parse).collect();

        Ok(Scene {
            camera,
            general,
            objects,
        })
    }
}
