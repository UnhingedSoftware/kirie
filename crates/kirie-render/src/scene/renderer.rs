use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use kirie_audio::{AudioCapture, AudioSpectrum};
use kirie_platform::{RenderTarget, Renderer, SurfaceSize};
use kirie_scene::SceneModel;
use kirie_scene::material::Blending;
use kirie_scene::object::{ImageObject, Object, ObjectKind};
use kirie_scene::resolve::AssetSource;
use kirie_scene::scene::Projection;
use kirie_scene::value::DynamicValue;
use kirie_shader::reflect::{ParamDefault, Parameter};
use kirie_shader::{IncludeResolver, reflect::SamplerSlot};

use crate::particle::SpriteInstance;
use crate::scaling::{ClampMode, ScalingMode};

use super::animation::{AnimOutput, PropertyAnimator};
use super::extras::{self, ParticleGpu, TextGpu, TextPipeline};
use super::fbo::{FBO_FORMAT, Fbo};
use super::matrix::{self, Mat4};
use super::pipeline::{self, BindKind, BuiltPass, ModuleBinding};
use super::plan::{self, Geometry, PassOutput};
use super::scripting::{ParticleOp, PropTarget, PropUpdate, ScriptHost, as_f32, as_rgb, as_vec3};
use super::text::TextFonts;
use super::texture::TextureRegistry;
use super::uniforms::{Builtins, GlobalsLayout, pack_globals};

#[derive(Debug, Clone, PartialEq)]
pub struct SceneOptions {
    pub scaling: ScalingMode,
    pub clamp: ClampMode,
    pub render_scale: f32,
    pub disable_parallax: bool,
    pub fit_render_to_output: bool,
    pub only_objects: Vec<i64>,
    pub skip_objects: Vec<i64>,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions {
            scaling: ScalingMode::default(),
            clamp: ClampMode::default(),
            render_scale: 1.0,
            disable_parallax: false,
            fit_render_to_output: false,
            only_objects: Vec::new(),
            skip_objects: Vec::new(),
        }
    }
}

struct SourceIncludes<'a>(&'a dyn AssetSource);

impl IncludeResolver for SourceIncludes<'_> {
    fn resolve(&self, include_name: &str) -> Option<String> {
        let bytes = self.0.load(&format!("shaders/{include_name}"))?;
        String::from_utf8(bytes).ok()
    }
}

struct PassGpu {
    pipeline: wgpu::RenderPipeline,
    g0_bind: wgpu::BindGroup,
    g1_bind: wgpu::BindGroup,
    vs_ubo: Option<wgpu::Buffer>,
    fs_ubo: Option<wgpu::Buffer>,
    vs_globals: GlobalsLayout,
    fs_globals: GlobalsLayout,
    vs_params: BTreeMap<String, Vec<f32>>,
    fs_params: BTreeMap<String, Vec<f32>>,
    vertex_buffer: wgpu::Buffer,
    uv_crop: [f32; 2],
    effect_index: Option<usize>,
    puppet_indices: Option<wgpu::Buffer>,
    puppet_index_count: u32,
    output: PassOutput,
    geometry: Geometry,
    model_matrix: Mat4,
    blending: Blending,
    tex_resolution: [[f32; 4]; 8],
    material_pass: kirie_scene::material::Pass,
    params_vs: Arc<Vec<Parameter>>,
    params_fs: Arc<Vec<Parameter>>,
}

struct ObjectGpu {
    id: i64,
    parent: Option<i64>,
    passes: Vec<PassGpu>,
    fbos: [Option<Fbo>; 2],
    named_fbos: std::collections::HashMap<String, Fbo>,
    alpha: f32,
    brightness: f32,
    color: [f32; 4],
    visible: bool,
    reads_scene: bool,
    offscreen_donor: bool,
    parallax_depth: [f32; 2],
    scene_center: [f32; 2],
    local_to_scene: Mat4,
    angle_z: f32,
    final_front: Option<usize>,
    atlas: Option<Arc<super::texture::AtlasTexture>>,
    image_size: (u32, u32),
}

struct AtlasSlot {
    atlas: Arc<super::texture::AtlasTexture>,
    uploaded_page: usize,
}

struct RuntimeLayer {
    origin: [f32; 3],
    scale: [f32; 3],
    angles: [f32; 3],
    color: [f32; 3],
    alpha: f32,
    texture: Option<std::sync::Arc<super::texture::GpuTexture>>,
    tint: [f32; 3],
    tint_alpha: f32,
    base_size: [f32; 2],
    visible: bool,
    order: i64,
}

impl Default for RuntimeLayer {
    fn default() -> Self {
        RuntimeLayer {
            origin: [0.0; 3],
            scale: [1.0; 3],
            angles: [0.0; 3],
            color: [1.0; 3],
            texture: None,
            tint: [1.0; 3],
            tint_alpha: 1.0,
            base_size: [1.0, 1.0],
            alpha: 1.0,
            visible: true,
            order: 0,
        }
    }
}

enum SceneItem {
    Image(Box<ObjectGpu>),
    Particle(Box<ParticleGpu>),
    Text(Box<TextGpu>),
    Model(Box<super::model::ModelGpu>),
}

pub struct SceneRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    proj_w: u32,
    proj_h: u32,
    clear_color: wgpu::Color,
    screen_mvp: Mat4,
    items: Vec<SceneItem>,
    sprite_scratch: Vec<SpriteInstance>,
    pack_scratch: Vec<u8>,
    video_textures: Vec<super::texture::VideoTexture>,
    video_users: Vec<Vec<usize>>,
    atlas_textures: Vec<AtlasSlot>,
    pointer: [f32; 2],
    pointer_last: [f32; 2],
    pointer_left: bool,
    runtime_layers: std::collections::HashMap<i64, RuntimeLayer>,
    runtime_templates: std::collections::HashMap<String, RuntimeTemplate>,
    runtime_white: std::sync::Arc<super::texture::GpuTexture>,
    runtime_seq: i64,
    runtime_pipeline: Option<(wgpu::RenderPipeline, wgpu::Buffer, usize)>,
    parallax_disp: [f32; 2],
    text_pipeline: Option<TextPipeline>,
    text_fonts: Option<TextFonts>,
    scene_fbo: Fbo,
    scene_snapshot: Option<Fbo>,
    bloom: Option<super::bloom::Bloom>,
    blit_pipeline: wgpu::RenderPipeline,
    blit_bind: wgpu::BindGroup,
    blit_window: wgpu::Buffer,
    options: SceneOptions,
    elapsed: f64,
    window_for: Option<SurfaceSize>,
    ambient: [f32; 3],
    skylight: [f32; 3],
    blit_srgb: bool,
    audio: Option<Arc<AudioCapture>>,
    script: Option<ScriptHost>,
    animator: Option<PropertyAnimator>,
    tz_offset_secs: f64,
    camera: kirie_scene::scene::Camera,
    bag: kirie_scene::PropertyBag,
    general: kirie_scene::scene::General,
    model_depth: Option<wgpu::TextureView>,
    parent_by_id: HashMap<i64, Option<i64>>,
    locals: HashMap<i64, LocalXf>,
    media: Option<Arc<crate::media::MediaSource>>,
    zoom: f32,
    visible_by_id: HashMap<i64, bool>,
    visibility_bindings: Vec<VisBinding>,
    effect_vis_bindings: Vec<EffectVisBinding>,
    structural_props: std::collections::HashSet<String>,
}

struct VisBinding {
    id: i64,
    base: kirie_scene::user::UserSetting<bool>,
    image: Option<kirie_scene::user::UserSetting<bool>>,
}

struct EffectVisBinding {
    us: kirie_scene::user::UserSetting<bool>,
    planned: bool,
}

impl SceneRenderer {
    pub fn new(
        target: &RenderTarget<'_>,
        model: &SceneModel,
        source: &dyn AssetSource,
        options: SceneOptions,
        audio: Option<Arc<AudioCapture>>,
        user_props: &[(String, kirie_scene::PropertyValue)],
    ) -> Result<Self, super::SceneError> {
        let device = target.device;
        let queue = target.queue;
        let scene = &model.scene;

        let mut bag = kirie_scene::PropertyBag::new();
        for (name, value) in user_props {
            bag.insert(name.clone(), value.clone());
        }
        let general = scene.general.clone();

        let (proj_w, proj_h) = projection_size(model, target.size);
        if proj_w == 0 || proj_h == 0 {
            return Err(super::SceneError::BadProjection {
                width: proj_w,
                height: proj_h,
            });
        }

        let cam = &scene.camera;
        let screen_mvp = screen_camera_mvp((proj_w, proj_h), cam.eye, cam.center, cam.up, cam.farz);

        let clear = scene.general.clearcolor.value;
        let clear_color = wgpu::Color {
            r: f64::from(clear[0]),
            g: f64::from(clear[1]),
            b: f64::from(clear[2]),
            a: 1.0,
        };

        let mut registry = TextureRegistry::new(device, queue);
        let fbo_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kirie-scene-fbo-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        let parent_by_id: HashMap<i64, Option<i64>> =
            scene.objects.iter().map(|o| (o.base.id, o.base.parent)).collect();
        let anchors_by_object: HashMap<i64, HashMap<String, [f32; 2]>> = scene
            .objects
            .iter()
            .filter_map(|o| {
                let ObjectKind::Image(image) = &o.kind else {
                    return None;
                };
                let path = image.model.as_ref()?.puppet.as_ref()?;
                let bytes = source.load(path)?;
                let mesh = kirie_formats::model::PuppetMesh::parse(&bytes).ok()?;
                let wanted = image
                    .animationlayers
                    .iter()
                    .find(|layer| layer.visible.value)
                    .and_then(|layer| u32::try_from(layer.animation.value).ok());
                let animation = wanted.and_then(|id| mesh.animation(id));
                let anchors: HashMap<String, [f32; 2]> = mesh
                    .attachments
                    .iter()
                    .filter_map(|point| {
                        let at = mesh.anchor(&point.name, animation, 0.0)?;
                        Some((point.name.clone(), [at[0], at[1]]))
                    })
                    .collect();
                (!anchors.is_empty()).then_some((o.base.id, anchors))
            })
            .collect();

        let attach_offset = |o: &kirie_scene::object::Object| -> [f32; 2] {
            let (Some(name), Some(parent)) = (o.base.attachment.as_deref(), o.base.parent) else {
                return [0.0, 0.0];
            };
            anchors_by_object
                .get(&parent)
                .and_then(|anchors| anchors.get(name))
                .copied()
                .unwrap_or([0.0, 0.0])
        };

        let local_xf: HashMap<i64, LocalXf> = scene
            .objects
            .iter()
            .map(|o| {
                let attach = attach_offset(o);
                (
                    o.base.id,
                    LocalXf {
                        origin: [
                            o.base.origin.value[0] + attach[0],
                            o.base.origin.value[1] + attach[1],
                        ],
                        scale: [o.base.scale.value[0], o.base.scale.value[1]],
                        angle_z: o.base.angles.value[2],
                        parent: o.base.parent,
                    },
                )
            })
            .collect();
        let mut visible_by_id: HashMap<i64, bool> = scene
            .objects
            .iter()
            .map(|o| (o.base.id, o.base.visible.value))
            .collect();
        for o in &scene.objects {
            if let ObjectKind::Image(img) = &o.kind
                && !img.visible.value
            {
                visible_by_id.insert(o.base.id, false);
            }
        }

        let mut order: Vec<usize> = (0..scene.objects.len()).collect();
        if scene.general.customsortorder {
            order.sort_by_key(|&i| scene.objects[i].base.sortorder);
        }

        let mut rs = options.render_scale.clamp(0.25, 4.0);
        if options.fit_render_to_output {
            let (out_w, out_h) = target.size;
            let (proj_long, out_long) = (proj_w.max(proj_h), out_w.max(out_h));
            if out_long > 0 && proj_long > 0 {
                let fit = out_long as f32 / proj_long as f32;
                if fit < rs {
                    tracing::debug!(
                        projection = format!("{proj_w}x{proj_h}"),
                        output = format!("{out_w}x{out_h}"),
                        render_scale = rs,
                        fitted = fit,
                        "clamping render targets to the output size"
                    );
                    rs = fit;
                }
            }
        }
        let scale_dim = |d: u32| ((d as f32 * rs).round() as u32).max(1);
        let (fbo_w, fbo_h) = super::fbo::fit_within(
            scale_dim(proj_w),
            scale_dim(proj_h),
            device.limits().max_texture_dimension_2d,
        );
        let scene_fbo = Fbo::new(device, "kirie-scene-fbo", fbo_w, fbo_h);
        let scene_snapshot = Fbo::new(device, "kirie-scene-snapshot", fbo_w, fbo_h);

        let bloom = scene.general.bloom.value.then(|| {
            let env_f = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok());
            super::bloom::Bloom::new(
                device,
                queue,
                fbo_w,
                fbo_h,
                &scene_fbo.view,
                &scene_snapshot.view,
                env_f("KIRIE_BLOOM_STRENGTH").unwrap_or(scene.general.bloomstrength.value),
                env_f("KIRIE_BLOOM_THRESHOLD").unwrap_or(scene.general.bloomthreshold.value),
            )
        });

        let resolver = SourceIncludes(source);
        let mut items = Vec::new();
        let mut text_pipeline: Option<TextPipeline> = None;
        let mut text_fonts: Option<TextFonts> = None;

        let donor_ids: std::collections::HashSet<i64> = scene
            .objects
            .iter()
            .flat_map(|o| {
                o.base
                    .dependencies
                    .iter()
                    .copied()
                    .filter(move |dep| *dep != o.base.id)
            })
            .filter(|id| {
                scene
                    .objects
                    .iter()
                    .any(|o| o.base.id == *id && matches!(o.kind, ObjectKind::Image(_)))
            })
            .collect();
        let mut donor_built: std::collections::HashMap<usize, ObjectGpu> = std::collections::HashMap::new();
        let mut cross: std::collections::HashMap<String, wgpu::TextureView> =
            std::collections::HashMap::new();
        let param_cache = std::sync::Mutex::new(ParamCache::new());
        for &oi in &order {
            let object = &scene.objects[oi];
            if !donor_ids.contains(&object.base.id) {
                continue;
            }
            if let ObjectKind::Image(image) = &object.kind {
                let world = world_xf(object.base.id, &local_xf);
                if let Some(obj) = build_object(
                    device,
                    object,
                    image,
                    (proj_w, proj_h),
                    &screen_mvp,
                    source,
                    &resolver,
                    &registry,
                    &fbo_sampler,
                    &scene_snapshot,
                    world,
                    true,
                    &cross,
                    &param_cache,
                    rs,
                ) {
                    if let Some(front) = obj.final_front
                        && let Some(fbo) = obj.fbos[front].as_ref()
                    {
                        let id = object.base.id;
                        cross.insert(format!("_rt_imageLayerComposite_{id}_a"), fbo.view.clone());
                        cross.insert(format!("_rt_imageLayerComposite_{id}_b"), fbo.view.clone());
                    } else {
                        tracing::warn!(
                            id = object.base.id,
                            front = ?obj.final_front,
                            "a layer others draw from has no composite of its own"
                        );
                    }
                    donor_built.insert(oi, obj);
                }
            }
        }

