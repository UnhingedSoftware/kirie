use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::PackError;

/// What a package is: everything a library or the Workshop needs to list it,
/// and what the player needs to start it. Stored as JSON so haru can read it
/// without linking the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable identifier the creator picks; the same across updates of one
    /// wallpaper. Lowercase ASCII letters, digits, `-`, `_` and `.`.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    pub kind: Kind,
    /// The entry the player starts from: the video, the image, the web page
    /// or the scene graph.
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub mature: bool,
    /// Settings the user can change, in the order they are shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
    /// The oldest kirie that can play this package, as `major.minor.patch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_kirie: Option<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Image,
    Web,
    Scene,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Image => "image",
            Kind::Web => "web",
            Kind::Scene => "scene",
        }
    }

    fn entry_extensions(self) -> &'static [&'static str] {
        match self {
            Kind::Video => &["mp4", "webm", "mkv"],
            Kind::Image => &["png", "jpg", "jpeg", "webp", "ktx2"],
            Kind::Web => &["html", "htm"],
            Kind::Scene => &["kscene"],
        }
    }
}

/// Where a package came from. Converted packages stay on the machine that
/// converted them; only `Original` and `ConvertedOwn` may be published.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum Provenance {
    #[default]
    Original,
    /// Converted from another format, from an item someone else made.
    Converted { source: String, source_id: String },
    /// Converted from another format, from an item the publishing account is
    /// confirmed to have made.
    ConvertedOwn { source: String, source_id: String },
}

