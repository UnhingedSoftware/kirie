use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamDefault {
    Scalar(f64),
    Vector(Vec<f32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamType {
    Float,
    Int,
    Vec2,
    Vec3,
    Vec4,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub material: String,
    pub ty: ParamType,
    pub default: Option<ParamDefault>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamplerSlot {
    pub name: String,
    pub slot: Option<u32>,
    pub texture_binding: u32,
    pub sampler_binding: u32,
    pub default_texture: Option<String>,
    pub combo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VertexAttribute {
    pub name: String,
    pub location: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reflection {
    pub globals_block: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub samplers: Vec<SamplerSlot>,
    pub attributes: Vec<VertexAttribute>,
    pub active_combos: BTreeMap<String, i32>,
}
