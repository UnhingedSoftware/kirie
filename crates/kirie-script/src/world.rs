use std::collections::BTreeMap;

use rquickjs::loader::{BuiltinLoader, BuiltinResolver};
use rquickjs::{Array, CatchResultExt, Context, Ctx, Function, Module, Object, Runtime, Value};

use crate::error::ScriptError;
use crate::frame::{HostFrame, LayerState, LogLine, SceneOp, TickOutput};
use crate::value::{ScriptValue, json_to_js};

const BUILTINS_JS: &str = include_str!("js/builtins.js");
const HOST_JS: &str = include_str!("js/host.js");
const WE_MATH_JS: &str = include_str!("js/we_math.js");
const WE_COLOR_JS: &str = include_str!("js/we_color.js");
const WE_VECTOR_JS: &str = include_str!("js/we_vector.js");

type ModuleTickState = (String, Option<i64>, bool, ScriptValue, Option<String>, u8, u8);

const F_CURSOR_MOVE: u8 = 1;
const F_CURSOR_ENTER: u8 = 2;
const F_CURSOR_LEAVE: u8 = 4;
const F_CURSOR_DOWN: u8 = 8;
const F_CURSOR_UP: u8 = 16;
const F_CURSOR_CLICK: u8 = 32;

const CURSOR_EXPORTS: [(u8, &str); 6] = [
    (F_CURSOR_MOVE, "cursorMove"),
    (F_CURSOR_ENTER, "cursorEnter"),
    (F_CURSOR_LEAVE, "cursorLeave"),
    (F_CURSOR_DOWN, "cursorDown"),
    (F_CURSOR_UP, "cursorUp"),
    (F_CURSOR_CLICK, "cursorClick"),
];

const F_MEDIA_STATUS: u8 = 1;
const F_MEDIA_PLAYBACK: u8 = 2;
const F_MEDIA_PROPERTIES: u8 = 4;
const F_MEDIA_THUMBNAIL: u8 = 8;
const F_MEDIA_TIMELINE: u8 = 16;

const MEDIA_EXPORTS: [(u8, &str); 5] = [
    (F_MEDIA_STATUS, "mediaStatusChanged"),
    (F_MEDIA_PLAYBACK, "mediaPlaybackChanged"),
    (F_MEDIA_PROPERTIES, "mediaPropertiesChanged"),
    (F_MEDIA_THUMBNAIL, "mediaThumbnailChanged"),
    (F_MEDIA_TIMELINE, "mediaTimelineChanged"),
];

#[derive(Default)]
struct CursorState {
    prev: Option<([f32; 3], bool)>,
    hit: std::collections::HashMap<i64, bool>,
    down_on: std::collections::HashSet<i64>,
}

struct ModuleMeta {
    owner_id: Option<i64>,
    inited: bool,
    current: ScriptValue,
    workshop_id: Option<String>,
    cursor_exports: u8,
    media_exports: u8,
}

pub const SCRIPT_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
pub const SCRIPT_MEMORY_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone)]
struct Deadline(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl Deadline {
    fn new() -> Self {
        Deadline(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)))
    }

    fn arm(&self, budget: std::time::Duration) {
        let at = std::time::Instant::now() + budget;
        let micros = at
            .duration_since(*START)
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX);
        self.0.store(micros, std::sync::atomic::Ordering::Relaxed);
    }

    fn disarm(&self) {
        self.0.store(u64::MAX, std::sync::atomic::Ordering::Relaxed);
    }

    fn expired(&self) -> bool {
        let at = self.0.load(std::sync::atomic::Ordering::Relaxed);
        at != u64::MAX
            && u64::try_from(std::time::Instant::now().duration_since(*START).as_micros())
                .unwrap_or(u64::MAX)
                > at
    }
}

static START: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);

pub struct World {
    _runtime: Runtime,
    deadline: Deadline,
    context: Context,
    modules: BTreeMap<String, ModuleMeta>,
    order: Vec<String>,
    cursor: CursorState,
    prev_res: Option<[f64; 2]>,
    language: String,
}

struct DeadlineGuard(Deadline);

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.0.disarm();
    }
}

