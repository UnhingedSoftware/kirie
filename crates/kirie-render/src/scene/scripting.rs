use std::collections::BTreeMap;

use kirie_audio::AudioSpectrum;
use kirie_scene::object::ObjectKind;
use kirie_scene::user::ScriptBinding;
use kirie_scene::{PropertyValue, SceneModel};
use kirie_script::{
    AnimationState, AudioBuffers, HostFrame, LayerState, SceneOp, SceneState, ScriptEngine, ScriptValue,
    TickOutput,
};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropTarget {
    Alpha,
    Brightness,
    Color,
    Visible,
    Text,
    ParticleRate,
    Origin,
    Scale,
    Angles,
    ParallaxDepth,
    Volume,
}

impl PropTarget {
    fn from_field(name: &str) -> Option<Self> {
        Some(match name {
            "alpha" => Self::Alpha,
            "brightness" => Self::Brightness,
            "color" => Self::Color,
            "visible" => Self::Visible,
            "text" => Self::Text,
            "rate" => Self::ParticleRate,
            "origin" => Self::Origin,
            "scale" => Self::Scale,
            "angles" => Self::Angles,
            "parallaxDepth" => Self::ParallaxDepth,
            "volume" => Self::Volume,
            _ => return None,
        })
    }
}

struct ScriptedProp {
    key: String,
    object_id: i64,
    target: PropTarget,
    effect_constant: Option<(usize, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropUpdate {
    pub object_id: i64,
    pub target: PropTarget,
    pub value: ScriptValue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraOp {
    pub eye: Option<[f32; 3]>,
    pub center: Option<[f32; 3]>,
    pub up: Option<[f32; 3]>,
    pub fov: Option<f32>,
    pub zoom: Option<f32>,
}

fn merge_camera(
    slot: &mut Option<CameraOp>,
    eye: Option<[f32; 3]>,
    center: Option<[f32; 3]>,
    up: Option<[f32; 3]>,
    fov: Option<f32>,
    zoom: Option<f32>,
) {
    let dst = slot.get_or_insert_with(CameraOp::default);
    if eye.is_some() {
        dst.eye = eye;
    }
    if center.is_some() {
        dst.center = center;
    }
    if up.is_some() {
        dst.up = up;
    }
    if fov.is_some() {
        dst.fov = fov;
    }
    if zoom.is_some() {
        dst.zoom = zoom;
    }
}

pub struct ScriptHost {
    engine: ScriptEngine,
    props: Vec<ScriptedProp>,
    layers: Vec<LayerState>,
    scene: SceneState,
    user_props: BTreeMap<String, ScriptValue>,
    res: [f32; 2],
    elapsed: f64,
    created: Vec<(i64, String)>,
    destroyed: Vec<i64>,
    particle_ops: Vec<ParticleOp>,
    material_ops: Vec<(i64, usize, String, kirie_script::ScriptValue)>,
    scene_ops: Vec<(String, kirie_script::ScriptValue)>,
    video_ops: Vec<(i64, String, f64)>,
    anim_ops: Vec<(u32, String, f64)>,
    animations: Vec<AnimationState>,
    animation_events: Vec<(i64, String, f32)>,
    wants_media: bool,
    media_prev: Option<MediaPrev>,
    camera_op: Option<CameraOp>,
    order_dirty: bool,
    parent_updates: Vec<(i64, i64)>,
    scene_dirty: bool,
    frame: Option<Box<HostFrame>>,
    user_props_dirty: bool,
    last_tick: std::time::Instant,
    tz_offset_secs: f64,
    overrides: Vec<(String, ScriptValue)>,
}

impl ScriptHost {
    #[must_use]
    pub fn build(
        model: &SceneModel,
        res: (u32, u32),
        user_props: &[(String, PropertyValue)],
    ) -> Option<Self> {
        let mut pending: Vec<Pending> = Vec::new();
        let mut layers: Vec<LayerState> = Vec::with_capacity(model.scene.objects.len());
        let mut render_order: Vec<usize> = (0..model.scene.objects.len()).collect();
        if model.scene.general.customsortorder {
            render_order.sort_by_key(|&i| model.scene.objects[i].base.sortorder);
        }
        for &oi in &render_order {
            let object = &model.scene.objects[oi];
            let id = object.base.id;
            layers.push(layer_state(object));
            let base = &object.base;
            collect(&mut pending, id, "origin", &base.origin.script, || {
                ScriptValue::Vec3(base.origin.value)
            });
            collect(&mut pending, id, "scale", &base.scale.script, || {
                ScriptValue::Vec3(base.scale.value)
            });
            collect(&mut pending, id, "angles", &base.angles.script, || {
                ScriptValue::Vec3(base.angles.value)
            });
            if !matches!(&object.kind, ObjectKind::Image(_) | ObjectKind::Text(_)) {
                collect(&mut pending, id, "visible", &base.visible.script, || {
                    ScriptValue::Bool(base.visible.value)
                });
            }
            match &object.kind {
                ObjectKind::Image(img) => {
                    collect(&mut pending, id, "alpha", &img.alpha.script, || {
                        ScriptValue::Float(f64::from(img.alpha.value))
                    });
                    collect(&mut pending, id, "brightness", &img.brightness.script, || {
                        ScriptValue::Float(f64::from(img.brightness.value))
                    });
                    collect(&mut pending, id, "color", &img.color.script, || {
                        color_value(img.color.value)
                    });
                    collect(&mut pending, id, "visible", &img.visible.script, || {
                        ScriptValue::Bool(img.visible.value)
                    });
                    collect_effect_constants(&mut pending, id, &img.effects);
                }
                ObjectKind::Particle(pobj) => {
                    collect(
                        &mut pending,
                        id,
                        "rate",
                        &pobj.instanceoverride.rate.script,
                        || ScriptValue::Float(f64::from(pobj.instanceoverride.rate.value)),
                    );
                }
                ObjectKind::Text(txt) => {
                    collect(&mut pending, id, "text", &txt.text.script, || {
                        ScriptValue::Str(txt.text.value.clone())
                    });
                    collect(&mut pending, id, "alpha", &txt.alpha.script, || {
                        ScriptValue::Float(f64::from(txt.alpha.value))
                    });
                    collect(&mut pending, id, "color", &txt.color.script, || {
                        color_value(txt.color.value)
                    });
                    collect(&mut pending, id, "visible", &txt.visible.script, || {
                        ScriptValue::Bool(txt.visible.value)
                    });
                    collect_effect_constants(&mut pending, id, &txt.effects);
                }
                ObjectKind::Sound(snd) => {
                    collect(&mut pending, id, "volume", &snd.volume.script, || {
                        ScriptValue::Float(f64::from(snd.volume.value))
                    });
                }
                _ => {}
            }
        }

        let has_text_scripts =
            model.scene.objects.iter().any(
                |o| matches!(&o.kind, kirie_scene::object::ObjectKind::Text(t) if t.text.script.is_some()),
            );
        if pending.is_empty() && !has_text_scripts {
            return None;
        }

        let engine = match ScriptEngine::new() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "script engine failed to start; scene runs without scripts");
                return None;
            }
        };

        const MEDIA_EXPORT_NAMES: [&str; 5] = [
            "mediaStatusChanged",
            "mediaPlaybackChanged",
            "mediaPropertiesChanged",
            "mediaThumbnailChanged",
            "mediaTimelineChanged",
        ];
        let wants_media = pending
            .iter()
            .any(|p| MEDIA_EXPORT_NAMES.iter().any(|n| p.source.contains(n)));
        let storage_id = pending.iter().find_map(|p| extract_workshop_id(&p.source));

        let mut props = Vec::with_capacity(pending.len());
        for p in pending {
            match engine.load_property_script(
                p.prop.key.clone(),
                p.source,
                Some(p.prop.object_id),
                p.initial,
                p.script_props,
            ) {
                Ok(()) => props.push(p.prop),
                Err(e) => {
                    tracing::warn!(key = %p.prop.key, error = %e, "property script failed to load; skipped");
                }
            }
        }

        if props.is_empty() {
            return None;
        }
        tracing::info!(scripts = props.len(), "scene script host started");

        if let Some(id) = &storage_id
            && let Some(home) = std::env::var_os("HOME")
        {
            let path = std::path::PathBuf::from(home)
                .join(".cache/kirie/storage")
                .join(format!("{id}.json"));
            let _ = engine.set_storage_path(path);
        }

        Some(ScriptHost {
            engine,
            props,
            layers,
            scene: scene_state(model),
            user_props: user_props
                .iter()
                .map(|(k, v)| (k.clone(), prop_to_script(v)))
                .collect(),
            res: [res.0 as f32, res.1 as f32],
            elapsed: 0.0,
            created: Vec::new(),
            destroyed: Vec::new(),
            particle_ops: Vec::new(),
            material_ops: Vec::new(),
            scene_ops: Vec::new(),
            video_ops: Vec::new(),
            anim_ops: Vec::new(),
            animations: Vec::new(),
            animation_events: Vec::new(),
            wants_media,
            media_prev: None,
            camera_op: None,
            order_dirty: false,
            parent_updates: Vec::new(),
            scene_dirty: false,
            frame: None,
            user_props_dirty: false,
            last_tick: std::time::Instant::now(),
            tz_offset_secs: local_utc_offset_secs(),
            overrides: Vec::new(),
        })
    }

