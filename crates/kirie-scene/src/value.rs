use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use thiserror::Error;

pub type Vec2 = [f32; 2];
pub type Vec3 = [f32; 3];
pub type Color = [f32; 4];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VecError {
    #[error("vector has {found} components (expected {expected})")]
    ComponentCount { expected: usize, found: usize },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ColorError {
    #[error("unsupported hex color length {len} (expected 3, 4, 6 or 8 digits)")]
    HexLength { len: usize },
    #[error("invalid hex digits in color {digits:?}")]
    HexDigits { digits: String },
    #[error("color has {count} components (expected 3 or 4)")]
    ComponentCount { count: usize },
}

pub fn parse_vec_components(s: &str) -> Vec<f32> {
    s.split(' ').take(4).map(strtof).collect()
}

pub fn parse_vec<const N: usize>(s: &str) -> Result<[f32; N], VecError> {
    let parts = parse_vec_components(s);
    let mut out = [0.0; N];
    if parts.len() != N {
        return Err(VecError::ComponentCount {
            expected: N,
            found: parts.len(),
        });
    }
    out.copy_from_slice(&parts);
    Ok(out)
}

pub fn parse_color(s: &str, alpha: f32, force_float: bool) -> Result<Color, ColorError> {
    let normalized = s.replace(',', " ");

    if let Some(digits) = normalized.strip_prefix('#') {
        let expanded: String = match digits.len() {
            3 => {
                let mut e: String = digits.chars().flat_map(|c| [c, c]).collect();
                e.push_str("ff");
                e
            }
            4 => digits.chars().flat_map(|c| [c, c]).collect(),
            6 => format!("{digits}ff"),
            8 => digits.to_owned(),
            len => return Err(ColorError::HexLength { len }),
        };
        let v = u32::from_str_radix(&expanded, 16).map_err(|_| ColorError::HexDigits {
            digits: digits.to_owned(),
        })?;
        let byte = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
        return Ok([byte(24), byte(16), byte(8), byte(0)]);
    }

    let parts: Vec<&str> = normalized.split(' ').collect();
    let is_float = force_float || normalized.contains('.');
    let scale = |raw: &str| -> f32 {
        if is_float {
            strtof(raw)
        } else {
            strtoi(raw) as f32 / 255.0
        }
    };
    match parts.as_slice() {
        [r, g, b] => Ok([scale(r), scale(g), scale(b), alpha]),
        [r, g, b, a] => Ok([scale(r), scale(g), scale(b), scale(a)]),
        other => Err(ColorError::ComponentCount { count: other.len() }),
    }
}

pub const WHITE: Color = [1.0, 1.0, 1.0, 1.0];
pub const BLACK: Color = [0.0, 0.0, 0.0, 1.0];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "v", rename_all = "lowercase")]
pub enum DynamicValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Vec(Vec<f32>),
    Color(Color),
    Null,
}

impl DynamicValue {
    pub fn decode(value: &Value, color_expected: bool) -> Self {
        match value {
            Value::Null => DynamicValue::Null,
            Value::Bool(b) => DynamicValue::Bool(*b),
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    DynamicValue::Int(n.as_i64().unwrap_or(0))
                } else {
                    DynamicValue::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => {
                if color_expected {
                    return parse_color(s, 1.0, false)
                        .map_or_else(|_| DynamicValue::Str(s.clone()), DynamicValue::Color);
                }
                let tokens = s.split(' ').count();
                match tokens {
                    0 | 1 => match parse_whole_f32(s) {
                        Some(f) => DynamicValue::Float(f64::from(f)),
                        None => DynamicValue::Str(s.clone()),
                    },
                    _ => DynamicValue::Vec(parse_vec_components(s)),
                }
            }
            other => DynamicValue::Str(other.to_string()),
        }
    }

