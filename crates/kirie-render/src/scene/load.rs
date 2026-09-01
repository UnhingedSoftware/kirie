use std::path::Path;
use std::sync::Arc;

use kirie_audio::AudioCapture;
use kirie_formats::pkg::OwnedPkg;
use kirie_formats::project::Project;
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_scene::resolve::AssetSource;
use kirie_scene::{PropertyBag, PropertyValue, Scene, SceneModel};

use super::error::SceneError;
use super::renderer::{SceneOptions, SceneRenderer};

fn apply_property_override(bag: &mut PropertyBag, name: &str, raw: &str) -> bool {
    let Some(current) = bag.get(name) else {
        return false;
    };
    let parsed = match current {
        PropertyValue::Bool(_) => {
            let t = matches!(raw.trim(), "1" | "true" | "True" | "TRUE");
            PropertyValue::Bool(t)
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
    bag.set(name, parsed)
}

#[derive(Debug, thiserror::Error)]
pub enum SceneLoadError {
    #[error("cannot open scene.pkg: {0}")]
    Pkg(String),
    #[error("cannot read scene.json from scene.pkg: {0}")]
    SceneJson(String),
    #[error("cannot parse scene.json: {0}")]
    Parse(String),
    #[error("cannot build scene renderer: {0}")]
    Build(#[from] SceneError),
}

struct CompositeSource<'a> {
    pkg: &'a OwnedPkg,
    assets: Option<&'a Path>,
}

impl AssetSource for CompositeSource<'_> {
    fn load(&self, path: &str) -> Option<Vec<u8>> {
        if let Ok(bytes) = self.pkg.read_name(path.as_bytes()) {
            return Some(bytes.to_vec());
        }
        std::fs::read(self.assets?.join(path)).ok()
    }
}

#[must_use]
pub fn scene_entry(pkg: &OwnedPkg, scene_dir: &Path) -> Vec<u8> {
    let named = std::fs::read(scene_dir.join("project.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("file")
                .and_then(serde_json::Value::as_str)
                .map(std::string::ToString::to_string)
        })
        .filter(|file| file.ends_with(".json"))
        .map(std::string::String::into_bytes)
        .filter(|name| pkg.read_name(name).is_ok());
    named.unwrap_or_else(|| b"scene.json".to_vec())
}

#[must_use]
pub fn scene_package(scene_dir: &Path) -> std::path::PathBuf {
    let named = std::fs::read(scene_dir.join("project.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("file")
                .and_then(serde_json::Value::as_str)
                .map(std::string::ToString::to_string)
        })
        .and_then(|file| {
            let stem = Path::new(&file).file_stem()?.to_string_lossy().into_owned();
            let path = scene_dir.join(format!("{stem}.pkg"));
            path.is_file().then_some(path)
        });
    named.unwrap_or_else(|| scene_dir.join("scene.pkg"))
}

pub fn load_workshop_scene(
    target: &RenderTarget<'_>,
    scene_dir: &Path,
    assets_dir: Option<&Path>,
    options: SceneOptions,
    audio: Option<Arc<AudioCapture>>,
    properties: &[(String, String)],
) -> Result<Box<dyn Renderer + Send>, SceneLoadError> {
    kirie_shader::translate::set_cache_dir(Some(scene_dir.join(".kirie-cache")));
    let pkg_path = scene_package(scene_dir);
    let pkg = match kirie_bake::map_readonly(&pkg_path) {
        Ok(map) => OwnedPkg::from_external(map),
        Err(_) => OwnedPkg::from_path(&pkg_path),
    }
    .map_err(|e| SceneLoadError::Pkg(e.to_string()))?;

    let project_bytes = std::fs::read(scene_dir.join("project.json")).ok();
    let project = project_bytes.as_deref().and_then(|bytes| {
        let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
        Project::from_value(value).ok()
    });
    let mut bag = project
        .as_ref()
        .map(PropertyBag::from_project)
        .unwrap_or_default();

    for (name, raw) in properties {
        apply_property_override(&mut bag, name, raw);
    }

    let user_props: Vec<(String, PropertyValue)> = project
        .as_ref()
        .map(|p| {
            p.general
                .properties
                .keys()
                .filter_map(|name| bag.get(name).map(|v| (name.clone(), v.clone())))
                .collect()
        })
        .unwrap_or_default();

    let source = CompositeSource {
        pkg: &pkg,
        assets: assets_dir,
    };

    let bundle_cache = kirie_bake::Cache::open_default().ok();
    let bundle_src = super::bundle::bundle_source(pkg.as_bytes(), project_bytes.as_deref(), assets_dir);
    let baked = bundle_cache
        .as_ref()
        .and_then(|cache| super::bundle::try_load_model(cache, &bundle_src));

    let mut model = if let Some(model) = baked {
        model
    } else {
        let scene = {
            let bytes = pkg
                .read_name(&scene_entry(&pkg, scene_dir))
                .map_err(|e| SceneLoadError::SceneJson(e.to_string()))?;
            Scene::from_slice(bytes).map_err(|e| SceneLoadError::Parse(e.to_string()))?
        };

        let defaults = project
            .as_ref()
            .map(PropertyBag::from_project)
            .unwrap_or_default();
        let mut model = SceneModel::resolve(scene, &defaults);
        let problems = model.load_assets(&source, &defaults);
        for p in &problems {
            tracing::debug!(path = %p.path, reason = %p.reason, "scene asset problem");
        }
        if let Some(cache) = &bundle_cache {
            super::bundle::store_model(cache, &bundle_src, &model);
        }
        model
    };
    model.reresolve(&bag);

    match SceneRenderer::new(target, &model, &source, options, audio, &user_props) {
        Ok(renderer) => Ok(Box::new(renderer)),
        Err(SceneError::NoRenderableObjects) => {
            tracing::warn!(
                dir = %scene_dir.display(),
                "scene has no renderable objects; presenting clear color"
            );
            Ok(Box::new(ClearColorRenderer::new(
                target,
                model.scene.general.clearcolor.value,
            )))
        }
        Err(e) => Err(SceneLoadError::Build(e)),
    }
}

pub fn start_background_prebake(
    workshop_root: &Path,
    assets_dir: Option<&Path>,
    should_pause: Option<kirie_bake::PauseFn>,
) -> Option<kirie_bake::BackgroundBaker> {
    let cache = kirie_bake::Cache::open_default().ok()?;
    let assets_a: Option<std::path::PathBuf> = assets_dir.map(std::path::Path::to_path_buf);
    let assets_b = assets_a.clone();

    let source_fn: kirie_bake::SourceFn = std::sync::Arc::new(move |item: &Path| {
        let pkg_path = scene_package(item);
        let pkg = kirie_bake::map_readonly(&pkg_path).map_err(|e| kirie_bake::BakeError::Io {
            path: pkg_path.clone(),
            source: e,
        })?;
        let project = std::fs::read(item.join("project.json")).ok();
        Ok(super::bundle::bundle_source(
            (*pkg).as_ref(),
            project.as_deref(),
            assets_a.as_deref(),
        ))
    });

    let content_fn: kirie_bake::ContentFn = std::sync::Arc::new(move |item: &Path, _src: &[u8]| {
        let pkg_path = scene_package(item);
        let map = kirie_bake::map_readonly(&pkg_path).map_err(|e| kirie_bake::BakeError::Io {
            path: pkg_path.clone(),
            source: e,
        })?;
        let pkg =
            OwnedPkg::from_external(map).map_err(|e| kirie_bake::BakeError::Serialize(e.to_string()))?;
        kirie_shader::translate::set_cache_dir(Some(item.join(".kirie-cache")));
        let scene = {
            let bytes = pkg
                .read_name(&scene_entry(&pkg, item))
                .map_err(|e| kirie_bake::BakeError::Serialize(e.to_string()))?;
            Scene::from_slice(bytes).map_err(|e| kirie_bake::BakeError::Serialize(e.to_string()))?
        };
        let project = std::fs::read(item.join("project.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| Project::from_value(value).ok());
        let defaults = project
            .as_ref()
            .map(PropertyBag::from_project)
            .unwrap_or_default();
        let mut model = SceneModel::resolve(scene, &defaults);
        let source = CompositeSource {
            pkg: &pkg,
            assets: assets_b.as_deref(),
        };
        let _ = model.load_assets(&source, &defaults);
        let mut content = kirie_bake::BundleContent::new();
        content
            .set_scene_model(&model)
            .map_err(|e| kirie_bake::BakeError::Serialize(e.to_string()))?;
        Ok(content)
    });

    let mut config = kirie_bake::BakerConfig::new(cache, source_fn, content_fn);
    if let Some(pause) = should_pause {
        config.should_pause = pause;
    }
    let mut baker = kirie_bake::BackgroundBaker::start(config);
    let mut queued = 0usize;
    if let Ok(entries) = std::fs::read_dir(workshop_root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if scene_package(&dir).is_file() {
                baker.enqueue(&dir);
                queued += 1;
            }
        }
    }
    let _ = baker.watch(workshop_root);
    tracing::info!(root = %workshop_root.display(), queued, "background pre-bake started");
    Some(baker)
}

struct ClearColorRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    color: wgpu::Color,
}

impl ClearColorRenderer {
    fn new(target: &RenderTarget<'_>, clear: [f32; 4]) -> Self {
        Self {
            device: target.device.clone(),
            queue: target.queue.clone(),
            color: wgpu::Color {
                r: f64::from(clear[0]),
                g: f64::from(clear[1]),
                b: f64::from(clear[2]),
                a: 1.0,
            },
        }
    }
}

impl Renderer for ClearColorRenderer {
    fn render(&mut self, view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-scene-clear-encoder"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-scene-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
    }
}