    pub fn create_text_layer(
        &mut self,
        source: &str,
        properties: serde_json::Value,
        initial_text: &str,
    ) -> Option<u32> {
        match self.engine.create_layer_script(source, properties, initial_text) {
            Ok(h) if h != 0 => Some(h),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(error = %e, "text layer script failed to load");
                None
            }
        }
    }

    pub fn tick_text_layer(&mut self, handle: u32, time: f64, dt: f64) -> Option<String> {
        let _ = self.engine.tick_layer(handle, time, dt, 60.0);
        self.engine.layer_text(handle).ok()
    }

    pub fn tick(
        &mut self,
        dt: f32,
        audio: Option<&AudioSpectrum>,
        pointer: [f32; 2],
        pointer_scene: [f32; 2],
        pointer_left: bool,
        media: Option<&crate::media::MediaState>,
    ) -> Vec<PropUpdate> {
        self.elapsed += f64::from(dt);
        let mut frame = match self.frame.take() {
            Some(f) => f,
            None => {
                let mut f = Box::new(HostFrame::default());
                f.scene = self.scene.clone();
                f.user_props = self.user_props.clone();
                self.user_props_dirty = false;
                self.scene_dirty = false;
                f
            }
        };
        frame.runtime = self.elapsed;
        let now = std::time::Instant::now();
        let wall = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        frame.frametime = if dt > 0.0 { f64::from(dt) } else { wall.max(1e-4) };
        frame.now = self.elapsed * 1000.0;
        frame.res_x = f64::from(self.res[0]);
        frame.res_y = f64::from(self.res[1]);
        frame.pointer_screen = pointer;
        frame.pointer_world = [pointer_scene[0], pointer_scene[1], 0.0];
        frame.pointer_left_down = pointer_left;
        frame.time_of_day = day_fraction(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
            self.tz_offset_secs,
        );
        frame.media = media.and_then(|st| media_frame(st, &mut self.media_prev));
        match (audio, &mut frame.audio) {
            (Some(s), Some(bufs)) => {
                bufs.audio16.clear();
                bufs.audio16.extend_from_slice(&s.audio16);
                bufs.audio32.clear();
                bufs.audio32.extend_from_slice(&s.audio32);
                bufs.audio64.clear();
                bufs.audio64.extend_from_slice(&s.audio64);
            }
            (Some(s), slot @ None) => {
                *slot = Some(AudioBuffers {
                    audio16: s.audio16.to_vec(),
                    audio32: s.audio32.to_vec(),
                    audio64: s.audio64.to_vec(),
                });
            }
            (None, slot) => *slot = None,
        }
        frame.layers.clone_from(&self.layers);
        if self.user_props_dirty {
            frame.user_props.clone_from(&self.user_props);
            self.user_props_dirty = false;
        }
        if self.scene_dirty {
            frame.scene.clone_from(&self.scene);
            self.scene_dirty = false;
        }
        std::mem::swap(&mut frame.animations, &mut self.animations);
        std::mem::swap(&mut frame.animation_events, &mut self.animation_events);
        self.animation_events.clear();

        let overrides = std::mem::take(&mut self.overrides);
        let output = match self.engine.tick_reuse(frame, overrides) {
            Ok((o, frame)) => {
                self.frame = Some(frame);
                o
            }
            Err(e) => {
                tracing::warn!(error = %e, "script tick failed; leaving properties unchanged");
                return Vec::new();
            }
        };
        self.process_output(output)
    }

    pub fn apply_user_property(&mut self, key: &str, value: &kirie_scene::PropertyValue) -> Vec<PropUpdate> {
        let sv = prop_to_script(value);
        self.user_props.insert(key.to_owned(), sv.clone());
        self.user_props_dirty = true;
        match self.engine.dispatch_user_property(key.to_owned(), sv) {
            Ok(output) => self.process_output(output),
            Err(e) => {
                tracing::warn!(error = %e, "script user-property dispatch failed (unchanged)");
                Vec::new()
            }
        }
    }

    pub fn take_created(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.created)
    }

    pub fn note_animation(&mut self, updates: &[PropUpdate], overrides: &[(String, ScriptValue)]) {
        for u in updates {
            self.record_layer(u.object_id, u.target, &u.value);
        }
        self.overrides.extend_from_slice(overrides);
    }

    pub fn note_animation_state(&mut self, states: Vec<AnimationState>, events: Vec<(i64, String, f32)>) {
        self.animations = states;
        self.animation_events = events;
    }

    fn process_output(&mut self, output: TickOutput) -> Vec<PropUpdate> {
        for err in &output.errors {
            tracing::debug!(error = %err, "script runtime error (script stays loaded, V9)");
        }
        for log in &output.logs {
            if log.error {
                tracing::debug!(target: "kirie_script::console", "{}", log.message);
            } else {
                tracing::trace!(target: "kirie_script::console", "{}", log.message);
            }
        }

        let mut updates = Vec::new();
        for (key, value) in output.property_results {
            let Some(prop) = self.props.iter().find(|p| p.key == key) else {
                continue;
            };
            if let Some((ei, cname)) = &prop.effect_constant {
                self.material_ops
                    .push((prop.object_id, *ei, cname.clone(), value));
                continue;
            }
            let (object_id, target) = (prop.object_id, prop.target);
            self.record_layer(object_id, target, &value);
            updates.push(PropUpdate {
                object_id,
                target,
                value,
            });
        }
        for op in output.ops {
            match op {
                SceneOp::SetProperty {
                    layer_id,
                    name,
                    value,
                } => {
                    if let Some(target) = PropTarget::from_field(&name) {
                        self.record_layer(layer_id, target, &value);
                        updates.push(PropUpdate {
                            object_id: layer_id,
                            target,
                            value,
                        });
                    }
                }
                SceneOp::CreateLayer {
                    layer_id, path, text, ..
                } => {
                    self.layers.push(LayerState {
                        id: layer_id,
                        name: path.clone(),
                        origin: Some([0.0; 3]),
                        scale: Some([1.0; 3]),
                        angles: Some([0.0; 3]),
                        visible: Some(true),
                        alpha: Some(1.0),
                        color: Some([1.0; 3]),
                        text,
                        ..LayerState::default()
                    });
                    if !path.is_empty() {
                        self.created.push((layer_id, path));
                    }
                }
                SceneOp::SetCameraTransforms {
                    eye,
                    center,
                    up,
                    fov,
                    zoom,
                } => {
                    merge_camera(&mut self.camera_op, eye, center, up, fov, zoom);
                    if let Some(f) = fov {
                        self.scene.camera.fov = f;
                        self.scene.fov = f;
                        self.scene_dirty = true;
                    }
                }
                SceneOp::SortLayer { layer_id, index } => {
                    if sort_layer_apply(&mut self.layers, layer_id, index) {
                        self.order_dirty = true;
                    }
                }
                SceneOp::SetParent { layer_id, parent } => {
                    if let Some(l) = self.layers.iter_mut().find(|l| l.id == layer_id) {
                        l.parent = parent;
                    }
                    if let Some(u) = parent_update(layer_id, parent) {
                        self.parent_updates.push(u);
                    }
                }
                SceneOp::VideoCommand { layer_id, cmd, value } => {
                    self.video_ops.push((layer_id, cmd, value));
                }
                SceneOp::AnimationCommand { index, cmd, value } => {
                    self.anim_ops.push((index, cmd, value));
                }
                SceneOp::SetMaterialProperty {
                    layer_id,
                    effect,
                    name,
                    value,
                } => {
                    self.material_ops.push((layer_id, effect as usize, name, value));
                }
                SceneOp::ParticleCommand { layer_id, cmd } => {
                    self.particle_ops.push(ParticleOp::Command { id: layer_id, cmd });
                }
                SceneOp::EmitParticles { layer_id, count } => {
                    self.particle_ops.push(ParticleOp::Emit { id: layer_id, count });
                }
                SceneOp::SetInstance {
                    layer_id,
                    name,
                    value,
                } => {
                    self.particle_ops.push(ParticleOp::Instance {
                        id: layer_id,
                        name,
                        value,
                    });
                }
                SceneOp::DestroyLayer { layer_id } => {
                    tracing::debug!(layer_id, "script destroyed layer");
                    self.layers.retain(|l| l.id != layer_id);
                    self.destroyed.push(layer_id);
                }
                SceneOp::SetSceneProperty { name, value } => {
                    if apply_scene_state(&mut self.scene, &name, &value) {
                        self.scene_dirty = true;
                        self.scene_ops.push((name, value));
                    }
                }
            }
        }
        updates
    }

    pub fn take_scene_ops(&mut self) -> Vec<(String, kirie_script::ScriptValue)> {
        std::mem::take(&mut self.scene_ops)
    }

    pub fn take_destroyed(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.destroyed)
    }

    pub fn take_particle_ops(&mut self) -> Vec<ParticleOp> {
        std::mem::take(&mut self.particle_ops)
    }

    pub fn set_scene_fov(&mut self, fov: f32) {
        if (self.scene.camera.fov - fov).abs() > f32::EPSILON {
            self.scene.camera.fov = fov;
            self.scene.fov = fov;
            self.scene_dirty = true;
        }
    }

    pub fn take_video_ops(&mut self) -> Vec<(i64, String, f64)> {
        std::mem::take(&mut self.video_ops)
    }

    pub fn take_anim_ops(&mut self) -> Vec<(u32, String, f64)> {
        std::mem::take(&mut self.anim_ops)
    }

    pub fn take_material_ops(&mut self) -> Vec<(i64, usize, String, kirie_script::ScriptValue)> {
        std::mem::take(&mut self.material_ops)
    }

    #[must_use]
    pub fn wants_media(&self) -> bool {
        self.wants_media
    }

    pub fn take_camera(&mut self) -> Option<CameraOp> {
        self.camera_op.take()
    }

    pub fn take_layer_order(&mut self) -> Option<Vec<i64>> {
        if !self.order_dirty {
            return None;
        }
        self.order_dirty = false;
        Some(self.layers.iter().map(|l| l.id).collect())
    }

    pub fn take_parent_updates(&mut self) -> Vec<(i64, i64)> {
        std::mem::take(&mut self.parent_updates)
    }

    fn record_layer(&mut self, id: i64, target: PropTarget, value: &ScriptValue) {
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else {
            return;
        };
        match target {
            PropTarget::Alpha => {
                if let Some(a) = as_f32(value) {
                    layer.alpha = Some(a);
                }
            }
            PropTarget::Color => {
                if let Some(c) = as_rgb(value) {
                    layer.color = Some(c);
                }
            }
            PropTarget::Visible => {
                if let ScriptValue::Bool(b) = value {
                    layer.visible = Some(*b);
                }
            }
            PropTarget::Text => {
                if let ScriptValue::Str(s) = value {
                    layer.text = Some(s.clone());
                }
            }
            PropTarget::Origin => {
                if let Some(v) = as_vec3(value) {
                    layer.origin = Some(v);
                }
            }
            PropTarget::Scale => {
                if let Some(v) = as_vec3(value) {
                    layer.scale = Some(v);
                }
            }
            PropTarget::Angles => {
                if let Some(v) = as_vec3(value) {
                    layer.angles = Some(v);
                }
            }
            PropTarget::ParallaxDepth => {
                if let Some(v) = as_vec3(value) {
                    layer.parallax_depth = Some(v[0]);
                }
            }
            PropTarget::Brightness | PropTarget::ParticleRate | PropTarget::Volume => {}
        }
    }
}