impl World {
    pub fn new() -> Result<Self, ScriptError> {
        let runtime = Runtime::new().map_err(|e| ScriptError::Internal(e.to_string()))?;
        let deadline = Deadline::new();
        let watch = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || watch.expired())));
        runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT);
        let resolver = BuiltinResolver::default()
            .with_module("WEMath")
            .with_module("WEColor")
            .with_module("WEVector");
        let loader = BuiltinLoader::default()
            .with_module("WEMath", WE_MATH_JS)
            .with_module("WEColor", WE_COLOR_JS)
            .with_module("WEVector", WE_VECTOR_JS);
        runtime.set_loader(resolver, loader);
        let context = Context::full(&runtime).map_err(|e| ScriptError::Internal(e.to_string()))?;

        context
            .with(|ctx| -> Result<(), ScriptError> {
                eval_global(&ctx, "<builtins>", BUILTINS_JS)?;
                eval_global(&ctx, "<host>", HOST_JS)?;
                Ok(())
            })
            .map_err(|e| match e {
                ScriptError::Load { message, .. } => ScriptError::Internal(message),
                other => other,
            })?;

        Ok(World {
            _runtime: runtime,
            deadline,
            context,
            modules: BTreeMap::new(),
            order: Vec::new(),
            cursor: CursorState::default(),
            prev_res: None,
            language: system_language(),
        })
    }

    pub fn load_property_script(
        &mut self,
        key: &str,
        source: &str,
        owner_id: Option<i64>,
        initial: ScriptValue,
        script_properties: &serde_json::Value,
    ) -> Result<(), ScriptError> {
        if self.modules.contains_key(key) {
            return Ok(());
        }
        let key_owned = key.to_owned();
        let loaded = self
            .context
            .with(|ctx| -> Result<(Option<String>, u8, u8), ScriptError> {
                let host: Object = global(&ctx, "__host")?;
                host.set("scriptProps", json_to_js(&ctx, script_properties).internal()?)
                    .internal()?;

                let module = Module::declare(ctx.clone(), key_owned.clone(), source)
                    .catch(&ctx)
                    .map_err(|e| ScriptError::Load {
                        key: key_owned.clone(),
                        message: e.to_string(),
                    })?;
                let (module, _promise) = module.eval().catch(&ctx).map_err(|e| ScriptError::Load {
                    key: key_owned.clone(),
                    message: e.to_string(),
                })?;
                drain_jobs(&ctx);
                let namespace = module.namespace().internal()?;
                let register: Function = global(&ctx, "__registerModule")?;
                register
                    .call::<_, ()>((key_owned.clone(), namespace.clone()))
                    .internal()?;
                let workshop_id: Option<String> = namespace.get("__workshopId").ok();
                let mut cursor_exports = 0u8;
                for (bit, name) in CURSOR_EXPORTS {
                    let f: Option<Function> = namespace.get(name).ok();
                    if f.is_some() {
                        cursor_exports |= bit;
                    }
                }
                let mut media_exports = 0u8;
                for (bit, name) in MEDIA_EXPORTS {
                    let f: Option<Function> = namespace.get(name).ok();
                    if f.is_some() {
                        media_exports |= bit;
                    }
                }
                Ok((workshop_id, cursor_exports, media_exports))
            })?;
        let (workshop_id, cursor_exports, media_exports) = loaded;

        self.modules.insert(
            key.to_owned(),
            ModuleMeta {
                owner_id,
                inited: false,
                current: initial,
                workshop_id,
                cursor_exports,
                media_exports,
            },
        );
        self.order.push(key.to_owned());
        Ok(())
    }

    pub fn tick(&mut self, frame: &HostFrame, overrides: &[(String, ScriptValue)]) -> TickOutput {
        self.deadline.arm(SCRIPT_BUDGET);
        let _guard = DeadlineGuard(self.deadline.clone());
        for (k, v) in overrides {
            if let Some(m) = self.modules.get_mut(k) {
                m.current = v.clone();
            }
        }
        let metas: Vec<ModuleTickState> = self
            .order
            .iter()
            .filter_map(|k| self.modules.get(k).map(|m| (k, m)))
            .map(|(k, m)| {
                (
                    k.clone(),
                    m.owner_id,
                    m.inited,
                    m.current.clone(),
                    m.workshop_id.clone(),
                    m.cursor_exports,
                    m.media_exports,
                )
            })
            .collect();

        let cursor = &mut self.cursor;
        let res = [frame.res_x, frame.res_y];
        let res_changed = self.prev_res.is_some_and(|r| r != res);
        self.prev_res = Some(res);
        let language = self.language.clone();
        let (results, mut out) = self.context.with(|ctx| {
            let mut out = TickOutput::default();
            if let Err(e) = apply_frame(&ctx, frame) {
                out.errors.push(e);
            }
            let _ = call_void(&ctx, "__snapshotInitialLayers", ());
            let _ = call_void(&ctx, "__tickTimers", ());
            let _ = call_void(&ctx, "__flushStorage", ());
            dispatch_cursor(&ctx, &metas, frame, cursor, &mut out);
            dispatch_media(&ctx, &metas, frame, &mut out);

            for (key, owner, inited, current, workshop, _, _) in &metas {
                if *inited {
                    continue;
                }
                bind_this_layer(&ctx, *owner, key);
                set_workshop_id(&ctx, workshop.as_deref());
                let arg = match current.to_js(&ctx) {
                    Ok(v) => v,
                    Err(e) => {
                        out.errors.push(ScriptError::Internal(e.to_string()));
                        continue;
                    }
                };
                if let Err(msg) = call_export(&ctx, key, "init", arg) {
                    out.errors.push(ScriptError::Runtime {
                        key: key.clone(),
                        phase: "init",
                        message: msg,
                    });
                }
                match general_settings(&ctx, &language) {
                    Ok(payload) => {
                        if let Err(msg) = call_export(&ctx, key, "applyGeneralSettings", payload) {
                            out.errors.push(ScriptError::Runtime {
                                key: key.clone(),
                                phase: "applyGeneralSettings",
                                message: msg,
                            });
                        }
                    }
                    Err(e) => out.errors.push(ScriptError::Internal(e)),
                }
                match build_all(&ctx, &frame.user_props) {
                    Ok(payload) => {
                        if let Err(msg) = call_export(&ctx, key, "applyUserProperties", payload) {
                            out.errors.push(ScriptError::Runtime {
                                key: key.clone(),
                                phase: "applyUserProperties",
                                message: msg,
                            });
                        }
                    }
                    Err(e) => out.errors.push(e),
                }
            }

            dispatch_animation_events(&ctx, &metas, frame, &mut out);
            dispatch_puppet_events(&ctx, frame, &mut out);

            let mut results: Vec<(String, ScriptValue, bool)> = Vec::new();
            for (key, owner, _, current, workshop, _, _) in &metas {
                bind_this_layer(&ctx, *owner, key);
                set_workshop_id(&ctx, workshop.as_deref());
                let arg = match current.to_js(&ctx) {
                    Ok(v) => v,
                    Err(e) => {
                        out.errors.push(ScriptError::Internal(e.to_string()));
                        continue;
                    }
                };
                if res_changed {
                    match call_ret2(&ctx, "__vec2", (frame.res_x, frame.res_y)) {
                        Ok(size) => {
                            if let Err(msg) = call_export(&ctx, key, "resizeScreen", size) {
                                out.errors.push(ScriptError::Runtime {
                                    key: key.clone(),
                                    phase: "resizeScreen",
                                    message: msg,
                                });
                            }
                        }
                        Err(e) => out.errors.push(e),
                    }
                }
                let arg = match (*owner, key.rsplit_once('_')) {
                    (Some(id), Some((prop, _))) => match call_ret2(&ctx, "__getLayerProp", (id, prop)) {
                        Ok(v) if !v.is_undefined() => v,
                        _ => arg,
                    },
                    _ => arg,
                };
                match call_export_ret(&ctx, key, "update", arg) {
                    Ok(Some(ret)) => {
                        results.push((key.clone(), ScriptValue::from_js(&ret), true));
                    }
                    Ok(None) => { /* no update export — leave value untouched */ }
                    Err(msg) => {
                        out.errors.push(ScriptError::Runtime {
                            key: key.clone(),
                            phase: "update",
                            message: msg,
                        });
                    }
                }
            }
            drain_side_effects(&ctx, &mut out);
            (results, out)
        });

        for (key, value, applied) in results {
            if let Some(m) = self.modules.get_mut(&key) {
                m.inited = true;
                if applied {
                    m.current = value.clone();
                    out.property_results.push((key, value));
                }
            }
        }
        for m in self.modules.values_mut() {
            m.inited = true;
        }
        out
    }

    pub fn dispatch_user_property(&mut self, key: &str, value: &ScriptValue) -> TickOutput {
        self.deadline.arm(SCRIPT_BUDGET);
        let _guard = DeadlineGuard(self.deadline.clone());
        let keys: Vec<(String, Option<i64>)> = self
            .order
            .iter()
            .filter_map(|k| self.modules.get(k).map(|m| (k.clone(), m.owner_id)))
            .collect();
        self.context.with(|ctx| {
            let mut out = TickOutput::default();
            let payload = match build_single(&ctx, key, value) {
                Ok(p) => p,
                Err(e) => {
                    out.errors.push(e);
                    return out;
                }
            };
            for (mkey, owner) in &keys {
                bind_this_layer(&ctx, *owner, mkey);
                if let Err(msg) = call_export(&ctx, mkey, "applyUserProperties", payload.clone()) {
                    out.errors.push(ScriptError::Runtime {
                        key: mkey.clone(),
                        phase: "applyUserProperties",
                        message: msg,
                    });
                }
            }
            drain_side_effects(&ctx, &mut out);
            out
        })
    }

    pub fn set_storage_path(&mut self, path: std::path::PathBuf) {
        self.context.with(|ctx| {
            if let Ok(existing) = std::fs::read_to_string(&path)
                && let Ok(seed) = ctx.globals().get::<_, Function>("__seedStorage")
            {
                let _ = seed.call::<_, ()>((existing,));
            }
            let write_path = path.clone();
            let persist = Function::new(ctx.clone(), move |json: String| {
                if let Some(dir) = write_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if json.len() <= 100 * 1024 {
                    let _ = std::fs::write(&write_path, json);
                }
            });
            if let Ok(f) = persist {
                let _ = ctx.globals().set("__persistStorage", f);
            }
        });
    }

    pub fn eval_to_string(&self, source: &str) -> Result<String, ScriptError> {
        self.context.with(|ctx| {
            ctx.eval::<Value, _>(source)
                .catch(&ctx)
                .map_err(|e| ScriptError::Runtime {
                    key: String::new(),
                    phase: "eval",
                    message: e.to_string(),
                })
                .map(|v| stringify(&ctx, &v))
        })
    }
}

