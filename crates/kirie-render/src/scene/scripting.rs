//! SceneScript integration for the scene renderer (docs/scripting-api.md §3/§5;
//! SPEC.md §V3).
//!
//! A scene's inline property scripts (`"script"` bindings on object leaf
//! fields, docs/format-scene-json.md §3.1) are driven here. Following SPEC.md
//! §V3, the QuickJS runtime lives on its own thread inside
//! [`kirie_script::ScriptEngine`]; this host never touches JS memory. Each frame
//! it marshals an immutable [`HostFrame`] snapshot *in* and reads typed
//! [`kirie_script::SceneOp`]s + property results *out*, translating them into
//! [`PropUpdate`]s the render loop applies to its live objects.
//!
//! # Scope
//!
//! The object-leaf properties that flow into the per-object builtin uniforms
//! (`g_Alpha`, `g_Brightness`, `g_Color`), object visibility and text content
//! are wired back, plus the scene ops: runtime layers (`createLayer` +
//! transform/color ops driving their solid quads), the runtime camera override
//! (`setCameraTransforms` → the perspective camera 3D models re-read each
//! frame; the 2D ortho screen MVP is untouched, reference `Camera.h:24-26`),
//! z-order moves (`sortLayer` → the reference's single reordered render list)
//! and reparenting (`setParent` → the ancestor-visibility gate). Remaining
//! limits: scene-object transform ops (`origin`/`scale`/`angles` on *baked*
//! objects) require a geometry rebuild the 2D compositor does not do
//! per-frame, and a reparent does not re-anchor a baked transform (`world_xf`
//! composes parent chains at build) — visibility gating is the live part
//! (both tracked).

use std::collections::BTreeMap;

use kirie_audio::AudioSpectrum;
use kirie_scene::object::ObjectKind;
use kirie_scene::user::ScriptBinding;
use kirie_scene::{PropertyValue, SceneModel};
use kirie_script::{
    AudioBuffers, HostFrame, LayerState, SceneOp, SceneState, ScriptEngine, ScriptValue, TickOutput,
};
use serde_json::{Map, Value};

/// Which live render value a scripted property (or a script `SetProperty` op)
/// drives. Only these flow into the per-object builtin uniforms / visibility /
/// text this compositor tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropTarget {
    /// `g_Alpha` — object alpha.
    Alpha,
    /// `g_Brightness` — object brightness.
    Brightness,
    /// `g_Color` / `g_Color4` — object color.
    Color,
    /// Object visibility (drops the object's draw when false).
    Visible,
    /// Text object content.
    Text,
    /// Particle `instanceoverride.rate` — live emission-rate multiplier. The
    /// standard audio-reactive particle recipe drives exactly this: a property
    /// script reads `engine.registerAudioBuffers(..)` and returns the scaled
    /// rate every frame.
    ParticleRate,
    /// `origin` — layer translation (runtime layers; scene units, JSON space).
    Origin,
    /// `scale` — layer scale (runtime bar layers: pixel dims).
    Scale,
    /// `angles` — layer rotation, degrees.
    Angles,
    /// `parallaxDepth` — per-layer parallax factor (d.ts IImageLayer, Vec2).
    ParallaxDepth,
}

impl PropTarget {
    /// Map a scene-object leaf field name to a target, if we drive it.
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
            _ => return None,
        })
    }
}

/// One property script bound to a scene object leaf, keyed by its module key.
struct ScriptedProp {
    /// `"<field>_<objectId>"` module key (matches [`kirie_script`] results).
    key: String,
    /// The owning object id.
    object_id: i64,
    /// The live value it drives.
    target: PropTarget,
    /// `Some((effect idx, constant name))` when this script drives an effect
    /// pass's `constantshadervalues` entry (the rainbow-coloring pattern) —
    /// results then route through the material-op seam, not a PropUpdate.
    effect_constant: Option<(usize, String)>,
}

/// A typed property update the render loop applies to its live objects
/// (docs/scripting-api.md §5.1 / §8).
#[derive(Clone, Debug, PartialEq)]
pub struct PropUpdate {
    /// Target object id.
    pub object_id: i64,
    /// Which live value to write.
    pub target: PropTarget,
    /// The new value (as returned by the script).
    pub value: ScriptValue,
}