fn dynamic_to_script(v: &kirie_scene::value::DynamicValue) -> ScriptValue {
    use kirie_scene::value::DynamicValue as D;
    match v {
        D::Bool(b) => ScriptValue::Bool(*b),
        D::Int(i) => ScriptValue::Int(*i),
        D::Float(f) => ScriptValue::Float(*f),
        D::Str(s) => ScriptValue::Str(s.clone()),
        D::Vec(c) => match c.len() {
            2 => ScriptValue::Vec2([c[0], c[1]]),
            3 => ScriptValue::Vec3([c[0], c[1], c[2]]),
            4 => ScriptValue::Vec4([c[0], c[1], c[2], c[3]]),
            _ => ScriptValue::Null,
        },
        D::Color(c) => ScriptValue::Vec4(*c),
        D::Null => ScriptValue::Null,
    }
}

fn extract_workshop_id(source: &str) -> Option<String> {
    let i = source.find("__workshopId")?;
    let rest = &source[i..];
    let open = rest.find(['\'', '"'])?;
    let quote = rest.as_bytes()[open] as char;
    let close = rest[open + 1..].find(quote)?;
    let id = &rest[open + 1..open + 1 + close];
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_owned())
}

struct MediaPrev {
    available: bool,
    state: i32,
    title: String,
    artist: String,
    album: String,
    art_key: usize,
    position: f64,
}

