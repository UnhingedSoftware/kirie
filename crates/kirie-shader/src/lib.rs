#![cfg_attr(not(test), forbid(unsafe_code))]

use std::collections::BTreeMap;
use std::path::PathBuf;

use thiserror::Error;

pub mod annotation;
pub mod coerce;
pub mod hlslmod;
mod hlslrelax;
pub mod matinverse;
pub mod modernize;
pub mod preprocess;
mod repair;
pub mod reflect;
pub mod translate;

pub use reflect::Reflection;

pub const TRANSLATOR_VERSION: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
}

impl Stage {
    #[must_use]
    pub fn naga(self) -> naga::ShaderStage {
        match self {
            Stage::Vertex => naga::ShaderStage::Vertex,
            Stage::Fragment => naga::ShaderStage::Fragment,
        }
    }

    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Stage::Vertex => "vert",
            Stage::Fragment => "frag",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TranslatePath {
    NagaGlsl,
    Shaderc,
}

pub trait IncludeResolver {
    fn resolve(&self, include_name: &str) -> Option<String>;
}

#[derive(Debug, Clone)]
pub struct FsIncludeResolver {
    pub roots: Vec<PathBuf>,
}

impl FsIncludeResolver {
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl IncludeResolver for FsIncludeResolver {
    fn resolve(&self, include_name: &str) -> Option<String> {
        for root in &self.roots {
            let path = root.join(include_name);
            if let Ok(text) = std::fs::read_to_string(&path) {
                return Some(text);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct MapIncludeResolver {
    pub headers: BTreeMap<String, String>,
    pub fallback: Option<FsIncludeResolver>,
}

impl IncludeResolver for MapIncludeResolver {
    fn resolve(&self, include_name: &str) -> Option<String> {
        if let Some(s) = self.headers.get(include_name) {
            return Some(s.clone());
        }
        self.fallback.as_ref().and_then(|f| f.resolve(include_name))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShaderInputs {
    pub combos: BTreeMap<String, i32>,
    pub override_combos: BTreeMap<String, i32>,
    pub populated_texture_slots: std::collections::BTreeSet<u32>,
}

#[derive(Debug, Clone)]
pub struct TranslatedShader {
    pub module: naga::Module,
    pub reflection: Reflection,
    pub path: TranslatePath,
    pub glsl: String,
}

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("annotation error in {file}: {source}")]
    Annotation {
        file: String,
        source: annotation::AnnotationError,
    },
    #[error("no `main` entry point found in {file}")]
    NoMain { file: String },
    #[error("translation failed for {file}:\n  naga glsl-in: {naga}\n  shaderc: {shaderc}")]
    Compile {
        file: String,
        naga: String,
        shaderc: String,
    },
    #[error("naga validation failed for {file}: {diag}")]
    Validate { file: String, diag: String },
}

pub fn translate(
    stage: Stage,
    filename: &str,
    source: &str,
    resolver: &dyn IncludeResolver,
    inputs: &ShaderInputs,
) -> Result<TranslatedShader, TranslateError> {
    let unit_key = translate::unit_cache_key(stage, source, inputs);
    if let Some(ts) = translate::unit_cache_load(&unit_key, resolver) {
        return Ok(ts);
    }

    let recording = RecordingResolver {
        inner: resolver,
        seen: std::cell::RefCell::new(Vec::new()),
    };
    let assembled = preprocess::preprocess(stage, filename, source, &recording, inputs)?;
    let (glsl, reflection) = modernize::modernize(stage, assembled);
    let out = translate::translate(stage, filename, glsl, reflection)?;
    translate::unit_cache_store(&unit_key, recording.seen.into_inner(), &out);
    Ok(out)
}

struct RecordingResolver<'a> {
    inner: &'a dyn IncludeResolver,
    seen: std::cell::RefCell<Vec<(String, [u8; 32])>>,
}

impl IncludeResolver for RecordingResolver<'_> {
    fn resolve(&self, include_name: &str) -> Option<String> {
        let body = self.inner.resolve(include_name)?;
        let mut seen = self.seen.borrow_mut();
        if !seen.iter().any(|(n, _)| n == include_name) {
            seen.push((include_name.to_owned(), *blake3::hash(body.as_bytes()).as_bytes()));
        }
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoIncludes;
    impl IncludeResolver for NoIncludes {
        fn resolve(&self, _: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn translate_minimal_fragment_with_sampler_and_uniform() {
        let src = "\
uniform sampler2D g_Texture0; // {\"default\":\"util/white\"}\n\
uniform float g_Brightness; // {\"material\":\"Brightness\",\"default\":1}\n\
varying vec2 v_TexCoord;\n\
void main() {\n\
    gl_FragColor = texSample2D(g_Texture0, v_TexCoord) * g_Brightness;\n\
}\n";
        let ts = translate(
            Stage::Fragment,
            "test.frag",
            src,
            &NoIncludes,
            &ShaderInputs::default(),
        )
        .expect("translation should succeed");
        assert!(ts.module.entry_points.iter().any(|e| e.name == "main"));
        assert_eq!(ts.reflection.samplers.len(), 1);
        assert_eq!(ts.reflection.samplers[0].slot, Some(0));
        assert_eq!(ts.reflection.parameters.len(), 1);
        assert_eq!(ts.reflection.parameters[0].material, "Brightness");
        assert_eq!(ts.reflection.globals_block, vec!["g_Brightness"]);
    }

    #[test]
    fn a_combo_reads_as_a_boolean_condition() {
        let src = "\
// [COMBO] {\"material\":\"Invert\",\"combo\":\"INVERT\",\"type\":\"options\",\"default\":0}\n\
uniform sampler2D g_Texture0; // {\"default\":\"util/white\"}\n\
varying vec2 v_TexCoord;\n\
void main() {\n\
    float mask = texSample2D(g_Texture0, v_TexCoord).r;\n\
    mask = INVERT ? 1.0 - mask : mask;\n\
    if (INVERT) { mask = mask * 0.5; }\n\
    gl_FragColor = vec4(mask);\n\
}\n";
        translate(
            Stage::Fragment,
            "combo.frag",
            src,
            &NoIncludes,
            &ShaderInputs::default(),
        )
        .expect("a combo used as a condition still translates");
    }

    #[test]
    fn translate_minimal_vertex() {
        let src = "\
attribute vec3 a_Position;\n\
attribute vec2 a_TexCoord;\n\
varying vec2 v_TexCoord;\n\
uniform mat4 g_ModelViewProjectionMatrix;\n\
void main() {\n\
    v_TexCoord = a_TexCoord;\n\
    gl_Position = mul(g_ModelViewProjectionMatrix, vec4(a_Position, 1.0));\n\
}\n";
        let ts = translate(
            Stage::Vertex,
            "test.vert",
            src,
            &NoIncludes,
            &ShaderInputs::default(),
        )
        .expect("vertex translation should succeed");
        assert!(ts.module.entry_points.iter().any(|e| e.name == "main"));
        let names: Vec<&str> = ts.reflection.attributes.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"a_Position"));
        assert!(names.contains(&"a_TexCoord"));
    }

    // A tripwire, not a fact: every cached shader on every machine is keyed by
    // this number, so bumping it throws all of them away. Change it when the
    // translation changes — and change this line in the same commit.
    #[test]
    fn the_translator_version_changes_deliberately() {
        assert_eq!(
            TRANSLATOR_VERSION, 9,
            "translation changed? bump this too — it invalidates every shader cache"
        );
    }
}
