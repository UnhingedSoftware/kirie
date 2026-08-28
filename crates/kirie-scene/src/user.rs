use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::value::{Color, Vec2, Vec3, coerce_bool, coerce_f64, coerce_i64, parse_color, parse_vec};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserRef {
    Name(String),
    Conditional { name: String, condition: String },
}

impl UserRef {
    pub fn name(&self) -> &str {
        match self {
            UserRef::Name(n) | UserRef::Conditional { name: n, .. } => n,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptBinding {
    pub source: String,
    pub properties: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserSetting<T> {
    pub value: T,
    pub user: Option<UserRef>,
    pub script: Option<ScriptBinding>,
}

impl<T> UserSetting<T> {
    pub fn literal(value: T) -> Self {
        UserSetting {
            value,
            user: None,
            script: None,
        }
    }

    pub fn is_bound(&self) -> bool {
        self.user.is_some() || self.script.is_some()
    }
}

fn parse_bindings(obj: &Map<String, Value>) -> (Option<UserRef>, Option<ScriptBinding>) {
    let user = match obj.get("user") {
        Some(Value::String(name)) => Some(UserRef::Name(name.clone())),
        Some(Value::Object(o)) => match (o.get("name"), o.get("condition")) {
            (Some(Value::String(name)), Some(cond)) => Some(UserRef::Conditional {
                name: name.clone(),
                condition: cond.as_str().map(str::to_owned).unwrap_or_default(),
            }),
            _ => None,
        },
        _ => None,
    };
    let script = match obj.get("script") {
        Some(Value::String(source)) => Some(ScriptBinding {
            source: source.clone(),
            properties: match obj.get("scriptproperties") {
                Some(Value::Object(p)) => p.clone(),
                _ => Map::new(),
            },
        }),
        _ => None,
    };
    (user, script)
}

pub fn read_user<T>(
    map: &Map<String, Value>,
    key: &str,
    default: T,
    parse: impl Fn(&Value) -> Option<T>,
) -> UserSetting<T> {
    match map.get(key) {
        None | Some(Value::Null) => UserSetting::literal(default),
        Some(Value::Object(obj)) => {
            let (user, script) = parse_bindings(obj);
            let value = obj.get("value").and_then(&parse).unwrap_or(default);
            UserSetting { value, user, script }
        }
        Some(other) => UserSetting {
            value: parse(other).unwrap_or(default),
            user: None,
            script: None,
        },
    }
}

pub fn user_bool(map: &Map<String, Value>, key: &str, default: bool) -> UserSetting<bool> {
    read_user(map, key, default, |v| coerce_bool(v).or(Some(default)))
}

pub fn user_f32(map: &Map<String, Value>, key: &str, default: f32) -> UserSetting<f32> {
    read_user(map, key, default, |v| coerce_f64(v).map(|f| f as f32))
}

pub fn user_i64(map: &Map<String, Value>, key: &str, default: i64) -> UserSetting<i64> {
    read_user(map, key, default, coerce_i64)
}

pub fn user_string(map: &Map<String, Value>, key: &str, default: &str) -> UserSetting<String> {
    read_user(map, key, default.to_owned(), |v| v.as_str().map(str::to_owned))
}

pub fn user_vec2(map: &Map<String, Value>, key: &str, default: Vec2) -> UserSetting<Vec2> {
    read_user(map, key, default, |v| {
        v.as_str().and_then(|s| parse_vec::<2>(s).ok())
    })
}

pub fn user_vec3(map: &Map<String, Value>, key: &str, default: Vec3) -> UserSetting<Vec3> {
    read_user(map, key, default, |v| {
        v.as_str().and_then(|s| parse_vec::<3>(s).ok())
    })
}

pub fn user_color(map: &Map<String, Value>, key: &str, default: Color) -> UserSetting<Color> {
    read_user(map, key, default, |v| {
        v.as_str().and_then(|s| parse_color(s, 1.0, false).ok())
    })
}

pub type ConstantValues = BTreeMap<String, UserSetting<crate::value::DynamicValue>>;

pub fn parse_constant_values(value: Option<&Value>) -> ConstantValues {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(obj)) = value {
        for (name, raw) in obj {
            out.insert(
                name.clone(),
                read_user(&single(name, raw), name, crate::value::DynamicValue::Null, |v| {
                    Some(crate::value::DynamicValue::decode(v, false))
                }),
            );
        }
    }
    out
}

fn single(key: &str, value: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value.clone());
    m
}