fn media_frame(
    state: &crate::media::MediaState,
    prev: &mut Option<MediaPrev>,
) -> Option<kirie_script::MediaFrame> {
    let ev = crate::media::MediaPlaybackEvent::from_state(state);
    let art_key = state
        .art
        .as_ref()
        .map_or(0, |a| std::sync::Arc::as_ptr(a) as usize);
    let (status, playback, properties, thumbnail, timeline) = match prev.as_ref() {
        None => (true, true, true, true, true),
        Some(p) => (
            p.available != ev.available,
            p.state != ev.state,
            p.title != ev.title || p.artist != ev.artist || p.album != ev.album,
            p.art_key != art_key,
            (p.position - ev.position_secs).abs() > 1e-9,
        ),
    };
    *prev = Some(MediaPrev {
        available: ev.available,
        state: ev.state,
        title: ev.title.clone(),
        artist: ev.artist.clone(),
        album: ev.album.clone(),
        art_key,
        position: ev.position_secs,
    });
    if !(status || playback || properties || thumbnail || timeline) {
        return None;
    }
    let colors = match (
        &ev.primary_color,
        &ev.secondary_color,
        &ev.text_color,
        &ev.high_contrast_color,
    ) {
        (Some(p), Some(s), Some(t), Some(h)) => match (hex_rgb(p), hex_rgb(s), hex_rgb(t), hex_rgb(h)) {
            (Some(p), Some(s), Some(t), Some(h)) => Some([p, s, s, t, h]),
            _ => None,
        },
        _ => None,
    };
    Some(kirie_script::MediaFrame {
        enabled: ev.available,
        state: ev.state,
        title: ev.title,
        artist: ev.artist,
        album_title: ev.album,
        position: ev.position_secs,
        duration: ev.duration_secs,
        has_thumbnail: ev.thumbnail.is_some(),
        colors,
        status_changed: status,
        playback_changed: playback,
        properties_changed: properties,
        thumbnail_changed: thumbnail,
        timeline_changed: timeline,
    })
}