fn eval_global(ctx: &Ctx<'_>, name: &str, src: &str) -> Result<(), ScriptError> {
    ctx.eval::<(), _>(src).catch(ctx).map_err(|e| ScriptError::Load {
        key: name.to_owned(),
        message: e.to_string(),
    })
}

fn global<'js, T: rquickjs::FromJs<'js>>(ctx: &Ctx<'js>, name: &str) -> Result<T, ScriptError> {
    ctx.globals().get(name).internal()
}

fn drain_jobs(ctx: &Ctx<'_>) {
    while ctx.execute_pending_job() {}
}

fn call_void<'js, A: rquickjs::function::IntoArgs<'js>>(
    ctx: &Ctx<'js>,
    name: &str,
    args: A,
) -> Result<(), ScriptError> {
    let f: Function = global(ctx, name)?;
    f.call::<_, Value>(args)
        .catch(ctx)
        .map(|_| ())
        .map_err(|e| ScriptError::Internal(e.to_string()))
}

fn call_ret2<'js, A: rquickjs::function::IntoArgs<'js>>(
    ctx: &Ctx<'js>,
    name: &str,
    args: A,
) -> Result<Value<'js>, ScriptError> {
    let f: Function = global(ctx, name)?;
    f.call::<_, Value>(args)
        .catch(ctx)
        .map_err(|e| ScriptError::Internal(e.to_string()))
}

