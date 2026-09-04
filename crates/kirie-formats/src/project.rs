use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Number, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("project.json root is not a JSON object")]
    NotAnObject,
    #[error("project title missing")]
    TitleMissing,
    #[error("project title must be a string")]
    TitleNotString,
    #[error("project's main file missing")]
    FileMissing,
    #[error("project's main file must be a string")]
    FileNotString,
    #[error("cannot determine project type from file {file:?}")]
    TypeUndeterminable { file: String },
    #[error("property {name:?}: {source}")]
    Property { name: String, source: PropertyError },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PropertyError {
    #[error("required field {field:?} missing")]
    MissingField { field: &'static str },
    #[error("field {field:?} has the wrong JSON type (expected {expected})")]
    WrongType {
        field: &'static str,
        expected: &'static str,
    },
    #[error("combo `options` must be an array")]
    OptionsNotArray,
    #[error("invalid color value: {0}")]
    Color(#[from] ColorError),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallpaperType {
    Scene,
    Web,
    Video,
    Image,
    Application,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredType(pub String);

impl DeclaredType {
    pub fn classify(&self) -> Option<WallpaperType> {
        let lower = self.0.to_ascii_lowercase();
        match lower.as_str() {
            "scene" => Some(WallpaperType::Scene),
            "video" => Some(WallpaperType::Video),
            "web" => Some(WallpaperType::Web),
            "application" => Some(WallpaperType::Application),
            "image" => Some(WallpaperType::Image),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkshopId {
    Text(String),
    Number(Number),
}

impl std::fmt::Display for WorkshopId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkshopId::Text(s) => f.write_str(s),
            WorkshopId::Number(n) => write!(f, "{n}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ComboOption {
    pub label: String,
    pub value: String,
    pub numeric: bool,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyKind {
    Bool {
        value: bool,
    },
    Slider {
        value: f32,
        min: f32,
        max: f32,
        step: f32,
    },
    Color {
        value: [f32; 3],
    },
    Combo {
        options: Vec<ComboOption>,
        value: String,
    },
    Text,
    TextInput {
        value: String,
    },
    UserShortcut {
        value: String,
    },
    File {
        value: String,
    },
    Directory {
        value: String,
    },
    SceneTexture {
        value: String,
    },
}

impl PropertyKind {
    pub fn type_tag(&self) -> &'static str {
        match self {
            PropertyKind::Bool { .. } => "bool",
            PropertyKind::Slider { .. } => "slider",
            PropertyKind::Color { .. } => "color",
            PropertyKind::Combo { .. } => "combo",
            PropertyKind::Text => "text",
            PropertyKind::TextInput { .. } => "textinput",
            PropertyKind::UserShortcut { .. } => "usershortcut",
            PropertyKind::File { .. } => "file",
            PropertyKind::Directory { .. } => "directory",
            PropertyKind::SceneTexture { .. } => "scenetexture",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub text: String,
    pub order: i64,
    pub kind: PropertyKind,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyEntry {
    Property(Property),
    Group(Map<String, Value>),
    Unrecognized(Value),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct General {
    pub supportsaudioprocessing: bool,
    pub properties: BTreeMap<String, PropertyEntry>,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub title: String,
    pub file: String,
    pub declared_type: Option<DeclaredType>,
    pub resolved_type: WallpaperType,
    pub workshopid: Option<WorkshopId>,
    pub general: General,
    pub extra: Map<String, Value>,
}

impl Project {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| ProjectError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let value: Value = serde_json::from_slice(&bytes)?;
        Self::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Self, ProjectError> {
        let Value::Object(mut map) = value else {
            return Err(ProjectError::NotAnObject);
        };

        let title = match map.remove("title") {
            None => return Err(ProjectError::TitleMissing),
            Some(Value::String(s)) => s,
            Some(_) => return Err(ProjectError::TitleNotString),
        };

        let file = match map.remove("file") {
            None => return Err(ProjectError::FileMissing),
            Some(Value::String(s)) => s,
            Some(_) => return Err(ProjectError::FileNotString),
        };

        let declared_type = match map.remove("type") {
            None => None,
            Some(Value::String(s)) => Some(DeclaredType(s)),
            Some(Value::Null) => {
                map.insert("type".to_owned(), Value::Null);
                None
            }
            Some(other) => {
                map.insert("type".to_owned(), other);
                Some(DeclaredType(String::new()))
            }
        };

        let workshopid = match map.remove("workshopid") {
            None => None,
            Some(Value::String(s)) => Some(WorkshopId::Text(s)),
            Some(Value::Number(n)) => Some(WorkshopId::Number(n)),
            Some(other) => {
                map.insert("workshopid".to_owned(), other);
                None
            }
        };

        let general = match map.remove("general") {
            Some(Value::Object(g)) => parse_general(g)?,
            Some(other) => {
                map.insert("general".to_owned(), other);
                General::default()
            }
            None => General::default(),
        };

        let resolved_type = resolve_type(&file, declared_type.as_ref())?;

        Ok(Project {
            title,
            file,
            declared_type,
            resolved_type,
            workshopid,
            general,
            extra: map,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut map = self.extra.clone();
        map.insert("title".to_owned(), Value::String(self.title.clone()));
        map.insert("file".to_owned(), Value::String(self.file.clone()));
        if let Some(t) = &self.declared_type
            && !map.contains_key("type")
        {
            map.insert("type".to_owned(), Value::String(t.0.clone()));
        }
        if let Some(w) = &self.workshopid {
            let v = match w {
                WorkshopId::Text(s) => Value::String(s.clone()),
                WorkshopId::Number(n) => Value::Number(n.clone()),
            };
            map.insert("workshopid".to_owned(), v);
        }
        if self.general != General::default() {
            map.insert("general".to_owned(), general_to_value(&self.general));
        }
        Value::Object(map)
    }

    pub fn category(&self) -> Option<&str> {
        self.extra.get("category").and_then(Value::as_str)
    }

    pub fn is_asset(&self) -> bool {
        self.category() == Some("Asset")
    }

    pub fn passes_preflight(&self) -> bool {
        self.declared_type.is_some() || self.extra.contains_key("type")
    }
}

impl Serialize for Project {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Project::from_value(value).map_err(D::Error::custom)
    }
}

pub fn resolve_type(file: &str, declared: Option<&DeclaredType>) -> Result<WallpaperType, ProjectError> {
    let lower = file.to_ascii_lowercase();

    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        return Ok(WallpaperType::Web);
    }

    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "json" | "pkg" => return Ok(WallpaperType::Scene),
        "html" | "htm" => return Ok(WallpaperType::Web),
        "mp4" | "webm" | "mkv" | "avi" | "mov" | "m4v" | "wmv" => return Ok(WallpaperType::Video),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => return Ok(WallpaperType::Image),
        "exe" => return Ok(WallpaperType::Application),
        _ => {}
    }

    if let Some(t) = declared.and_then(DeclaredType::classify) {
        return Ok(t);
    }

    Err(ProjectError::TypeUndeterminable {
        file: file.to_owned(),
    })
}

pub fn parse_property_color(s: &str) -> Result<[f32; 3], ColorError> {
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
        return Ok([byte(24), byte(16), byte(8)]);
    }

    let parts: Vec<&str> = normalized.split(' ').collect();
    match parts.as_slice() {
        [r, g, b] | [r, g, b, _] => Ok([strtof(r), strtof(g), strtof(b)]),
        other => Err(ColorError::ComponentCount { count: other.len() }),
    }
}

fn strtof(s: &str) -> f32 {
    float_prefix(skip_c_whitespace(s)).parse().unwrap_or(0.0)
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

fn coerce_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64().unwrap_or(f64::NAN) != 0.0),
        Value::String(s) => Some(matches!(s.as_str(), "1" | "true" | "True" | "TRUE")),
        _ => None,
    }
}

fn coerce_f64(v: &Value) -> Option<f64> {
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

fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => Some(n.as_i64().unwrap_or_else(|| n.as_f64().map_or(0, |f| f as i64))),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => Some(int_prefix(skip_c_whitespace(s)).parse().unwrap_or(0)),
        _ => None,
    }
}

fn number_to_int_string(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        (n.as_f64().unwrap_or(0.0) as i64).to_string()
    }
}

fn parse_general(mut map: Map<String, Value>) -> Result<General, ProjectError> {
    let supportsaudioprocessing = match map.get("supportsaudioprocessing").and_then(coerce_bool) {
        Some(b) => {
            map.remove("supportsaudioprocessing");
            b
        }
        None => false,
    };

    let mut properties = BTreeMap::new();
    match map.remove("properties") {
        Some(Value::Object(props)) => {
            for (name, raw) in props {
                let entry = parse_property_entry(raw).map_err(|source| ProjectError::Property {
                    name: name.clone(),
                    source,
                })?;
                properties.insert(name, entry);
            }
        }
        Some(other) => {
            map.insert("properties".to_owned(), other);
        }
        None => {}
    }

    Ok(General {
        supportsaudioprocessing,
        properties,
        extra: map,
    })
}

fn parse_property_entry(value: Value) -> Result<PropertyEntry, PropertyError> {
    let Value::Object(mut raw) = value else {
        return Ok(PropertyEntry::Unrecognized(value));
    };

    let tag = match raw.get("type") {
        None => return Ok(PropertyEntry::Group(raw)),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Ok(PropertyEntry::Unrecognized(Value::Object(raw))),
    };

    let kind = match tag.as_str() {
        "group" => return Ok(PropertyEntry::Group(raw)),
        "bool" => PropertyKind::Bool {
            value: raw
                .remove("value")
                .as_ref()
                .and_then(coerce_bool)
                .unwrap_or(false),
        },
        "slider" => {
            let value = match raw.remove("value") {
                None => return Err(PropertyError::MissingField { field: "value" }),
                Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) as f32,
                Some(Value::Bool(b)) => {
                    if b {
                        1.0
                    } else {
                        0.0
                    }
                }
                Some(_) => {
                    return Err(PropertyError::WrongType {
                        field: "value",
                        expected: "number or bool",
                    });
                }
            };
            let mut take = |field: &str| -> f32 {
                raw.remove(field).as_ref().and_then(coerce_f64).unwrap_or(0.0) as f32
            };
            let (min, max, step) = (take("min"), take("max"), take("step"));
            PropertyKind::Slider {
                value,
                min,
                max,
                step,
            }
        }
        "color" => {
            let value = match raw.remove("value") {
                None => return Err(PropertyError::MissingField { field: "value" }),
                Some(Value::String(s)) => parse_property_color(&s)?,
                Some(_) => {
                    return Err(PropertyError::WrongType {
                        field: "value",
                        expected: "string",
                    });
                }
            };
            PropertyKind::Color { value }
        }
        "combo" => {
            let raw_options = match raw.remove("options") {
                None => return Err(PropertyError::MissingField { field: "options" }),
                Some(Value::Array(a)) => a,
                Some(_) => return Err(PropertyError::OptionsNotArray),
            };
            let mut options = Vec::new();
            for entry in raw_options {
                let Value::Object(mut opt) = entry else { continue };
                let label = match opt.remove("label") {
                    None => {
                        return Err(PropertyError::MissingField {
                            field: "options[].label",
                        });
                    }
                    Some(Value::String(s)) => s,
                    Some(_) => {
                        return Err(PropertyError::WrongType {
                            field: "options[].label",
                            expected: "string",
                        });
                    }
                };
                let (value, numeric) = match opt.remove("value") {
                    None => {
                        return Err(PropertyError::MissingField {
                            field: "options[].value",
                        });
                    }
                    Some(Value::String(s)) => (s, false),
                    Some(Value::Number(n)) => (number_to_int_string(&n), true),
                    Some(_) => {
                        return Err(PropertyError::WrongType {
                            field: "options[].value",
                            expected: "string or number",
                        });
                    }
                };
                options.push(ComboOption {
                    label,
                    value,
                    numeric,
                    extra: opt,
                });
            }
            let value = match raw.remove("value") {
                Some(Value::String(s)) => s,
                Some(Value::Number(n)) => number_to_int_string(&n),
                _ => options.first().map(|o| o.value.clone()).unwrap_or_default(),
            };
            PropertyKind::Combo { options, value }
        }
        "text" => PropertyKind::Text,
        "scenetexture" => {
            let value = match raw.remove("value") {
                None => return Err(PropertyError::MissingField { field: "value" }),
                Some(Value::String(s)) => s,
                Some(_) => {
                    return Err(PropertyError::WrongType {
                        field: "value",
                        expected: "string",
                    });
                }
            };
            PropertyKind::SceneTexture { value }
        }
        "file" => PropertyKind::File {
            value: take_optional_string(&mut raw),
        },
        "directory" => PropertyKind::Directory {
            value: take_optional_string(&mut raw),
        },
        "textinput" => PropertyKind::TextInput {
            value: take_textinput_value(&mut raw)?,
        },
        "usershortcut" => PropertyKind::UserShortcut {
            value: take_textinput_value(&mut raw)?,
        },
        _ => return Ok(PropertyEntry::Unrecognized(Value::Object(raw))),
    };

    raw.remove("type");

    let text = match raw.remove("text") {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let order = if matches!(kind, PropertyKind::Text) {
        0
    } else {
        raw.remove("order").as_ref().and_then(coerce_i64).unwrap_or(0)
    };

    Ok(PropertyEntry::Property(Property {
        text,
        order,
        kind,
        extra: raw,
    }))
}

fn take_optional_string(raw: &mut Map<String, Value>) -> String {
    match raw.remove("value") {
        Some(Value::String(s)) => s,
        _ => String::new(),
    }
}

fn take_textinput_value(raw: &mut Map<String, Value>) -> Result<String, PropertyError> {
    match raw.remove("value") {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s),
        Some(other) => Ok(other.to_string()),
    }
}

fn general_to_value(general: &General) -> Value {
    let mut map = general.extra.clone();
    if general.supportsaudioprocessing {
        map.insert("supportsaudioprocessing".to_owned(), Value::Bool(true));
    }
    if !general.properties.is_empty() {
        let props: Map<String, Value> = general
            .properties
            .iter()
            .map(|(name, entry)| (name.clone(), property_entry_to_value(entry)))
            .collect();
        map.insert("properties".to_owned(), Value::Object(props));
    }
    Value::Object(map)
}

fn f32_number(v: f32) -> Value {
    let widened = if v == f32::INFINITY {
        f64::MAX
    } else if v == f32::NEG_INFINITY {
        f64::MIN
    } else {
        f64::from(v)
    };
    Value::Number(Number::from_f64(widened).unwrap_or_else(|| Number::from(0)))
}

fn property_entry_to_value(entry: &PropertyEntry) -> Value {
    let property = match entry {
        PropertyEntry::Group(raw) => return Value::Object(raw.clone()),
        PropertyEntry::Unrecognized(v) => return v.clone(),
        PropertyEntry::Property(p) => p,
    };
    let mut map = property.extra.clone();
    map.insert(
        "type".to_owned(),
        Value::String(property.kind.type_tag().to_owned()),
    );
    map.insert("text".to_owned(), Value::String(property.text.clone()));
    if !matches!(property.kind, PropertyKind::Text) {
        map.insert("order".to_owned(), Value::Number(property.order.into()));
    }
    match &property.kind {
        PropertyKind::Bool { value } => {
            map.insert("value".to_owned(), Value::Bool(*value));
        }
        PropertyKind::Slider {
            value,
            min,
            max,
            step,
        } => {
            map.insert("value".to_owned(), f32_number(*value));
            map.insert("min".to_owned(), f32_number(*min));
            map.insert("max".to_owned(), f32_number(*max));
            map.insert("step".to_owned(), f32_number(*step));
        }
        PropertyKind::Color { value: [r, g, b] } => {
            map.insert("value".to_owned(), Value::String(format!("{r} {g} {b}")));
        }
        PropertyKind::Combo { options, value } => {
            let opts: Vec<Value> = options
                .iter()
                .map(|o| {
                    let mut m = o.extra.clone();
                    m.insert("label".to_owned(), Value::String(o.label.clone()));
                    let value = match o.value.parse::<i64>() {
                        Ok(n) if o.numeric => Value::from(n),
                        _ => Value::String(o.value.clone()),
                    };
                    m.insert("value".to_owned(), value);
                    Value::Object(m)
                })
                .collect();
            map.insert("options".to_owned(), Value::Array(opts));
            map.insert("value".to_owned(), Value::String(value.clone()));
        }
        PropertyKind::Text => {}
        PropertyKind::TextInput { value }
        | PropertyKind::UserShortcut { value }
        | PropertyKind::File { value }
        | PropertyKind::Directory { value }
        | PropertyKind::SceneTexture { value } => {
            map.insert("value".to_owned(), Value::String(value.clone()));
        }
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";

    fn corpus_dir() -> Option<PathBuf> {
        let dir = std::env::var("KIRIE_CORPUS").map_or_else(|_| PathBuf::from(CORPUS_DIR), PathBuf::from);
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!("skipping corpus test: {} not present", dir.display());
            None
        }
    }

    const DOC_ITEM_IDS: &[&str] = &[
        "1388331347",
        "1627026721",
        "2082653325",
        "2085292947",
        "2155933185",
        "2395163768",
        "2968833989",
        "3047596375",
        "3118949804",
        "3293156956",
        "3347128360",
        "3421423611",
        "3428443753",
        "3445942378",
        "3551997868",
        "3576956643",
        "3585875739",
        "3587565260",
        "3600453929",
        "3609007632",
        "3611478368",
        "3631634316",
        "3679122549",
        "3738467344",
    ];

    fn corpus_manifests() -> Option<Vec<(String, Vec<u8>)>> {
        let dir = corpus_dir()?;
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
            let entry = entry.expect("corpus dir entry");
            let manifest = entry.path().join("project.json");
            let id = entry.file_name().to_string_lossy().into_owned();
            if manifest.is_file() && DOC_ITEM_IDS.contains(&id.as_str()) {
                out.push((id, std::fs::read(&manifest).expect("read manifest")));
            }
        }
        out.sort();
        Some(out)
    }

    fn parse(json: &str) -> Project {
        Project::from_value(serde_json::from_str(json).expect("valid JSON")).expect("valid project")
    }

    fn parse_err(json: &str) -> ProjectError {
        Project::from_value(serde_json::from_str(json).expect("valid JSON"))
            .expect_err("expected a parse error")
    }

    fn property<'p>(p: &'p Project, name: &str) -> &'p Property {
        match p.general.properties.get(name) {
            Some(PropertyEntry::Property(prop)) => prop,
            other => panic!("property {name:?} is {other:?}"),
        }
    }

    #[test]
    fn a_text_input_without_a_value_still_loads() {
        let raw = serde_json::json!({
            "type": "textinput",
            "text": "ui_font",
            "order": 9090,
            "condition": "weather_show.value"
        });
        let Value::Object(map) = raw else {
            panic!("the fixture is an object");
        };
        let parsed = parse_property_entry(Value::Object(map)).expect("a missing value is not fatal");
        match parsed {
            PropertyEntry::Property(p) => {
                assert_eq!(p.kind, PropertyKind::TextInput { value: String::new() });
            }
            other => panic!("expected a property, got {other:?}"),
        }
    }

    #[test]
    fn absolute_floor_manifest() {
        let p = parse(r#"{"title": "x", "file": "scene.json"}"#);
        assert_eq!(p.title, "x");
        assert_eq!(p.file, "scene.json");
        assert_eq!(p.resolved_type, WallpaperType::Scene);
        assert_eq!(p.declared_type, None);
        assert!(!p.passes_preflight());
        assert_eq!(p.workshopid, None);
        assert_eq!(p.general, General::default());
    }

    #[test]
    fn minimal_scene_manifest() {
        let p = parse(
            r#"{
            "contentrating": "Mature",
            "file": "scene.json",
            "general": {
                "properties": {
                    "schemecolor": {
                        "order": 0,
                        "text": "ui_browse_properties_scheme_color",
                        "type": "color",
                        "value": "0 0 0"
                    },
                    "style": {
                        "options": [
                            { "label": "X-Ray", "value": "1" },
                            { "label": "CG 1", "value": "2" }
                        ],
                        "order": 100,
                        "text": "Style",
                        "type": "combo"
                    },
                    "x_ray_radius": {
                        "fraction": true, "precision": 2,
                        "min": 0.1, "max": 1.2, "step": 0.1,
                        "order": 101,
                        "text": "X-Ray radius",
                        "type": "slider",
                        "value": 0.6
                    }
                }
            },
            "preview": "preview.jpg",
            "tags": [ "Anime" ],
            "title": "[R18] 松永 時雨 01 [X-Ray]",
            "type": "scene"
        }"#,
        );
        assert_eq!(p.resolved_type, WallpaperType::Scene);
        assert_eq!(p.declared_type, Some(DeclaredType("scene".to_owned())));
        assert!(p.passes_preflight());
        assert_eq!(p.title, "[R18] 松永 時雨 01 [X-Ray]");
        assert_eq!(p.workshopid, None);
        assert_eq!(
            p.extra.get("contentrating"),
            Some(&Value::String("Mature".to_owned()))
        );
        assert_eq!(p.general.properties.len(), 3);

        let scheme = property(&p, "schemecolor");
        assert_eq!(
            scheme.kind,
            PropertyKind::Color {
                value: [0.0, 0.0, 0.0]
            }
        );
        assert_eq!(scheme.text, "ui_browse_properties_scheme_color");
        assert_eq!(scheme.order, 0);

        let style = property(&p, "style");
        let PropertyKind::Combo { options, value } = &style.kind else {
            panic!("style is {:?}", style.kind);
        };
        assert_eq!(value, "1");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "X-Ray");
        assert_eq!(options[1].value, "2");

        let radius = property(&p, "x_ray_radius");
        assert_eq!(
            radius.kind,
            PropertyKind::Slider {
                value: 0.6,
                min: 0.1,
                max: 1.2,
                step: 0.1
            }
        );
        assert_eq!(radius.order, 101);
        assert_eq!(radius.extra.get("fraction"), Some(&Value::Bool(true)));
        assert_eq!(radius.extra.get("precision"), Some(&Value::Number(2.into())));
    }

    #[test]
    fn minimal_video_manifest() {
        let p = parse(
            r#"{
            "contentrating": "Everyone",
            "file": "冷冰冰的誓言.mp4",
            "general": {
                "properties": {
                    "schemecolor": {
                        "order": 0,
                        "text": "ui_browse_properties_scheme_color",
                        "type": "color",
                        "value": "0.00000 0.00000 0.00000"
                    }
                }
            },
            "preview": "preview.jpg",
            "ratingsex": "none",
            "ratingviolence": "none",
            "tags": [ "Anime" ],
            "title": "冷冰冰的誓言",
            "type": "video",
            "version": 0
        }"#,
        );
        assert_eq!(p.resolved_type, WallpaperType::Video);
        assert_eq!(p.file, "冷冰冰的誓言.mp4");
        assert_eq!(p.title, "冷冰冰的誓言");
        assert_eq!(p.workshopid, None);
        assert_eq!(p.extra.get("version"), Some(&Value::Number(0.into())));
        assert_eq!(
            property(&p, "schemecolor").kind,
            PropertyKind::Color {
                value: [0.0, 0.0, 0.0]
            }
        );
    }

    #[test]
    fn minimal_web_manifest() {
        let p = parse(
            r#"{
            "contentrating": "Everyone",
            "file": "index.html",
            "general": {
                "properties": {
                    "schemecolor": {
                        "order": 0,
                        "text": "ui_browse_properties_scheme_color",
                        "type": "color",
                        "value": "0.7529411764705882 0.7529411764705882 0.7529411764705882"
                    },
                    "custom_user_bg": {
                        "filter": "*.jpg|*.png|*.jpeg|*.webp",
                        "index": 0, "order": 100,
                        "text": "Custom BG Image",
                        "type": "file"
                    },
                    "audio_sensitivity": {
                        "index": 5, "order": 105,
                        "min": 0.5, "max": 1.5, "step": 0.1,
                        "text": "Audio Sensitivity",
                        "type": "slider",
                        "value": 1
                    }
                }
            },
            "preview": "preview.jpg",
            "title": "[16:9] 超かぐや姫 All-in-One",
            "type": "web",
            "version": 2,
            "visibility": "public",
            "workshopid": "3679122549",
            "workshopurl": "steam://url/CommunityFilePage/3679122549"
        }"#,
        );
        assert_eq!(p.resolved_type, WallpaperType::Web);
        assert_eq!(p.workshopid, Some(WorkshopId::Text("3679122549".to_owned())));

        let grey: f32 = "0.7529411764705882".parse().expect("float");
        assert_eq!(
            property(&p, "schemecolor").kind,
            PropertyKind::Color { value: [grey; 3] }
        );

        let bg = property(&p, "custom_user_bg");
        assert_eq!(bg.kind, PropertyKind::File { value: String::new() });
        assert_eq!(
            bg.extra.get("filter"),
            Some(&Value::String("*.jpg|*.png|*.jpeg|*.webp".to_owned()))
        );

        assert_eq!(
            property(&p, "audio_sensitivity").kind,
            PropertyKind::Slider {
                value: 1.0,
                min: 0.5,
                max: 1.5,
                step: 0.1
            }
        );
    }

    #[test]
    fn asset_manifest_is_scene_by_extension_but_flagged() {
        let p = parse(
            r#"{
            "category": "Asset",
            "contentrating": "Everyone",
            "file": "effects/gradient_generator/effect.json",
            "preview": "preview.jpg",
            "tags": [ "Background" ],
            "title": "Gradient generator",
            "visibility": "public",
            "workshopid": "3347128360"
        }"#,
        );
        assert_eq!(p.resolved_type, WallpaperType::Scene);
        assert_eq!(p.declared_type, None);
        assert!(!p.passes_preflight());
        assert!(p.is_asset());
        assert_eq!(p.category(), Some("Asset"));
    }

    #[test]
    fn type_resolution_follows_spec_order() {
        for f in [
            "http://example.com",
            "HTTPS://EXAMPLE.COM/x",
            "www.example.com",
            "WWW.X.COM",
        ] {
            assert_eq!(resolve_type(f, None).unwrap(), WallpaperType::Web, "{f}");
        }
        assert_eq!(resolve_type("scene.json", None).unwrap(), WallpaperType::Scene);
        assert_eq!(resolve_type("SCENE.PKG", None).unwrap(), WallpaperType::Scene);
        assert_eq!(resolve_type("index.html", None).unwrap(), WallpaperType::Web);
        assert_eq!(resolve_type("a.htm", None).unwrap(), WallpaperType::Web);
        for f in ["a.mp4", "a.webm", "a.mkv", "a.avi", "a.mov", "a.m4v", "a.wmv"] {
            assert_eq!(resolve_type(f, None).unwrap(), WallpaperType::Video, "{f}");
        }
        for f in ["a.png", "a.jpg", "a.jpeg", "a.gif", "a.bmp", "a.webp"] {
            assert_eq!(resolve_type(f, None).unwrap(), WallpaperType::Image, "{f}");
        }
        assert_eq!(resolve_type("a.exe", None).unwrap(), WallpaperType::Application);
        let scene = DeclaredType("scene".to_owned());
        assert_eq!(resolve_type("a.mp4", Some(&scene)).unwrap(), WallpaperType::Video);
        for (t, want) in [
            ("Scene", WallpaperType::Scene),
            ("VIDEO", WallpaperType::Video),
            ("Web", WallpaperType::Web),
            ("application", WallpaperType::Application),
            ("Image", WallpaperType::Image),
        ] {
            let d = DeclaredType(t.to_owned());
            assert_eq!(resolve_type("main.dat", Some(&d)).unwrap(), want, "{t}");
        }
        let video = DeclaredType("video".to_owned());
        assert_eq!(resolve_type("movie", Some(&video)).unwrap(), WallpaperType::Video);
        let weird = DeclaredType("WeIrD".to_owned());
        assert!(matches!(
            resolve_type("main.dat", Some(&weird)),
            Err(ProjectError::TypeUndeterminable { .. })
        ));
        assert!(matches!(
            resolve_type("main.dat", None),
            Err(ProjectError::TypeUndeterminable { .. })
        ));
    }

    #[test]
    fn unknown_declared_type_is_preserved() {
        let p = parse(r#"{"title": "x", "file": "scene.json", "type": "WeIrD"}"#);
        assert_eq!(p.resolved_type, WallpaperType::Scene);
        assert_eq!(p.declared_type, Some(DeclaredType("WeIrD".to_owned())));
        assert_eq!(p.declared_type.as_ref().unwrap().classify(), None);
        assert!(p.passes_preflight());
        let p2 = Project::from_value(p.to_value()).unwrap();
        assert_eq!(p, p2);
        assert_eq!(p2.to_value()["type"], Value::String("WeIrD".to_owned()));
    }

    #[test]
    fn nonstring_type_preserved_and_round_trips() {
        let p = parse(r#"{"title": "x", "file": "scene.json", "type": 5}"#);
        assert_eq!(p.declared_type, Some(DeclaredType(String::new())));
        assert_eq!(p.extra.get("type"), Some(&Value::Number(5.into())));
        assert!(p.passes_preflight());
        let value = p.to_value();
        assert_eq!(value["type"], Value::Number(5.into()));
        let p2 = Project::from_value(value).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn null_type_passes_preflight_and_round_trips() {
        let p = parse(r#"{"title": "x", "file": "scene.json", "type": null}"#);
        assert_eq!(p.declared_type, None);
        assert_eq!(p.extra.get("type"), Some(&Value::Null));
        assert!(p.passes_preflight());
        let value = p.to_value();
        assert_eq!(value["type"], Value::Null);
        let p2 = Project::from_value(value).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn property_type_matrix() {
        let p = parse(
            r#"{
            "title": "matrix", "file": "scene.json", "type": "scene",
            "general": { "properties": {
                "b1":  { "type": "bool", "value": true, "order": 1, "text": "b" },
                "b2":  { "type": "bool", "value": "1" },
                "b3":  { "type": "bool", "value": 0 },
                "b4":  { "type": "bool" },
                "s1":  { "type": "slider", "value": 2, "min": "2.5", "max": 10, "step": true },
                "s2":  { "type": "slider", "value": true },
                "c1":  { "type": "color", "value": "0.012 0.192 0.251" },
                "co1": { "type": "combo", "options": [
                            { "label": "A", "value": 5 },
                            "skipped-non-object",
                            { "label": "B", "value": "b", "note": "kept" }
                         ] },
                "co2": { "type": "combo", "options": [], "value": "z" },
                "t1":  { "type": "text", "text": "label", "order": 7, "value": "ignored" },
                "ti1": { "type": "textinput", "value": "hello" },
                "ti2": { "type": "textinput", "value": 12 },
                "us1": { "type": "usershortcut", "value": "ctrl+k" },
                "f1":  { "type": "file", "filter": "*.png" },
                "f2":  { "type": "file", "value": "pic.png" },
                "d1":  { "type": "directory" },
                "st1": { "type": "scenetexture", "value": "materials/tex.tex" },
                "g1":  { "type": "group", "text": "Header", "order": 3 },
                "g2":  { "text": "no type at all" },
                "u1":  { "type": "", "text": "_______ Section" },
                "u2":  { "type": 42 },
                "u3":  "not an object"
            } }
        }"#,
        );
        let props = &p.general.properties;
        assert_eq!(props.len(), 22);

        assert_eq!(property(&p, "b1").kind, PropertyKind::Bool { value: true });
        assert_eq!(property(&p, "b1").order, 1);
        assert_eq!(property(&p, "b1").text, "b");
        assert_eq!(property(&p, "b2").kind, PropertyKind::Bool { value: true });
        assert_eq!(property(&p, "b3").kind, PropertyKind::Bool { value: false });
        assert_eq!(property(&p, "b4").kind, PropertyKind::Bool { value: false });

        assert_eq!(
            property(&p, "s1").kind,
            PropertyKind::Slider {
                value: 2.0,
                min: 2.5,
                max: 10.0,
                step: 1.0
            }
        );
        assert_eq!(
            property(&p, "s2").kind,
            PropertyKind::Slider {
                value: 1.0,
                min: 0.0,
                max: 0.0,
                step: 0.0
            }
        );

        assert_eq!(
            property(&p, "c1").kind,
            PropertyKind::Color {
                value: [0.012, 0.192, 0.251]
            }
        );

        let PropertyKind::Combo { options, value } = &property(&p, "co1").kind else {
            panic!("co1 not a combo");
        };
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "5");
        assert_eq!(value, "5");
        assert_eq!(
            options[1].extra.get("note"),
            Some(&Value::String("kept".to_owned()))
        );
        assert_eq!(
            property(&p, "co2").kind,
            PropertyKind::Combo {
                options: vec![],
                value: "z".to_owned()
            }
        );

        let t1 = property(&p, "t1");
        assert_eq!(t1.kind, PropertyKind::Text);
        assert_eq!(t1.text, "label");
        assert_eq!(t1.order, 0);
        assert_eq!(t1.extra.get("order"), Some(&Value::Number(7.into())));
        assert_eq!(t1.extra.get("value"), Some(&Value::String("ignored".to_owned())));

        assert_eq!(
            property(&p, "ti1").kind,
            PropertyKind::TextInput {
                value: "hello".to_owned()
            }
        );
        assert_eq!(
            property(&p, "ti2").kind,
            PropertyKind::TextInput {
                value: "12".to_owned()
            }
        );
        assert_eq!(
            property(&p, "us1").kind,
            PropertyKind::UserShortcut {
                value: "ctrl+k".to_owned()
            }
        );

        assert_eq!(
            property(&p, "f1").kind,
            PropertyKind::File { value: String::new() }
        );
        assert_eq!(
            property(&p, "f2").kind,
            PropertyKind::File {
                value: "pic.png".to_owned()
            }
        );
        assert_eq!(
            property(&p, "d1").kind,
            PropertyKind::Directory { value: String::new() }
        );

        assert_eq!(
            property(&p, "st1").kind,
            PropertyKind::SceneTexture {
                value: "materials/tex.tex".to_owned()
            }
        );

        assert!(matches!(props.get("g1"), Some(PropertyEntry::Group(_))));
        assert!(matches!(props.get("g2"), Some(PropertyEntry::Group(_))));
        assert!(matches!(props.get("u1"), Some(PropertyEntry::Unrecognized(_))));
        assert!(matches!(props.get("u2"), Some(PropertyEntry::Unrecognized(_))));
        assert!(matches!(
            props.get("u3"),
            Some(PropertyEntry::Unrecognized(Value::String(_)))
        ));

        let p2 = Project::from_value(p.to_value()).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn malformed_top_level() {
        assert!(matches!(
            Project::from_value(serde_json::json!([])),
            Err(ProjectError::NotAnObject)
        ));
        assert!(matches!(
            parse_err(r#"{"file": "scene.json"}"#),
            ProjectError::TitleMissing
        ));
        assert!(matches!(
            parse_err(r#"{"title": 42, "file": "scene.json"}"#),
            ProjectError::TitleNotString
        ));
        assert!(matches!(
            parse_err(r#"{"title": true, "file": "scene.json"}"#),
            ProjectError::TitleNotString
        ));
        assert!(matches!(
            parse_err(r#"{"title": "x"}"#),
            ProjectError::FileMissing
        ));
        assert!(matches!(
            parse_err(r#"{"title": "x", "file": 5}"#),
            ProjectError::FileNotString
        ));
        assert!(matches!(
            parse_err(r#"{"title": "x", "file": "main.dat"}"#),
            ProjectError::TypeUndeterminable { .. }
        ));
        assert!(matches!(
            Project::from_path("/nonexistent/project.json"),
            Err(ProjectError::Io { .. })
        ));
        assert!(serde_json::from_str::<Value>(r#"{"title": "x",}"#).is_err());
    }

    #[test]
    fn malformed_properties() {
        fn prop_err(body: &str) -> PropertyError {
            let json =
                format!(r#"{{"title":"x","file":"scene.json","general":{{"properties":{{"p":{body}}}}}}}"#);
            match parse_err(&json) {
                ProjectError::Property { name, source } => {
                    assert_eq!(name, "p");
                    source
                }
                other => panic!("expected property error, got {other:?}"),
            }
        }

        assert_eq!(
            prop_err(r#"{"type":"color"}"#),
            PropertyError::MissingField { field: "value" }
        );
        assert_eq!(
            prop_err(r#"{"type":"color","value":5}"#),
            PropertyError::WrongType {
                field: "value",
                expected: "string"
            }
        );
        assert_eq!(
            prop_err(r#"{"type":"color","value":"1 1"}"#),
            PropertyError::Color(ColorError::ComponentCount { count: 2 })
        );
        assert_eq!(
            prop_err(r#"{"type":"color","value":"1 1 1 1 1"}"#),
            PropertyError::Color(ColorError::ComponentCount { count: 5 })
        );
        assert_eq!(
            prop_err(r##"{"type":"color","value":"#12345"}"##),
            PropertyError::Color(ColorError::HexLength { len: 5 })
        );
        assert_eq!(
            prop_err(r##"{"type":"color","value":"#zzz"}"##),
            PropertyError::Color(ColorError::HexDigits {
                digits: "zzz".to_owned()
            })
        );

        assert_eq!(
            prop_err(r#"{"type":"slider"}"#),
            PropertyError::MissingField { field: "value" }
        );
        assert_eq!(
            prop_err(r#"{"type":"slider","value":"3"}"#),
            PropertyError::WrongType {
                field: "value",
                expected: "number or bool"
            }
        );

        assert_eq!(
            prop_err(r#"{"type":"combo"}"#),
            PropertyError::MissingField { field: "options" }
        );
        assert_eq!(
            prop_err(r#"{"type":"combo","options":"x"}"#),
            PropertyError::OptionsNotArray
        );
        assert_eq!(
            prop_err(r#"{"type":"combo","options":[{"value":"1"}]}"#),
            PropertyError::MissingField {
                field: "options[].label"
            }
        );
        assert_eq!(
            prop_err(r#"{"type":"combo","options":[{"label":"A"}]}"#),
            PropertyError::MissingField {
                field: "options[].value"
            }
        );
        assert_eq!(
            prop_err(r#"{"type":"combo","options":[{"label":"A","value":true}]}"#),
            PropertyError::WrongType {
                field: "options[].value",
                expected: "string or number"
            }
        );

        assert_eq!(
            prop_err(r#"{"type":"scenetexture"}"#),
            PropertyError::MissingField { field: "value" }
        );
        assert_eq!(
            prop_err(r#"{"type":"scenetexture","value":9}"#),
            PropertyError::WrongType {
                field: "value",
                expected: "string"
            }
        );
    }

    #[test]
    fn color_parsing() {
        assert_eq!(
            parse_property_color("0.012 0.192 0.251").unwrap(),
            [0.012, 0.192, 0.251]
        );
        assert_eq!(parse_property_color("1 1 1").unwrap(), [1.0, 1.0, 1.0]);
        assert_eq!(parse_property_color("0 0 0").unwrap(), [0.0, 0.0, 0.0]);
        assert_eq!(parse_property_color("1,0,0.5").unwrap(), [1.0, 0.0, 0.5]);
        assert_eq!(parse_property_color("0.1 0.2 0.3 0.4").unwrap(), [0.1, 0.2, 0.3]);
        assert_eq!(parse_property_color("1 1 1 ").unwrap(), [1.0, 1.0, 1.0]);
        assert_eq!(parse_property_color("a b c").unwrap(), [0.0, 0.0, 0.0]);
        assert_eq!(parse_property_color("1.5x 2 3").unwrap(), [1.5, 2.0, 3.0]);

        assert_eq!(parse_property_color("#fff").unwrap(), [1.0, 1.0, 1.0]);
        assert_eq!(parse_property_color("#f00f").unwrap(), [1.0, 0.0, 0.0]);
        assert_eq!(parse_property_color("#ff0000").unwrap(), [1.0, 0.0, 0.0]);
        let g: f32 = 0x80 as f32 / 255.0;
        assert_eq!(parse_property_color("#008000").unwrap(), [0.0, g, 0.0]);
        assert_eq!(parse_property_color("#ffffffff").unwrap(), [1.0, 1.0, 1.0]);

        assert_eq!(
            parse_property_color("1 1"),
            Err(ColorError::ComponentCount { count: 2 })
        );
        assert_eq!(
            parse_property_color(""),
            Err(ColorError::ComponentCount { count: 1 })
        );
        assert_eq!(
            parse_property_color("#12345"),
            Err(ColorError::HexLength { len: 5 })
        );
        assert_eq!(
            parse_property_color("#gg0000"),
            Err(ColorError::HexDigits {
                digits: "gg0000".to_owned()
            })
        );
    }

    #[test]
    fn string_coercion_follows_stoll_stod_semantics() {
        let slider = |min: &str| -> f32 {
            let json = format!(
                r#"{{"title":"x","file":"scene.json","general":{{"properties":{{
                    "s":{{"type":"slider","value":1,"min":{min}}}}}}}}}"#
            );
            let p = parse(&json);
            match property(&p, "s").kind {
                PropertyKind::Slider { min, .. } => min,
                ref other => panic!("not a slider: {other:?}"),
            }
        };
        assert_eq!(slider(r#""2.5x""#), 2.5);
        assert_eq!(slider(r#"" \t-3.5e1junk""#), -35.0);
        assert_eq!(slider(r#""2e""#), 2.0);
        assert_eq!(slider(r#""2e+""#), 2.0);
        assert_eq!(slider(r#"".5""#), 0.5);
        assert_eq!(slider(r#""1.""#), 1.0);
        assert_eq!(slider(r#""x1""#), 0.0);
        assert_eq!(slider(r#""""#), 0.0);
        assert_eq!(slider(r#""1e999""#), 0.0);
        assert_eq!(slider(r#""-1e999""#), 0.0);
        assert_eq!(slider(r#""inf""#), f32::INFINITY);
        assert_eq!(slider(r#""-Infinity""#), f32::NEG_INFINITY);

        let order = |order: &str| -> i64 {
            let json = format!(
                r#"{{"title":"x","file":"scene.json","general":{{"properties":{{
                    "b":{{"type":"bool","value":true,"order":{order}}}}}}}}}"#
            );
            property(&parse(&json), "b").order
        };
        assert_eq!(order(r#""42abc""#), 42);
        assert_eq!(order(r#""+7""#), 7);
        assert_eq!(order(r#""-7.9""#), -7);
        assert_eq!(order(r#""abc""#), 0);
        assert_eq!(order(r#""99999999999999999999""#), 0);
        assert_eq!(order(r#""-99999999999999999999""#), 0);
    }

    #[test]
    fn slider_value_beyond_f32_range_round_trips() {
        let p = parse(
            r#"{"title":"x","file":"scene.json","general":{"properties":{
                "s":{"type":"slider","value":1e300,"min":-1e300}}}}"#,
        );
        assert_eq!(
            property(&p, "s").kind,
            PropertyKind::Slider {
                value: f32::INFINITY,
                min: f32::NEG_INFINITY,
                max: 0.0,
                step: 0.0
            }
        );
        let p2 = Project::from_value(p.to_value()).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn pathological_numeric_strings_parse_in_linear_time() {
        let long = format!("1{}x{}", "9".repeat(100_000), "9".repeat(100_000));
        let started = std::time::Instant::now();
        assert_eq!(parse_property_color(&format!("{long} 0 0")).unwrap()[1], 0.0);
        assert_eq!(coerce_i64(&Value::String(long)), Some(0));
        assert_eq!(coerce_f64(&Value::String("e".repeat(100_000))), Some(0.0));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "prefix parse took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn workshopid_string_or_number() {
        let p = parse(r#"{"title":"x","file":"scene.json","workshopid":"3679122549"}"#);
        assert_eq!(p.workshopid, Some(WorkshopId::Text("3679122549".to_owned())));
        assert_eq!(p.workshopid.as_ref().unwrap().to_string(), "3679122549");

        let p = parse(r#"{"title":"x","file":"scene.json","workshopid":3679122549}"#);
        assert!(matches!(p.workshopid, Some(WorkshopId::Number(_))));
        assert_eq!(p.workshopid.as_ref().unwrap().to_string(), "3679122549");
        assert!(p.to_value()["workshopid"].is_number());

        let p = parse(r#"{"title":"x","file":"scene.json","workshopid":true}"#);
        assert_eq!(p.workshopid, None);
        assert_eq!(p.extra.get("workshopid"), Some(&Value::Bool(true)));
    }

    #[test]
    fn general_flags_and_coercion() {
        let p = parse(
            r#"{"title":"x","file":"scene.json",
                "general":{"supportsaudioprocessing":true,"supportsvideo":true,"supportsvideoflags":1}}"#,
        );
        assert!(p.general.supportsaudioprocessing);
        assert_eq!(p.general.extra.get("supportsvideo"), Some(&Value::Bool(true)));
        assert_eq!(
            p.general.extra.get("supportsvideoflags"),
            Some(&Value::Number(1.into()))
        );

        let p = parse(r#"{"title":"x","file":"scene.json","general":{"supportsaudioprocessing":"1"}}"#);
        assert!(p.general.supportsaudioprocessing);
        let p = parse(r#"{"title":"x","file":"scene.json","general":{"supportsaudioprocessing":0}}"#);
        assert!(!p.general.supportsaudioprocessing);
    }

    #[test]
    fn serde_round_trip_preserves_model_and_unknown_keys() {
        let src = r#"{
            "title": "rt", "file": "scene.json", "type": "Scene",
            "workshopid": "123",
            "preview": "preview.gif",
            "description": "hello [b]world[/b]",
            "tags": ["Anime"],
            "contentrating": "Everyone",
            "version": 4,
            "approved": true,
            "custom_unknown_key": {"nested": [1, 2, 3]},
            "general": {
                "supportsaudioprocessing": true,
                "supportsvideo": true,
                "properties": {
                    "schemecolor": {"type": "color", "value": "1 0 0", "order": 0,
                                    "text": "ui_browse_properties_scheme_color"},
                    "speed": {"type": "slider", "value": 1.5, "min": 0.1, "max": 3,
                              "step": 0.1, "order": 100, "text": "Speed",
                              "condition": "toggle.value", "index": 0, "precision": 1},
                    "toggle": {"type": "bool", "value": true, "order": 101, "text": "T"},
                    "sep": {"type": "group", "text": "Header", "order": 99},
                    "banner": {"text": "<img src=x>"},
                    "weird": {"type": "", "text": "_______"}
                }
            }
        }"#;
        let p1: Project = serde_json::from_str(src).expect("deserialize");
        assert!(p1.extra.contains_key("custom_unknown_key"));
        assert_eq!(
            p1.extra.get("preview"),
            Some(&Value::String("preview.gif".to_owned()))
        );
        let speed = property(&p1, "speed");
        assert_eq!(
            speed.extra.get("condition"),
            Some(&Value::String("toggle.value".to_owned()))
        );

        let text = serde_json::to_string(&p1).expect("serialize");
        let p2: Project = serde_json::from_str(&text).expect("re-deserialize");
        assert_eq!(p1, p2);

        let p3 = Project::from_value(p1.to_value()).expect("from_value");
        assert_eq!(p1, p3);

        assert!(matches!(
            p2.general.properties.get("sep"),
            Some(PropertyEntry::Group(_))
        ));
        assert!(matches!(
            p2.general.properties.get("banner"),
            Some(PropertyEntry::Group(_))
        ));
        assert!(matches!(
            p2.general.properties.get("weird"),
            Some(PropertyEntry::Unrecognized(_))
        ));
        assert_eq!(p2.declared_type, Some(DeclaredType("Scene".to_owned())));
    }

    #[test]
    fn corpus_all_manifests_parse_with_expected_type_split() {
        let Some(manifests) = corpus_manifests() else {
            return;
        };
        assert_eq!(
            manifests.len(),
            24,
            "corpus should have 24 project.json manifests"
        );

        let mut declared: BTreeMap<String, usize> = BTreeMap::new();
        let mut resolved: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut assets: Vec<String> = Vec::new();
        let mut with_workshopid = 0usize;

        for (id, bytes) in &manifests {
            let value: Value = serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
            let p = Project::from_value(value).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!p.title.is_empty(), "{id}: empty title");
            assert!(!p.file.is_empty(), "{id}: empty file");

            let key = match &p.declared_type {
                Some(t) => t.0.to_ascii_lowercase(),
                None => "<absent>".to_owned(),
            };
            *declared.entry(key).or_default() += 1;

            let res = match p.resolved_type {
                WallpaperType::Scene => "scene",
                WallpaperType::Web => "web",
                WallpaperType::Video => "video",
                WallpaperType::Image => "image",
                WallpaperType::Application => "application",
            };
            *resolved.entry(res).or_default() += 1;

            if p.is_asset() {
                assets.push(id.clone());
                assert_eq!(p.declared_type, None, "{id}: asset has no type key (§3.3)");
                assert_eq!(
                    p.resolved_type,
                    WallpaperType::Scene,
                    "{id}: §3.3 misclassification"
                );
            }
            if let Some(w) = &p.workshopid {
                with_workshopid += 1;
                assert!(matches!(w, WorkshopId::Text(_)), "{id}: non-string workshopid");
                assert_eq!(w.to_string(), *id, "{id}: workshopid mismatch");
            }
        }

        let expected: BTreeMap<String, usize> = [
            ("scene".to_owned(), 19),
            ("web".to_owned(), 3),
            ("video".to_owned(), 1),
            ("<absent>".to_owned(), 1),
        ]
        .into_iter()
        .collect();
        assert_eq!(declared, expected);

        let expected_resolved: BTreeMap<&'static str, usize> =
            [("scene", 20), ("web", 3), ("video", 1)].into_iter().collect();
        assert_eq!(resolved, expected_resolved);

        assert_eq!(assets, vec!["3347128360".to_owned()]);
        assert_eq!(with_workshopid, 17);
    }

    #[test]
    fn corpus_every_declared_property_parses_to_matching_variant() {
        let Some(manifests) = corpus_manifests() else {
            return;
        };

        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut defaulted_combos = 0usize;

        for (id, bytes) in &manifests {
            let raw: Value = serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
            let p = Project::from_value(raw.clone()).unwrap_or_else(|e| panic!("{id}: {e}"));

            let raw_props = raw
                .get("general")
                .and_then(|g| g.get("properties"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                p.general.properties.len(),
                raw_props.len(),
                "{id}: property count"
            );

            for (name, entry) in &p.general.properties {
                let raw_prop = raw_props
                    .get(name)
                    .unwrap_or_else(|| panic!("{id}:{name} missing"));
                let raw_tag = raw_prop.get("type").and_then(Value::as_str);

                let variant: &'static str = match entry {
                    PropertyEntry::Property(prop) => {
                        assert_eq!(
                            Some(prop.kind.type_tag()),
                            raw_tag,
                            "{id}:{name}: variant/tag mismatch"
                        );
                        prop.kind.type_tag()
                    }
                    PropertyEntry::Group(_) => {
                        assert!(
                            raw_tag.is_none() || raw_tag == Some("group"),
                            "{id}:{name}: Group from tag {raw_tag:?}"
                        );
                        "group"
                    }
                    PropertyEntry::Unrecognized(_) => {
                        assert!(
                            raw_tag.is_some_and(|t| !matches!(
                                t,
                                "color"
                                    | "bool"
                                    | "slider"
                                    | "combo"
                                    | "text"
                                    | "scenetexture"
                                    | "file"
                                    | "directory"
                                    | "textinput"
                                    | "usershortcut"
                                    | "group"
                            )),
                            "{id}:{name}: Unrecognized from tag {raw_tag:?}"
                        );
                        "unrecognized"
                    }
                };
                *counts.entry(variant).or_default() += 1;

                if let PropertyEntry::Property(Property {
                    kind: PropertyKind::Combo { options, value },
                    ..
                }) = entry
                    && raw_prop.get("value").is_none()
                {
                    defaulted_combos += 1;
                    let first = options.first().map(|o| o.value.as_str()).unwrap_or("");
                    assert_eq!(value, first, "{id}:{name}: combo default");
                }
            }
        }

        let expected: BTreeMap<&'static str, usize> = [
            ("bool", 49),
            ("color", 37),
            ("combo", 11),
            ("file", 2),
            ("group", 16),
            ("slider", 66),
            ("text", 11),
            ("textinput", 12),
            ("unrecognized", 7),
        ]
        .into_iter()
        .collect();
        assert_eq!(counts, expected);
        assert_eq!(counts.values().sum::<usize>(), 211);
        assert_eq!(defaulted_combos, 2);
    }

    #[test]
    fn corpus_round_trip() {
        let Some(dir) = corpus_dir() else { return };
        let mut seen = 0usize;
        for entry in std::fs::read_dir(&dir).expect("read corpus dir") {
            let manifest = entry.expect("dir entry").path().join("project.json");
            if !manifest.is_file() {
                continue;
            }
            let readable = std::fs::read(&manifest)
                .map(|bytes| bytes.contains(&b'{'))
                .unwrap_or(false);
            if !readable {
                continue;
            }
            seen += 1;
            let p1 = Project::from_path(&manifest).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
            let p2 =
                Project::from_value(p1.to_value()).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
            assert_eq!(p1, p2, "{}: value round-trip", manifest.display());

            let text = serde_json::to_string(&p1).expect("serialize");
            let p3: Project =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
            assert_eq!(p1, p3, "{}: serde round-trip", manifest.display());
        }
        assert!(seen >= 24, "corpus manifest count {seen} below floor 24");
    }
}