fn hex_rgb(s: &str) -> Option<[f32; 3]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([
        f32::from(byte(0)?) / 255.0,
        f32::from(byte(2)?) / 255.0,
        f32::from(byte(4)?) / 255.0,
    ])
}

pub enum ParticleOp {
    Command {
        id: i64,
        cmd: String,
    },
    Emit {
        id: i64,
        count: u32,
    },
    Instance {
        id: i64,
        name: String,
        value: kirie_script::ScriptValue,
    },
}

fn parent_update(layer_id: i64, parent: Option<i64>) -> Option<(i64, i64)> {
    match parent {
        Some(p) if p != layer_id => Some((layer_id, p)),
        _ => None,
    }
}

fn sort_layer_apply(layers: &mut Vec<LayerState>, layer_id: i64, index: i64) -> bool {
    let Some(pos) = layers.iter().position(|l| l.id == layer_id) else {
        return false;
    };
    let layer = layers.remove(pos);
    let at = if index < 0 {
        layers.len()
    } else {
        (index as usize).min(layers.len())
    };
    layers.insert(at, layer);
    true
}

fn flatten_props(props: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (k, v) in props {
        let val = match v {
            Value::Object(o) => o.get("value").cloned().unwrap_or_else(|| v.clone()),
            other => other.clone(),
        };
        out.insert(k.clone(), val);
    }
    Value::Object(out)
}

