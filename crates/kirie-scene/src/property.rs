use std::collections::BTreeMap;

use kirie_formats::project::{Project, PropertyEntry, PropertyKind};

use crate::value::{Color, DynamicValue, strtof};

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Number(f64),
    Color(Color),
    Combo(String),
    Text(String),
}

impl PropertyValue {
    pub fn as_condition_string(&self) -> String {
        match self {
            PropertyValue::Bool(b) => {
                if *b {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                }
            }
            PropertyValue::Number(n) => n.to_string(),
            PropertyValue::Color([r, g, b, a]) => format!("{r} {g} {b} {a}"),
            PropertyValue::Combo(s) | PropertyValue::Text(s) => s.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropertyBag {
    values: BTreeMap<String, PropertyValue>,
}

impl PropertyBag {
    pub fn new() -> Self {
        PropertyBag::default()
    }

    pub fn from_project(project: &Project) -> Self {
        let mut values = BTreeMap::new();
        for (name, entry) in &project.general.properties {
            if let PropertyEntry::Property(p) = entry {
                let value = match &p.kind {
                    PropertyKind::Bool { value } => PropertyValue::Bool(*value),
                    PropertyKind::Slider { value, .. } => PropertyValue::Number(f64::from(*value)),
                    PropertyKind::Color { value: [r, g, b] } => PropertyValue::Color([*r, *g, *b, 1.0]),
                    PropertyKind::Combo { value, .. } => PropertyValue::Combo(value.clone()),
                    PropertyKind::Text => PropertyValue::Text(String::new()),
                    PropertyKind::TextInput { value }
                    | PropertyKind::UserShortcut { value }
                    | PropertyKind::File { value }
                    | PropertyKind::Directory { value }
                    | PropertyKind::SceneTexture { value } => PropertyValue::Text(value.clone()),
                };
                values.insert(name.clone(), value);
            }
        }
        PropertyBag { values }
    }

    pub fn get(&self, name: &str) -> Option<&PropertyValue> {
        self.values.get(name)
    }

    pub fn set(&mut self, name: &str, value: PropertyValue) -> bool {
        if let Some(slot) = self.values.get_mut(name) {
            *slot = value;
            true
        } else {
            false
        }
    }

    pub fn insert(&mut self, name: impl Into<String>, value: PropertyValue) {
        self.values.insert(name.into(), value);
    }

    pub fn set_from_str(&mut self, name: &str, raw: &str) -> bool {
        let Some(current) = self.values.get(name) else {
            return false;
        };
        let parsed = match current {
            PropertyValue::Bool(_) => {
                PropertyValue::Bool(matches!(raw.trim(), "1" | "true" | "True" | "TRUE"))
            }
            PropertyValue::Number(_) => match raw.trim().parse::<f64>() {
                Ok(n) => PropertyValue::Number(n),
                Err(_) => return false,
            },
            PropertyValue::Color(_) => {
                let mut c = [0.0f32; 4];
                c[3] = 1.0;
                let mut any = false;
                for (i, tok) in raw.split_whitespace().take(4).enumerate() {
                    match tok.parse::<f32>() {
                        Ok(v) => {
                            c[i] = v;
                            any = true;
                        }
                        Err(_) => return false,
                    }
                }
                if !any {
                    return false;
                }
                PropertyValue::Color(c)
            }
            PropertyValue::Combo(_) => PropertyValue::Combo(raw.trim().to_owned()),
            PropertyValue::Text(_) => PropertyValue::Text(raw.to_owned()),
        };
        self.set(name, parsed)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

pub trait Resolvable: Sized {
    fn from_property(value: &PropertyValue) -> Self;
    fn from_bool(b: bool) -> Self;
}

fn prop_f32(v: &PropertyValue) -> f32 {
    match v {
        PropertyValue::Bool(b) => f32::from(*b),
        PropertyValue::Number(n) => *n as f32,
        PropertyValue::Color([r, ..]) => *r,
        PropertyValue::Combo(s) | PropertyValue::Text(s) => strtof(s),
    }
}

impl Resolvable for bool {
    fn from_property(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Bool(b) => *b,
            PropertyValue::Combo(s) | PropertyValue::Text(s) => {
                matches!(s.as_str(), "1" | "true" | "True" | "TRUE")
            }
            other => prop_f32(other) != 0.0,
        }
    }
    fn from_bool(b: bool) -> Self {
        b
    }
}

impl Resolvable for f32 {
    fn from_property(value: &PropertyValue) -> Self {
        prop_f32(value)
    }
    fn from_bool(b: bool) -> Self {
        f32::from(b)
    }
}

impl Resolvable for i64 {
    fn from_property(value: &PropertyValue) -> Self {
        prop_f32(value) as i64
    }
    fn from_bool(b: bool) -> Self {
        i64::from(b)
    }
}

impl Resolvable for String {
    fn from_property(value: &PropertyValue) -> Self {
        value.as_condition_string()
    }
    fn from_bool(b: bool) -> Self {
        b.to_string()
    }
}

impl Resolvable for [f32; 2] {
    fn from_property(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Color([r, g, ..]) => [*r, *g],
            other => {
                let x = prop_f32(other);
                [x, x]
            }
        }
    }
    fn from_bool(b: bool) -> Self {
        let x = f32::from(b);
        [x, x]
    }
}

impl Resolvable for [f32; 3] {
    fn from_property(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Color([r, g, b, _]) => [*r, *g, *b],
            other => {
                let x = prop_f32(other);
                [x, x, x]
            }
        }
    }
    fn from_bool(b: bool) -> Self {
        let x = f32::from(b);
        [x, x, x]
    }
}

impl Resolvable for [f32; 4] {
    fn from_property(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Color(c) => *c,
            other => {
                let x = prop_f32(other);
                [x, x, x, 1.0]
            }
        }
    }
    fn from_bool(b: bool) -> Self {
        let x = f32::from(b);
        [x, x, x, 1.0]
    }
}

impl Resolvable for DynamicValue {
    fn from_property(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Bool(b) => DynamicValue::Bool(*b),
            PropertyValue::Number(n) => DynamicValue::Float(*n),
            PropertyValue::Color(c) => DynamicValue::Color(*c),
            PropertyValue::Combo(s) | PropertyValue::Text(s) => DynamicValue::Str(s.clone()),
        }
    }
    fn from_bool(b: bool) -> Self {
        DynamicValue::Bool(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_properties_resolve_to_empty_text() {
        let project = Project::from_value(serde_json::json!({
            "title": "t",
            "file": "scene.json",
            "type": "scene",
            "general": { "properties": {
                "heading": { "type": "text", "text": "<h4>Notepad</h4>", "value": false, "order": 1 },
                "name": { "type": "textinput", "text": "Name", "value": "TYO", "order": 2 }
            } }
        }))
        .expect("project parses");
        let bag = PropertyBag::from_project(&project);
        assert_eq!(bag.get("heading"), Some(&PropertyValue::Text(String::new())));
        assert_eq!(bag.get("name"), Some(&PropertyValue::Text("TYO".into())));
    }
}
