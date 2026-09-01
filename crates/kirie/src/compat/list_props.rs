use std::collections::BTreeMap;
use std::path::Path;

use kirie_formats::project::{ComboOption, Project, PropertyEntry, PropertyKind};
use serde_json::{Map, Value, json};

use crate::compat::args::CompatArgs;

pub fn run(args: &CompatArgs) -> Result<(), String> {
    let overrides = overrides_map(&args.set_properties);
    let project = match args.default_background.as_deref().and_then(load_source) {
        Some(p) => p,
        None => {
            if args.list_properties_json {
                println!("[]");
            }
            return Ok(());
        }
    };

    let views = property_views(&project, &overrides);
    if args.list_properties_json {
        println!("{}", Value::Array(views.iter().map(PropView::to_json).collect()));
    } else {
        for v in &views {
            v.print_human();
        }
    }
    Ok(())
}

pub fn properties_json_string(source: &Path, overrides: &BTreeMap<String, String>) -> String {
    let shown = with_saved(source, overrides);
    match load_source(source) {
        Some(project) => {
            let array: Vec<Value> = property_views(&project, &shown)
                .iter()
                .map(PropView::to_json)
                .collect();
            Value::Array(array).to_string()
        }
        None => "[]".to_string(),
    }
}

fn with_saved(source: &Path, overrides: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut shown: BTreeMap<String, String> = super::saved_props::read(source).into_iter().collect();
    for (key, value) in overrides {
        shown.insert(key.clone(), value.clone());
    }
    shown
}

pub fn load_source(source: impl AsRef<Path>) -> Option<Project> {
    let path = source.as_ref();
    if path.is_dir() {
        return Project::from_path(path.join("project.json")).ok();
    }
    if path.is_file() {
        let name = path.file_name().and_then(|n| n.to_str());
        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("pkg") {
            return Project::from_path(path.parent()?.join("project.json")).ok();
        }
        if name == Some("project.json") || ext == Some("json") {
            return Project::from_path(path).ok();
        }
    }
    None
}

fn overrides_map(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    pairs.iter().cloned().collect()
}

fn property_views(project: &Project, overrides: &BTreeMap<String, String>) -> Vec<PropView> {
    let mut props: Vec<(&String, &kirie_formats::project::Property)> = project
        .general
        .properties
        .iter()
        .filter_map(|(k, e)| match e {
            PropertyEntry::Property(p) => Some((k, p)),
            PropertyEntry::Group(_) | PropertyEntry::Unrecognized(_) => None,
        })
        .collect();
    props.sort_by(|a, b| a.1.order.cmp(&b.1.order).then_with(|| a.0.cmp(b.0)));
    props
        .into_iter()
        .map(|(key, prop)| PropView::new(key, prop, overrides.get(key).map(String::as_str)))
        .collect()
}

struct PropView {
    key: String,
    text: String,
    order: i64,
    type_tag: &'static str,
    default: Value,
    value: Value,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    options: Option<Vec<Value>>,
}

impl PropView {
    fn new(key: &str, prop: &kirie_formats::project::Property, over: Option<&str>) -> Self {
        let type_tag = prop.kind.type_tag();
        let (default, min, max, step, options) = match &prop.kind {
            PropertyKind::Bool { value } => (json!(value), None, None, None, None),
            PropertyKind::Slider {
                value,
                min,
                max,
                step,
            } => (
                json!(value),
                Some(f64::from(*min)),
                Some(f64::from(*max)),
                Some(f64::from(*step)),
                None,
            ),
            PropertyKind::Color { value } => (json!(format_color(value)), None, None, None, None),
            PropertyKind::Combo { options, value } => {
                (json!(value), None, None, None, Some(combo_options(options)))
            }
            PropertyKind::Text => (json!(""), None, None, None, None),
            PropertyKind::TextInput { value }
            | PropertyKind::UserShortcut { value }
            | PropertyKind::File { value }
            | PropertyKind::Directory { value }
            | PropertyKind::SceneTexture { value } => (json!(value), None, None, None, None),
        };
        let value = match over {
            Some(raw) => fold_override(&prop.kind, &default, raw),
            None => default.clone(),
        };
        Self {
            key: key.to_owned(),
            text: prop.text.clone(),
            order: prop.order,
            type_tag,
            default,
            value,
            min,
            max,
            step,
            options,
        }
    }

    fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("key".into(), json!(self.key));
        obj.insert("type".into(), json!(self.type_tag));
        obj.insert("order".into(), json!(self.order));
        obj.insert("text".into(), json!(self.text));
        obj.insert("default".into(), self.default.clone());
        obj.insert("value".into(), self.value.clone());
        if let (Some(min), Some(max), Some(step)) = (self.min, self.max, self.step) {
            obj.insert("min".into(), json!(min));
            obj.insert("max".into(), json!(max));
            obj.insert("step".into(), json!(step));
        }
        if let Some(options) = &self.options {
            obj.insert("options".into(), Value::Array(options.clone()));
        }
        Value::Object(obj)
    }

    fn print_human(&self) {
        println!("{} - {}", self.key, self.type_tag);
        if !self.text.is_empty() {
            println!("    Text: {}", self.text);
        }
        println!("    Value: {}", scalar_str(&self.value));
        if self.value != self.default {
            println!("    Default: {}", scalar_str(&self.default));
        }
        if let (Some(min), Some(max), Some(step)) = (self.min, self.max, self.step) {
            println!("    Min: {min}  Max: {max}  Step: {step}");
        }
        if let Some(options) = &self.options {
            for o in options {
                if let (Some(label), Some(value)) = (o.get("label"), o.get("value")) {
                    println!("    - {} = {}", scalar_str(label), scalar_str(value));
                }
            }
        }
    }
}