/// The merged runtime camera override a frame's `thisScene.setCameraTransforms`
/// calls produced (reference `scene_set_camera_transforms`,
/// `Scripting/SceneObject.cpp:261-286`): each field is independent — the
/// reference only writes the members present on the argument object, so a
/// partial call leaves the others at their current (possibly earlier-overridden)
/// value. Multiple calls in one tick merge last-wins per field, exactly as the
/// reference's sequential setter calls would.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraOp {
    /// New eye position, if overridden.
    pub eye: Option<[f32; 3]>,
    /// New look-at center, if overridden.
    pub center: Option<[f32; 3]>,
    /// New up vector, if overridden.
    pub up: Option<[f32; 3]>,
    /// New vertical fov (degrees), if overridden.
    pub fov: Option<f32>,
    /// New 2D zoom multiplier, if overridden (CameraTransforms.zoom).
    pub zoom: Option<f32>,
}

/// Merge one `SetCameraTransforms` op into the pending override (last-wins per
/// field — mirrors the reference applying each present member immediately,
/// `SceneObject.cpp:272-286`).
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

/// The per-scene script host: the engine plus the frame snapshot it re-marshals
/// each tick.
pub struct ScriptHost {
    engine: ScriptEngine,
    props: Vec<ScriptedProp>,
    /// Every scene object as a `thisScene` layer, in render order, kept updated
    /// as ops/results apply so next frame's `thisLayer`/`getLayer` reads reflect
    /// the mutations (docs §4.1/§6.2). A script that iterates `getLayerCount()`
    /// and toggles group visibility (the layer-switcher pattern) needs the whole
    /// scene here, not just the scripted leaves.
    layers: Vec<LayerState>,
    scene: SceneState,
    /// `engine.userProperties` — resolved project-property values (combo/slider/
    /// bool/…) the scripts branch on (docs §6.1). Populated from the property bag
    /// so `mode_combo`/`style_*`/… drive the same visibility the oracle shows.
    user_props: BTreeMap<String, ScriptValue>,
    res: [f32; 2],
    elapsed: f64,
    /// Runtime layers created by scripts this session (`thisScene.createLayer`),
    /// drained by the renderer via [`Self::take_created`]: (synthetic id, path).
    created: Vec<(i64, String)>,
    /// Layer ids destroyed by scripts (`thisScene.destroyLayer`), drained via
    /// [`Self::take_destroyed`].
    destroyed: Vec<i64>,
    /// Particle playback/instance ops, drained via
    /// [`Self::take_particle_ops`].
    particle_ops: Vec<ParticleOp>,
    /// Live material-constant writes `(layer, effect idx, name, value)`,
    /// drained via [`Self::take_material_ops`].
    material_ops: Vec<(i64, usize, String, kirie_script::ScriptValue)>,
    /// Video playback commands `(layer, cmd, value)`, drained via
    /// [`Self::take_video_ops`].
    video_ops: Vec<(i64, String, f64)>,
    /// True when any loaded script mentions a media event export — the
    /// renderer only starts the MPRIS worker for scenes that listen.
    wants_media: bool,
    /// Previous media snapshot digest, for per-category change detection.
    media_prev: Option<MediaPrev>,
    /// Pending runtime camera override (`thisScene.setCameraTransforms`),
    /// merged across the tick's ops, drained via [`Self::take_camera`].
    camera_op: Option<CameraOp>,
    /// True when a `sortLayer` op reordered [`Self::layers`] since the renderer
    /// last drained the order via [`Self::take_layer_order`].
    order_dirty: bool,
    /// Applied `setParent` ops — `(layer id, new parent id)` — drained via
    /// [`Self::take_parent_updates`].
    parent_updates: Vec<(i64, i64)>,
    /// True when [`Self::scene`] changed since the retained frame last copied it
    /// (a script fov override — reference `getCameraTransforms` reports the
    /// overridden fov via `getFov()`, `SceneObject.cpp:257`): the per-tick
    /// refresh must re-clone the scene state.
    scene_dirty: bool,
    /// The retained [`HostFrame`] recycled across ticks: sent boxed to the
    /// script thread and handed back with the output
    /// ([`ScriptEngine::tick_reuse`]), so the audio band buffers, the layer
    /// snapshot vector (and its per-layer strings) and the user-prop map are
    /// reused in place instead of being re-allocated every frame. `None` only
    /// before the first tick or after a failed round-trip (the box is lost with
    /// the channel); the next tick rebuilds it from scratch.
    frame: Option<Box<HostFrame>>,
    /// True when [`Self::user_props`] changed since the retained frame last
    /// copied it (a live `setProperty`) — the only time the per-tick refresh
    /// must re-clone the map.
    user_props_dirty: bool,
    /// Wall-clock instant of the previous tick, for the `dt == 0` frametime
    /// fallback (see [`Self::tick`]). Starts at host creation, mirroring the
    /// reference where the first frame's `frametime` is the startup elapsed.
    last_tick: std::time::Instant,
    /// Local UTC offset in seconds, captured once at build for
    /// `engine.timeOfDay` (reference reads the OS local clock). A DST flip
    /// mid-run leaves it stale until the next scene build — acceptable for a
    /// day-fraction that scripts use for day/night cycles.
    tz_offset_secs: f64,
}