struct Pending {
    prop: ScriptedProp,
    source: String,
    initial: ScriptValue,
    script_props: Value,
}

fn collect_effect_constants(pending: &mut Vec<Pending>, id: i64, effects: &[kirie_scene::object::Effect]) {
    for (ei, eff) in effects.iter().enumerate() {
        for ov in &eff.passes {
            for (cname, us) in &ov.constantshadervalues {
                let Some(b) = &us.script else { continue };
                pending.push(Pending {
                    prop: ScriptedProp {
                        key: format!("fx{ei}{cname}_{id}"),
                        object_id: id,
                        target: PropTarget::Color,
                        effect_constant: Some((ei, cname.clone())),
                    },
                    source: b.source.clone(),
                    initial: dynamic_to_script(&us.value),
                    script_props: flatten_props(&b.properties),
                });
            }
        }
    }
}

fn collect(
    out: &mut Vec<Pending>,
    id: i64,
    field: &str,
    binding: &Option<ScriptBinding>,
    initial: impl FnOnce() -> ScriptValue,
) {
    let (Some(target), Some(b)) = (PropTarget::from_field(field), binding.as_ref()) else {
        return;
    };
    out.push(Pending {
        prop: ScriptedProp {
            key: format!("{field}_{id}"),
            object_id: id,
            target,
            effect_constant: None,
        },
        source: b.source.clone(),
        initial: initial(),
        script_props: flatten_props(&b.properties),
    });
}

fn layer_state(object: &kirie_scene::object::Object) -> LayerState {
    let base = &object.base;
    let mut ls = LayerState {
        id: base.id,
        name: base.name.clone(),
        parent: base.parent,
        origin: Some(base.origin.value),
        scale: Some(base.scale.value),
        angles: Some(base.angles.value),
        visible: Some(base.visible.value),
        ..LayerState::default()
    };
    ls.solid = object.extra.get("solid").and_then(serde_json::Value::as_bool);
    match &object.kind {
        ObjectKind::Image(img) => {
            ls.color = Some([img.color.value[0], img.color.value[1], img.color.value[2]]);
            ls.alpha = Some(img.alpha.value);
            ls.visible = Some(img.visible.value && base.visible.value);
            ls.size = Some(img.size);
            ls.effects = Some(
                img.effects
                    .iter()
                    .map(|e| {
                        let mats = e.resolved.as_ref().map_or(0, |f| {
                            e.resolved.as_ref().map_or(0, |_| {
                                f.passes.iter().filter(|p| p.material.is_some()).count() as u32
                            })
                        });
                        (e.name.clone(), mats)
                    })
                    .collect(),
            );
        }
        ObjectKind::Text(txt) => {
            ls.color = Some([txt.color.value[0], txt.color.value[1], txt.color.value[2]]);
            ls.alpha = Some(txt.alpha.value);
            ls.visible = Some(txt.visible.value && base.visible.value);
            ls.point_size = Some(txt.pointsize.value);
            ls.text = Some(txt.text.value.clone());
            ls.size = Some(txt.size);
        }
        _ => {}
    }
    ls
}