        let shader_cache_dir = kirie_shader::translate::cache_dir();
        let parallel_built: HashMap<usize, ObjectGpu> = {
            use rayon::prelude::*;
            let image_indices: Vec<usize> = order
                .iter()
                .copied()
                .filter(|oi| {
                    !donor_built.contains_key(oi) && matches!(&scene.objects[*oi].kind, ObjectKind::Image(_))
                })
                .collect();
            image_indices
                .into_par_iter()
                .filter_map(|oi| {
                    kirie_shader::translate::set_cache_dir(shader_cache_dir.clone());
                    let object = &scene.objects[oi];
                    let ObjectKind::Image(image) = &object.kind else {
                        return None;
                    };
                    let world = world_xf(object.base.id, &local_xf);
                    build_object(
                        device,
                        object,
                        image,
                        (proj_w, proj_h),
                        &screen_mvp,
                        source,
                        &resolver,
                        &registry,
                        &fbo_sampler,
                        &scene_snapshot,
                        world,
                        false,
                        &cross,
                        &param_cache,
                        rs,
                    )
                    .map(|obj| (oi, obj))
                })
                .collect()
        };
        let mut parallel_built = parallel_built;

        for &oi in &order {
            let object = &scene.objects[oi];
            if let Some(obj) = donor_built.remove(&oi) {
                items.push(SceneItem::Image(Box::new(obj)));
                continue;
            }
            match &object.kind {
                ObjectKind::Image(_) => {
                    if let Some(obj) = parallel_built.remove(&oi) {
                        items.push(SceneItem::Image(Box::new(obj)));
                    }
                }
                ObjectKind::Particle(pobj) => {
                    if let Some(pg) = extras::build_particle(
                        device,
                        queue,
                        object,
                        pobj,
                        (proj_w, proj_h),
                        &screen_mvp,
                        source,
                        &mut registry,
                    ) {
                        items.push(SceneItem::Particle(Box::new(pg)));
                    }
                }
                ObjectKind::Text(tobj) => {
                    let tp = text_pipeline.get_or_insert_with(|| extras::build_text_pipeline(device));
                    let fonts = text_fonts.get_or_insert_with(TextFonts::new);
                    let world = world_xf(object.base.id, &local_xf);
                    if let Some(tg) = extras::build_text(
                        device,
                        queue,
                        tp,
                        fonts,
                        object,
                        tobj,
                        (proj_w, proj_h),
                        &screen_mvp,
                        source,
                        (world.0, world.1),
                    ) {
                        items.push(SceneItem::Text(Box::new(tg)));
                    }
                }
                ObjectKind::Model(mobj) => {
                    if let Some(mg) = super::model::build_model(
                        device,
                        object,
                        mobj,
                        (proj_w, proj_h),
                        source,
                        &resolver,
                        &mut registry,
                        &fbo_sampler,
                        &scene_snapshot,
                    ) {
                        items.push(SceneItem::Model(Box::new(mg)));
                    }
                }
                other => {
                    tracing::debug!(id = object.base.id, kind = ?std::mem::discriminant(other), "non-drawn object skipped (docs §5.6, §7.2)");
                }
            }
        }

        if items.is_empty() {
            return Err(super::SceneError::NoRenderableObjects);
        }

        let scene_snapshot = (bloom.is_some()
            || items.iter().any(|it| match it {
                SceneItem::Image(o) => o.reads_scene,
                SceneItem::Model(m) => m.reads_scene,
                _ => false,
            }))
        .then_some(scene_snapshot);

        if !options.only_objects.is_empty() || !options.skip_objects.is_empty() {
            for item in &mut items {
                let id = match item {
                    SceneItem::Image(o) => o.id,
                    SceneItem::Text(t) => t.id,
                    SceneItem::Particle(p) => p.id,
                    SceneItem::Model(m) => m.id,
                };
                let wanted = options.only_objects.is_empty() || options.only_objects.contains(&id);
                if wanted && !options.skip_objects.contains(&id) {
                    continue;
                }
                match item {
                    SceneItem::Image(o) => o.visible = false,
                    SceneItem::Text(t) => t.visible = false,
                    SceneItem::Particle(p) => p.visible = false,
                    SceneItem::Model(m) => m.visible = false,
                }
            }
        }

        let (blit_pipeline, blit_bind, blit_window) =
            build_blit(device, target.format, &scene_fbo, &fbo_sampler);

        let mut script = ScriptHost::build(model, (proj_w, proj_h), user_props);
        let animator = PropertyAnimator::build(model);
        let tz_offset_secs = if animator.is_some() {
            super::scripting::local_utc_offset_secs()
        } else {
            0.0
        };
        let zoom = model.scene.general.zoom.value;
        let zoom = if zoom > 0.0 { zoom } else { 1.0 };

        if let Some(host) = script.as_mut() {
            for item in &mut items {
                if let SceneItem::Text(tg) = item {
                    let initial = tg.current_text().to_owned();
                    if let Some(ts) = tg.script.as_mut() {
                        ts.handle = host.create_text_layer(&ts.source, ts.properties.clone(), &initial);
                    }
                }
            }
        }

        let media = script.as_ref().filter(|h| h.wants_media()).map(|_| {
            Arc::new(crate::media::MediaSource::start(
                crate::media::MediaConfig::default(),
            ))
        });

        let model_depth = items
            .iter()
            .any(|it| matches!(it, SceneItem::Model(_)))
            .then(|| super::model::create_depth_texture(device, fbo_w, fbo_h));

        let runtime_templates = collect_runtime_templates(model, source, user_props, &registry);
        let runtime_white = registry.white();