fn fold_override(kind: &PropertyKind, default: &Value, raw: &str) -> Value {
    match kind {
        PropertyKind::Bool { .. } => {
            let on = raw == "true" || raw.parse::<f64>().map(|n| n != 0.0).unwrap_or(false);
            json!(on)
        }
        PropertyKind::Slider { .. } => match raw.parse::<f64>() {
            Ok(n) => json!(n),
            Err(_) => default.clone(),
        },
        _ => json!(raw),
    }
}

fn combo_options(options: &[ComboOption]) -> Vec<Value> {
    options
        .iter()
        .map(|o| json!({ "label": o.label, "value": o.value }))
        .collect()
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn format_color(rgb: &[f32; 3]) -> String {
    format!("{:.6} {:.6} {:.6}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(props: Value) -> Project {
        Project::from_value(json!({
            "title": "t",
            "file": "scene.json",
            "type": "scene",
            "general": { "properties": props },
        }))
        .expect("valid project")
    }

    #[test]
    fn schema_has_stable_fields_and_typed_default() {
        let p = project(json!({
            "bloom": { "text": "Bloom", "order": 2, "type": "bool", "value": true },
            "fov": {
                "text": "FOV", "order": 1, "type": "slider",
                "value": 45.0, "min": 10.0, "max": 90.0, "step": 1.0
            },
        }));
        let views = property_views(&p, &BTreeMap::new());
        assert_eq!(views[0].key, "fov");
        let fov = views[0].to_json();
        assert_eq!(fov["type"], json!("slider"));
        assert_eq!(fov["default"], json!(45.0));
        assert_eq!(fov["value"], json!(45.0));
        assert_eq!(fov["min"], json!(10.0));
        assert_eq!(fov["max"], json!(90.0));
        assert_eq!(fov["step"], json!(1.0));
        assert_eq!(fov["order"], json!(1));
        assert_eq!(fov["text"], json!("FOV"));
        let bloom = views[1].to_json();
        assert_eq!(bloom["type"], json!("bool"));
        assert_eq!(bloom["default"], json!(true));
        assert!(bloom.get("min").is_none(), "bool must not carry min");
    }

    #[test]
    fn override_folds_into_value_typed() {
        let p = project(json!({
            "bloom": { "type": "bool", "value": false },
            "fov": { "type": "slider", "value": 45.0, "min": 0.0, "max": 90.0, "step": 1.0 },
            "outline": { "type": "color", "value": "0 0 0" },
        }));
        let mut over = BTreeMap::new();
        over.insert("bloom".to_string(), "1".to_string());
        over.insert("fov".to_string(), "70".to_string());
        over.insert("outline".to_string(), "0.5 0.25 0.75".to_string());
        let by_key: BTreeMap<_, _> = property_views(&p, &over)
            .into_iter()
            .map(|v| (v.key.clone(), v.to_json()))
            .collect();
        assert_eq!(by_key["bloom"]["default"], json!(false));
        assert_eq!(by_key["bloom"]["value"], json!(true));
        assert_eq!(by_key["fov"]["default"], json!(45.0));
        assert_eq!(by_key["fov"]["value"], json!(70.0));
        assert_eq!(by_key["outline"]["default"], json!("0.000000 0.000000 0.000000"));
        assert_eq!(by_key["outline"]["value"], json!("0.5 0.25 0.75"));
    }

    #[test]
    fn json_string_is_single_line_and_empty_on_missing() {
        let p = project(json!({ "a": { "type": "bool", "value": true } }));
        let s = Value::Array(
            property_views(&p, &BTreeMap::new())
                .iter()
                .map(PropView::to_json)
                .collect(),
        )
        .to_string();
        assert!(!s.contains('\n'), "schema JSON must be single-line");
        assert!(s.starts_with('[') && s.ends_with(']'));
        assert_eq!(
            properties_json_string(Path::new("/definitely/not/a/thing"), &BTreeMap::new()),
            "[]"
        );
    }

    #[test]
    fn non_numeric_slider_override_keeps_default() {
        let p = project(json!({
            "fov": { "type": "slider", "value": 45.0, "min": 0.0, "max": 90.0, "step": 1.0 }
        }));
        let mut over = BTreeMap::new();
        over.insert("fov".to_string(), "garbage".to_string());
        let v = property_views(&p, &over).remove(0).to_json();
        assert_eq!(v["value"], json!(45.0));
    }
}