impl Provenance {
    /// Whether a package with this provenance may be uploaded to the Workshop.
    pub fn publishable(&self) -> bool {
        !matches!(self, Provenance::Converted { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub value: PropertyValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PropertyValue {
    Slider {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
    },
    Toggle {
        default: bool,
    },
    /// Linear RGB, each 0 to 1.
    Color {
        default: [f32; 3],
    },
    Choice {
        options: Vec<Choice>,
        default: String,
    },
    Text {
        default: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Manifest {
    /// Parse a manifest read from a package. Fields this version does not
    /// know are ignored, so a newer package still lists; `min_kirie` is how a
    /// package says it needs more than this version can do.
    pub fn from_json(bytes: &[u8]) -> Result<Self, PackError> {
        serde_json::from_slice(bytes).map_err(|e| PackError::BadManifest(e.to_string()))
    }

    /// Parse a manifest a person wrote, refusing top-level fields this
    /// version does not know, so a misspelt field is reported instead of
    /// silently dropped.
    pub fn from_json_strict(bytes: &[u8]) -> Result<Self, PackError> {
        const KNOWN: [&str; 12] = [
            "id",
            "title",
            "description",
            "author",
            "kind",
            "entry",
            "preview",
            "tags",
            "mature",
            "properties",
            "min_kirie",
            "provenance",
        ];
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| PackError::BadManifest(e.to_string()))?;
        if let Some(fields) = value.as_object()
            && let Some(unknown) = fields.keys().find(|k| !KNOWN.contains(&k.as_str()))
        {
            return Err(PackError::BadManifest(format!("unknown field {unknown:?}")));
        }
        serde_json::from_value(value).map_err(|e| PackError::BadManifest(e.to_string()))
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a manifest always serializes")
    }

    /// Check the manifest on its own and against the entries the package
    /// holds.
    pub fn validate<'a>(&self, entries: impl IntoIterator<Item = &'a str>) -> Result<(), PackError> {
        let bad = |why: String| Err(PackError::BadManifest(why));
        let paths: HashSet<&str> = entries.into_iter().collect();

        if self.id.is_empty() || self.id.len() > 128 {
            return bad("id must be 1 to 128 characters".into());
        }
        if !self
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
        {
            return bad(format!("id {:?} may only use a-z, 0-9, -, _ and .", self.id));
        }
        if self.title.trim().is_empty() {
            return bad("title is empty".into());
        }
        if !paths.contains(self.entry.as_str()) {
            return bad(format!("entry {:?} is not in the package", self.entry));
        }
        let ext = self.entry.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        let allowed = self.kind.entry_extensions();
        if !ext.as_deref().is_some_and(|e| allowed.contains(&e)) {
            return bad(format!(
                "a {} package's entry must end in one of {}, not {:?}",
                self.kind.as_str(),
                allowed.join(", "),
                self.entry
            ));
        }
        if let Some(preview) = &self.preview
            && !paths.contains(preview.as_str())
        {
            return bad(format!("preview {preview:?} is not in the package"));
        }
        if let Some(v) = &self.min_kirie
            && !is_version(v)
        {
            return bad(format!("min_kirie {v:?} is not major.minor.patch"));
        }
        if let Provenance::Converted { source, source_id } | Provenance::ConvertedOwn { source, source_id } =
            &self.provenance
            && (source.is_empty() || source_id.is_empty())
        {
            return bad("a converted package must name its source and source_id".into());
        }

        let mut keys = HashSet::new();
        for p in &self.properties {
            if p.key.is_empty() {
                return bad("a property has an empty key".into());
            }
            if !keys.insert(p.key.as_str()) {
                return bad(format!("property {:?} appears twice", p.key));
            }
            if let Err(why) = p.value.check() {
                return bad(format!("property {:?}: {why}", p.key));
            }
        }
        Ok(())
    }
}

impl PropertyValue {
    fn check(&self) -> Result<(), String> {
        match self {
            PropertyValue::Slider {
                min,
                max,
                step,
                default,
            } => {
                if ![min, max, step, default].iter().all(|v| v.is_finite()) {
                    return Err("slider values must be finite".into());
                }
                if min >= max {
                    return Err("slider min must be below max".into());
                }
                if *step <= 0.0 {
                    return Err("slider step must be above 0".into());
                }
                if default < min || default > max {
                    return Err("slider default is outside min..max".into());
                }
            }
            PropertyValue::Color { default } => {
                if !default.iter().all(|c| (0.0..=1.0).contains(c)) {
                    return Err("colour channels must be between 0 and 1".into());
                }
            }
            PropertyValue::Choice { options, default } => {
                if options.is_empty() {
                    return Err("a choice needs at least one option".into());
                }
                if !options.iter().any(|o| &o.value == default) {
                    return Err(format!("default {default:?} is not one of the options"));
                }
            }
            PropertyValue::Toggle { .. } | PropertyValue::Text { .. } => {}
        }
        Ok(())
    }
}

fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> Manifest {
        Manifest {
            id: "rain-city".into(),
            title: "Rain city".into(),
            description: String::new(),
            author: String::new(),
            kind: Kind::Video,
            entry: "loop.mp4".into(),
            preview: Some("preview.jpg".into()),
            tags: vec![],
            mature: false,
            properties: vec![],
            min_kirie: None,
            provenance: Provenance::Original,
        }
    }

    const FILES: [&str; 2] = ["loop.mp4", "preview.jpg"];

    #[test]
    fn a_plain_video_manifest_is_valid() {
        video().validate(FILES).unwrap();
    }

    #[test]
    fn the_entry_must_exist_and_fit_the_kind() {
        let mut m = video();
        m.entry = "missing.mp4".into();
        assert!(m.validate(FILES).is_err());
        let mut m = video();
        m.kind = Kind::Scene;
        assert!(m.validate(FILES).is_err());
    }

    #[test]
    fn ids_are_restricted() {
        for bad in ["", "Rain", "rain city", "rain/city"] {
            let mut m = video();
            m.id = bad.into();
            assert!(m.validate(FILES).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn properties_are_checked() {
        let slider = |min, max, default| Property {
            key: "speed".into(),
            label: "Speed".into(),
            value: PropertyValue::Slider {
                min,
                max,
                step: 0.1,
                default,
            },
        };
        let mut m = video();
        m.properties = vec![slider(0.0, 2.0, 1.0)];
        m.validate(FILES).unwrap();
        m.properties = vec![slider(0.0, 2.0, 3.0)];
        assert!(m.validate(FILES).is_err());
        m.properties = vec![slider(0.0, 2.0, 1.0), slider(0.0, 2.0, 1.0)];
        assert!(m.validate(FILES).is_err());
    }

    #[test]
    fn json_round_trips_and_reads_what_a_person_writes() {
        let text = br#"{
            "id": "rain-city",
            "title": "Rain city",
            "kind": "video",
            "entry": "loop.mp4",
            "properties": [
                {"key": "tint", "label": "Tint", "type": "color", "default": [1, 0.5, 0]},
                {"key": "mode", "label": "Mode", "type": "choice", "default": "day",
                 "options": [{"value": "day", "label": "Day"}, {"value": "night", "label": "Night"}]}
            ],
            "provenance": {"origin": "converted", "source": "wallpaper-engine", "source_id": "123"}
        }"#;
        let m = Manifest::from_json(text).unwrap();
        assert_eq!(m.properties.len(), 2);
        assert!(!m.provenance.publishable());
        assert_eq!(Manifest::from_json(&m.to_json()).unwrap(), m);
    }

    #[test]
    fn unknown_fields_are_refused_only_when_strict() {
        let text = br#"{"id":"a","title":"A","kind":"video","entry":"a.mp4","colour":"red"}"#;
        assert!(Manifest::from_json_strict(text).is_err());
        Manifest::from_json(text).unwrap();
    }
}