fn bind_this_layer(ctx: &Ctx<'_>, owner: Option<i64>, key: &str) {
    if let Ok(f) = global::<Function>(ctx, "__bindThisLayer") {
        let arg = match owner {
            Some(id) => Value::new_number(ctx.clone(), id as f64),
            None => Value::new_null(ctx.clone()),
        };
        let _ = f.call::<_, ()>((arg, key));
    }
}

fn set_workshop_id(ctx: &Ctx<'_>, id: Option<&str>) {
    if let Ok(host) = global::<Object>(ctx, "__host") {
        let _ = match id {
            Some(s) => {
                rquickjs::String::from_str(ctx.clone(), s).map(|v| host.set("workshopId", v.into_value()))
            }
            None => Ok(host.set("workshopId", Value::new_null(ctx.clone()))),
        };
    }
}

fn dispatch_cursor(
    ctx: &Ctx<'_>,
    metas: &[ModuleTickState],
    frame: &HostFrame,
    st: &mut CursorState,
    out: &mut TickOutput,
) {
    let world = frame.pointer_world;
    let left = frame.pointer_left_down;
    let Some((prev_world, prev_left)) = st.prev else {
        st.prev = Some((world, left));
        for l in &frame.layers {
            if l.solid == Some(true) {
                st.hit.insert(l.id, hit_test(l, world));
            }
        }
        return;
    };
    let moved = world != prev_world;
    let down_edge = left && !prev_left;
    let up_edge = !left && prev_left;
    st.prev = Some((world, left));
    if !moved && !down_edge && !up_edge {
        return;
    }

    let mut new_hit: Vec<(i64, bool)> = Vec::new();
    for (key, owner, _, _, _, flags, _) in metas {
        if *flags == 0 {
            continue;
        }
        let Some(id) = owner else { continue };
        let Some(layer) = frame.layers.iter().find(|l| l.id == *id) else {
            continue;
        };
        if layer.solid != Some(true) {
            continue;
        }
        let hit = hit_test(layer, world);
        let was_hit = st.hit.get(id).copied().unwrap_or(false);
        new_hit.push((*id, hit));

        bind_this_layer(ctx, *owner, key);
        let mut fire = |name: &'static str, bit: u8| {
            if flags & bit == 0 {
                return;
            }
            let ev = match cursor_event(ctx, world, layer) {
                Ok(ev) => ev,
                Err(e) => {
                    out.errors.push(ScriptError::Internal(e));
                    return;
                }
            };
            if let Err(msg) = call_export(ctx, key, name, ev) {
                out.errors.push(ScriptError::Runtime {
                    key: key.clone(),
                    phase: name,
                    message: msg,
                });
            }
        };
        if moved {
            fire("cursorMove", F_CURSOR_MOVE);
        }
        if hit && !was_hit {
            fire("cursorEnter", F_CURSOR_ENTER);
        }
        if !hit && was_hit {
            fire("cursorLeave", F_CURSOR_LEAVE);
        }
        if down_edge && hit {
            fire("cursorDown", F_CURSOR_DOWN);
            st.down_on.insert(*id);
        }
        if up_edge && hit {
            fire("cursorUp", F_CURSOR_UP);
            if st.down_on.contains(id) {
                fire("cursorClick", F_CURSOR_CLICK);
            }
        }
    }
    for (id, hit) in new_hit {
        st.hit.insert(id, hit);
    }
    if up_edge {
        st.down_on.clear();
    }
}