    pub fn as_f32(&self) -> f32 {
        match self {
            DynamicValue::Bool(b) => f32::from(*b),
            DynamicValue::Int(i) => *i as f32,
            DynamicValue::Float(f) => *f as f32,
            DynamicValue::Str(s) => strtof(s),
            DynamicValue::Vec(v) => v.first().copied().unwrap_or(0.0),
            DynamicValue::Color(c) => c[0],
            DynamicValue::Null => 0.0,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            DynamicValue::Bool(b) => *b,
            DynamicValue::Null => false,
            DynamicValue::Str(s) => matches!(s.as_str(), "1" | "true" | "True" | "TRUE"),
            other => other.as_f32() != 0.0,
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            DynamicValue::Str(s) => s.clone(),
            DynamicValue::Bool(b) => b.to_string(),
            DynamicValue::Int(i) => i.to_string(),
            DynamicValue::Float(f) => f.to_string(),
            DynamicValue::Vec(v) => v.iter().map(f32::to_string).collect::<Vec<_>>().join(" "),
            DynamicValue::Color(c) => format!("{} {} {} {}", c[0], c[1], c[2], c[3]),
            DynamicValue::Null => String::new(),
        }
    }
}

pub fn coerce_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64().unwrap_or(f64::NAN) != 0.0),
        Value::String(s) => Some(matches!(s.as_str(), "1" | "true" | "True" | "TRUE")),
        _ => None,
    }
}

pub fn coerce_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let prefix = float_prefix(skip_c_whitespace(s));
            let parsed = prefix.parse::<f64>().unwrap_or(0.0);
            Some(if parsed.is_infinite() && !is_infinity_token(prefix) {
                0.0
            } else {
                parsed
            })
        }
        _ => None,
    }
}

pub fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => Some(n.as_i64().unwrap_or_else(|| n.as_f64().map_or(0, |f| f as i64))),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => Some(int_prefix(skip_c_whitespace(s)).parse().unwrap_or(0)),
        _ => None,
    }
}

pub fn coerce_u32(v: &Value) -> Option<u32> {
    coerce_i64(v).map(|i| i.clamp(0, i64::from(u32::MAX)) as u32)
}

pub fn strtof(s: &str) -> f32 {
    float_prefix(skip_c_whitespace(s)).parse().unwrap_or(0.0)
}

fn strtoi(s: &str) -> i64 {
    int_prefix(skip_c_whitespace(s)).parse().unwrap_or(0)
}

fn parse_whole_f32(s: &str) -> Option<f32> {
    let trimmed = skip_c_whitespace(s);
    let prefix = float_prefix(trimmed);
    if !prefix.is_empty() && prefix.len() == trimmed.trim_end().len() {
        prefix.parse().ok()
    } else {
        None
    }
}

fn skip_c_whitespace(s: &str) -> &str {
    s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r'])
}

fn float_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    for token in ["infinity", "inf", "nan"] {
        if s.len() >= i + token.len() && s[i..i + token.len()].eq_ignore_ascii_case(token) {
            return &s[..i + token.len()];
        }
    }
    let mut valid = 0;
    let int_start = i;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let has_int_digits = i > int_start;
    if has_int_digits {
        valid = i;
    }
    if bytes.get(i) == Some(&b'.') {
        let frac_start = i + 1;
        let mut j = frac_start;
        while bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if has_int_digits || j > frac_start {
            valid = j;
            i = j;
        }
    }
    if valid > 0 && matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp_digits_start = j;
        while bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j > exp_digits_start {
            valid = j;
        }
    }
    &s[..valid]
}

fn int_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    let sign = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut i = sign;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if i == sign { "" } else { &s[..i] }
}

fn is_infinity_token(prefix: &str) -> bool {
    let rest = prefix.trim_start_matches(['+', '-']);
    rest.eq_ignore_ascii_case("inf") || rest.eq_ignore_ascii_case("infinity")
}

pub fn f32_number(v: f32) -> Value {
    let widened = if v == f32::INFINITY {
        f64::MAX
    } else if v == f32::NEG_INFINITY {
        f64::MIN
    } else {
        f64::from(v)
    };
    Value::Number(Number::from_f64(widened).unwrap_or_else(|| Number::from(0)))
}