        Ok(SceneRenderer {
            device: device.clone(),
            queue: queue.clone(),
            proj_w,
            proj_h,
            clear_color,
            screen_mvp,
            video_users: {
                let videos = registry.peek_video_names();
                videos
                    .iter()
                    .map(|name| {
                        items
                            .iter()
                            .enumerate()
                            .filter(|(_, item)| match item {
                                SceneItem::Image(o) => o.passes.iter().any(|p| {
                                    p.material_pass
                                        .textures
                                        .iter()
                                        .any(|t| t.as_deref() == Some(name.as_str()))
                                }),
                                _ => false,
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .collect()
            },
            items,
            sprite_scratch: Vec::new(),
            pack_scratch: Vec::new(),
            video_textures: registry.take_videos(),
            atlas_textures: registry
                .take_atlases()
                .into_iter()
                .map(|atlas| AtlasSlot {
                    atlas,
                    uploaded_page: 0,
                })
                .collect(),
            pointer: [0.5, 0.5],
            pointer_last: [0.5, 0.5],
            pointer_left: false,
            locals: local_xf,
            media,
            zoom,
            parallax_disp: [0.0, 0.0],
            runtime_layers: std::collections::HashMap::new(),
            runtime_templates,
            runtime_white,
            runtime_seq: 0,
            runtime_pipeline: None,
            text_pipeline,
            text_fonts,
            scene_fbo,
            scene_snapshot,
            bloom,
            bag,
            general,
            blit_pipeline,
            blit_bind,
            blit_window,
            options,
            elapsed: 0.0,
            window_for: None,
            ambient: [
                scene.general.ambientcolor.value[0],
                scene.general.ambientcolor.value[1],
                scene.general.ambientcolor.value[2],
            ],
            skylight: [
                scene.general.skylightcolor.value[0],
                scene.general.skylightcolor.value[1],
                scene.general.skylightcolor.value[2],
            ],
            blit_srgb: target.format.is_srgb(),
            audio,
            script,
            animator,
            tz_offset_secs,
            camera: scene.camera.clone(),
            model_depth,
            parent_by_id,
            visible_by_id,
            visibility_bindings: collect_visibility_bindings(scene),
            effect_vis_bindings: collect_effect_vis_bindings(scene),
            structural_props: collect_structural_props(scene),
        })
    }

    #[must_use]
    pub fn projection_size(&self) -> (u32, u32) {
        (self.proj_w, self.proj_h)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn debug_pass_count(&self) -> usize {
        self.items
            .iter()
            .map(|it| match it {
                SceneItem::Image(o) => o.passes.len(),
                _ => 0,
            })
            .sum()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn debug_particle_count(&self) -> usize {
        self.items
            .iter()
            .filter(|it| matches!(it, SceneItem::Particle(_)))
            .count()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn debug_text_count(&self) -> usize {
        self.items
            .iter()
            .filter(|it| matches!(it, SceneItem::Text(_)))
            .count()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn debug_live_particles(&self) -> usize {
        self.items
            .iter()
            .map(|it| match it {
                SceneItem::Particle(p) => p.sim.live_count(),
                _ => 0,
            })
            .sum()
    }

    fn apply_image_transforms(&mut self, updates: &[PropUpdate]) {
        let mut dirty: Vec<i64> = Vec::new();
        for u in updates {
            if !matches!(
                u.target,
                PropTarget::Origin | PropTarget::Scale | PropTarget::Angles
            ) {
                continue;
            }
            let Some(l) = self.locals.get_mut(&u.object_id) else {
                continue;
            };
            let Some(v) = as_vec3(&u.value) else { continue };
            match u.target {
                PropTarget::Origin => l.origin = [v[0], v[1]],
                PropTarget::Scale => l.scale = [v[0], v[1]],
                PropTarget::Angles => l.angle_z = v[2],
                _ => unreachable!(),
            }
            dirty.push(u.object_id);
        }
        for u in updates {
            let (origin, scale) = match u.target {
                PropTarget::Origin => (as_vec3(&u.value).map(|v| [v[0], v[1]]), None),
                PropTarget::Scale => (None, as_vec3(&u.value).map(|v| [v[0], v[1]])),
                _ => continue,
            };
            if origin.is_none() && scale.is_none() {
                continue;
            }
            for item in &mut self.items {
                if let SceneItem::Text(tg) = item
                    && tg.id == u.object_id
                {
                    tg.set_transform(&self.device, origin, scale);
                }
            }
        }
        if dirty.is_empty() {
            return;
        }
        let (sw, sh) = (self.proj_w, self.proj_h);
        for item in &mut self.items {
            let SceneItem::Image(o) = item else { continue };
            let mut affected = dirty.contains(&o.id);
            let mut cur = self.locals.get(&o.id).and_then(|l| l.parent);
            for _ in 0..64 {
                if affected {
                    break;
                }
                let Some(c) = cur else { break };
                affected = dirty.contains(&c);
                cur = self.locals.get(&c).and_then(|l| l.parent);
            }
            if !affected {
                continue;
            }
            let (origin, scale, angle_z) = world_xf(o.id, &self.locals);
            let quad = scene_space_quad(origin, o.image_size, scale, angle_z, (sw, sh));
            for pass in &o.passes {
                if pass.geometry != Geometry::Scene {
                    continue;
                }
                let mut verts = quad;
                if pass.uv_crop != [1.0, 1.0] {
                    apply_uv_crop(&mut verts, pass.uv_crop);
                }
                self.queue
                    .write_buffer(&pass.vertex_buffer, 0, bytemuck::cast_slice(&verts));
            }
            o.scene_center = [origin[0] - sw as f32 / 2.0, origin[1] - sh as f32 / 2.0];
            o.angle_z = angle_z;
        }
    }

    fn apply_scene_property(&mut self, name: &str, value: &kirie_script::ScriptValue) {
        use super::scripting::{as_f32, as_rgb};
        let as_bool = || match value {
            kirie_script::ScriptValue::Bool(b) => Some(*b),
            _ => as_f32(value).map(|f| f != 0.0),
        };
        let g = &mut self.general;
        match name {
            "bloom" => {
                if let Some(b) = as_bool() {
                    g.bloom.value = b;
                }
            }
            "bloomstrength" => {
                if let Some(f) = as_f32(value) {
                    g.bloomstrength.value = f;
                }
            }
            "bloomthreshold" => {
                if let Some(f) = as_f32(value) {
                    g.bloomthreshold.value = f;
                }
            }
            "camerafade" => {
                if let Some(b) = as_bool() {
                    g.camerafade.value = b;
                }
            }
            "camerashake" => {
                if let Some(b) = as_bool() {
                    g.camerashake.value = b;
                }
            }
            "camerashakespeed" => {
                if let Some(f) = as_f32(value) {
                    g.camerashakespeed.value = f;
                }
            }
            "camerashakeamplitude" => {
                if let Some(f) = as_f32(value) {
                    g.camerashakeamplitude.value = f;
                }
            }
            "camerashakeroughness" => {
                if let Some(f) = as_f32(value) {
                    g.camerashakeroughness.value = f;
                }
            }
            "cameraparallax" => {
                if let Some(b) = as_bool() {
                    g.cameraparallax.value = b;
                }
            }
            "cameraparallaxamount" => {
                if let Some(f) = as_f32(value) {
                    g.cameraparallaxamount.value = f;
                }
            }
            "cameraparallaxdelay" => {
                if let Some(f) = as_f32(value) {
                    g.cameraparallaxdelay.value = f;
                }
            }
            "cameraparallaxmouseinfluence" => {
                if let Some(f) = as_f32(value) {
                    g.cameraparallaxmouseinfluence.value = f;
                }
            }
            "clearcolor" => {
                if let Some(c) = as_rgb(value) {
                    g.clearcolor.value = [c[0], c[1], c[2], 1.0];
                    self.clear_color = wgpu::Color {
                        r: f64::from(c[0]),
                        g: f64::from(c[1]),
                        b: f64::from(c[2]),
                        a: 1.0,
                    };
                }
            }
            "ambientcolor" => {
                if let Some(c) = as_rgb(value) {
                    g.ambientcolor.value = [c[0], c[1], c[2], 1.0];
                    self.ambient = c;
                }
            }
            "skylightcolor" => {
                if let Some(c) = as_rgb(value) {
                    g.skylightcolor.value = [c[0], c[1], c[2], 1.0];
                    self.skylight = c;
                }
            }
            _ => return,
        }
        if matches!(name, "bloomstrength" | "bloomthreshold")
            && let Some(bloom) = &self.bloom
        {
            bloom.set_params(
                &self.queue,
                self.general.bloomstrength.value,
                self.general.bloomthreshold.value,
            );
        }
    }

    fn apply_animation_side_effects(
        &mut self,
        effect: Vec<(i64, usize, String, kirie_script::ScriptValue)>,
        particle: Vec<(i64, String, f32)>,
        zoom: Option<f32>,
        text_width: Vec<(i64, f32)>,
    ) {
        for (layer_id, effect_idx, name, value) in effect {
            apply_material_op(&mut self.items, layer_id, effect_idx, &name, &value);
        }
        for (id, name, v) in particle {
            for item in &mut self.items {
                if let SceneItem::Particle(pg) = item
                    && pg.id == id
                {
                    pg.sim.set_instance_scalar(&name, v);
                }
            }
        }
        if let Some(z) = zoom
            && z > 0.0
            && (z - self.zoom).abs() > f32::EPSILON
        {
            self.zoom = z;
            self.window_for = None;
        }
        if !text_width.is_empty()
            && let (Some(tp), Some(fonts)) = (self.text_pipeline.as_ref(), self.text_fonts.as_mut())
        {
            for (id, w) in text_width {
                for item in &mut self.items {
                    if let SceneItem::Text(tg) = item
                        && tg.id == id
                    {
                        tg.set_max_width(&self.device, &self.queue, tp, fonts, w);
                    }
                }
            }
        }
    }

    fn apply_script_scene_ops(&mut self) {
        let Some(script) = self.script.as_mut() else {
            return;
        };
        for (id, path) in script.take_created() {
            tracing::debug!(id, %path, "runtime layer created by script");
            let order = self.runtime_seq;
            let template = self.runtime_templates.get(&path).or_else(|| {
                let tail = path.rsplit('/').next().unwrap_or(path.as_str());
                self.runtime_templates
                    .iter()
                    .find(|(k, _)| k.rsplit('/').next() == Some(tail))
                    .map(|(_, v)| v)
            });
            let (texture, tint, tint_alpha, base_size) = template
                .map_or((None, [1.0; 3], 1.0, [1.0, 1.0]), |t| {
                    (t.texture.clone(), t.tint, t.alpha, t.size)
                });
            self.runtime_layers.entry(id).or_insert_with(|| RuntimeLayer {
                order,
                texture,
                tint,
                tint_alpha,
                base_size,
                ..RuntimeLayer::default()
            });
            self.runtime_seq += 1;
        }
        for (layer_id, cmd, value) in script.take_video_ops() {
            let Some(item_idx) = self.items.iter().position(|it| match it {
                SceneItem::Image(o) => o.id == layer_id,
                _ => false,
            }) else {
                continue;
            };
            for (vi, vt) in self.video_textures.iter().enumerate() {
                let uses = self.video_users.get(vi).is_some_and(|u| u.contains(&item_idx));
                if !uses {
                    continue;
                }
                match cmd.as_str() {
                    "play" => vt.script_paused.set(false),
                    "pause" | "stop" => vt.script_paused.set(true),
                    "rate" => vt.control.set_speed(value.max(0.0)),
                    _ => {}
                }
            }
        }
        for (layer_id, effect_idx, name, value) in script.take_material_ops() {
            apply_material_op(&mut self.items, layer_id, effect_idx, &name, &value);
        }
        let scene_ops = script.take_scene_ops();
        for op in script.take_particle_ops() {
            let target = match &op {
                ParticleOp::Command { id, .. }
                | ParticleOp::Emit { id, .. }
                | ParticleOp::Instance { id, .. } => *id,
            };
            for item in &mut self.items {
                let SceneItem::Particle(pg) = item else { continue };
                if pg.id != target {
                    continue;
                }
                match &op {
                    ParticleOp::Command { cmd, .. } => match cmd.as_str() {
                        "play" => pg.sim.play(),
                        "pause" => pg.sim.pause(),
                        "stop" => pg.sim.stop(),
                        _ => {}
                    },
                    ParticleOp::Emit { count, .. } => pg.sim.emit_burst(*count),
                    ParticleOp::Instance { name, value, .. } => {
                        if let Some(rest) = name.strip_prefix("controlpoint") {
                            if let (Ok(idx), Some(v)) = (rest.parse::<usize>(), as_vec3(value)) {
                                pg.sim.set_control_point(idx, [v[0], -v[1], v[2]]);
                            }
                        } else if name == "colorn" {
                            if let Some(v) = as_vec3(value) {
                                pg.sim.set_instance_colorn(v);
                            }
                        } else if let Some(v) = as_f32(value) {
                            pg.sim.set_instance_scalar(name, v);
                        }
                    }
                }
            }
        }
        for id in script.take_destroyed() {
            if self.runtime_layers.remove(&id).is_none() {
                self.visible_by_id.insert(id, false);
                for item in &mut self.items {
                    match item {
                        SceneItem::Image(o) if o.id == id => o.visible = false,
                        SceneItem::Text(t) if t.id == id => t.visible = false,
                        SceneItem::Particle(p) if p.id == id => p.visible = false,
                        SceneItem::Model(m) if m.id == id => m.visible = false,
                        _ => {}
                    }
                }
            }
        }
        if let Some(cam) = script.take_camera() {
            if let Some(e) = cam.eye {
                self.camera.eye = e;
            }
            if let Some(c) = cam.center {
                self.camera.center = c;
            }
            if let Some(u) = cam.up {
                self.camera.up = u;
            }
            if let Some(f) = cam.fov {
                self.camera.fov.value = f;
            }
            if let Some(z) = cam.zoom
                && z > 0.0
                && (z - self.zoom).abs() > f32::EPSILON
            {
                self.zoom = z;
                self.window_for = None;
            }
        }
        for (id, parent) in script.take_parent_updates() {
            self.parent_by_id.insert(id, Some(parent));
            for item in &mut self.items {
                if let SceneItem::Image(o) = item
                    && o.id == id
                {
                    o.parent = Some(parent);
                }
            }
        }
        if let Some(order) = script.take_layer_order() {
            let pos: HashMap<i64, usize> = order.iter().enumerate().map(|(i, &id)| (id, i)).collect();
            for (id, layer) in &mut self.runtime_layers {
                if let Some(&p) = pos.get(id) {
                    layer.order = p as i64;
                }
            }
            self.runtime_seq = order.len() as i64;
            self.items
                .sort_by_key(|it| pos.get(&item_id(it)).copied().unwrap_or(usize::MAX));
        }
        for (name, value) in scene_ops {
            self.apply_scene_property(&name, &value);
        }
    }
}

fn item_id(item: &SceneItem) -> i64 {
    match item {
        SceneItem::Image(o) => o.id,
        SceneItem::Particle(p) => p.id,
        SceneItem::Text(t) => t.id,
        SceneItem::Model(m) => m.id,
    }
}

type ParamCache = HashMap<String, Vec<(Arc<Vec<Parameter>>, Arc<Vec<Parameter>>)>>;

fn intern_params(
    cache: &mut ParamCache,
    shader: &str,
    vs: Vec<Parameter>,
    fs: Vec<Parameter>,
) -> (Arc<Vec<Parameter>>, Arc<Vec<Parameter>>) {
    let variants = cache.entry(shader.to_owned()).or_default();
    if let Some((v, f)) = variants.iter().find(|(v, f)| **v == vs && **f == fs) {
        return (Arc::clone(v), Arc::clone(f));
    }
    let entry = (Arc::new(vs), Arc::new(fs));
    variants.push(entry.clone());
    entry
}

#[allow(clippy::too_many_arguments)]
fn build_object(
    device: &wgpu::Device,
    object: &Object,
    image: &ImageObject,
    scene_size: (u32, u32),
    screen_mvp: &Mat4,
    source: &dyn AssetSource,
    resolver: &dyn IncludeResolver,
    registry: &TextureRegistry,
    fbo_sampler: &wgpu::Sampler,
    scene_snapshot: &Fbo,
    world: WorldXf,
    offscreen_donor: bool,
    cross: &std::collections::HashMap<String, wgpu::TextureView>,
    param_cache: &std::sync::Mutex<ParamCache>,
    fbo_scale: f32,
) -> Option<ObjectGpu> {
    let visible = offscreen_donor || (image.visible.value && object.base.visible.value);
    let color_blend = (image.color_blend_mode.value > 0)
        .then(|| source.load(plan::COLOR_BLEND_MATERIAL))
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|v| kirie_scene::material::Material::from_value(&v));
    let chain = plan::plan_image(image, true, offscreen_donor, color_blend.as_ref());
    if std::env::var_os("KIRIE_SLOT_DEBUG").is_some() {
        tracing::info!(
            id = object.base.id,
            passes = chain.passes.len(),
            shaders = ?chain.passes.iter().map(|p| p.pass.shader.clone()).collect::<Vec<_>>(),
            "plan"
        );
    }
    if chain.passes.is_empty() {
        return None;
    }
    let fullscreen = image.model.as_ref().is_some_and(|m| m.fullscreen);

    let puppet = if fullscreen {
        None
    } else {
        image
            .model
            .as_ref()
            .and_then(|m| m.puppet.as_ref())
            .and_then(|path| {
                let bytes = source.load(path)?;
                match kirie_formats::model::PuppetMesh::parse(&bytes) {
                    Ok(mesh) if !mesh.indices.is_empty() => {
                        tracing::debug!(
                            id = object.base.id,
                            %path,
                            verts = mesh.vertices.len(),
                            indices = mesh.indices.len(),
                            "loaded puppet mesh"
                        );
                        Some(mesh)
                    }
                    Ok(_) => None,
                    Err(e) => {
                        tracing::debug!(id = object.base.id, %path, error = %e, "puppet mesh parse failed; flat quad");
                        None
                    }
                }
            })
    };

    let pose = puppet.as_ref().map(|mesh| {
        let wanted = image
            .animationlayers
            .iter()
            .find(|layer| layer.visible.value)
            .and_then(|layer| u32::try_from(layer.animation.value).ok());
        let animation = wanted.and_then(|id| mesh.animation(id));
        mesh.pose(animation, 0.0)
    });

    let (mut iw, mut ih) = (image.size[0] as u32, image.size[1] as u32);
    let (world_origin, world_scale, world_angle_z) = world;
    let mut origin = world_origin;
    if iw == 0 || ih == 0 {
        iw = scene_size.0;
        ih = scene_size.1;
        origin = [scene_size.0 as f32 / 2.0, scene_size.1 as f32 / 2.0];
    }

    let layer_tex = base_layer_texture(image, source, registry);
    let layer_reads_scene = base_layer_name(image).as_deref().is_some_and(is_scene_rt);
    let mut reads_scene = layer_reads_scene;
    let layer_atlas = base_layer_name(image)
        .filter(|n| !n.starts_with("_rt_") && !n.starts_with("_alias_"))
        .and_then(|n| registry.atlas_for(&n));

    let scale = world_scale;
    let angle_z = world_angle_z;
    let scene_quad = scene_space_quad(origin, (iw, ih), [scale[0], scale[1]], angle_z, scene_size);
    let model_matrix = matrix::ortho(0.0, iw as f32, 0.0, ih as f32, 0.0, 1.0);

    struct Survivor {
        built: BuiltPass,
        raw: kirie_scene::material::Pass,
        params_vs: Arc<Vec<Parameter>>,
        params_fs: Arc<Vec<Parameter>>,
        target: Option<String>,
        binds: Vec<(u32, String)>,
        is_puppet_base: bool,
        effect_index: Option<usize>,
    }
    let mut built: Vec<Survivor> = Vec::new();
    for (ci, plan_pass) in chain.passes.iter().enumerate() {
        let (vs_src, fs_src) = if plan_pass.shader == plan::COPY_COMMAND_SHADER {
            (COPY_COMMAND_VERT.to_owned(), COPY_COMMAND_FRAG.to_owned())
        } else {
            let vs_name = format!("shaders/{}.vert", plan_pass.shader);
            let fs_name = format!("shaders/{}.frag", plan_pass.shader);
            let (Some(vs_bytes), Some(fs_bytes)) = (source.load(&vs_name), source.load(&fs_name)) else {
                tracing::debug!(shader = %plan_pass.shader, "missing shader source; pass skipped");
                continue;
            };
            let (Ok(vs_src), Ok(fs_src)) = (String::from_utf8(vs_bytes), String::from_utf8(fs_bytes)) else {
                continue;
            };
            (vs_src, fs_src)
        };
        let is_puppet_base = puppet.is_some() && ci == 0;
        let topology = if is_puppet_base {
            wgpu::PrimitiveTopology::TriangleList
        } else {
            wgpu::PrimitiveTopology::TriangleStrip
        };
        match pipeline::build_pass(
            device,
            FBO_FORMAT,
            effective_blending(is_puppet_base, plan_pass.blending),
            plan_pass.cull,
            kirie_scene::material::DepthMode::Disabled,
            kirie_scene::material::DepthMode::Disabled,
            topology,
            &plan_pass.pass,
            &vs_src,
            &fs_src,
            resolver,
        ) {
            Ok(mut b) => {
                let (params_vs, params_fs) = intern_params(
                    &mut param_cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    &plan_pass.shader,
                    std::mem::take(&mut b.vs_params),
                    std::mem::take(&mut b.fs_params),
                );
                let mut raw = plan_pass.pass.clone();
                raw.constantshadervalues
                    .retain(|k, _| params_vs.iter().chain(params_fs.iter()).any(|p| p.material == *k));
                built.push(Survivor {
                    built: b,
                    raw,
                    params_vs,
                    params_fs,
                    target: plan_pass.target.clone(),
                    binds: plan_pass.binds.clone(),
                    is_puppet_base,
                    effect_index: plan_pass.effect_index,
                });
            }
            Err(e) => {
                tracing::debug!(shader = %plan_pass.shader, error = %e, "pass shader failed to build; skipped");
            }
        }
    }
    if built.is_empty() {
        return None;
    }

    let n = built.len();
    let sdim = |d: u32| ((d as f32 * fbo_scale).round() as u32).max(1);
    let fbos = if n > 1 || offscreen_donor {
        [
            Some(Fbo::new(device, "kirie-image-fbo-a", sdim(iw), sdim(ih))),
            Some(Fbo::new(device, "kirie-image-fbo-b", sdim(iw), sdim(ih))),
        ]
    } else {
        [None, None]
    };

    let mut named_fbos: std::collections::HashMap<String, Fbo> = std::collections::HashMap::new();
    for decl in &chain.named_fbos {
        let s = if decl.scale > 0.0 { decl.scale } else { 1.0 };
        let w = ((iw as f32 / s).round() as u32).max(1);
        let h = ((ih as f32 / s).round() as u32).max(1);
        named_fbos.insert(
            decl.name.clone(),
            Fbo::new(device, "kirie-effect-fbo", sdim(w), sdim(h)),
        );
    }
    let comp_a = format!("_rt_imageLayerComposite_{}_a", object.base.id);
    let comp_b = format!("_rt_imageLayerComposite_{}_b", object.base.id);

    let is_composite = |target: &Option<String>| match target {
        None => true,
        Some(t) => !named_fbos.contains_key(t),
    };
    let last_comp = built
        .iter()
        .rposition(|s| is_composite(&s.target))
        .unwrap_or(n - 1);

    let mut passes = Vec::with_capacity(n);
    let mut comp_front: Option<usize> = None;
    for (i, sv) in built.into_iter().enumerate() {
        let Survivor {
            built: built_pass,
            raw: raw_pass,
            params_vs,
            params_fs,
            target,
            binds,
            is_puppet_base,
            effect_index,
        } = sv;
        let is_scene = i == last_comp && !offscreen_donor;
        let composite = is_composite(&target);

        let mut raw_pass = raw_pass;
        for (slot, name) in &binds {
            let idx = *slot as usize;
            if idx >= raw_pass.textures.len() {
                raw_pass.textures.resize(idx + 1, None);
            }
            if raw_pass.textures[idx].is_none() {
                raw_pass.textures[idx] = Some(name.clone());
            }
        }

        let geometry = if is_puppet_base {
            if is_scene {
                Geometry::Puppet
            } else {
                Geometry::PuppetCopy
            }
        } else if is_scene {
            if fullscreen {
                Geometry::Pass
            } else {
                Geometry::Scene
            }
        } else if i == 0 {
            if layer_reads_scene && is_compose_layer(&raw_pass.shader) {
                Geometry::SceneCopy
            } else {
                Geometry::Copy
            }
        } else {
            Geometry::Pass
        };

        let reads_layer = i == 0;
        let uv_crop = if reads_layer && !layer_reads_scene {
            layer_tex.uv_crop
        } else {
            [1.0, 1.0]
        };
        let (vertex_buffer, puppet_indices) = match geometry {
            Geometry::Puppet | Geometry::PuppetCopy => {
                let mesh = puppet.as_ref().expect("puppet base has a mesh");
                let verts = if matches!(geometry, Geometry::Puppet) {
                    puppet_scene_vertices(mesh, origin, scale, angle_z, scene_size, pose.as_deref())
                } else {
                    puppet_copy_vertices(mesh, (iw, ih), uv_crop, pose.as_deref())
                };
                (
                    create_buffer_init(
                        device,
                        "kirie-puppet-vb",
                        bytemuck::cast_slice(&verts),
                        wgpu::BufferUsages::VERTEX,
                    ),
                    Some(create_puppet_index_buffer(device, mesh)),
                )
            }
            _ => {
                let mut verts = match geometry {
                    Geometry::Scene | Geometry::SceneCopy => scene_quad,
                    _ => ndc_quad(1.0, 1.0),
                };
                if uv_crop != [1.0, 1.0] {
                    apply_uv_crop(&mut verts, uv_crop);
                }
                (
                    create_vertex_buffer(device, &verts, built_pass.uv_location.is_some()),
                    None,
                )
            }
        };
        let puppet_index_count = puppet
            .as_ref()
            .filter(|_| puppet_indices.is_some())
            .map_or(0, |m| m.indices.len() as u32);

        let vs_params = resolve_params(&params_vs, &raw_pass);
        let fs_params = resolve_params(&params_fs, &raw_pass);

        let (input_view, input_sampler): (&wgpu::TextureView, &wgpu::Sampler) = if reads_layer {
            if layer_reads_scene {
                (&scene_snapshot.view, fbo_sampler)
            } else {
                (&layer_tex.view, &layer_tex.sampler)
            }
        } else {
            match comp_front {
                Some(k) => (fbos[k].as_ref().map_or(&layer_tex.view, |f| &f.view), fbo_sampler),
                None => (&layer_tex.view, &layer_tex.sampler),
            }
        };

        let comp_view: &wgpu::TextureView = match comp_front {
            Some(k) => fbos[k].as_ref().map_or(input_view, |f| &f.view),
            None => input_view,
        };
        let mut named: std::collections::HashMap<&str, (&wgpu::TextureView, &wgpu::Sampler)> =
            std::collections::HashMap::new();
        for (name, view) in cross {
            named.insert(name.as_str(), (view, fbo_sampler));
        }
        named.insert("previous", (comp_view, fbo_sampler));
        named.insert(comp_a.as_str(), (comp_view, fbo_sampler));
        named.insert(comp_b.as_str(), (comp_view, fbo_sampler));
        for (name, fbo) in &named_fbos {
            named.insert(name.as_str(), (&fbo.view, fbo_sampler));
        }

        if raw_pass.textures.iter().flatten().any(|n| is_scene_rt(n))
            || samples_scene_by_default(&raw_pass, &built_pass.vs_samplers)
            || samples_scene_by_default(&raw_pass, &built_pass.fs_samplers)
        {
            reads_scene = true;
        }

        let vs_ubo =
            (!built_pass.vs_globals.is_empty()).then(|| create_ubo(device, built_pass.vs_globals.size));
        let fs_ubo =
            (!built_pass.fs_globals.is_empty()).then(|| create_ubo(device, built_pass.fs_globals.size));

        let g0_bind = build_bind_group(
            device,
            &built_pass.g0_layout,
            vs_ubo.as_ref(),
            &built_pass.g0_bindings,
            &built_pass.vs_samplers,
            input_view,
            input_sampler,
            registry,
            source,
            &raw_pass,
            (&scene_snapshot.view, fbo_sampler),
            &named,
            true,
        );
        let g1_bind = build_bind_group(
            device,
            &built_pass.g1_layout,
            fs_ubo.as_ref(),
            &built_pass.g1_bindings,
            &built_pass.fs_samplers,
            input_view,
            input_sampler,
            registry,
            source,
            &raw_pass,
            (&scene_snapshot.view, fbo_sampler),
            &named,
            true,
        );

        let img_res = [iw as f32, ih as f32, iw as f32, ih as f32];
        let mut tex_resolution = [img_res; 8];
        tex_resolution[0] = if reads_layer && !layer_reads_scene {
            tex_res(&layer_tex)
        } else {
            img_res
        };
        for (si, slot) in raw_pass.textures.iter().enumerate().take(8).skip(1) {
            let Some(name) = slot else { continue };
            tex_resolution[si] = if name.starts_with("_rt_")
                || name.starts_with("_alias_")
                || named.contains_key(name.as_str())
            {
                img_res
            } else {
                tex_res(&registry.get(name, source))
            };
        }

        let output = if is_scene {
            PassOutput::Scene
        } else if composite {
            let dst = match comp_front {
                Some(k) => 1 - k,
                None => 0,
            };
            comp_front = Some(dst);
            PassOutput::Fbo(dst)
        } else {
            PassOutput::Named(target.clone().unwrap_or_default())
        };

        let BuiltPass {
            pipeline,
            vs_globals,
            fs_globals,
            ..
        } = built_pass;

        passes.push(PassGpu {
            pipeline,
            g0_bind,
            g1_bind,
            vs_ubo,
            fs_ubo,
            vs_globals,
            fs_globals,
            vs_params,
            fs_params,
            vertex_buffer,
            puppet_indices,
            puppet_index_count,
            output,
            geometry,
            uv_crop,
            effect_index,
            model_matrix,
            blending: effective_blending(is_puppet_base, raw_pass.blending),
            tex_resolution,
            params_vs,
            params_fs,
            material_pass: raw_pass,
        });
    }
    let _ = screen_mvp;
    tracing::trace!(target: "kirie_render::ptrdbg",
        id = object.base.id,
        n_passes = passes.len(),
        geoms = ?passes.iter().map(|p| format!("{:?}", p.geometry)).collect::<Vec<_>>(),
        "object built");
    Some(ObjectGpu {
        id: object.base.id,
        parent: object.base.parent,
        passes,
        fbos,
        named_fbos,
        alpha: image.alpha.value,
        brightness: image.brightness.value,
        color: image.color.value,
        visible,
        reads_scene,
        offscreen_donor,
        final_front: comp_front,
        parallax_depth: image.parallax_depth.value,
        scene_center: [
            origin[0] - scene_size.0 as f32 / 2.0,
            origin[1] - scene_size.1 as f32 / 2.0,
        ],
        local_to_scene: local_to_scene(origin, (iw, ih), [scale[0], scale[1]], angle_z, scene_size),
        angle_z: world_angle_z,
        atlas: layer_atlas,
        image_size: (iw, ih),
    })
}

fn collect_visibility_bindings(scene: &kirie_scene::Scene) -> Vec<VisBinding> {
    scene
        .objects
        .iter()
        .filter_map(|o| {
            let image = match &o.kind {
                ObjectKind::Image(img) => Some(img.visible.clone()),
                _ => None,
            };
            let bound = o.base.visible.user.is_some() || image.as_ref().is_some_and(|us| us.user.is_some());
            bound.then(|| VisBinding {
                id: o.base.id,
                base: o.base.visible.clone(),
                image,
            })
        })
        .collect()
}

fn collect_effect_vis_bindings(scene: &kirie_scene::Scene) -> Vec<EffectVisBinding> {
    let mut out = Vec::new();
    for o in &scene.objects {
        if let ObjectKind::Image(img) = &o.kind {
            for eff in &img.effects {
                if eff.visible.user.is_some() {
                    out.push(EffectVisBinding {
                        us: eff.visible.clone(),
                        planned: eff.visible.value,
                    });
                }
            }
        }
    }
    out
}

fn collect_structural_props(scene: &kirie_scene::Scene) -> std::collections::HashSet<String> {
    use kirie_scene::user::UserSetting;
    let mut out = std::collections::HashSet::new();
    fn add<T>(out: &mut std::collections::HashSet<String>, us: &UserSetting<T>) {
        if let Some(user) = &us.user {
            out.insert(user.name().to_owned());
        }
    }
    add(&mut out, &scene.general.bloom);
    for o in &scene.objects {
        add(&mut out, &o.base.origin);
        add(&mut out, &o.base.scale);
        add(&mut out, &o.base.angles);
        match &o.kind {
            ObjectKind::Image(img) => {
                add(&mut out, &img.scale);
                add(&mut out, &img.angles);
                add(&mut out, &img.color_blend_mode);
                for layer in &img.animationlayers {
                    add(&mut out, &layer.rate);
                    add(&mut out, &layer.visible);
                    add(&mut out, &layer.blend);
                    add(&mut out, &layer.animation);
                }
            }
            ObjectKind::Particle(p) => {
                add(&mut out, &p.scale);
                add(&mut out, &p.angles);
                add(&mut out, &p.visible);
                let ov = &p.instanceoverride;
                add(&mut out, &ov.enabled);
                add(&mut out, &ov.alpha);
                add(&mut out, &ov.size);
                add(&mut out, &ov.lifetime);
                add(&mut out, &ov.rate);
                add(&mut out, &ov.speed);
                add(&mut out, &ov.count);
                add(&mut out, &ov.color);
                add(&mut out, &ov.colorn);
            }
            ObjectKind::Text(t) => {
                add(&mut out, &t.text);
                add(&mut out, &t.pointsize);
                add(&mut out, &t.scale);
                add(&mut out, &t.color);
                add(&mut out, &t.alpha);
                add(&mut out, &t.visible);
            }
            _ => {}
        }
    }
    out
}

fn ancestors_visible(
    parent_by_id: &HashMap<i64, Option<i64>>,
    visible_by_id: &HashMap<i64, bool>,
    start: Option<i64>,
) -> bool {
    let mut cur = start;
    for _ in 0..64 {
        let Some(id) = cur else { return true };
        if !visible_by_id.get(&id).copied().unwrap_or(true) {
            return false;
        }
        cur = parent_by_id.get(&id).copied().flatten();
    }
    true
}

impl Renderer for SceneRenderer {
    fn redraw_hint(&self) -> kirie_platform::RedrawHint {
        let animated = self.script.is_some()
            || self.audio.is_some()
            || !self.video_textures.is_empty()
            || !self.atlas_textures.is_empty()
            || !self.runtime_layers.is_empty()
            || (self.general.cameraparallax.value && !self.options.disable_parallax)
            || self.items.iter().any(|it| match it {
                SceneItem::Particle(_) => true,
                SceneItem::Model(m) => m.has_animation(),
                _ => false,
            });
        if animated {
            kirie_platform::RedrawHint::Unknown
        } else {
            kirie_platform::RedrawHint::Static
        }
    }

    fn render(&mut self, view: &wgpu::TextureView, size: SurfaceSize, dt: f32) {
        self.elapsed += f64::from(dt);
        let time = self.elapsed as f32;
        let texel = [1.0 / self.proj_w as f32, 1.0 / self.proj_h as f32];

        let spectrum = self.audio.as_ref().map(|a| a.latest_spectrum());

        let pointer_scene = {
            let (pw, ph) = self.projection_size();
            [self.pointer[0] * pw as f32, self.pointer[1] * ph as f32]
        };
        let media_state = self.media.as_ref().map(|m| m.latest());
        let anim = match self.animator.as_mut() {
            Some(a) => {
                let mut out = a.tick(dt, super::scripting::time_of_day_now(self.tz_offset_secs) as f32);
                if let Some(script) = self.script.as_mut() {
                    script.note_animation(&out.updates, &out.overrides);
                    script.note_animation_state(a.snapshot(), std::mem::take(&mut out.events));
                }
                out
            }
            None => AnimOutput::default(),
        };
        let mut updates = anim.updates;
        updates.extend(match &mut self.script {
            Some(script) => script.tick(
                dt,
                spectrum.as_deref(),
                self.pointer,
                pointer_scene,
                self.pointer_left,
                media_state.as_deref(),
            ),
            None => Vec::new(),
        });
        if let (Some(script), Some(a)) = (self.script.as_mut(), self.animator.as_mut()) {
            for (index, cmd, value) in script.take_anim_ops() {
                a.command(index as usize, &cmd, value as f32);
            }
        }
        self.apply_animation_side_effects(anim.effect, anim.particle, anim.zoom, anim.text_width);
        self.apply_script_scene_ops();
        if !updates.is_empty() {
            for u in &updates {
                if matches!(u.target, PropTarget::Visible)
                    && let kirie_script::ScriptValue::Bool(v) = &u.value
                {
                    self.visible_by_id.insert(u.object_id, *v);
                }
            }
            apply_runtime_updates(&mut self.runtime_layers, &updates);
            apply_script_updates(&mut self.items, &updates);
            self.apply_image_transforms(&updates);
            for u in &updates {
                let (color, alpha) = match u.target {
                    PropTarget::Color => (as_rgb(&u.value), None),
                    PropTarget::Alpha => (None, as_f32(&u.value)),
                    _ => continue,
                };
                for item in &mut self.items {
                    if let SceneItem::Text(tg) = item
                        && tg.id == u.object_id
                    {
                        tg.set_tint(&self.queue, color, alpha);
                    }
                }
            }
        }

        if let (Some(host), Some(tp), Some(fonts)) = (
            self.script.as_mut(),
            self.text_pipeline.as_ref(),
            self.text_fonts.as_mut(),
        ) {
            let elapsed = self.elapsed;
            for item in &mut self.items {
                if let SceneItem::Text(tg) = item
                    && let Some(handle) = tg.script.as_ref().and_then(|s| s.handle)
                    && let Some(new_text) = host.tick_text_layer(handle, elapsed, f64::from(dt))
                {
                    tg.retext(&self.device, &self.queue, tp, fonts, &new_text);
                }
            }
            for u in &updates {
                if u.target != PropTarget::Text {
                    continue;
                }
                let kirie_script::ScriptValue::Str(s) = &u.value else {
                    continue;
                };
                for item in &mut self.items {
                    if let SceneItem::Text(tg) = item
                        && tg.id == u.object_id
                        && tg.current_text() != s.as_str()
                    {
                        tg.retext(&self.device, &self.queue, tp, fonts, s);
                    }
                }
            }
        }

        if self.window_for != Some(size) {
            let mut window = self
                .options
                .scaling
                .uv_window((self.proj_w, self.proj_h), (size.width, size.height))
                .slid(crate::scaling::focus());
            if self.zoom != 1.0 {
                let (cx, cy) = ((window.u0 + window.u1) * 0.5, (window.v0 + window.v1) * 0.5);
                let (hw, hh) = (
                    (window.u1 - window.u0) * 0.5 / self.zoom,
                    (window.v1 - window.v0) * 0.5 / self.zoom,
                );
                window.u0 = cx - hw;
                window.u1 = cx + hw;
                window.v0 = cy - hh;
                window.v1 = cy + hh;
            }
            let clamp_mode = match self.options.clamp {
                ClampMode::Clamp => 0u32,
                ClampMode::Border => 1u32,
                ClampMode::Repeat => 2u32,
            };
            self.queue.write_buffer(
                &self.blit_window,
                0,
                bytemuck::bytes_of(&BlitWindow {
                    rect: [window.u0, window.v0, window.u1, window.v1],
                    clamp_mode,
                    srgb: u32::from(self.blit_srgb),
                    _pad: [0; 2],
                }),
            );
            self.window_for = Some(size);
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-scene-encoder"),
            });

        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-scene-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.scene_fbo.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let scene_view = &self.scene_fbo.view;
        let scene_tex = &self.scene_fbo.texture;
        let snap_tex = self.scene_snapshot.as_ref().map(|s| &s.texture);
        let copy_extent = wgpu::Extent3d {
            width: self.scene_fbo.width,
            height: self.scene_fbo.height,
            depth_or_array_layers: 1,
        };
        let audio = spectrum.as_deref();
        let parent_by_id = &self.parent_by_id;
        let visible_by_id = &self.visible_by_id;
        let pack_scratch = &mut self.pack_scratch;
        let pointer = self.pointer;
        let pointer_last = self.pointer_last;
        let parallax = if self.options.disable_parallax {
            ([0.0, 0.0], 0.0, self.proj_w as f32)
        } else {
            (
                self.parallax_disp,
                self.general.cameraparallaxamount.value,
                self.proj_w as f32,
            )
        };

        self.pointer_last = self.pointer;
        if self.general.cameraparallax.value && !self.options.disable_parallax {
            let amount = self.general.cameraparallaxamount.value;
            let influence = self.general.cameraparallaxmouseinfluence.value;
            let t = (self.general.cameraparallaxdelay.value * dt).clamp(0.0, 1.0);
            for axis in 0..2 {
                let target = (self.pointer[axis] - 0.5) * amount * influence;
                self.parallax_disp[axis] += (target - self.parallax_disp[axis]) * t;
            }
        }

        for (vi, vt) in self.video_textures.iter().enumerate() {
            let users = self.video_users.get(vi);
            let displayed = users.is_none_or(|u| u.is_empty())
                || users.is_some_and(|u| {
                    u.iter().any(|&i| match &self.items[i] {
                        SceneItem::Image(o) => o.visible,
                        _ => true,
                    })
                });
            let play = displayed && !vt.script_paused.get();
            if !play {
                if !vt.paused.get() {
                    vt.control.set_pause(true);
                    vt.paused.set(true);
                }
                continue;
            }
            if vt.paused.get() {
                vt.control.set_pause(false);
                vt.paused.set(false);
            }
            let mut newest = None;
            while let Some(f) = vt.player.recv_frame_timeout(std::time::Duration::ZERO) {
                newest = Some(f);
            }
            if let Some(f) = newest
                && (f.width, f.height) == vt.size
            {
                if f.pixels == kirie_video::FramePixels::Nv12 {
                    if let Some(rig) = &vt.nv12 {
                        rig.convert(&self.device, &self.queue, f.width, f.height, &f.data);
                    }
                    continue;
                }
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &vt.gpu.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &f.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * f.width),
                        rows_per_image: Some(f.height),
                    },
                    wgpu::Extent3d {
                        width: f.width,
                        height: f.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        for slot in &mut self.atlas_textures {
            let frame = slot.atlas.placement_at(self.elapsed);
            if frame.page == slot.uploaded_page {
                continue;
            }
            let Some(page) = slot.atlas.pages.get(frame.page) else {
                continue;
            };
            let gpu = &slot.atlas.gpu;
            if (page.width, page.height) != (gpu.width, gpu.height) {
                continue;
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &page.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * page.width),
                    rows_per_image: Some(page.height),
                },
                wgpu::Extent3d {
                    width: page.width,
                    height: page.height,
                    depth_or_array_layers: 1,
                },
            );
            slot.uploaded_page = frame.page;
        }

        for item in &mut self.items {
            if let SceneItem::Image(object) = item
                && object.offscreen_donor
                && !object.reads_scene
            {
                draw_image_object(
                    &mut encoder,
                    &self.queue,
                    object,
                    scene_view,
                    self.screen_mvp,
                    self.ambient,
                    self.skylight,
                    time,
                    self.elapsed,
                    texel,
                    audio,
                    pack_scratch,
                    pointer,
                    pointer_last,
                    parallax,
                );
            }
        }

        for item in &mut self.items {
            match item {
                SceneItem::Image(object) if object.offscreen_donor && !object.reads_scene => {}
                SceneItem::Image(object)
                    if !object.offscreen_donor
                        && (!object.visible
                            || !ancestors_visible(parent_by_id, visible_by_id, object.parent)) => {}
                SceneItem::Image(object) => {
                    if object.reads_scene
                        && let Some(snap_tex) = snap_tex
                    {
                        encoder.copy_texture_to_texture(
                            scene_tex.as_image_copy(),
                            snap_tex.as_image_copy(),
                            copy_extent,
                        );
                    }
                    draw_image_object(
                        &mut encoder,
                        &self.queue,
                        object,
                        scene_view,
                        self.screen_mvp,
                        self.ambient,
                        self.skylight,
                        time,
                        self.elapsed,
                        texel,
                        audio,
                        pack_scratch,
                        pointer,
                        pointer_last,
                        parallax,
                    );
                }
                SceneItem::Particle(pg)
                    if !pg.visible
                        || !ancestors_visible(
                            parent_by_id,
                            visible_by_id,
                            parent_by_id.get(&pg.id).copied().flatten(),
                        ) => {}
                SceneItem::Particle(pg) => {
                    if pg.sim.follows_pointer() {
                        pg.sim.set_pointer_local([
                            pointer_scene[0] - pg.origin[0],
                            pg.origin[1] - pointer_scene[1],
                            0.0,
                        ]);
                    }
                    pg.sim.update(dt);
                    pg.sim.write_sprites(&mut self.sprite_scratch);
                    let n = pg
                        .renderer
                        .upload(&self.queue, &pg.view_projection, &self.sprite_scratch);
                    pg.renderer.draw(&mut encoder, scene_view, n);
                }
                SceneItem::Text(tg)
                    if !tg.visible
                        || tg.blank
                        || !ancestors_visible(
                            parent_by_id,
                            visible_by_id,
                            parent_by_id.get(&tg.id).copied().flatten(),
                        ) => {}
                SceneItem::Text(tg) => {
                    if let Some(tp) = &self.text_pipeline {
                        extras::draw_text(&mut encoder, tp, tg, scene_view);
                    }
                }
                SceneItem::Model(mg)
                    if !mg.visible
                        || !ancestors_visible(
                            parent_by_id,
                            visible_by_id,
                            parent_by_id.get(&mg.id).copied().flatten(),
                        ) => {}
                SceneItem::Model(mg) => {
                    if mg.reads_scene
                        && let Some(snap_tex) = snap_tex
                    {
                        encoder.copy_texture_to_texture(
                            scene_tex.as_image_copy(),
                            snap_tex.as_image_copy(),
                            copy_extent,
                        );
                    }
                    if let Some(depth_view) = self.model_depth.as_ref() {
                        let aspect = if self.proj_h > 0 {
                            self.proj_w as f32 / self.proj_h as f32
                        } else {
                            16.0 / 9.0
                        };
                        super::model::draw_model(
                            &mut encoder,
                            &self.queue,
                            mg,
                            scene_view,
                            depth_view,
                            &self.camera,
                            aspect,
                            self.ambient,
                            self.skylight,
                            time,
                            texel,
                            audio,
                            pack_scratch,
                            pointer,
                            pointer_last,
                        );
                    }
                }
            }
        }

        if self.runtime_layers.values().any(|l| l.visible && l.alpha > 0.0) {
            let (sw, sh) = (self.proj_w as f32, self.proj_h as f32);
            let mut verts: Vec<f32> = Vec::with_capacity(self.runtime_layers.len() * 48);
            let mut batches: Vec<(std::sync::Arc<super::texture::GpuTexture>, u32, u32)> = Vec::new();
            for id in runtime_draw_order(&self.runtime_layers) {
                let l = &self.runtime_layers[&id];
                if !l.visible || l.alpha <= 0.0 {
                    continue;
                }
                if !ancestors_visible(
                    &self.parent_by_id,
                    &self.visible_by_id,
                    self.parent_by_id.get(&id).copied().flatten(),
                ) {
                    continue;
                }
                let cx = l.origin[0] - sw / 2.0;
                let cy = l.origin[1] - sh / 2.0;
                let (hw, hh) = (
                    l.scale[0] * l.base_size[0] / 2.0,
                    l.scale[1] * l.base_size[1] / 2.0,
                );
                let (sn, cs) = (-l.angles[2].to_radians()).sin_cos();
                let corner = |dx: f32, dy: f32| {
                    [
                        (cx + dx * cs - dy * sn) / (sw / 2.0),
                        (cy + dx * sn + dy * cs) / (sh / 2.0),
                    ]
                };
                let tl = corner(-hw, hh);
                let bl = corner(-hw, -hh);
                let tr = corner(hw, hh);
                let br = corner(hw, -hh);
                let (r, g, b, a) = (
                    l.color[0] * l.tint[0],
                    l.color[1] * l.tint[1],
                    l.color[2] * l.tint[2],
                    l.alpha * l.tint_alpha,
                );
                let tex = l.texture.clone().unwrap_or_else(|| self.runtime_white.clone());
                let first = (verts.len() / 8) as u32;
                for (v, uv) in [
                    (tl, [0.0, 0.0]),
                    (bl, [0.0, 1.0]),
                    (tr, [1.0, 0.0]),
                    (tr, [1.0, 0.0]),
                    (bl, [0.0, 1.0]),
                    (br, [1.0, 1.0]),
                ] {
                    verts.extend_from_slice(&[v[0], v[1], uv[0], uv[1], r, g, b, a]);
                }
                match batches.last_mut() {
                    Some((t, _, count)) if std::sync::Arc::ptr_eq(t, &tex) => *count += 6,
                    _ => batches.push((tex, first, 6)),
                }
            }
            if !verts.is_empty() {
                let needed = verts.len() * 4;
                let rebuild = match &self.runtime_pipeline {
                    Some((_, _, cap)) => *cap < needed,
                    None => true,
                };
                if rebuild {
                    let pipeline = self
                        .runtime_pipeline
                        .take()
                        .map(|(p, _, _)| p)
                        .unwrap_or_else(|| build_runtime_pipeline(&self.device));
                    let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("kirie-runtime-layer-verts"),
                        size: (needed.max(4096)) as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let cap = needed.max(4096);
                    self.runtime_pipeline = Some((pipeline, buf, cap));
                }
                if let Some((pipeline, buf, _)) = &self.runtime_pipeline {
                    self.queue.write_buffer(buf, 0, bytemuck::cast_slice(&verts));
                    let layout = pipeline.get_bind_group_layout(0);
                    let binds: Vec<wgpu::BindGroup> = batches
                        .iter()
                        .map(|(t, _, _)| {
                            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("kirie-runtime-layer-tex"),
                                layout: &layout,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource: wgpu::BindingResource::TextureView(&t.view),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu::BindingResource::Sampler(&t.sampler),
                                    },
                                ],
                            })
                        })
                        .collect();
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("kirie-runtime-layers"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: scene_view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_pipeline(pipeline);
                    rp.set_vertex_buffer(0, buf.slice(..));
                    for ((_, first, count), bind) in batches.iter().zip(&binds) {
                        rp.set_bind_group(0, bind, &[]);
                        rp.draw(*first..*first + *count, 0..1);
                    }
                }
            }
        }

        if let (Some(bloom), Some(snap)) = (&self.bloom, &self.scene_snapshot) {
            bloom.run(&mut encoder, &self.scene_fbo, snap);
        }

        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-scene-blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.blit_pipeline);
            rp.set_bind_group(0, &self.blit_bind, &[]);
            rp.draw(0..4, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
    }

    fn set_property(&mut self, key: &str, value: &str) -> kirie_platform::PropertyImpact {
        if !self.bag.set_from_str(key, value) {
            return kirie_platform::PropertyImpact::Live;
        }
        for item in &mut self.items {
            if let SceneItem::Image(o) = item {
                for pass in &mut o.passes {
                    kirie_scene::resolve::resolve_constants(
                        &mut pass.material_pass.constantshadervalues,
                        &self.bag,
                    );
                    pass.vs_params = resolve_params(&pass.params_vs, &pass.material_pass);
                    pass.fs_params = resolve_params(&pass.params_fs, &pass.material_pass);
                }
            }
        }
        self.camera.reresolve(&self.bag);
        if let Some(host) = self.script.as_mut() {
            host.set_scene_fov(self.camera.fov.value);
        }
        self.general.resolve(&self.bag);
        self.ambient = [
            self.general.ambientcolor.value[0],
            self.general.ambientcolor.value[1],
            self.general.ambientcolor.value[2],
        ];
        self.skylight = [
            self.general.skylightcolor.value[0],
            self.general.skylightcolor.value[1],
            self.general.skylightcolor.value[2],
        ];
        let clear = self.general.clearcolor.value;
        self.clear_color = wgpu::Color {
            r: f64::from(clear[0]),
            g: f64::from(clear[1]),
            b: f64::from(clear[2]),
            a: 1.0,
        };
        if let Some(bloom) = &self.bloom {
            bloom.set_params(
                &self.queue,
                self.general.bloomstrength.value,
                self.general.bloomthreshold.value,
            );
        }
        for vb in &mut self.visibility_bindings {
            kirie_scene::resolve::resolve_us(&mut vb.base, &self.bag);
            let mut vis = vb.base.value;
            if let Some(img) = &mut vb.image {
                kirie_scene::resolve::resolve_us(img, &self.bag);
                vis = vis && img.value;
            }
            self.visible_by_id.insert(vb.id, vis);
            for item in &mut self.items {
                if let SceneItem::Image(o) = item
                    && o.id == vb.id
                {
                    o.visible = vis;
                }
            }
        }
        let value = self.bag.get(key).cloned();
        let updates = match (self.script.as_mut(), value.as_ref()) {
            (Some(script), Some(v)) => script.apply_user_property(key, v),
            _ => Vec::new(),
        };
        for u in &updates {
            if matches!(u.target, PropTarget::Visible)
                && let kirie_script::ScriptValue::Bool(v) = &u.value
            {
                self.visible_by_id.insert(u.object_id, *v);
            }
        }
        self.apply_script_scene_ops();
        if !updates.is_empty() {
            apply_runtime_updates(&mut self.runtime_layers, &updates);
            apply_script_updates(&mut self.items, &updates);
            self.apply_image_transforms(&updates);
        }
        let mut effect_diverged = false;
        for eb in &mut self.effect_vis_bindings {
            kirie_scene::resolve::resolve_us(&mut eb.us, &self.bag);
            if eb.us.value != eb.planned {
                effect_diverged = true;
            }
        }
        if effect_diverged || self.structural_props.contains(key) {
            kirie_platform::PropertyImpact::NeedsRebuild
        } else {
            kirie_platform::PropertyImpact::Live
        }
    }

    fn set_pointer(&mut self, x: f32, y: f32) {
        self.pointer = [x, y];
    }

    fn set_pointer_buttons(&mut self, left_down: bool) {
        self.pointer_left = left_down;
    }
}

struct RuntimeTemplate {
    texture: Option<std::sync::Arc<super::texture::GpuTexture>>,
    tint: [f32; 3],
    alpha: f32,
    size: [f32; 2],
}

fn collect_runtime_templates(
    model: &SceneModel,
    source: &dyn AssetSource,
    user_props: &[(String, kirie_scene::PropertyValue)],
    registry: &super::texture::TextureRegistry,
) -> std::collections::HashMap<String, RuntimeTemplate> {
    use kirie_scene::user::UserSetting;

    let mut paths: std::collections::HashSet<(String, Option<String>)> = std::collections::HashSet::new();
    let mut scan = |script: &Option<kirie_scene::user::ScriptBinding>| {
        let Some(b) = script else { return };
        let wid = b
            .source
            .find("__workshopId")
            .and_then(|i| {
                let rest = &b.source[i..];
                let open = rest.find(['\'', '"'])?;
                let quote = rest.as_bytes()[open] as char;
                let close = rest[open + 1..].find(quote)?;
                Some(rest[open + 1..open + 1 + close].to_owned())
            })
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()));
        for (i, _) in b.source.match_indices("createLayer") {
            let rest = &b.source[i + "createLayer".len()..];
            let Some(open) = rest.find(['\'', '"']) else {
                continue;
            };
            let quote = rest.as_bytes()[open] as char;
            let Some(close) = rest[open + 1..].find(quote) else {
                continue;
            };
            let path = &rest[open + 1..open + 1 + close];
            if !path.is_empty() && path.len() < 256 {
                paths.insert((path.to_owned(), wid.clone()));
            }
        }
    };
    for object in &model.scene.objects {
        match &object.kind {
            kirie_scene::object::ObjectKind::Image(img) => {
                scan(&img.alpha.script);
                scan(&img.brightness.script);
                scan(&img.color.script);
                scan(&img.visible.script);
            }
            kirie_scene::object::ObjectKind::Text(txt) => {
                scan(&txt.text.script);
                scan(&txt.alpha.script);
                scan(&txt.color.script);
                scan(&txt.visible.script);
            }
            kirie_scene::object::ObjectKind::Particle(p) => {
                scan(&p.instanceoverride.rate.script);
            }
            _ => {}
        }
        let _ = &object.base;
    }

    let lookup = |name: &str| -> Option<&kirie_scene::PropertyValue> {
        user_props.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    };
    let resolve_color = |v: &serde_json::Value| -> Option<[f32; 3]> {
        let obj = v.as_object();
        if let Some(o) = obj
            && let Some(user) = o.get("user").and_then(|u| u.as_str())
            && let Some(kirie_scene::PropertyValue::Color(c)) = lookup(user)
        {
            return Some([c[0], c[1], c[2]]);
        }
        let s = obj
            .and_then(|o| o.get("value"))
            .and_then(|x| x.as_str())
            .or_else(|| v.as_str())?;
        let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
        Some([it.next()?, it.next()?, it.next()?])
    };
    let resolve_f32 = |v: &serde_json::Value| -> Option<f32> {
        let obj = v.as_object();
        if let Some(o) = obj
            && let Some(user) = o.get("user").and_then(|u| u.as_str())
            && let Some(kirie_scene::PropertyValue::Number(n)) = lookup(user)
        {
            return Some(*n as f32);
        }
        obj.and_then(|o| o.get("value"))
            .and_then(serde_json::Value::as_f64)
            .or_else(|| v.as_f64())
            .map(|f| f as f32)
    };

    let mut out = std::collections::HashMap::new();
    for (path, wid) in paths {
        let remapped = wid.as_ref().and_then(|w| {
            path.split_once('/')
                .map(|(kind, rest)| format!("{kind}/workshop/{w}/{rest}"))
        });
        let Some(model_bytes) = source
            .load(&path)
            .or_else(|| remapped.as_ref().and_then(|p| source.load(p)))
        else {
            tracing::debug!(%path, "createLayer model not found; layer stays a solid quad");
            continue;
        };
        let Ok(model_json) = serde_json::from_slice::<serde_json::Value>(&model_bytes) else {
            continue;
        };
        let Some(mat_path) = model_json.get("material").and_then(|m| m.as_str()) else {
            continue;
        };
        let mat_remap = wid.as_ref().and_then(|w| {
            mat_path
                .split_once('/')
                .map(|(kind, rest)| format!("{kind}/workshop/{w}/{rest}"))
        });
        let Some(mat_bytes) = source
            .load(mat_path)
            .or_else(|| mat_remap.as_ref().and_then(|p| source.load(p)))
        else {
            continue;
        };
        let Ok(mat) = serde_json::from_slice::<serde_json::Value>(&mat_bytes) else {
            continue;
        };
        let Some(pass) = mat.get("passes").and_then(|p| p.get(0)) else {
            continue;
        };
        let consts = pass.get("constantshadervalues");
        let tint = consts
            .and_then(|c| c.as_object())
            .and_then(|c| {
                c.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("color"))
                    .and_then(|(_, v)| resolve_color(v))
            })
            .unwrap_or([1.0, 1.0, 1.0]);
        let alpha = consts
            .and_then(|c| c.as_object())
            .and_then(|c| {
                c.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("alpha"))
                    .and_then(|(_, v)| resolve_f32(v))
            })
            .unwrap_or(1.0);
        let texture = pass
            .get("textures")
            .and_then(|t| t.get(0))
            .and_then(|t| t.as_str())
            .map(|name| registry.get(name, source));
        let size = texture.as_ref().map_or([1.0, 1.0], |t| t.real_size);
        tracing::info!(%path, ?tint, alpha, "runtime layer material resolved");
        out.insert(
            path,
            RuntimeTemplate {
                texture,
                tint,
                alpha,
                size,
            },
        );
    }
    let _: Option<&UserSetting<f32>> = None;
    out
}