impl ScriptHost {
    /// Build a host for `model` if it has any driveable property script;
    /// `None` when the scene has no scripts (the common case) or the engine
    /// fails to start (SPEC.md §V9: scripts are best-effort, never fatal).
    #[must_use]
    pub fn build(
        model: &SceneModel,
        res: (u32, u32),
        user_props: &[(String, PropertyValue)],
    ) -> Option<Self> {
        // Collect every driveable scripted leaf first (source + initial value +
        // flattened props), so an empty set skips the QuickJS thread spawn.
        // Every object is also snapshotted as a `thisScene` layer (in render
        // order) so scripts can enumerate/toggle layers they do not directly
        // drive (docs §6.2 `getLayer`/`getLayerCount`).
        let mut pending: Vec<Pending> = Vec::new();
        let mut layers: Vec<LayerState> = Vec::with_capacity(model.scene.objects.len());
        // RENDER order (docs §5.7: declaration order, stable-sorted by
        // `sortorder` under `general.customsortorder` — same walk as the item
        // build): the getLayer(i)/sortLayer index space is the scriptable
        // subset of the *render* order (`CScene::getScriptableLayerIndex`,
        // CScene.cpp:524-536), not declaration order.
        let mut render_order: Vec<usize> = (0..model.scene.objects.len()).collect();
        if model.scene.general.customsortorder {
            render_order.sort_by_key(|&i| model.scene.objects[i].base.sortorder);
        }
        for &oi in &render_order {
            let object = &model.scene.objects[oi];
            let id = object.base.id;
            layers.push(layer_state(object));
            // Base transform leaves exist on every kind (docs §7.1) — a
            // cursor-follow script on `origin` or a spin script on `angles`
            // must load regardless of the object's kind.
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
            // Kinds below register their own `visible` leaf; for the rest
            // (models, groups, lights) the base-level script is the only one.
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
                    // Effect-pass constant scripts (the rainbow-coloring
                    // pattern: a binding on an effect override's
                    // constantshadervalues entry drives that shader constant
                    // per tick). Routed through the live setMaterialProperty
                    // seam by effect index + constant name.
                    for (ei, eff) in img.effects.iter().enumerate() {
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
                }
                _ => {}
            }
        }

        // Text-layer scripts (WE clock/date drivers on a text object's `text`
        // setting, docs §7.2) also need the engine — a scene with only those
        // and no property scripts must still spawn it.
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

        // Media-listener detection (string scan; the exact export detection
        // happens at module load in kirie-script — this only gates whether
        // the renderer starts the MPRIS worker at all).
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
        // localStorage identity: the wallpaper's workshop id (from the editor's
        // `__workshopId` header). Scenes without one keep memory-only storage.
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
        // One-shot per scene load (not per frame): INFO like the audio-capture
        // startup line, so a live run shows the script surface came up.
        tracing::info!(scripts = props.len(), "scene script host started");