fn dispatch_media(ctx: &Ctx<'_>, metas: &[ModuleTickState], frame: &HostFrame, out: &mut TickOutput) {
    let Some(m) = &frame.media else { return };
    if metas.iter().all(|t| t.6 == 0) {
        return;
    }
    let color = |i: usize| -> serde_json::Value {
        m.colors
            .map_or(serde_json::json!([0.0, 0.0, 0.0]), |c| serde_json::json!(c[i]))
    };
    let events: [(serde_json::Value, &'static str, bool, u8); 5] = [
        (
            serde_json::json!({ "enabled": m.enabled }),
            "mediaStatusChanged",
            m.status_changed,
            F_MEDIA_STATUS,
        ),
        (
            serde_json::json!({ "state": m.state }),
            "mediaPlaybackChanged",
            m.playback_changed,
            F_MEDIA_PLAYBACK,
        ),
        (
            serde_json::json!({
                "title": m.title, "artist": m.artist, "subTitle": "",
                "albumTitle": m.album_title, "albumArtist": "",
                "genres": "", "contentType": "",
            }),
            "mediaPropertiesChanged",
            m.properties_changed,
            F_MEDIA_PROPERTIES,
        ),
        (
            serde_json::json!({
                "hasThumbnail": m.has_thumbnail,
                "primaryColor": color(0), "secondaryColor": color(1),
                "tertiaryColor": color(2), "textColor": color(3),
                "highContrastColor": color(4),
            }),
            "mediaThumbnailChanged",
            m.thumbnail_changed,
            F_MEDIA_THUMBNAIL,
        ),
        (
            serde_json::json!({ "position": m.position, "duration": m.duration }),
            "mediaTimelineChanged",
            m.timeline_changed,
            F_MEDIA_TIMELINE,
        ),
    ];
    for (key, owner, _, _, _, _, media_flags) in metas {
        if *media_flags == 0 {
            continue;
        }
        bind_this_layer(ctx, *owner, key);
        for (payload, name, fires, bit) in &events {
            if !fires || media_flags & bit == 0 {
                continue;
            }
            let arg = match json_to_js(ctx, payload) {
                Ok(v) => v,
                Err(e) => {
                    out.errors.push(ScriptError::Internal(e.to_string()));
                    continue;
                }
            };
            let arg = match call_ret2(ctx, "__mediaEvent", (arg,)) {
                Ok(v) => v,
                Err(e) => {
                    out.errors.push(e);
                    continue;
                }
            };
            if let Err(msg) = call_export(ctx, key, name, arg) {
                out.errors.push(ScriptError::Runtime {
                    key: key.clone(),
                    phase: name,
                    message: msg,
                });
            }
        }
    }
}

fn dispatch_puppet_events(ctx: &Ctx<'_>, frame: &HostFrame, out: &mut TickOutput) {
    for (object_id, name) in &frame.puppet_events {
        if let Err(e) = call_ret2(ctx, "__puppetEnded", (*object_id as f64, name.as_str())) {
            out.errors.push(e);
        }
    }
}

fn dispatch_animation_events(
    ctx: &Ctx<'_>,
    metas: &[ModuleTickState],
    frame: &HostFrame,
    out: &mut TickOutput,
) {
    for (object_id, name, at) in &frame.animation_events {
        for (key, owner, _, current, workshop, _, _) in metas {
            if *owner != Some(*object_id) {
                continue;
            }
            bind_this_layer(ctx, *owner, key);
            set_workshop_id(ctx, workshop.as_deref());
            let event = match Object::new(ctx.clone())
                .and_then(|o| o.set("name", name.as_str()).map(|()| o))
                .and_then(|o| o.set("frame", f64::from(*at)).map(|()| o))
            {
                Ok(o) => o.into_value(),
                Err(e) => {
                    out.errors.push(ScriptError::Internal(e.to_string()));
                    continue;
                }
            };
            let value = match current.to_js(ctx) {
                Ok(v) => v,
                Err(e) => {
                    out.errors.push(ScriptError::Internal(e.to_string()));
                    continue;
                }
            };
            if let Err(msg) = call_export2(ctx, key, "animationEvent", event, value) {
                out.errors.push(ScriptError::Runtime {
                    key: key.clone(),
                    phase: "animationEvent",
                    message: msg,
                });
            }
        }
    }
}

fn general_settings<'js>(ctx: &Ctx<'js>, language: &str) -> Result<Value<'js>, String> {
    let obj = Object::new(ctx.clone()).map_err(|e| e.to_string())?;
    obj.set("language", language).map_err(|e| e.to_string())?;
    Ok(obj.into_value())
}

