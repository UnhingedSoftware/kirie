use std::sync::Arc;

use crate::compat::args::{ClampMode, ScalingMode};

static RENDER_SCALE_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub(crate) fn set_render_scale(scale: f32) {
    let s = if scale.is_finite() {
        scale.clamp(0.25, 4.0)
    } else {
        1.0
    };
    RENDER_SCALE_BITS.store(s.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn render_scale() -> f32 {
    f32::from_bits(RENDER_SCALE_BITS.load(std::sync::atomic::Ordering::Relaxed))
}

static OBJECT_FILTER: std::sync::Mutex<(Vec<i64>, Vec<i64>)> =
    std::sync::Mutex::new((Vec::new(), Vec::new()));

pub(crate) fn set_object_filter(debug: &[super::args::RenderDebug]) {
    let mut only = Vec::new();
    let mut skip = Vec::new();
    for entry in debug {
        match entry {
            super::args::RenderDebug::Object(id) => only.push(*id),
            super::args::RenderDebug::SkipObject(id) => skip.push(*id),
            _ => {}
        }
    }
    if let Ok(mut slot) = OBJECT_FILTER.lock() {
        *slot = (only, skip);
    }
}

pub(crate) fn object_filter() -> (Vec<i64>, Vec<i64>) {
    OBJECT_FILTER.lock().map(|slot| slot.clone()).unwrap_or_default()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
static DISABLE_PARALLAX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn set_disable_parallax(on: bool) {
    DISABLE_PARALLAX.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn disable_parallax() -> bool {
    DISABLE_PARALLAX.load(std::sync::atomic::Ordering::Relaxed)
}

static FIT_RENDER_TO_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_fit_render_to_output(on: bool) {
    FIT_RENDER_TO_OUTPUT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn fit_render_to_output() -> bool {
    FIT_RENDER_TO_OUTPUT.load(std::sync::atomic::Ordering::Relaxed)
}

#[must_use]
pub fn to_video_scaling(mode: ScalingMode) -> kirie_video::ScalingMode {
    match mode {
        ScalingMode::Default => kirie_video::ScalingMode::Default,
        ScalingMode::Fit => kirie_video::ScalingMode::Fit,
        ScalingMode::Fill => kirie_video::ScalingMode::Fill,
        ScalingMode::Stretch => kirie_video::ScalingMode::Stretch,
    }
}

#[must_use]
pub fn to_render_scaling(mode: ScalingMode) -> kirie_render::ScalingMode {
    match mode {
        ScalingMode::Default => kirie_render::ScalingMode::Default,
        ScalingMode::Fit => kirie_render::ScalingMode::Fit,
        ScalingMode::Fill => kirie_render::ScalingMode::Fill,
        ScalingMode::Stretch => kirie_render::ScalingMode::Stretch,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn power_save_flag() -> Arc<std::sync::atomic::AtomicBool> {
    static FLAG: std::sync::OnceLock<Arc<std::sync::atomic::AtomicBool>> = std::sync::OnceLock::new();
    FLAG.get_or_init(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .clone()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn battery_fps_target() -> Arc<std::sync::atomic::AtomicU32> {
    static TARGET: std::sync::OnceLock<Arc<std::sync::atomic::AtomicU32>> = std::sync::OnceLock::new();
    TARGET
        .get_or_init(|| Arc::new(std::sync::atomic::AtomicU32::new(10)))
        .clone()
}

#[must_use]
pub fn to_render_clamp(mode: ClampMode) -> kirie_render::ClampMode {
    match mode {
        ClampMode::Clamp => kirie_render::ClampMode::Clamp,
        ClampMode::Border => kirie_render::ClampMode::Border,
        ClampMode::Repeat => kirie_render::ClampMode::Repeat,
    }
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
pub fn web_props_json(dir: &std::path::Path, overrides: &[(String, String)]) -> String {
    use kirie_formats::project::{Project, PropertyEntry, PropertyKind};
    let Ok(project) = Project::from_path(dir.join("project.json")) else {
        return "{}".to_owned();
    };
    let over: std::collections::HashMap<&str, &str> =
        overrides.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("{");
    let mut first = true;
    for (name, entry) in &project.general.properties {
        let PropertyEntry::Property(p) = entry else {
            continue;
        };
        let raw = over.get(name.as_str()).copied();
        let value = match &p.kind {
            PropertyKind::Bool { value } => {
                let v = raw.map_or(*value, |r| matches!(r.trim(), "1" | "true" | "True" | "TRUE"));
                if v { "true".to_owned() } else { "false".to_owned() }
            }
            PropertyKind::Slider { value, .. } => {
                let v = raw
                    .and_then(|r| r.trim().parse::<f64>().ok())
                    .unwrap_or(f64::from(*value));
                format!("{v}")
            }
            PropertyKind::Color { value: [r, g, b] } => {
                let s = raw.map_or_else(|| format!("{r:.4} {g:.4} {b:.4}"), str::to_owned);
                format!("\"{}\"", esc(&s))
            }
            PropertyKind::Text => continue,
            PropertyKind::Combo { value, .. }
            | PropertyKind::TextInput { value }
            | PropertyKind::UserShortcut { value }
            | PropertyKind::File { value }
            | PropertyKind::Directory { value }
            | PropertyKind::SceneTexture { value } => {
                format!("\"{}\"", esc(raw.unwrap_or(value)))
            }
        };
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{}\":{{\"value\":{value}}}", esc(name)));
    }
    out.push('}');
    out
}
