use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

use crate::reflect::{ParamDefault, ParamType};

pub const COMBO_PREFIX: &str = "// [COMBO] ";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnnotationError {
    #[error("malformed annotation JSON: {0}")]
    BadJson(String),
    #[error("[COMBO] annotation missing required \"combo\" key")]
    MissingCombo,
    #[error("[COMBO] \"default\" must be an integer")]
    NonIntComboDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboAnnotation {
    pub combo: String,
    pub default: i32,
    pub require: BTreeMap<String, i32>,
}

pub fn parse_combo_line(line: &str) -> Result<Option<ComboAnnotation>, AnnotationError> {
    let Some(idx) = line.find(COMBO_PREFIX) else {
        return Ok(None);
    };
    let json = line[idx + COMBO_PREFIX.len()..].trim();
    let value: Value = serde_json::from_str(json).map_err(|e| AnnotationError::BadJson(e.to_string()))?;

    let combo = value
        .get("combo")
        .and_then(Value::as_str)
        .ok_or(AnnotationError::MissingCombo)?
        .to_string();

    let default = match value.get("default") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|_| n.as_f64().is_some_and(|f| f.fract() == 0.0))
            .ok_or(AnnotationError::NonIntComboDefault)? as i32,
        Some(_) => return Err(AnnotationError::NonIntComboDefault),
    };

    let mut require = BTreeMap::new();
    if let Some(obj) = value.get("require").and_then(Value::as_object) {
        for (k, v) in obj {
            if let Some(i) = v.as_i64() {
                require.insert(k.clone(), i as i32);
            }
        }
    }

    Ok(Some(ComboAnnotation {
        combo,
        default,
        require,
    }))
}

#[derive(Debug, Clone, PartialEq)]
pub enum UniformAnnotation {
    Parameter {
        name: String,
        ty: ParamType,
        material: Option<String>,
        default: Option<ParamDefault>,
    },
    Sampler {
        name: String,
        default_texture: Option<String>,
        combo: Option<String>,
        require: BTreeMap<String, i32>,
        require_any: bool,
    },
}

pub fn parse_uniform_line(line: &str) -> Result<Option<UniformAnnotation>, AnnotationError> {
    let Some(comment_at) = line.find("// ") else {
        return Ok(None);
    };
    let Some(semi_at) = line.find(';') else {
        return Ok(None);
    };
    if semi_at >= comment_at {
        return Ok(None);
    }
    let decl = &line[..semi_at];
    let Some(after_uniform) = decl.find("uniform ") else {
        return Ok(None);
    };
    let tokens: Vec<&str> = decl[after_uniform + "uniform ".len()..]
        .split_whitespace()
        .collect();
    if tokens.len() < 2 {
        return Ok(None);
    }
    let name = tokens[tokens.len() - 1];
    let name = name.split('[').next().unwrap_or(name).to_string();
    let type_tok = tokens[tokens.len() - 2];

    let json = line[comment_at + 3..].trim();
    let value: Value = serde_json::from_str(json).map_err(|e| AnnotationError::BadJson(e.to_string()))?;

    match type_tok {
        "sampler2D" | "sampler2DComparison" => {
            let default_texture = value.get("default").and_then(Value::as_str).map(str::to_string);
            let combo = value.get("combo").and_then(Value::as_str).map(str::to_string);
            let mut require = BTreeMap::new();
            if let Some(obj) = value.get("require").and_then(Value::as_object) {
                for (k, v) in obj {
                    if let Some(i) = v.as_i64() {
                        require.insert(k.clone(), i as i32);
                    }
                }
            }
            let require_any = value.get("requireany").and_then(Value::as_bool).unwrap_or(false);
            Ok(Some(UniformAnnotation::Sampler {
                name,
                default_texture,
                combo,
                require,
                require_any,
            }))
        }
        other => {
            let Some(ty) = param_type(other) else {
                return Ok(None);
            };
            let material = value.get("material").and_then(Value::as_str).map(str::to_string);
            let default = parse_param_default(ty, value.get("default"));
            Ok(Some(UniformAnnotation::Parameter {
                name,
                ty,
                material,
                default,
            }))
        }
    }
}

fn param_type(tok: &str) -> Option<ParamType> {
    Some(match tok {
        "float" => ParamType::Float,
        "int" => ParamType::Int,
        "vec2" => ParamType::Vec2,
        "vec3" => ParamType::Vec3,
        "vec4" => ParamType::Vec4,
        _ => return None,
    })
}

fn parse_param_default(ty: ParamType, raw: Option<&Value>) -> Option<ParamDefault> {
    let raw = raw?;
    match ty {
        ParamType::Float => match raw {
            Value::Number(n) => Some(ParamDefault::Scalar(n.as_f64()?)),
            Value::String(s) => s.parse::<f64>().ok().map(ParamDefault::Scalar),
            _ => None,
        },
        ParamType::Int => match raw {
            Value::Number(n) => Some(ParamDefault::Scalar(n.as_i64()? as f64)),
            Value::String(s) => Some(ParamDefault::Scalar(stoi_prefix(s) as f64)),
            _ => None,
        },
        ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 => {
            let s = raw.as_str()?;
            let comps: Vec<f32> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            let want = match ty {
                ParamType::Vec2 => 2,
                ParamType::Vec3 => 3,
                _ => 4,
            };
            if comps.len() >= want {
                Some(ParamDefault::Vector(comps[..want].to_vec()))
            } else {
                None
            }
        }
    }
}