fn system_language() -> String {
    let raw = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let tag = raw
        .split('.')
        .next()
        .unwrap_or("")
        .replace('_', "-")
        .to_lowercase();
    if tag.is_empty() || tag == "c" || tag == "posix" {
        "en-us".to_owned()
    } else {
        tag
    }
}

fn hit_test(l: &LayerState, world: [f32; 3]) -> bool {
    let (Some(origin), Some(size)) = (l.origin, l.size) else {
        return false;
    };
    let scale = l.scale.unwrap_or([1.0; 3]);
    let hw = (size[0] * scale[0]).abs() * 0.5;
    let hh = (size[1] * scale[1]).abs() * 0.5;
    (world[0] - origin[0]).abs() <= hw && (world[1] - origin[1]).abs() <= hh
}

fn cursor_event<'js>(ctx: &Ctx<'js>, world: [f32; 3], layer: &LayerState) -> Result<Value<'js>, String> {
    let origin = layer.origin.unwrap_or([0.0; 3]);
    let f: Function = ctx.globals().get("__cursorEvent").map_err(|e| e.to_string())?;
    f.call((
        world[0],
        world[1],
        world[2],
        world[0] - origin[0],
        world[1] - origin[1],
        world[2] - origin[2],
    ))
    .catch(ctx)
    .map_err(|e| e.to_string())
}

fn call_export<'js>(ctx: &Ctx<'js>, key: &str, name: &str, arg: Value<'js>) -> Result<(), String> {
    call_export_ret(ctx, key, name, arg).map(|_| ())
}