fn apply_scene_state(scene: &mut SceneState, name: &str, value: &ScriptValue) -> bool {
    let as_bool = || match value {
        ScriptValue::Bool(b) => Some(*b),
        _ => as_f32(value).map(|f| f != 0.0),
    };
    match name {
        "bloom" => scene.bloom = as_bool().unwrap_or(scene.bloom),
        "bloomstrength" => scene.bloomstrength = as_f32(value).map_or(scene.bloomstrength, |f| f as i64),
        "bloomthreshold" => scene.bloomthreshold = as_f32(value).map_or(scene.bloomthreshold, |f| f as i64),
        "clearenabled" => scene.clearenabled = as_bool().unwrap_or(scene.clearenabled),
        "camerafade" => scene.camerafade = as_bool().unwrap_or(scene.camerafade),
        "camerashake" => scene.camerashake = as_bool().unwrap_or(scene.camerashake),
        "camerashakespeed" => scene.camerashakespeed = as_f32(value).unwrap_or(scene.camerashakespeed),
        "camerashakeamplitude" => {
            scene.camerashakeamplitude = as_f32(value).unwrap_or(scene.camerashakeamplitude);
        }
        "camerashakeroughness" => {
            scene.camerashakeroughness = as_f32(value).unwrap_or(scene.camerashakeroughness);
        }
        "cameraparallax" => scene.cameraparallax = as_bool().unwrap_or(scene.cameraparallax),
        "cameraparallaxamount" => {
            scene.cameraparallaxamount = as_f32(value).unwrap_or(scene.cameraparallaxamount);
        }
        "cameraparallaxdelay" => {
            scene.cameraparallaxdelay = as_f32(value).unwrap_or(scene.cameraparallaxdelay);
        }
        "cameraparallaxmouseinfluence" => {
            scene.cameraparallaxmouseinfluence = as_f32(value).unwrap_or(scene.cameraparallaxmouseinfluence);
        }
        "clearcolor" => scene.clearcolor = as_rgb(value).unwrap_or(scene.clearcolor),
        "ambientcolor" => scene.ambientcolor = as_rgb(value).unwrap_or(scene.ambientcolor),
        "skylightcolor" => scene.skylightcolor = as_rgb(value).unwrap_or(scene.skylightcolor),
        _ => return false,
    }
    true
}

fn scene_state(model: &SceneModel) -> SceneState {
    let g = &model.scene.general;
    let cam = &model.scene.camera;
    SceneState {
        clearcolor: [
            g.clearcolor.value[0],
            g.clearcolor.value[1],
            g.clearcolor.value[2],
        ],
        ambientcolor: [
            g.ambientcolor.value[0],
            g.ambientcolor.value[1],
            g.ambientcolor.value[2],
        ],
        skylightcolor: [
            g.skylightcolor.value[0],
            g.skylightcolor.value[1],
            g.skylightcolor.value[2],
        ],
        bloom: g.bloom.value,
        bloomstrength: g.bloomstrength.value as i64,
        bloomthreshold: g.bloomthreshold.value as i64,
        fov: cam.fov.value,
        nearz: cam.nearz,
        farz: cam.farz,
        camerafade: g.camerafade.value,
        camerashake: g.camerashake.value,
        camerashakespeed: g.camerashakespeed.value,
        camerashakeamplitude: g.camerashakeamplitude.value,
        camerashakeroughness: g.camerashakeroughness.value,
        cameraparallax: g.cameraparallax.value,
        cameraparallaxamount: g.cameraparallaxamount.value,
        cameraparallaxdelay: g.cameraparallaxdelay.value,
        cameraparallaxmouseinfluence: g.cameraparallaxmouseinfluence.value,
        camera: kirie_script::CameraState {
            eye: cam.eye,
            center: cam.center,
            up: cam.up,
            fov: cam.fov.value,
        },
        ..SceneState::default()
    }
}

fn color_value(c: [f32; 4]) -> ScriptValue {
    ScriptValue::Vec3([c[0], c[1], c[2]])
}

fn prop_to_script(v: &PropertyValue) -> ScriptValue {
    match v {
        PropertyValue::Bool(b) => ScriptValue::Bool(*b),
        PropertyValue::Number(n) => ScriptValue::Float(*n),
        PropertyValue::Color([r, g, b, _]) => ScriptValue::Vec3([*r, *g, *b]),
        PropertyValue::Combo(s) | PropertyValue::Text(s) => ScriptValue::Str(s.clone()),
    }
}