        // Persistent localStorage (docs: settings survive restarts), keyed by
        // workshop id under the user cache.
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
            video_ops: Vec::new(),
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
        })
    }

    /// Create a text-layer script (a WE clock/date driver on a text object,
    /// docs §7.2). Returns the layer handle, `None` on failure (the text then
    /// just keeps its literal string — V9 degrade).
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

    /// Tick a text-layer script and read back its current rendered string
    /// (docs §7.2: deferred `init`, then `update(text)`).
    pub fn tick_text_layer(&mut self, handle: u32, time: f64, dt: f64) -> Option<String> {
        let _ = self.engine.tick_layer(handle, time, dt, 60.0);
        self.engine.layer_text(handle).ok()
    }

    /// Advance one frame: marshal a [`HostFrame`], tick the engine, and return
    /// the typed property updates to apply. `dt` is seconds since last frame;
    /// `audio` is the latest mono spectrum. All three band reductions are
    /// marshalled so `registerAudioBuffers(16|32|64)` each read their matching
    /// array (docs/scripting-api.md §6.1). Never blocks the render thread beyond
    /// the bounded channel round-trip (SPEC.md §V3/§V4) and never panics on a
    /// throwing script (§V9).
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
        // Recycle the retained frame (the common case) instead of marshalling a
        // fresh snapshot: only the parts that can actually change since last
        // tick are rewritten, in place. First tick (or after a lost round-trip)
        // builds it from scratch.
        let mut frame = match self.frame.take() {
            Some(f) => f,
            None => {
                let mut f = Box::new(HostFrame::default());
                // Refreshed below only when dirtied (fov override / setProperty).
                f.scene = self.scene.clone();
                f.user_props = self.user_props.clone();
                self.user_props_dirty = false;
                self.scene_dirty = false;
                f
            }
        };
        frame.runtime = self.elapsed;
        // Reference `engine.frametime` is `g_Time - g_TimeLast` off the wall
        // clock (WallpaperApplication.cpp:900-908, EngineObject.cpp:76-78), so
        // it is positive even on the first frame (startup elapsed) and on
        // re-render ticks the platform drives with `dt = 0` (live screenshot
        // capture). A literal 0 NaN-poisons scripts that normalize by
        // `frametime * (1 / frametime)` (Starscape's camera driver dies from
        // frame 2 onward), so fall back to real elapsed time when the render
        // loop didn't provide a delta.
        let now = std::time::Instant::now();
        let wall = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        frame.frametime = if dt > 0.0 { f64::from(dt) } else { wall.max(1e-4) };
        frame.now = self.elapsed * 1000.0;
        frame.res_x = f64::from(self.res[0]);
        frame.res_y = f64::from(self.res[1]);
        // Platform-fed pointer (T26), surface-normalized [0,1] top-left.
        frame.pointer_screen = pointer;
        // `input.cursorWorldPosition` — scene-space px (top-left origin), the
        // same coordinate space object origins live in, so scripts can compare
        // or assign it directly (`thisLayer.origin = input.cursorWorldPosition`).
        frame.pointer_world = [pointer_scene[0], pointer_scene[1], 0.0];
        frame.pointer_left_down = pointer_left;
        // `engine.timeOfDay` — local wall-clock day fraction ∈ [0,1).
        frame.time_of_day = day_fraction(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
            self.tz_offset_secs,
        );
        // Media events: `Some` only on ticks where a category changed.
        frame.media = media.and_then(|st| media_frame(st, &mut self.media_prev));
        // Audio bands land in the retained buffers (clear + extend, no allocs
        // once warm). Presence is stable per run, so the None arm's drop of the
        // warm buffers is a one-off transition, not churn.
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
        // Layers mutate between ticks (record_layer) — refresh via `clone_from`
        // so the vector and each layer's strings keep their allocations.
        frame.layers.clone_from(&self.layers);
        // User props only change on a live `setProperty` (apply_user_property).
        if self.user_props_dirty {
            frame.user_props.clone_from(&self.user_props);
            self.user_props_dirty = false;
        }
        // Scene state only changes on a script fov override (see `scene_dirty`).
        if self.scene_dirty {
            frame.scene.clone_from(&self.scene);
            self.scene_dirty = false;
        }

        let output = match self.engine.tick_reuse(frame, Vec::new()) {
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

    /// Fire `applyUserProperties({key: value})` on the scene's scripts for a live
    /// `setProperty` (docs §5.3) and return the resulting property updates. Also
    /// refreshes the cached `engine.userProperties` so later `update()` ticks see
    /// the new value. This is what makes a **script-driven** property (e.g. a
    /// `coloring` combo that recolors the scene) update live, not just direct
    /// material/camera/general bindings.
    pub fn apply_user_property(&mut self, key: &str, value: &kirie_scene::PropertyValue) -> Vec<PropUpdate> {
        let sv = prop_to_script(value);
        self.user_props.insert(key.to_owned(), sv.clone());
        // The retained frame's copy of `engine.userProperties` is now stale;
        // the next tick re-clones it (and only then — see `user_props_dirty`).
        self.user_props_dirty = true;
        match self.engine.dispatch_user_property(key.to_owned(), sv) {
            Ok(output) => self.process_output(output),
            Err(e) => {
                tracing::warn!(error = %e, "script user-property dispatch failed (unchanged)");
                Vec::new()
            }
        }
    }

    /// Drain runtime layers scripts created since the last call: (id, path).
    pub fn take_created(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.created)
    }

    /// Turn a script [`TickOutput`] into the typed [`PropUpdate`]s to apply,
    /// updating the cached layer snapshot as it goes. Shared by [`Self::tick`] and
    /// [`Self::apply_user_property`].
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
        // Property-script results (docs §5.1: update()'s return drives the prop).
        for (key, value) in output.property_results {
            let Some(prop) = self.props.iter().find(|p| p.key == key) else {
                continue;
            };
            if let Some((ei, cname)) = &prop.effect_constant {
                // Effect-constant scripts route through the material seam —
                // the same live path getEffect().setMaterialProperty takes.
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
        // Imperative scene ops (docs §6.2/§8): leaf writes, runtime layers,
        // camera overrides.
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
                SceneOp::CreateLayer { layer_id, path, .. } => {
                    // Mirror host.js's synthetic record into the retained layer
                    // list so next tick's marshal keeps the JS proxy readable
                    // (the marshal overwrites `__host.layers` wholesale) and the
                    // new layer enters the sort space at the end — the reference
                    // appends created layers to the render order (top).
                    self.layers.push(LayerState {
                        id: layer_id,
                        name: path.clone(),
                        origin: Some([0.0; 3]),
                        scale: Some([1.0; 3]),
                        angles: Some([0.0; 3]),
                        visible: Some(true),
                        alpha: Some(1.0),
                        color: Some([1.0; 3]),
                        ..LayerState::default()
                    });
                    self.created.push((layer_id, path));
                }
                SceneOp::SetCameraTransforms {
                    eye,
                    center,
                    up,
                    fov,
                    zoom,
                } => {
                    merge_camera(&mut self.camera_op, eye, center, up, fov, zoom);
                    // `getCameraTransforms` reports the BASE eye/center/up but
                    // the *overridden* fov (`getBaseEye`/`getFov`,
                    // `SceneObject.cpp:252-257`) — mirror only fov into the
                    // scene snapshot scripts read next tick.
                    if let Some(f) = fov {
                        self.scene.camera.fov = f;
                        self.scene.fov = f;
                        self.scene_dirty = true;
                    }
                }
                SceneOp::SortLayer { layer_id, index } => {
                    // The host's layer list is the authoritative scriptable
                    // order (getLayer(i) indexes it); apply the reference move
                    // here, then hand the renderer the new order to mirror.
                    if sort_layer_apply(&mut self.layers, layer_id, index) {
                        self.order_dirty = true;
                    }
                }
                SceneOp::SetParent { layer_id, parent } => {
                    // Keep the retained snapshot in step with host.js, which
                    // already updated its own record this tick (including a
                    // null detach — the JS-side read shows it).
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
                    // Drop the snapshot record — next tick's marshal no longer
                    // carries it, so every proxy read returns null from here on
                    // (the d.ts end-of-frame removal). The renderer drains the
                    // id via take_destroyed.
                    self.layers.retain(|l| l.id != layer_id);
                    self.destroyed.push(layer_id);
                }
            }
        }
        updates
    }

    /// Drain the layer ids destroyed by scripts this tick
    /// (`thisScene.destroyLayer`).
    pub fn take_destroyed(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.destroyed)
    }

    /// Drain the tick's particle ops (`play`/`pause`/`stop`/`emitParticles`/
    /// `instance.*` writes), for the renderer to route into each system's sim.
    pub fn take_particle_ops(&mut self) -> Vec<ParticleOp> {
        std::mem::take(&mut self.particle_ops)
    }

    /// Sync the scene snapshot's camera fov after a live property change.
    /// The reference's `getCameraTransforms` reports the CURRENT fov
    /// (`getFov`, SceneObject.cpp:257) — camera scripts echo it back through
    /// `setCameraTransforms` every tick, so a stale snapshot value would
    /// clobber a user's fov slider on the very next frame.
    pub fn set_scene_fov(&mut self, fov: f32) {
        if (self.scene.camera.fov - fov).abs() > f32::EPSILON {
            self.scene.camera.fov = fov;
            self.scene.fov = fov;
            self.scene_dirty = true;
        }
    }

    /// Drain the tick's video playback commands.
    pub fn take_video_ops(&mut self) -> Vec<(i64, String, f64)> {
        std::mem::take(&mut self.video_ops)
    }

    /// Drain the tick's `setMaterialProperty` writes.
    pub fn take_material_ops(&mut self) -> Vec<(i64, usize, String, kirie_script::ScriptValue)> {
        std::mem::take(&mut self.material_ops)
    }

    /// Whether any loaded script mentions a media event export — the renderer
    /// starts the MPRIS worker only for scenes that listen.
    #[must_use]
    pub fn wants_media(&self) -> bool {
        self.wants_media
    }

    /// Drain the tick's merged runtime camera override, if any
    /// (`thisScene.setCameraTransforms`). The renderer applies it to the live
    /// perspective camera the 3D models re-read every frame; the 2D
    /// orthographic screen MVP is deliberately untouched (reference
    /// `Camera.h:24-26`).
    pub fn take_camera(&mut self) -> Option<CameraOp> {
        self.camera_op.take()
    }

    /// Drain the full scriptable-layer order (ids, bottom → top) when a
    /// `sortLayer` op reordered it this tick; `None` when unchanged. The
    /// renderer stable-sorts its drawable items to these relative positions and
    /// renumbers runtime layers, mirroring the reference's single reordered
    /// `m_objectsByRenderOrder` (`CScene::moveLayerToScriptableIndex`,
    /// CScene.cpp:538-562).
    pub fn take_layer_order(&mut self) -> Option<Vec<i64>> {
        if !self.order_dirty {
            return None;
        }
        self.order_dirty = false;
        Some(self.layers.iter().map(|l| l.id).collect())
    }

    /// Drain the tick's applied `setParent` ops: `(layer id, new parent id)`.
    /// The renderer feeds these into the ancestor-visibility gate so hiding the
    /// new parent group hides the reparented layer (docs §7.1).
    pub fn take_parent_updates(&mut self) -> Vec<(i64, i64)> {
        std::mem::take(&mut self.parent_updates)
    }

    /// Keep the cached layer snapshot in step with an applied value so next
    /// frame's `thisLayer` reads reflect it (docs §4.1).
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
            // Brightness is not a registered `thisLayer` property (docs §4.1),
            // and a particle rate has no layer mirror either.
            PropTarget::Brightness | PropTarget::ParticleRate => {}
        }
    }
}

