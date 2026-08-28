use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::user::{ConstantValues, parse_constant_values};
use crate::value::coerce_i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Blending {
    #[default]
    Normal,
    Translucent,
    Additive,
}

impl Blending {
    pub fn parse(s: &str) -> Self {
        match s {
            "translucent" => Blending::Translucent,
            "additive" => Blending::Additive,
            _ => Blending::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CullMode {
    #[default]
    NoCull,
    Normal,
}

impl CullMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "normal" => CullMode::Normal,
            _ => CullMode::NoCull,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DepthMode {
    #[default]
    Disabled,
    Enabled,
}

impl DepthMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "enabled" => DepthMode::Enabled,
            _ => DepthMode::Disabled,
        }
    }
}

pub type TextureSlots = Vec<Option<String>>;

pub fn parse_textures(value: Option<&Value>) -> TextureSlots {
    let Some(Value::Array(arr)) = value else {
        return Vec::new();
    };
    arr.iter()
        .map(|entry| match entry {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_owned),
            _ => None,
        })
        .collect()
}

pub type Combos = BTreeMap<String, i64>;

pub fn parse_combos(value: Option<&Value>) -> Combos {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(obj)) = value {
        for (k, v) in obj {
            out.insert(k.clone(), coerce_i64(v).unwrap_or(0));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pass {
    pub blending: Blending,
    pub cullmode: CullMode,
    pub depthtest: DepthMode,
    pub depthwrite: DepthMode,
    pub shader: String,
    pub textures: TextureSlots,
    pub usertextures: TextureSlots,
    pub combos: Combos,
    pub constantshadervalues: ConstantValues,
}

impl Pass {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        Pass {
            blending: obj
                .get("blending")
                .and_then(Value::as_str)
                .map_or(Blending::Normal, Blending::parse),
            cullmode: obj
                .get("cullmode")
                .and_then(Value::as_str)
                .map_or(CullMode::NoCull, CullMode::parse),
            depthtest: obj
                .get("depthtest")
                .and_then(Value::as_str)
                .map_or(DepthMode::Disabled, DepthMode::parse),
            depthwrite: obj
                .get("depthwrite")
                .and_then(Value::as_str)
                .map_or(DepthMode::Disabled, DepthMode::parse),
            shader: obj
                .get("shader")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            textures: parse_textures(obj.get("textures")),
            usertextures: parse_textures(obj.get("usertextures")),
            combos: parse_combos(obj.get("combos")),
            constantshadervalues: parse_constant_values(obj.get("constantshadervalues")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Material {
    pub passes: Vec<Pass>,
}

impl Material {
    pub fn from_value(value: &Value) -> Self {
        let passes = match value.get("passes") {
            Some(Value::Array(arr)) => arr.iter().filter_map(Value::as_object).map(Pass::parse).collect(),
            _ => Vec::new(),
        };
        Material { passes }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub material: String,
    pub solidlayer: bool,
    pub fullscreen: bool,
    pub passthrough: bool,
    pub autosize: bool,
    pub nopadding: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub puppet: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelFileError {
    #[error("model file: required key `material` missing")]
    MaterialMissing,
}

impl ModelFile {
    pub fn from_value(value: &Value) -> Result<Self, ModelFileError> {
        let material = value
            .get("material")
            .and_then(Value::as_str)
            .ok_or(ModelFileError::MaterialMissing)?
            .to_owned();
        let flag = |k: &str| value.get(k).and_then(crate::value::coerce_bool).unwrap_or(false);
        let int = |k: &str| value.get(k).and_then(coerce_i64);
        Ok(ModelFile {
            material,
            solidlayer: flag("solidlayer"),
            fullscreen: flag("fullscreen"),
            passthrough: flag("passthrough"),
            autosize: flag("autosize"),
            nopadding: flag("nopadding"),
            width: int("width"),
            height: int("height"),
            puppet: value.get("puppet").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PassCommand {
    Copy,
    Swap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bind {
    pub name: String,
    pub index: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPass {
    pub material: Option<String>,
    pub resolved: Option<Material>,
    pub bind: Vec<Bind>,
    pub command: Option<PassCommand>,
    pub source: Option<String>,
    pub target: Option<String>,
}

impl EffectPass {
    pub fn parse(obj: &Map<String, Value>) -> Self {
        let bind = match obj.get("bind") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|b| {
                    let o = b.as_object()?;
                    Some(Bind {
                        name: o.get("name").and_then(Value::as_str)?.to_owned(),
                        index: coerce_i64(o.get("index")?)?,
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let command = obj.get("command").and_then(Value::as_str).map(|c| {
            if c == "copy" {
                PassCommand::Copy
            } else {
                PassCommand::Swap
            }
        });
        EffectPass {
            material: obj.get("material").and_then(Value::as_str).map(str::to_owned),
            resolved: None,
            bind,
            command,
            source: obj.get("source").and_then(Value::as_str).map(str::to_owned),
            target: obj.get("target").and_then(Value::as_str).map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fbo {
    pub name: String,
    pub format: String,
    pub scale: f32,
    pub unique: bool,
}

impl Fbo {
    pub fn parse(obj: &Map<String, Value>) -> Option<Self> {
        let name = obj.get("name").and_then(Value::as_str)?.to_owned();
        Some(Fbo {
            name,
            format: obj
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("rgba8888")
                .to_owned(),
            scale: obj.get("scale").and_then(crate::value::coerce_f64).unwrap_or(1.0) as f32,
            unique: obj
                .get("unique")
                .and_then(crate::value::coerce_bool)
                .unwrap_or(false),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EffectFile {
    pub name: String,
    pub description: String,
    pub group: String,
    pub preview: String,
    pub dependencies: Vec<String>,
    pub passes: Vec<EffectPass>,
    pub fbos: Vec<Fbo>,
}

impl EffectFile {
    pub fn from_value(value: &Value) -> Self {
        let str_field = |k: &str| {
            value
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let dependencies = match value.get("dependencies") {
            Some(Value::Array(arr)) => arr.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        };
        let passes = match value.get("passes") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(Value::as_object)
                .map(EffectPass::parse)
                .collect(),
            _ => Vec::new(),
        };
        let fbos = match value.get("fbos") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(Value::as_object)
                .filter_map(Fbo::parse)
                .collect(),
            _ => Vec::new(),
        };
        EffectFile {
            name: str_field("name"),
            description: str_field("description"),
            group: str_field("group"),
            preview: str_field("preview"),
            dependencies,
            passes,
            fbos,
        }
    }
}