fn stoi_prefix(s: &str) -> i64 {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut acc: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        acc = acc.saturating_mul(10).saturating_add((bytes[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return 0;
    }
    if neg { -acc } else { acc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_basic_and_default() {
        let line =
            r#"// [COMBO] {"material":"ui_editor_properties_lighting","combo":"LIGHTING","default":1}"#;
        let c = parse_combo_line(line).unwrap().unwrap();
        assert_eq!(c.combo, "LIGHTING");
        assert_eq!(c.default, 1);
        assert!(c.require.is_empty());
    }

    #[test]
    fn combo_absent_default_is_zero_and_require_chain() {
        let line = r#"// [COMBO] {"combo":"RIMLIGHTING","require":{"LIGHTING":1}}"#;
        let c = parse_combo_line(line).unwrap().unwrap();
        assert_eq!(c.default, 0);
        assert_eq!(c.require.get("LIGHTING"), Some(&1));
    }

    #[test]
    fn combo_missing_key_is_hard_error() {
        let line = r#"// [COMBO] {"material":"x"}"#;
        assert_eq!(parse_combo_line(line), Err(AnnotationError::MissingCombo));
    }

    #[test]
    fn combo_non_int_default_rejected() {
        assert_eq!(
            parse_combo_line(r#"// [COMBO] {"combo":"X","default":0.5}"#),
            Err(AnnotationError::NonIntComboDefault)
        );
        assert_eq!(
            parse_combo_line(r#"// [COMBO] {"combo":"X","default":"1"}"#),
            Err(AnnotationError::NonIntComboDefault)
        );
    }

    #[test]
    fn combo_off_variants_not_matched() {
        assert_eq!(parse_combo_line(r#"// [COMBO_OFF] {"combo":"X"}"#), Ok(None));
        assert_eq!(parse_combo_line(r#"// [COMBO_DISABLED] {"combo":"X"}"#), Ok(None));
    }

    #[test]
    fn combo_malformed_json_is_typed_error() {
        assert!(matches!(
            parse_combo_line("// [COMBO] {not json}"),
            Err(AnnotationError::BadJson(_))
        ));
    }

    #[test]
    fn uniform_parameter_with_material_and_default() {
        let line = r#"uniform float g_Brightness; // {"material":"Brightness","default":1,"range":[0,2]}"#;
        let u = parse_uniform_line(line).unwrap().unwrap();
        match u {
            UniformAnnotation::Parameter {
                name,
                ty,
                material,
                default,
            } => {
                assert_eq!(name, "g_Brightness");
                assert_eq!(ty, ParamType::Float);
                assert_eq!(material.as_deref(), Some("Brightness"));
                assert_eq!(default, Some(ParamDefault::Scalar(1.0)));
            }
            _ => panic!("expected parameter"),
        }
    }

    #[test]
    fn uniform_vec_default_space_separated() {
        let line = r#"uniform vec3 g_Tint; // {"material":"tint","default":"1 0.5 0"}"#;
        let u = parse_uniform_line(line).unwrap().unwrap();
        match u {
            UniformAnnotation::Parameter { default, .. } => {
                assert_eq!(default, Some(ParamDefault::Vector(vec![1.0, 0.5, 0.0])));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn uniform_int_string_default_truncates() {
        let line = r#"uniform int g_Mode; // {"material":"mode","default":"0.5"}"#;
        let u = parse_uniform_line(line).unwrap().unwrap();
        match u {
            UniformAnnotation::Parameter { default, .. } => {
                assert_eq!(default, Some(ParamDefault::Scalar(0.0)));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn uniform_sampler_with_combo_and_default() {
        let line = r#"uniform sampler2D g_Texture1; // {"combo":"NORMALMAP","default":"util/black","requireany":true,"require":{"LIGHTING":1}}"#;
        let u = parse_uniform_line(line).unwrap().unwrap();
        match u {
            UniformAnnotation::Sampler {
                name,
                default_texture,
                combo,
                require_any,
                require,
            } => {
                assert_eq!(name, "g_Texture1");
                assert_eq!(default_texture.as_deref(), Some("util/black"));
                assert_eq!(combo.as_deref(), Some("NORMALMAP"));
                assert!(require_any);
                assert_eq!(require.get("LIGHTING"), Some(&1));
            }
            _ => panic!("expected sampler"),
        }
    }

    #[test]
    fn uniform_commented_out_is_skipped() {
        assert_eq!(parse_uniform_line("// uniform float g_Dead; {}").unwrap(), None);
    }

    #[test]
    fn uniform_unknown_type_ignored() {
        assert_eq!(
            parse_uniform_line(r#"uniform mat4 g_M; // {"material":"m"}"#).unwrap(),
            None
        );
    }
}