/// A `constantshadervalues` literal as the script's initial value.
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

/// Extract the editor-stamped `__workshopId = '123'` literal from a script
/// source (the same convention the runtime-template collector matches).
fn extract_workshop_id(source: &str) -> Option<String> {
    let i = source.find("__workshopId")?;
    let rest = &source[i..];
    let open = rest.find(['\'', '"'])?;
    let quote = rest.as_bytes()[open] as char;
    let close = rest[open + 1..].find(quote)?;
    let id = &rest[open + 1..open + 1 + close];
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_owned())
}

/// Digest of the previous tick's media snapshot, for change detection.
struct MediaPrev {
    available: bool,
    state: i32,
    title: String,
    artist: String,
    album: String,
    /// `Arc::as_ptr` of the decoded art (0 = none) — stable per track, the
    /// same identity key the web feed's art cache uses.
    art_key: usize,
    position: f64,
}

/// Project a media snapshot into this tick's [`MediaFrame`], comparing against
/// `prev` per category. `None` when nothing changed (the common case). The
/// first snapshot fires every category so scripts receive the initial state.
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
    // Palette: primary/secondary/text/high-contrast come derived; the d.ts
    // also wants a tertiary — serve the secondary again (closest derived
    // swatch; the reference's third swatch is another luminance step).
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