pub fn as_f32(v: &ScriptValue) -> Option<f32> {
    match v {
        ScriptValue::Float(f) => Some(*f as f32),
        ScriptValue::Int(i) => Some(*i as f32),
        ScriptValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

pub fn as_vec3(v: &ScriptValue) -> Option<[f32; 3]> {
    match v {
        ScriptValue::Vec3(a) => Some(*a),
        ScriptValue::Vec2(a) => Some([a[0], a[1], 0.0]),
        ScriptValue::Float(f) => Some([*f as f32; 3]),
        _ => None,
    }
}

pub fn as_rgb(v: &ScriptValue) -> Option<[f32; 3]> {
    match v {
        ScriptValue::Vec3(c) => Some(*c),
        ScriptValue::Vec4(c) => Some([c[0], c[1], c[2]]),
        ScriptValue::Vec2(c) => Some([c[0], c[1], 0.0]),
        ScriptValue::Float(f) => Some([*f as f32; 3]),
        _ => None,
    }
}

fn day_fraction(unix_secs: f64, tz_offset_secs: f64) -> f64 {
    ((unix_secs + tz_offset_secs) / 86_400.0).rem_euclid(1.0)
}

#[must_use]
pub fn time_of_day_now(tz_offset_secs: f64) -> f64 {
    day_fraction(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64()),
        tz_offset_secs,
    )
}

pub fn local_utc_offset_secs() -> f64 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_utc_offset(s.trim()))
        .unwrap_or(0.0)
}

fn parse_utc_offset(s: &str) -> Option<f64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1.0, &s[1..]),
        b'-' => (-1.0, &s[1..]),
        _ => return None,
    };
    if digits.len() < 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: f64 = digits[..2].parse().ok()?;
    let minutes: f64 = if digits.len() >= 4 {
        digits[2..4].parse().ok()?
    } else {
        0.0
    };
    Some(sign * (hours * 3600.0 + minutes * 60.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_offset_parses_both_signs_and_short_forms() {
        assert_eq!(parse_utc_offset("+0200"), Some(7200.0));
        assert_eq!(parse_utc_offset("-0530"), Some(-19800.0));
        assert_eq!(parse_utc_offset("+00"), Some(0.0));
        assert_eq!(parse_utc_offset("0200"), None);
        assert_eq!(parse_utc_offset("+2b00"), None);
        assert_eq!(parse_utc_offset(""), None);
    }

    #[test]
    fn day_fraction_wraps_and_handles_negative_offsets() {
        let f = day_fraction(86_400.0 * 3.0, 7200.0);
        assert!((f - 2.0 / 24.0).abs() < 1e-9);
        let f = day_fraction(86_400.0 * 3.0 + 3600.0, -19800.0);
        assert!((f - 19.5 / 24.0).abs() < 1e-9);
        assert!((0.0..1.0).contains(&day_fraction(0.0, -1.0)));
    }

    fn layer_list(ids: &[i64]) -> Vec<LayerState> {
        ids.iter()
            .map(|&id| LayerState {
                id,
                ..LayerState::default()
            })
            .collect()
    }

    fn ids(layers: &[LayerState]) -> Vec<i64> {
        layers.iter().map(|l| l.id).collect()
    }

    #[test]
    fn sort_layer_moves_toward_bottom() {
        let mut layers = layer_list(&[10, 20, 30, 40]);
        assert!(sort_layer_apply(&mut layers, 30, 0));
        assert_eq!(ids(&layers), [30, 10, 20, 40]);
    }

    #[test]
    fn sort_layer_moves_toward_top() {
        let mut layers = layer_list(&[10, 20, 30, 40]);
        assert!(sort_layer_apply(&mut layers, 10, 2));
        assert_eq!(ids(&layers), [20, 30, 10, 40]);
    }

    #[test]
    fn sort_layer_negative_index_appends() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 10, -1));
        assert_eq!(ids(&layers), [20, 30, 10]);
    }

    #[test]
    fn sort_layer_past_end_appends() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 20, 99));
        assert_eq!(ids(&layers), [10, 30, 20]);
    }

    #[test]
    fn sort_layer_unknown_id_is_a_noop() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(!sort_layer_apply(&mut layers, 77, 0));
        assert_eq!(ids(&layers), [10, 20, 30]);
    }

    #[test]
    fn sort_layer_same_position_is_stable() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 20, 1));
        assert_eq!(ids(&layers), [10, 20, 30]);
    }

    #[test]
    fn set_parent_filters_like_the_reference() {
        assert_eq!(parent_update(3, Some(7)), Some((3, 7)));
        assert_eq!(parent_update(3, Some(3)), None);
        assert_eq!(parent_update(3, None), None);
    }

    #[test]
    fn camera_ops_merge_last_wins_per_field() {
        let mut slot = None;
        merge_camera(&mut slot, Some([1.0, 2.0, 3.0]), None, None, Some(60.0), None);
        merge_camera(
            &mut slot,
            None,
            Some([4.0, 5.0, 6.0]),
            None,
            Some(45.0),
            Some(2.0),
        );
        assert_eq!(
            slot,
            Some(CameraOp {
                eye: Some([1.0, 2.0, 3.0]),
                center: Some([4.0, 5.0, 6.0]),
                up: None,
                fov: Some(45.0),
                zoom: Some(2.0),
            })
        );
    }
}