fn build_runtime_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
    const SRC: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};
@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    o.color = color;
    return o;
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(t, s, i.uv) * i.color;
}
"#;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-runtime-layer-shader"),
        source: wgpu::ShaderSource::Wgsl(SRC.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-runtime-layer-pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 32,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 8,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 2,
                    },
                ],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: super::fbo::FBO_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn apply_material_op(
    items: &mut [SceneItem],
    layer_id: i64,
    effect_idx: usize,
    name: &str,
    value: &kirie_script::ScriptValue,
) {
    let dynv = match value {
        kirie_script::ScriptValue::Float(f) => DynamicValue::Float(*f),
        kirie_script::ScriptValue::Int(i) => DynamicValue::Int(*i),
        kirie_script::ScriptValue::Vec2(v) => DynamicValue::Vec(v.to_vec()),
        kirie_script::ScriptValue::Vec3(v) => DynamicValue::Vec(v.to_vec()),
        kirie_script::ScriptValue::Vec4(v) => DynamicValue::Vec(v.to_vec()),
        _ => return,
    };
    for item in items {
        let SceneItem::Image(o) = item else { continue };
        if o.id != layer_id {
            continue;
        }
        for pass in &mut o.passes {
            if pass.effect_index != Some(effect_idx) {
                continue;
            }
            pass.material_pass.constantshadervalues.insert(
                name.to_owned(),
                kirie_scene::user::UserSetting::literal(dynv.clone()),
            );
            pass.vs_params = resolve_params(&pass.params_vs, &pass.material_pass);
            pass.fs_params = resolve_params(&pass.params_fs, &pass.material_pass);
        }
    }
}