/// `#rrggbb` → RGB floats ∈ [0,1].
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

/// One drained particle op (see [`ScriptHost::take_particle_ops`]).
pub enum ParticleOp {
    /// `play`/`pause`/`stop`.
    Command {
        /// Target object id.
        id: i64,
        /// The command name.
        cmd: String,
    },
    /// `emitParticles(count)`.
    Emit {
        /// Target object id.
        id: i64,
        /// Particles to spawn immediately.
        count: u32,
    },
    /// `instance.<name> = value`.
    Instance {
        /// Target object id.
        id: i64,
        /// Instance property (scalar name, `colorn`, or `controlpointN`).
        name: String,
        /// The written value.
        value: kirie_script::ScriptValue,
    },
}

/// The render-side effect of one `setParent` op. The reference applies only a
/// resolved, NON-self parent (`layer_set_parent`,
/// `ScriptableObjectAdapter.cpp:226-229`: `if (parentId >= 0 && parentId !=
/// self->getId ())`): a null or unresolvable argument leaves the C++ object
/// untouched — there is no detach path — and self-parenting is ignored.
fn parent_update(layer_id: i64, parent: Option<i64>) -> Option<(i64, i64)> {
    match parent {
        Some(p) if p != layer_id => Some((layer_id, p)),
        _ => None,
    }
}