fn call_export2<'js>(
    ctx: &Ctx<'js>,
    key: &str,
    name: &str,
    arg: Value<'js>,
    arg2: Value<'js>,
) -> Result<(), String> {
    let f: Function = ctx.globals().get("__callExport").map_err(|e| e.to_string())?;
    f.call::<_, Value>((key, name, arg, arg2))
        .catch(ctx)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn call_export_ret<'js>(
    ctx: &Ctx<'js>,
    key: &str,
    name: &str,
    arg: Value<'js>,
) -> Result<Option<Value<'js>>, String> {
    let f: Function = ctx.globals().get("__callExport").map_err(|e| e.to_string())?;
    let ret: Object = f.call((key, name, arg)).catch(ctx).map_err(|e| e.to_string())?;
    if ret.get::<_, bool>("__missing").unwrap_or(false) {
        return Ok(None);
    }
    Ok(Some(ret.get("value").map_err(|e| e.to_string())?))
}

fn apply_frame(ctx: &Ctx<'_>, frame: &HostFrame) -> Result<(), ScriptError> {
    let host: Object = global(ctx, "__host")?;
    let json =
        serde_json::to_value(frame).map_err(|e| ScriptError::Internal(format!("frame serialize: {e}")))?;
    if let serde_json::Value::Object(map) = json {
        for (k, v) in &map {
            host.set(k.as_str(), json_to_js(ctx, v).internal()?).internal()?;
        }
        if !map.contains_key("audio") {
            host.set("audio", Value::new_null(ctx.clone())).internal()?;
        }
    }
    Ok(())
}

fn build_single<'js>(ctx: &Ctx<'js>, key: &str, value: &ScriptValue) -> Result<Value<'js>, ScriptError> {
    let obj = Object::new(ctx.clone()).internal()?;
    obj.set(key, value.to_js(ctx).internal()?).internal()?;
    Ok(obj.into_value())
}

fn build_all<'js>(ctx: &Ctx<'js>, props: &BTreeMap<String, ScriptValue>) -> Result<Value<'js>, ScriptError> {
    let obj = Object::new(ctx.clone()).internal()?;
    for (key, value) in props {
        obj.set(key.as_str(), value.to_js(ctx).internal()?).internal()?;
    }
    Ok(obj.into_value())
}

fn drain_side_effects(ctx: &Ctx<'_>, out: &mut TickOutput) {
    let host: Object = match global(ctx, "__host") {
        Ok(h) => h,
        Err(e) => {
            out.errors.push(e);
            return;
        }
    };
    if let Ok(ops) = host.get::<_, Array>("ops") {
        for i in 0..ops.len() {
            if let Ok(v) = ops.get::<Value>(i)
                && let Some(op) = parse_op(&v)
            {
                out.ops.push(op);
            }
        }
    }
    if let Ok(console) = host.get::<_, Array>("console") {
        for i in 0..console.len() {
            if let Ok(s) = console.get::<String>(i) {
                let error = s.starts_with('E');
                out.logs.push(LogLine {
                    error,
                    message: s.get(1..).unwrap_or("").to_owned(),
                });
            }
        }
    }
    if let Ok(empty) = Array::new(ctx.clone()) {
        let _ = host.set("ops", empty);
    }
    if let Ok(empty) = Array::new(ctx.clone()) {
        let _ = host.set("console", empty);
    }
}