fn apply_script_updates(items: &mut [SceneItem], updates: &[PropUpdate]) {
    for u in updates {
        for item in items.iter_mut() {
            if let SceneItem::Particle(pg) = item {
                if pg.id == u.object_id {
                    match u.target {
                        PropTarget::ParticleRate => {
                            if let Some(rate) = as_f32(&u.value) {
                                pg.sim.set_rate_override(rate);
                            }
                        }
                        PropTarget::Visible => {
                            if let kirie_script::ScriptValue::Bool(v) = &u.value {
                                pg.visible = *v;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if let SceneItem::Text(tg) = item {
                if tg.id == u.object_id
                    && u.target == PropTarget::Visible
                    && let kirie_script::ScriptValue::Bool(v) = &u.value
                {
                    tg.visible = *v;
                }
                continue;
            }
            if let SceneItem::Model(mg) = item {
                if mg.id == u.object_id {
                    match u.target {
                        PropTarget::Visible => {
                            if let kirie_script::ScriptValue::Bool(v) = &u.value {
                                mg.visible = *v;
                            }
                        }
                        PropTarget::Origin => {
                            if let Some(v) = as_vec3(&u.value) {
                                mg.set_origin(v);
                            }
                        }
                        PropTarget::Scale => {
                            if let Some(v) = as_vec3(&u.value) {
                                mg.set_scale(v);
                            }
                        }
                        PropTarget::Angles => {
                            if let Some(v) = as_vec3(&u.value) {
                                mg.set_angles(v);
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            let SceneItem::Image(object) = item else { continue };
            if object.id != u.object_id {
                continue;
            }
            match u.target {
                PropTarget::Alpha => {
                    if let Some(a) = as_f32(&u.value) {
                        object.alpha = a;
                    }
                }
                PropTarget::Brightness => {
                    if let Some(b) = as_f32(&u.value) {
                        object.brightness = b;
                    }
                }
                PropTarget::Color => {
                    if let Some(c) = as_rgb(&u.value) {
                        object.color = [c[0], c[1], c[2], object.color[3]];
                    }
                }
                PropTarget::Visible => {
                    if let kirie_script::ScriptValue::Bool(v) = &u.value {
                        object.visible = *v;
                    }
                }
                PropTarget::ParallaxDepth => {
                    if let Some(v) = as_vec3(&u.value) {
                        object.parallax_depth = [v[0], v[1]];
                    }
                }
                PropTarget::Text
                | PropTarget::Origin
                | PropTarget::Scale
                | PropTarget::Angles
                | PropTarget::ParticleRate
                | PropTarget::Volume => {}
            }
        }
    }
}

fn runtime_draw_order(layers: &std::collections::HashMap<i64, RuntimeLayer>) -> Vec<i64> {
    let mut ids: Vec<i64> = layers.keys().copied().collect();
    ids.sort_by_key(|id| (layers[id].order, std::cmp::Reverse(*id)));
    ids
}

fn apply_runtime_updates(layers: &mut std::collections::HashMap<i64, RuntimeLayer>, updates: &[PropUpdate]) {
    use super::scripting::as_vec3;
    for u in updates {
        let Some(l) = layers.get_mut(&u.object_id) else {
            continue;
        };
        match u.target {
            PropTarget::Origin => {
                if let Some(v) = as_vec3(&u.value) {
                    l.origin = v;
                }
            }
            PropTarget::Scale => {
                if let Some(v) = as_vec3(&u.value) {
                    l.scale = v;
                }
            }
            PropTarget::Angles => {
                if let Some(v) = as_vec3(&u.value) {
                    l.angles = v;
                }
            }
            PropTarget::Color => {
                if let Some(c) = as_rgb(&u.value) {
                    l.color = c;
                }
            }
            PropTarget::Alpha => {
                if let Some(a) = as_f32(&u.value) {
                    l.alpha = a;
                }
            }
            PropTarget::Visible => {
                if let kirie_script::ScriptValue::Bool(v) = &u.value {
                    l.visible = *v;
                }
            }
            PropTarget::Brightness
            | PropTarget::Text
            | PropTarget::ParticleRate
            | PropTarget::ParallaxDepth
            | PropTarget::Volume => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_image_object(
    encoder: &mut wgpu::CommandEncoder,
    queue: &wgpu::Queue,
    object: &ObjectGpu,
    scene_view: &wgpu::TextureView,
    screen_mvp: Mat4,
    ambient: [f32; 3],
    skylight: [f32; 3],
    time: f32,
    elapsed: f64,
    texel: [f32; 2],
    audio: Option<&AudioSpectrum>,
    scratch: &mut Vec<u8>,
    pointer: [f32; 2],
    pointer_last: [f32; 2],
    parallax: ([f32; 2], f32, f32),
) {
    let (disp, amount, ref_size) = parallax;
    let px = (object.parallax_depth[0] + amount) * disp[0] * ref_size;
    let py = (object.parallax_depth[1] + amount) * disp[1] * ref_size;
    let parallax_mvp = if px != 0.0 || py != 0.0 {
        matrix::mul(&screen_mvp, &matrix::translation([px, py, 0.0]))
    } else {
        screen_mvp
    };
    let atlas_anim = object.atlas.as_ref().map(|a| {
        let f = a.placement_at(elapsed);
        (f.translation, f.axes)
    });
    for (pass_index, pass) in object.passes.iter().enumerate() {
        let (t0_translation, t0_rotation) = match (pass_index, &atlas_anim) {
            (0, Some((t, r))) => (*t, *r),
            _ => ([0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
        };
        let mvp = match pass.geometry {
            Geometry::Scene | Geometry::Puppet | Geometry::SceneCopy => parallax_mvp,
            Geometry::PuppetCopy => pass.model_matrix,
            Geometry::Copy | Geometry::Pass => matrix::IDENTITY,
        };
        let effect_mvp = match pass.geometry {
            Geometry::Copy | Geometry::Pass => matrix::mul(&parallax_mvp, &object.local_to_scene),
            _ => mvp,
        };
        let mvp_inverse = match pass.geometry {
            Geometry::Scene | Geometry::Puppet | Geometry::SceneCopy => {
                let rot = if object.angle_z != 0.0 {
                    let [cx, cy] = object.scene_center;
                    let t_neg = matrix::translation([-cx, -cy, 0.0]);
                    let r = matrix::rotation_z(-object.angle_z);
                    let t_pos = matrix::translation([cx, cy, 0.0]);
                    matrix::mul(&t_pos, &matrix::mul(&r, &t_neg))
                } else {
                    matrix::IDENTITY
                };
                Some(matrix::inverse(&matrix::mul(&parallax_mvp, &rot)))
            }
            Geometry::Copy => {
                let (tw, th) = (pass.tex_resolution[0][0], pass.tex_resolution[0][1]);
                (tw > 0.0 && th > 0.0).then(|| {
                    let mut m = matrix::IDENTITY;
                    m[0] = tw / 2.0;
                    m[5] = th / 2.0;
                    m[12] = tw / 2.0;
                    m[13] = th / 2.0;
                    m
                })
            }
            Geometry::Pass | Geometry::PuppetCopy => None,
        };
        let builtins = Builtins {
            time,
            daytime: 0.0,
            brightness: object.brightness,
            alpha: object.alpha,
            color: [
                object.color[0],
                object.color[1],
                object.color[2],
                object.color[3] * object.alpha,
            ],
            ambient,
            skylight,
            pointer,
            pointer_last,
            texel_size: texel,
            mvp,
            effect_mvp,
            mvp_inverse,
            model: pass.model_matrix,
            view_projection: matrix::IDENTITY,
            eye: [0.0, 0.0, 1000.0],
            texture0_translation: t0_translation,
            texture0_rotation: t0_rotation,
            texture_resolution: pass.tex_resolution,
            audio16: audio.map_or([0.0; 16], |a| a.audio16),
            audio32: audio.map_or([0.0; 32], |a| a.audio32),
            audio64: audio.map_or([0.0; 64], |a| a.audio64),
        };
        if let Some(ubo) = &pass.vs_ubo {
            pack_globals(scratch, &pass.vs_globals, &builtins, &pass.vs_params);
            queue.write_buffer(ubo, 0, scratch);
        }
        if let Some(ubo) = &pass.fs_ubo {
            pack_globals(scratch, &pass.fs_globals, &builtins, &pass.fs_params);
            queue.write_buffer(ubo, 0, scratch);
        }

        let (target_view, load) = match &pass.output {
            PassOutput::Scene => (scene_view, wgpu::LoadOp::Load),
            PassOutput::Fbo(i) => (
                object.fbos[*i].as_ref().map_or(scene_view, |f| &f.view),
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            ),
            PassOutput::Named(name) => (
                object.named_fbos.get(name).map_or(scene_view, |f| &f.view),
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            ),
        };
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kirie-scene-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(&pass.pipeline);
        rp.set_bind_group(0, &pass.g0_bind, &[]);
        rp.set_bind_group(1, &pass.g1_bind, &[]);
        rp.set_vertex_buffer(0, pass.vertex_buffer.slice(..));
        if let Some(indices) = &pass.puppet_indices {
            rp.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint16);
            rp.draw_indexed(0..pass.puppet_index_count, 0, 0..1);
        } else {
            rp.draw(0..4, 0..1);
        }
        let _ = pass.blending;
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitWindow {
    rect: [f32; 4],
    clamp_mode: u32,
    srgb: u32,
    _pad: [u32; 2],
}

fn screen_camera_mvp(proj: (u32, u32), eye: [f32; 3], center: [f32; 3], up: [f32; 3], farz: f32) -> Mat4 {
    let far = farz.max(1000.0);
    let ortho = matrix::ortho(
        -(proj.0 as f32) / 2.0,
        proj.0 as f32 / 2.0,
        -(proj.1 as f32) / 2.0,
        proj.1 as f32 / 2.0,
        0.0,
        far,
    );
    let proj_eye = matrix::translate(&ortho, eye);
    let look = matrix::look_at(eye, center, up);
    let reference = matrix::mul(&proj_eye, &look);
    let flip = matrix::scale([1.0, -1.0, 1.0]);
    matrix::mul(&flip, &matrix::mul(&reference, &flip))
}

fn projection_size(model: &SceneModel, output: (u32, u32)) -> (u32, u32) {
    match model.scene.camera.projection {
        Projection::Orthogonal { width, height } if width > 0 && height > 0 => (width as u32, height as u32),
        _ => auto_projection(model, output),
    }
}

fn auto_projection(model: &SceneModel, output: (u32, u32)) -> (u32, u32) {
    let mut ext_w = 0.0f32;
    let mut ext_h = 0.0f32;
    for object in &model.scene.objects {
        if let ObjectKind::Image(img) = &object.kind {
            let ox = object.base.origin.value[0].abs();
            let oy = object.base.origin.value[1].abs();
            ext_w = ext_w.max(ox + img.size[0] / 2.0);
            ext_h = ext_h.max(oy + img.size[1] / 2.0);
        }
    }
    let w = (ext_w * 2.0).round() as u32;
    let h = (ext_h * 2.0).round() as u32;
    if w > 0 && h > 0 {
        (w, h)
    } else if output.0 > 0 && output.1 > 0 {
        output
    } else {
        (1920, 1080)
    }
}

fn base_layer_name(image: &ImageObject) -> Option<String> {
    image
        .material
        .as_ref()
        .and_then(|m| m.passes.first())
        .and_then(|p| p.textures.first())
        .and_then(|slot| slot.clone())
}

fn samples_scene_by_default(pass: &kirie_scene::material::Pass, samplers: &[SamplerSlot]) -> bool {
    samplers.iter().any(|slot| {
        let bound = slot
            .slot
            .and_then(|i| pass.textures.get(i as usize))
            .and_then(Clone::clone);
        match bound {
            Some(name) => is_scene_rt(&name),
            None => slot.default_texture.as_deref().is_some_and(is_scene_rt),
        }
    })
}

pub(super) fn is_compose_layer(shader: &str) -> bool {
    shader
        .rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("composelayer"))
}

pub(super) fn is_scene_rt(name: &str) -> bool {
    name == "_rt_FullFrameBuffer" || name == "_rt_MipMappedFrameBuffer"
}

fn base_layer_texture(
    image: &ImageObject,
    source: &dyn AssetSource,
    registry: &TextureRegistry,
) -> std::sync::Arc<super::texture::GpuTexture> {
    match base_layer_name(image) {
        Some(n) if !n.starts_with("_rt_") && !n.starts_with("_alias_") => registry.get(&n, source),
        _ => registry.white(),
    }
}

fn ndc_quad(ucrop: f32, vcrop: f32) -> [[f32; 5]; 4] {
    [
        [-1.0, 1.0, 0.0, 0.0, 0.0],
        [-1.0, -1.0, 0.0, 0.0, vcrop],
        [1.0, 1.0, 0.0, ucrop, 0.0],
        [1.0, -1.0, 0.0, ucrop, vcrop],
    ]
}

#[derive(Clone, Copy)]
struct LocalXf {
    origin: [f32; 2],
    scale: [f32; 2],
    angle_z: f32,
    parent: Option<i64>,
}

type WorldXf = ([f32; 2], [f32; 2], f32);

fn world_xf(id: i64, locals: &HashMap<i64, LocalXf>) -> WorldXf {
    let mut chain: Vec<LocalXf> = Vec::new();
    let mut cur = Some(id);
    for _ in 0..64 {
        let Some(c) = cur else { break };
        let Some(l) = locals.get(&c) else { break };
        chain.push(*l);
        cur = l.parent;
    }
    let (mut ox, mut oy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (1.0f32, 1.0f32);
    let mut ang = 0.0f32;
    for l in chain.iter().rev() {
        let (lx, ly) = (l.origin[0] * sx, l.origin[1] * sy);
        let (s, c) = ang.sin_cos();
        ox += lx * c - ly * s;
        oy += lx * s + ly * c;
        sx *= l.scale[0];
        sy *= l.scale[1];
        ang += l.angle_z;
    }
    ([ox, oy], [sx, sy], ang)
}

fn say_once(name: &str, shader: &str) {
    static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SAID.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut seen) = seen.lock()
        && seen.insert(name.to_owned())
    {
        tracing::warn!(texture = name, shader, "no such render target; drawing white");
    }
}

fn named_or_instance<'a>(
    named: &std::collections::HashMap<&str, (&'a wgpu::TextureView, &'a wgpu::Sampler)>,
    name: &str,
) -> Option<(&'a wgpu::TextureView, &'a wgpu::Sampler)> {
    if let Some(hit) = named.get(name) {
        return Some(*hit);
    }
    named
        .iter()
        .filter(|(key, _)| key.starts_with("_rt_") && name.starts_with(**key) && name.len() > key.len())
        .filter(|(key, _)| name.as_bytes().get(key.len()) == Some(&b'_'))
        .max_by_key(|(key, _)| key.len())
        .map(|(_, hit)| *hit)
}

fn local_to_scene(
    origin: [f32; 2],
    size: (u32, u32),
    scale: [f32; 2],
    angle_z: f32,
    scene: (u32, u32),
) -> Mat4 {
    let center = matrix::translation([
        origin[0] - scene.0 as f32 / 2.0,
        origin[1] - scene.1 as f32 / 2.0,
        0.0,
    ]);
    let half = matrix::scale([
        size.0 as f32 / 2.0 * scale[0],
        size.1 as f32 / 2.0 * scale[1],
        1.0,
    ]);
    matrix::mul(&center, &matrix::mul(&matrix::rotation_z(-angle_z), &half))
}

fn scene_space_quad(
    origin: [f32; 2],
    size: (u32, u32),
    scale: [f32; 2],
    angle_z: f32,
    scene: (u32, u32),
) -> [[f32; 5]; 4] {
    let (sw, sh) = (scene.0 as f32, scene.1 as f32);
    let hw = size.0 as f32 / 2.0 * scale[0];
    let hh = size.1 as f32 / 2.0 * scale[1];
    let cx = origin[0] - sw / 2.0;
    let cy = origin[1] - sh / 2.0;
    let (s, c) = (-angle_z).sin_cos();
    let corner = |dx: f32, dy: f32| [cx + dx * c - dy * s, cy + dx * s + dy * c, 0.0];
    let tl = corner(-hw, hh);
    let bl = corner(-hw, -hh);
    let tr = corner(hw, hh);
    let br = corner(hw, -hh);
    [
        [tl[0], tl[1], 0.0, 0.0, 0.0],
        [bl[0], bl[1], 0.0, 0.0, 1.0],
        [tr[0], tr[1], 0.0, 1.0, 0.0],
        [br[0], br[1], 0.0, 1.0, 1.0],
    ]
}

fn effective_blending(
    is_puppet_base: bool,
    planned: kirie_scene::material::Blending,
) -> kirie_scene::material::Blending {
    if is_puppet_base {
        kirie_scene::material::Blending::Translucent
    } else {
        planned
    }
}

fn skinned_position(vertex: &kirie_formats::model::PuppetVertex, pose: Option<&[[f32; 16]]>) -> [f32; 3] {
    let Some(pose) = pose.filter(|matrices| !matrices.is_empty()) else {
        return vertex.position;
    };
    let mut out = [0.0_f32; 3];
    let mut total = 0.0;
    for (index, weight) in vertex.bone_indices.iter().zip(vertex.bone_weights.iter()) {
        if *weight <= 0.0 {
            continue;
        }
        let Some(matrix) = usize::try_from(*index).ok().and_then(|at| pose.get(at)) else {
            continue;
        };
        let moved = kirie_formats::model::puppet_skin_point(vertex.position, *matrix);
        for (slot, value) in out.iter_mut().zip(moved.iter()) {
            *slot += value * weight;
        }
        total += weight;
    }
    if total <= 0.0 {
        return vertex.position;
    }
    for slot in &mut out {
        *slot /= total;
    }
    out
}

fn puppet_copy_vertices(
    mesh: &kirie_formats::model::PuppetMesh,
    size: (u32, u32),
    _uv_crop: [f32; 2],
    pose: Option<&[[f32; 16]]>,
) -> Vec<f32> {
    let (hw, hh) = (size.0 as f32 / 2.0, size.1 as f32 / 2.0);
    let mut out = Vec::with_capacity(mesh.vertices.len() * 5);
    for v in &mesh.vertices {
        let p = skinned_position(v, pose);
        out.extend_from_slice(&[hw + p[0], hh + p[1], 0.0, v.uv[0], v.uv[1]]);
    }
    out
}

fn puppet_scene_vertices(
    mesh: &kirie_formats::model::PuppetMesh,
    origin: [f32; 2],
    scale: [f32; 2],
    angle_z: f32,
    scene: (u32, u32),
    pose: Option<&[[f32; 16]]>,
) -> Vec<f32> {
    let (sw, sh) = (scene.0 as f32, scene.1 as f32);
    let cx = origin[0] - sw / 2.0;
    let cy = origin[1] - sh / 2.0;
    let (s, c) = (-angle_z).sin_cos();
    let mut out = Vec::with_capacity(mesh.vertices.len() * 5);
    for v in &mesh.vertices {
        let p = skinned_position(v, pose);
        let dx = p[0] * scale[0];
        let dy = p[1] * scale[1];
        out.extend_from_slice(&[cx + dx * c - dy * s, cy + dx * s + dy * c, 0.0, v.uv[0], v.uv[1]]);
    }
    out
}

fn create_puppet_index_buffer(
    device: &wgpu::Device,
    mesh: &kirie_formats::model::PuppetMesh,
) -> wgpu::Buffer {
    let mut indices = mesh.indices.clone();
    if !indices.len().is_multiple_of(2) {
        indices.push(0);
    }
    create_buffer_init(
        device,
        "kirie-puppet-ib",
        bytemuck::cast_slice(&indices),
        wgpu::BufferUsages::INDEX,
    )
}

pub(super) fn tex_res(t: &super::texture::GpuTexture) -> [f32; 4] {
    [t.width as f32, t.height as f32, t.real_size[0], t.real_size[1]]
}

fn apply_uv_crop(verts: &mut [[f32; 5]; 4], crop: [f32; 2]) {
    for v in verts.iter_mut() {
        v[3] *= crop[0];
        v[4] *= crop[1];
    }
}

pub(super) fn create_vertex_buffer(
    device: &wgpu::Device,
    verts: &[[f32; 5]; 4],
    _with_uv: bool,
) -> wgpu::Buffer {
    let mut bytes = Vec::with_capacity(4 * 20);
    for v in verts {
        for f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    create_buffer_init(device, "kirie-scene-vb", &bytes, wgpu::BufferUsages::VERTEX)
}

pub(super) fn create_ubo(device: &wgpu::Device, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kirie-scene-ubo"),
        size: size.max(16) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(super) fn create_buffer_init(
    device: &wgpu::Device,
    label: &str,
    data: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: data.len().max(4) as u64,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    {
        let mut view = buffer
            .slice(..)
            .get_mapped_range_mut()
            .expect("freshly mapped buffer");
        view.slice(..data.len()).copy_from_slice(data);
    }
    buffer.unmap();
    buffer
}

pub(super) fn resolve_params(
    params: &[Parameter],
    pass: &kirie_scene::material::Pass,
) -> BTreeMap<String, Vec<f32>> {
    let mut out = BTreeMap::new();
    for p in params {
        let given = pass.constantshadervalues.get(&p.material).or_else(|| {
            pass.constantshadervalues
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&p.material))
                .map(|(_, value)| value)
        });
        let value = given
            .map(|us| dynamic_components(&us.value, p))
            .or_else(|| p.default.as_ref().map(default_components));
        if let Some(v) = value {
            out.insert(p.name.clone(), v);
        }
    }
    out
}

fn dynamic_components(dv: &DynamicValue, p: &Parameter) -> Vec<f32> {
    match dv {
        DynamicValue::Vec(v) => v.clone(),
        DynamicValue::Color(c) => c.to_vec(),
        _ => vec![dv.as_f32()],
    }
    .into_iter()
    .chain(std::iter::repeat(0.0))
    .take(param_len(p))
    .collect()
}

fn default_components(d: &ParamDefault) -> Vec<f32> {
    match d {
        ParamDefault::Scalar(s) => vec![*s as f32],
        ParamDefault::Vector(v) => v.clone(),
    }
}

fn param_len(p: &Parameter) -> usize {
    use kirie_shader::reflect::ParamType;
    match p.ty {
        ParamType::Float | ParamType::Int => 1,
        ParamType::Vec2 => 2,
        ParamType::Vec3 => 3,
        ParamType::Vec4 => 4,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    ubo: Option<&wgpu::Buffer>,
    bindings: &[ModuleBinding],
    samplers: &[SamplerSlot],
    input_view: &wgpu::TextureView,
    input_sampler: &wgpu::Sampler,
    registry: &TextureRegistry,
    source: &dyn AssetSource,
    pass: &kirie_scene::material::Pass,
    scene: (&wgpu::TextureView, &wgpu::Sampler),
    named: &std::collections::HashMap<&str, (&wgpu::TextureView, &wgpu::Sampler)>,
    slot_zero_is_the_layer: bool,
) -> wgpu::BindGroup {
    enum Slot<'a> {
        Input,
        Scene,
        Named((&'a wgpu::TextureView, &'a wgpu::Sampler)),
        Tex(std::sync::Arc<super::texture::GpuTexture>),
    }
    let resolved: Vec<Slot> = samplers
        .iter()
        .map(|slot| {
            let name = slot
                .slot
                .and_then(|i| pass.textures.get(i as usize))
                .and_then(|s| s.clone())
                .or_else(|| slot.default_texture.clone());
            let loud = std::env::var_os("KIRIE_SLOT_DEBUG").is_some();
            if loud {
                tracing::info!(shader = %pass.shader, slot = ?slot.slot, sampler = %slot.name, name = ?name, "slot");
            }
            if let Some(hit) = name.as_deref().and_then(|n| named_or_instance(named, n)) {
                return Slot::Named(hit);
            }
            if name.as_deref().is_some_and(is_scene_rt) {
                return Slot::Scene;
            }
            if slot.slot == Some(0) && (slot_zero_is_the_layer || name.is_none()) {
                return Slot::Input;
            }
            match name {
                Some(n) if !n.starts_with("_rt_") && !n.starts_with("_alias_") => {
                    Slot::Tex(if slot_zero_is_the_layer {
                        registry.get(&n, source)
                    } else {
                        registry.get_wrapping(&n, source)
                    })
                }
                other => {
                    if let Some(name) = other {
                        say_once(&name, &pass.shader);
                    }
                    Slot::Tex(registry.white())
                }
            }
        })
        .collect();
    let white = registry.white();

    let mut entries = Vec::with_capacity(bindings.len());
    for mb in bindings {
        let resource = match mb.kind {
            BindKind::Ubo => match ubo {
                Some(u) => u.as_entire_binding(),
                None => continue,
            },
            BindKind::Texture => {
                let view = samplers
                    .iter()
                    .position(|s| s.texture_binding == mb.binding)
                    .map_or(&white.view, |i| match &resolved[i] {
                        Slot::Tex(t) => &t.view,
                        Slot::Scene => scene.0,
                        Slot::Named((v, _)) => v,
                        Slot::Input => input_view,
                    });
                wgpu::BindingResource::TextureView(view)
            }
            BindKind::Sampler => {
                let samp = samplers
                    .iter()
                    .position(|s| s.sampler_binding == mb.binding)
                    .map_or(&white.sampler, |i| match &resolved[i] {
                        Slot::Tex(t) => &t.sampler,
                        Slot::Scene => scene.1,
                        Slot::Named((_, s)) => s,
                        Slot::Input => input_sampler,
                    });
                wgpu::BindingResource::Sampler(samp)
            }
        };
        entries.push(wgpu::BindGroupEntry {
            binding: mb.binding,
            resource,
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kirie-scene-bg"),
        layout,
        entries: &entries,
    })
}

fn build_blit(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    scene_fbo: &Fbo,
    sampler: &wgpu::Sampler,
) -> (wgpu::RenderPipeline, wgpu::BindGroup, wgpu::Buffer) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kirie-scene-blit-shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
    });
    let window = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kirie-scene-blit-window"),
        size: std::mem::size_of::<BlitWindow>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kirie-scene-blit-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kirie-scene-blit-bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: window.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&scene_fbo.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kirie-scene-blit-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kirie-scene-blit-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bind, window)
}

const BLIT_WGSL: &str = r#"
struct Window { rect: vec4<f32>, clamp_mode: u32, srgb: u32, _p1: u32, _p2: u32 }
@group(0) @binding(0) var<uniform> win: Window;
@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // TL, BL, TR, BR — matches UvWindow::strip_corners ordering.
    var xs = array<f32, 4>(-1.0, -1.0, 1.0, 1.0);
    var ys = array<f32, 4>(1.0, -1.0, 1.0, -1.0);
    var us = array<f32, 4>(0.0, 0.0, 1.0, 1.0);
    var vs = array<f32, 4>(0.0, 1.0, 0.0, 1.0);
    var o: VsOut;
    o.pos = vec4<f32>(xs[i], ys[i], 0.0, 1.0);
    let u = mix(win.rect.x, win.rect.z, us[i]);
    let v = mix(win.rect.y, win.rect.w, vs[i]);
    o.uv = vec2<f32>(u, v);
    return o;
}

// Linear→sRGB inverse (sRGB decode), per channel. Applied before store when the
// surface is sRGB so wgpu's automatic linear→sRGB encode cancels it, writing the
// raw scene-FBO bytes to the surface — the reference's gamma-naive blit.
fn srgb_decode(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c <= vec3<f32>(0.04045);
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, cutoff);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var uv = in.uv;
    if (win.clamp_mode == 2u) {
        uv = fract(uv);
    } else if (win.clamp_mode == 1u) {
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
    } else {
        uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    }
    var c = textureSample(scene, samp, uv);
    if (win.srgb == 1u) {
        c = vec4<f32>(srgb_decode(c.rgb), c.a);
    }
    return c;
}
"#;

const COPY_COMMAND_VERT: &str = "\
attribute vec3 a_Position;\n\
attribute vec2 a_TexCoord;\n\
varying vec2 v_TexCoord;\n\
void main() {\n\
gl_Position = vec4(a_Position, 1.0);\n\
v_TexCoord = a_TexCoord;\n\
}\n";

const COPY_COMMAND_FRAG: &str = "\
uniform sampler2D g_Texture0;\n\
varying vec2 v_TexCoord;\n\
void main() {\n\
gl_FragColor = texSample2D(g_Texture0, v_TexCoord);\n\
}\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Mat4, p: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for row in 0..4 {
            for k in 0..4 {
                out[row] += m[k * 4 + row] * p[k];
            }
        }
        out
    }

    fn sampler(slot: u32, default_texture: Option<&str>) -> SamplerSlot {
        SamplerSlot {
            name: format!("g_Texture{slot}"),
            slot: Some(slot),
            texture_binding: 0,
            sampler_binding: 0,
            default_texture: default_texture.map(str::to_owned),
            combo: None,
        }
    }

    fn empty_pass() -> kirie_scene::material::Pass {
        kirie_scene::material::Pass {
            blending: kirie_scene::material::Blending::Normal,
            cullmode: kirie_scene::material::CullMode::NoCull,
            depthtest: kirie_scene::material::DepthMode::Disabled,
            depthwrite: kirie_scene::material::DepthMode::Disabled,
            shader: "genericimage3".to_owned(),
            textures: vec![],
            usertextures: vec![],
            combos: Default::default(),
            constantshadervalues: Default::default(),
        }
    }

    #[test]
    fn a_bloom_toggle_counts_as_structural() {
        let Ok(scene) = kirie_scene::Scene::from_value(&serde_json::json!({
            "camera": { "center": "0 0 0", "eye": "0 0 100", "up": "0 1 0" },
            "general": { "bloom": { "user": "bloom", "value": false } },
            "objects": []
        })) else {
            panic!("the scene should parse");
        };
        assert!(collect_structural_props(&scene).contains("bloom"));
    }

    #[test]
    fn a_shader_that_defaults_to_the_frame_buffer_still_reads_the_scene() {
        let samplers = [sampler(0, None), sampler(4, Some("_rt_FullFrameBuffer"))];
        assert!(samples_scene_by_default(&empty_pass(), &samplers));
    }

    #[test]
    fn a_bound_texture_wins_over_the_shaders_default() {
        let mut pass = empty_pass();
        pass.textures = vec![None, None, None, None, Some("materials/leaves".to_owned())];
        let samplers = [sampler(4, Some("_rt_FullFrameBuffer"))];
        assert!(!samples_scene_by_default(&pass, &samplers));
    }

    #[test]
    fn an_ordinary_shader_leaves_the_snapshot_alone() {
        let samplers = [sampler(0, None), sampler(1, Some("materials/mask"))];
        assert!(!samples_scene_by_default(&empty_pass(), &samplers));
    }

    #[test]
    fn the_effect_quad_lands_on_the_layer_quad() {
        let (origin, size, scale, angle, scene) =
            ([1263.985, 1976.601], (512, 512), [5.7, 5.7], 0.35, (2560, 1600));
        let m = local_to_scene(origin, size, scale, angle, scene);
        let quad = scene_space_quad(origin, size, scale, angle, scene);
        for (ndc, expected) in [
            ([-1.0, 1.0], quad[0]),
            ([-1.0, -1.0], quad[1]),
            ([1.0, 1.0], quad[2]),
            ([1.0, -1.0], quad[3]),
        ] {
            let x = m[0] * ndc[0] + m[4] * ndc[1] + m[12];
            let y = m[1] * ndc[0] + m[5] * ndc[1] + m[13];
            assert!(
                (x - expected[0]).abs() < 1e-2 && (y - expected[1]).abs() < 1e-2,
                "{ndc:?} -> {x},{y} vs {expected:?}"
            );
        }
    }

    #[test]
    fn only_the_compose_layer_samples_through_its_own_rect() {
        assert!(is_compose_layer("composelayer"));
        assert!(is_compose_layer("util/composelayer"));
        assert!(!is_compose_layer("genericimage2"));
        assert!(!is_compose_layer("effects/waterripple"));
    }

    #[test]
    fn a_child_layer_is_placed_through_its_parents() {
        let locals: HashMap<i64, LocalXf> = [
            (
                1,
                LocalXf {
                    origin: [1920.0, 1080.0],
                    scale: [1.0, 1.0],
                    angle_z: 0.0,
                    parent: None,
                },
            ),
            (
                2,
                LocalXf {
                    origin: [-3.0, 656.0],
                    scale: [1.0, 1.0],
                    angle_z: 0.0,
                    parent: Some(1),
                },
            ),
        ]
        .into_iter()
        .collect();

        let (origin, scale, angle) = world_xf(2, &locals);
        assert!((origin[0] - 1917.0).abs() < 0.001, "{origin:?}");
        assert!((origin[1] - 1736.0).abs() < 0.001, "{origin:?}");
        assert_eq!(scale, [1.0, 1.0]);
        assert!(angle.abs() < f32::EPSILON);
    }

    #[test]
    fn reparent_moves_visibility_gating() {
        let mut parent_by_id: HashMap<i64, Option<i64>> =
            [(1, None), (2, None), (3, Some(1))].into_iter().collect();
        let visible_by_id: HashMap<i64, bool> = [(1, true), (2, false), (3, true)].into_iter().collect();
        let start = |p: &HashMap<i64, Option<i64>>| p.get(&3).copied().flatten();
        assert!(ancestors_visible(
            &parent_by_id,
            &visible_by_id,
            start(&parent_by_id)
        ));
        parent_by_id.insert(3, Some(2));
        assert!(!ancestors_visible(
            &parent_by_id,
            &visible_by_id,
            start(&parent_by_id)
        ));
    }

    #[test]
    fn runtime_layers_draw_in_creation_order_by_default() {
        let mut layers = std::collections::HashMap::new();
        for (i, id) in [-1000i64, -1001, -1002].into_iter().enumerate() {
            layers.insert(
                id,
                RuntimeLayer {
                    order: i as i64,
                    ..RuntimeLayer::default()
                },
            );
        }
        assert_eq!(runtime_draw_order(&layers), [-1000, -1001, -1002]);
    }

    #[test]
    fn runtime_layers_draw_in_sorted_order_after_renumber() {
        let mut layers = std::collections::HashMap::new();
        for (order, id) in [(5i64, -1000i64), (3, -1001), (4, -1002), (3, -1003)] {
            layers.insert(
                id,
                RuntimeLayer {
                    order,
                    ..RuntimeLayer::default()
                },
            );
        }
        assert_eq!(runtime_draw_order(&layers), [-1001, -1003, -1002, -1000]);
    }

    fn reference_mvp(proj: (u32, u32), eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> Mat4 {
        let ortho = matrix::ortho(
            -(proj.0 as f32) / 2.0,
            proj.0 as f32 / 2.0,
            -(proj.1 as f32) / 2.0,
            proj.1 as f32 / 2.0,
            0.0,
            1000.0,
        );
        matrix::mul(&matrix::translate(&ortho, eye), &matrix::look_at(eye, center, up))
    }

    #[test]
    fn centered_camera_mvp_is_mirror_invariant() {
        let (eye, center, up) = ([0.0, 0.0, 1000.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let conj = screen_camera_mvp((1920, 1080), eye, center, up, 1000.0);
        let plain = reference_mvp((1920, 1080), eye, center, up);
        for (i, (a, b)) in conj.iter().zip(plain.iter()).enumerate() {
            assert!((a - b).abs() < 1e-5, "elem {i}: {a} vs {b}");
        }
    }

    #[test]
    fn tilted_camera_matches_the_flipped_reference() {
        let (eye, center, up) = ([0.0, 0.0, 1000.0], [0.0, 300.0, 0.0], [0.0, 1.0, 0.0]);
        let conj = screen_camera_mvp((1920, 1080), eye, center, up, 1000.0);
        let reference = reference_mvp((1920, 1080), eye, center, up);
        for v in [
            [0.0f32, 0.0, 0.0, 1.0],
            [100.0, 200.0, 0.0, 1.0],
            [-50.0, -120.0, 0.0, 1.0],
        ] {
            let kirie = apply(&conj, [v[0], -v[1], v[2], v[3]]);
            let mut expected = apply(&reference, v);
            expected[1] = -expected[1];
            for (i, (a, b)) in kirie.iter().zip(expected.iter()).enumerate() {
                assert!((a - b).abs() < 1e-4, "clip {i}: {a} vs {b} for {v:?}");
            }
        }
        let p = [0.0f32, 200.0, 0.0, 1.0];
        let old = apply(&reference, p);
        let new = apply(&conj, p);
        assert!((old[1] - new[1]).abs() > 1e-3, "tilt must not be mirror-even");
    }

    #[test]
    fn puppet_base_forces_translucent_blending() {
        use kirie_scene::material::Blending;
        for planned in [Blending::Normal, Blending::Translucent, Blending::Additive] {
            assert_eq!(effective_blending(true, planned), Blending::Translucent);
            assert_eq!(effective_blending(false, planned), planned);
        }
    }

    #[test]
    fn embedded_copy_command_shader_translates() {
        struct NoIncludes;
        impl IncludeResolver for NoIncludes {
            fn resolve(&self, _: &str) -> Option<String> {
                None
            }
        }
        let inputs = kirie_shader::ShaderInputs::default();
        kirie_shader::translate(
            kirie_shader::Stage::Vertex,
            "copy.vert",
            COPY_COMMAND_VERT,
            &NoIncludes,
            &inputs,
        )
        .expect("commands/copy vertex stage must translate");
        kirie_shader::translate(
            kirie_shader::Stage::Fragment,
            "copy.frag",
            COPY_COMMAND_FRAG,
            &NoIncludes,
            &inputs,
        )
        .expect("commands/copy fragment stage must translate");
    }

    #[test]
    fn uv_crop_scales_only_uv_columns_into_real_subrect() {
        let mut q = scene_space_quad([960.0, 540.0], (1920, 1080), [1.0, 1.0], 0.0, (1920, 1080));
        let pos_before: Vec<[f32; 3]> = q.iter().map(|v| [v[0], v[1], v[2]]).collect();
        apply_uv_crop(&mut q, [0.9375, 0.5]);
        for (v, p) in q.iter().zip(&pos_before) {
            assert_eq!([v[0], v[1], v[2]], *p, "position columns must not move");
        }
        assert_eq!([q[0][3], q[0][4]], [0.0, 0.0]);
        assert_eq!([q[1][3], q[1][4]], [0.0, 0.5]);
        assert_eq!([q[2][3], q[2][4]], [0.9375, 0.0]);
        assert_eq!([q[3][3], q[3][4]], [0.9375, 0.5]);
    }
}