/// `thisScene.sortLayer` — the reference's `CScene::moveLayerToScriptableIndex`
/// (CScene.cpp:538-562) over the script layer list: remove the layer, then
/// re-insert just before the layer now at `index`; a negative or past-the-end
/// index appends (top). Every kirie script layer is scriptable, so the
/// "index-th scriptable layer" of the reference is simply position `index`.
/// Returns `false` (list untouched) when `layer_id` is unknown — the reference
/// returns early when the layer is not in its render order (CScene.cpp:540-543).
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

/// Flatten a `scriptproperties` map to the effective values the engine's
/// `createScriptProperties().finish()` reads: each entry may be a
/// `{ "value": ... }` user setting (docs/scripting-api.md §5.5).
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

/// A scripted leaf staged before the engine is spawned.
struct Pending {
    prop: ScriptedProp,
    source: String,
    initial: ScriptValue,
    script_props: Value,
}

/// Stage a scripted leaf field if it carries a `"script"` binding we drive.
/// `initial` is evaluated lazily so unscripted fields cost nothing.
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

/// Snapshot an object's registered `thisLayer` properties (docs §4.1).
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
    // `solid` opts the layer into cursor events (d.ts ILayer.solid); it is a
    // kind-agnostic editor flag preserved verbatim in `extra` (§7.5).
    ls.solid = object.extra.get("solid").and_then(serde_json::Value::as_bool);
    match &object.kind {
        ObjectKind::Image(img) => {
            ls.color = Some([img.color.value[0], img.color.value[1], img.color.value[2]]);
            ls.alpha = Some(img.alpha.value);
            ls.visible = Some(img.visible.value && base.visible.value);
            ls.size = Some(img.size);
            // Effect handles (d.ts IEffectLayer): name + material-pass count
            // per authored effect, the getEffect/getMaterialCount surface.
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

/// Snapshot scene-level read-only members (docs §6.2 `thisScene`).
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
        // SceneState reports these as ints (docs §6.2 `thisScene.bloomstrength`).
        bloomstrength: g.bloomstrength.value as i64,
        bloomthreshold: g.bloomthreshold.value as i64,
        fov: cam.fov.value,
        nearz: cam.nearz,
        farz: cam.farz,
        // The authored (BASE) camera: `getCameraTransforms` must return this
        // stable base each frame, never a runtime override — a camera-controller
        // script recomputes from it and would drift if fed its own output
        // (reference `Camera.h:35-38`, `SceneObject.cpp:252-256`). Only the fov
        // member tracks the override (`getFov`, `SceneObject.cpp:257`).
        camera: kirie_script::CameraState {
            eye: cam.eye,
            center: cam.center,
            up: cam.up,
            fov: cam.fov.value,
        },
        ..SceneState::default()
    }
}

/// A scene `Color` ([r,g,b,a]) as the `Vec3` a color script expects.
fn color_value(c: [f32; 4]) -> ScriptValue {
    ScriptValue::Vec3([c[0], c[1], c[2]])
}

/// A resolved project property as the `engine.userProperties.<name>` value a
/// script reads (docs §6.1): combos/text as strings (`==`-compared),
/// sliders as numbers, bools as bools, colors as an RGB vec3.
fn prop_to_script(v: &PropertyValue) -> ScriptValue {
    match v {
        PropertyValue::Bool(b) => ScriptValue::Bool(*b),
        PropertyValue::Number(n) => ScriptValue::Float(*n),
        PropertyValue::Color([r, g, b, _]) => ScriptValue::Vec3([*r, *g, *b]),
        PropertyValue::Combo(s) | PropertyValue::Text(s) => ScriptValue::Str(s.clone()),
    }
}