fn parse_op(v: &Value<'_>) -> Option<SceneOp> {
    let obj = v.as_object()?;
    let op: String = obj.get("op").ok()?;
    match op.as_str() {
        "setProp" => Some(SceneOp::SetProperty {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            name: obj.get("name").ok()?,
            value: op_value(&obj.get::<_, Value>("value").ok()?),
        }),
        "setParent" => Some(SceneOp::SetParent {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            parent: obj
                .get::<_, Option<f64>>("parent")
                .ok()
                .flatten()
                .map(|f| f as i64),
        }),
        "setCameraTransforms" => Some(SceneOp::SetCameraTransforms {
            eye: get_vec3(obj, "eye"),
            center: get_vec3(obj, "center"),
            up: get_vec3(obj, "up"),
            fov: obj.get::<_, f64>("fov").ok().map(|f| f as f32),
            zoom: obj.get::<_, f64>("zoom").ok().map(|f| f as f32),
        }),
        "createLayer" => Some(SceneOp::CreateLayer {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            path: obj.get("path").ok()?,
            workshop_id: obj.get::<_, Option<String>>("workshopId").ok().flatten(),
            text: obj.get::<_, Option<String>>("text").ok().flatten(),
            config: obj.get::<_, Option<String>>("config").ok().flatten(),
        }),
        "sortLayer" => Some(SceneOp::SortLayer {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            index: obj.get::<_, f64>("index").ok()? as i64,
        }),
        "destroyLayer" => Some(SceneOp::DestroyLayer {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
        }),
        "particleCmd" => Some(SceneOp::ParticleCommand {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            cmd: obj.get("cmd").ok()?,
        }),
        "emitParticles" => Some(SceneOp::EmitParticles {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            count: obj.get::<_, f64>("count").ok()?.max(0.0) as u32,
        }),
        "setMatProp" => Some(SceneOp::SetMaterialProperty {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            effect: obj.get::<_, f64>("effect").ok()?.max(0.0) as u32,
            name: obj.get("name").ok()?,
            value: op_value(&obj.get::<_, Value>("value").ok()?),
        }),
        "videoCtl" => Some(SceneOp::VideoCommand {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            cmd: obj.get("cmd").ok()?,
            value: obj.get::<_, f64>("value").unwrap_or(0.0),
        }),
        "setInstance" => Some(SceneOp::SetInstance {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            name: obj.get("name").ok()?,
            value: op_value(&obj.get::<_, Value>("value").ok()?),
        }),
        "setScene" => Some(SceneOp::SetSceneProperty {
            name: obj.get("name").ok()?,
            value: op_value(&obj.get::<_, Value>("value").ok()?),
        }),
        "puppetCmd" => Some(SceneOp::PuppetCommand {
            layer_id: obj.get::<_, f64>("id").ok()? as i64,
            layer: obj.get("layer").ok()?,
            cmd: obj.get("cmd").ok()?,
            value: obj.get::<_, f64>("value").unwrap_or(0.0),
        }),
        "animCmd" => Some(SceneOp::AnimationCommand {
            index: obj.get::<_, f64>("index").ok()?.max(0.0) as u32,
            cmd: obj.get("cmd").ok()?,
            value: obj.get::<_, f64>("value").unwrap_or(0.0),
        }),
        _ => None,
    }
}

fn get_vec3(obj: &Object<'_>, key: &str) -> Option<[f32; 3]> {
    let arr: Array = obj.get(key).ok()?;
    if arr.len() < 3 {
        return None;
    }
    Some([
        arr.get::<f64>(0).ok()? as f32,
        arr.get::<f64>(1).ok()? as f32,
        arr.get::<f64>(2).ok()? as f32,
    ])
}

fn op_value(v: &Value<'_>) -> ScriptValue {
    if v.is_array()
        && let Some(arr) = v.as_array()
    {
        let comps: Vec<f32> = (0..arr.len())
            .filter_map(|i| arr.get::<f64>(i).ok().map(|f| f as f32))
            .collect();
        return match comps.len() {
            2 => ScriptValue::Vec2([comps[0], comps[1]]),
            3 => ScriptValue::Vec3([comps[0], comps[1], comps[2]]),
            4 => ScriptValue::Vec4([comps[0], comps[1], comps[2], comps[3]]),
            _ => ScriptValue::Null,
        };
    }
    ScriptValue::from_js(v)
}

fn stringify(ctx: &Ctx<'_>, v: &Value<'_>) -> String {
    if let Some(s) = v.as_string() {
        return s.to_string().unwrap_or_default();
    }
    let _ = ctx;
    match ScriptValue::from_js(v) {
        ScriptValue::Null => "null".to_owned(),
        ScriptValue::Bool(b) => b.to_string(),
        ScriptValue::Int(i) => i.to_string(),
        ScriptValue::Float(f) => f.to_string(),
        ScriptValue::Str(s) => s,
        ScriptValue::Vec2(v) => format!("{}, {}", v[0], v[1]),
        ScriptValue::Vec3(v) => format!("{}, {}, {}", v[0], v[1], v[2]),
        ScriptValue::Vec4(v) => format!("{}, {}, {}, {}", v[0], v[1], v[2], v[3]),
    }
}

trait Internalize<T> {
    fn internal(self) -> Result<T, ScriptError>;
}
impl<T> Internalize<T> for rquickjs::Result<T> {
    fn internal(self) -> Result<T, ScriptError> {
        self.map_err(|e| ScriptError::Internal(e.to_string()))
    }
}