/// Coerce a script value to a scalar (alpha/brightness).
pub fn as_f32(v: &ScriptValue) -> Option<f32> {
    match v {
        ScriptValue::Float(f) => Some(*f as f32),
        ScriptValue::Int(i) => Some(*i as f32),
        ScriptValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Coerce a script value to a vec3 (origin/scale/angles).
pub fn as_vec3(v: &ScriptValue) -> Option<[f32; 3]> {
    match v {
        ScriptValue::Vec3(a) => Some(*a),
        // Vec2-typed writes (parallaxDepth) extend with a zero Z.
        ScriptValue::Vec2(a) => Some([a[0], a[1], 0.0]),
        ScriptValue::Float(f) => Some([*f as f32; 3]),
        _ => None,
    }
}

/// Coerce a script value to an RGB triple (color).
pub fn as_rgb(v: &ScriptValue) -> Option<[f32; 3]> {
    match v {
        ScriptValue::Vec3(c) => Some(*c),
        ScriptValue::Vec4(c) => Some([c[0], c[1], c[2]]),
        ScriptValue::Vec2(c) => Some([c[0], c[1], 0.0]),
        ScriptValue::Float(f) => Some([*f as f32; 3]),
        _ => None,
    }
}

/// Local-time day fraction ∈ [0,1) from a UTC unix timestamp and a fixed
/// UTC offset. Pure so the wrap/negative-offset arithmetic is testable.
fn day_fraction(unix_secs: f64, tz_offset_secs: f64) -> f64 {
    ((unix_secs + tz_offset_secs) / 86_400.0).rem_euclid(1.0)
}

/// The system's UTC offset in seconds, via `date +%z` (one spawn per scene
/// build — the crate is `forbid(unsafe_code)`, so `localtime_r` is out, and a
/// timezone crate is a heavy dependency for one number). Falls back to UTC
/// when the spawn or parse fails.
fn local_utc_offset_secs() -> f64 {
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_utc_offset(s.trim()))
        .unwrap_or(0.0)
}

/// Parse an RFC-822 style numeric offset (`+0200`, `-0530`, `+00`) to seconds.
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
        // Midnight UTC at +02:00 is 02:00 local.
        let f = day_fraction(86_400.0 * 3.0, 7200.0);
        assert!((f - 2.0 / 24.0).abs() < 1e-9);
        // 01:00 UTC at -05:30 wraps back into the previous local day (19:30).
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

    /// Move down: remove-then-insert means the target position is counted in
    /// the list *without* the moved layer (CScene.cpp:544-561).
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

    /// A negative index appends (top) — the reference leaves `insertPos` at
    /// `end()` (CScene.cpp:546-548).
    #[test]
    fn sort_layer_negative_index_appends() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 10, -1));
        assert_eq!(ids(&layers), [20, 30, 10]);
    }

    /// A past-the-end index appends too — the reference's walk runs out of
    /// scriptable layers before reaching `index` (CScene.cpp:549-560).
    #[test]
    fn sort_layer_past_end_appends() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 20, 99));
        assert_eq!(ids(&layers), [10, 30, 20]);
    }

    /// An unknown id leaves the list untouched (CScene.cpp:540-543).
    #[test]
    fn sort_layer_unknown_id_is_a_noop() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(!sort_layer_apply(&mut layers, 77, 0));
        assert_eq!(ids(&layers), [10, 20, 30]);
    }

    /// Re-inserting at the layer's own position is stable.
    #[test]
    fn sort_layer_same_position_is_stable() {
        let mut layers = layer_list(&[10, 20, 30]);
        assert!(sort_layer_apply(&mut layers, 20, 1));
        assert_eq!(ids(&layers), [10, 20, 30]);
    }

    /// The reference applies only a resolved, non-self parent
    /// (`ScriptableObjectAdapter.cpp:226-229`): null (kirie's `None`) has no
    /// C++-side effect — there is no detach path — and self-parenting is
    /// ignored.
    #[test]
    fn set_parent_filters_like_the_reference() {
        assert_eq!(parent_update(3, Some(7)), Some((3, 7)));
        assert_eq!(parent_update(3, Some(3)), None); // self-parent ignored
        assert_eq!(parent_update(3, None), None); // null detach is a no-op
    }

    /// Partial `setCameraTransforms` calls merge per field, last-wins — the
    /// reference applies each present member as it lands
    /// (`SceneObject.cpp:272-286`), so two calls in one tick behave like the
    /// second overwriting only the fields it names.
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
