use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::compat::playlist::{self, PlaylistDefinition};
use crate::compat::resolve;

pub const WORKSHOP_APP_ID: &str = "431960";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    #[default]
    Normal,
    DesktopBackground,
    ExplicitWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layer {
    Background,
    #[default]
    Bottom,
    Top,
    Overlay,
}

impl Layer {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Overlay => "overlay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScalingMode {
    #[default]
    Default,
    Fit,
    Fill,
    Stretch,
}

impl ScalingMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClampMode {
    #[default]
    Clamp,
    Border,
    Repeat,
}

impl ClampMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clamp => "clamp",
            Self::Border => "border",
            Self::Repeat => "repeat",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderDebug {
    BaseOnly,
    NoSolidFinal,
    PassLog,
    Object(i64),
    SkipObject(i64),
    SkipEffect(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenConfig {
    pub name: String,
    pub is_span: bool,
    pub members: Vec<String>,
    pub background: Option<String>,
    pub scaling: ScalingMode,
    pub clamp: ClampMode,
    pub playlist: Option<PlaylistDefinition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompatArgs {
    pub argv: Vec<String>,
    pub help: bool,
    pub screens: Vec<ScreenConfig>,
    pub window: Option<WindowGeometry>,
    pub window_scaling: ScalingMode,
    pub window_clamp: ClampMode,
    pub window_playlist: Option<PlaylistDefinition>,
    pub default_background: Option<String>,
    pub mode: WindowMode,
    pub layer: Layer,
    pub fps: i64,
    pub playback_speed: f64,
    pub render_scale: f64,
    pub focus: (f32, f32),
    pub control_socket: Option<PathBuf>,
    pub audio_device: Option<String>,
    pub no_fullscreen_pause: bool,
    pub fullscreen_pause_only_active: bool,
    pub fullscreen_pause_ignore_appid: Vec<String>,
    pub release_hidden_after: Option<u64>,
    pub battery_fps: u32,
    pub fit_render_to_output: bool,
    pub volume: i64,
    pub silent: bool,
    pub noautomute: bool,
    pub no_audio_processing: bool,
    pub screenshot: Option<PathBuf>,
    pub screenshot_delay: u32,
    pub assets_dir: Option<PathBuf>,
    pub disable_particles: bool,
    pub disable_mouse: bool,
    pub disable_parallax: bool,
    pub list_properties: bool,
    pub list_properties_json: bool,
    pub set_properties: Vec<(String, String)>,
    pub dump_structure: bool,
    pub render_debug: Vec<RenderDebug>,
}

impl Default for CompatArgs {
    fn default() -> Self {
        Self {
            argv: Vec::new(),
            help: false,
            screens: Vec::new(),
            window: None,
            window_scaling: ScalingMode::Default,
            window_clamp: ClampMode::Clamp,
            window_playlist: None,
            default_background: None,
            mode: WindowMode::Normal,
            layer: Layer::Bottom,
            fps: 30,
            playback_speed: 1.0,
            render_scale: 1.0,
            focus: (0.0, 0.0),
            control_socket: None,
            audio_device: None,
            no_fullscreen_pause: false,
            fullscreen_pause_only_active: false,
            fullscreen_pause_ignore_appid: Vec::new(),
            release_hidden_after: None,
            battery_fps: 10,
            fit_render_to_output: false,
            volume: 15,
            silent: false,
            noautomute: false,
            no_audio_processing: false,
            screenshot: None,
            screenshot_delay: 5,
            assets_dir: None,
            disable_particles: false,
            disable_mouse: false,
            disable_parallax: false,
            list_properties: false,
            list_properties_json: false,
            set_properties: Vec::new(),
            dump_structure: false,
            render_debug: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub doubled: bool,
}

impl ParseError {
    fn doubled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            doubled: true,
        }
    }

    fn single(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            doubled: false,
        }
    }
}

pub const HELP_TEXT: &str = concat!(
    "Usage: linux-wallpaperengine [--help] [[--window VAR]...|[--screen-root VAR]...] ",
    "[--screen-span VAR]... [--bg VAR]... [--playlist VAR]... [--scaling VAR]... ",
    "[--clamp VAR]... [--layer VAR] [--fps VAR] [--playback-speed VAR] ",
    "[--render-scale VAR] [--control-socket VAR] [--audio-device VAR] ",
    "[--no-fullscreen-pause] [--fullscreen-pause-only-active] ",
    "[--fullscreen-pause-ignore-appid VAR]... [[--volume VAR]|[--silent]] ",
    "[--noautomute] [--no-audio-processing] [--screenshot VAR] ",
    "[--screenshot-delay VAR] [--assets-dir VAR] [--disable-particles] ",
    "[--disable-mouse] [--disable-parallax] [--list-properties] ",
    "[--list-properties-json] [--set-property VAR]... [--dump-structure] ",
    "[--render-debug VAR]... background id\n",
);

fn strtol(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let neg = match bytes.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut v: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add(i64::from(bytes[i] - b'0'));
        i += 1;
    }
    if i == start {
        return 0;
    }
    if neg { -v } else { v }
}

fn parse_geometry(value: &str) -> Result<WindowGeometry, ParseError> {
    let parts: Vec<&str> = value.split('x').collect();
    if parts.len() < 4 {
        return Err(ParseError::doubled(
            "Window geometry must be in the format: XxYxWxH",
        ));
    }
    Ok(WindowGeometry {
        x: strtol(parts[0]),
        y: strtol(parts[1]),
        w: strtol(parts[2]),
        h: strtol(parts[3]),
    })
}

fn scan_int(flag: &str, value: &str) -> Result<i64, ParseError> {
    value
        .trim()
        .parse::<i64>()
        .map_err(|_| ParseError::single(format!("Invalid numeric value '{value}' for {flag}")))
}

fn scan_focus(value: &str) -> Result<(f32, f32), ParseError> {
    let (x, y) = value.split_once(',').unwrap_or((value, "0"));
    let read = |text: &str| {
        text.trim()
            .parse::<f32>()
            .ok()
            .filter(|found| found.is_finite())
            .map(|found| found.clamp(-1.0, 1.0))
    };
    match (read(x), read(y)) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(ParseError {
            message: format!("--focus wants two numbers between -1 and 1, like 0.3,-0.2 (got {value})"),
            doubled: false,
        }),
    }
}

fn scan_float(flag: &str, value: &str) -> Result<f64, ParseError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| ParseError::single(format!("Invalid numeric value '{value}' for {flag}")))
}

fn choice<T: Copy>(argv0: &str, flag_value: &str, allowed: &[(&str, T)]) -> Result<T, ParseError> {
    if let Some((_, v)) = allowed.iter().find(|(s, _)| *s == flag_value) {
        return Ok(*v);
    }
    let names: Vec<&str> = allowed.iter().map(|(s, _)| *s).collect();
    Err(ParseError::single(format!(
        "Invalid argument \"{flag_value}\" - allowed options: {{{}}}. Use {argv0} --help for more information",
        names.join(", ")
    )))
}

fn render_debug_int(rest: &str) -> Result<i64, ParseError> {
    rest.parse::<i64>()
        .map_err(|_| ParseError::doubled(format!("Invalid numeric value for --render-debug: {rest}")))
}

fn parse_render_debug(value: &str) -> Result<RenderDebug, ParseError> {
    match value {
        "base-only" => Ok(RenderDebug::BaseOnly),
        "no-solid-final" => Ok(RenderDebug::NoSolidFinal),
        "pass-log" => Ok(RenderDebug::PassLog),
        _ => {
            if let Some(rest) = value.strip_prefix("object=") {
                Ok(RenderDebug::Object(render_debug_int(rest)?))
            } else if let Some(rest) = value.strip_prefix("skip-object=") {
                Ok(RenderDebug::SkipObject(render_debug_int(rest)?))
            } else if let Some(rest) = value.strip_prefix("skip-effect=") {
                Ok(RenderDebug::SkipEffect(render_debug_int(rest)?))
            } else {
                Err(ParseError::doubled(format!("Invalid render debug mode: {value}")))
            }
        }
    }
}

fn is_non_repeatable(canonical: &str) -> bool {
    matches!(
        canonical,
        "--layer"
            | "--fps"
            | "--playback-speed"
            | "--render-scale"
            | "--focus"
            | "--control-socket"
            | "--audio-device"
            | "--no-fullscreen-pause"
            | "--fullscreen-pause-only-active"
            | "--volume"
            | "--silent"
            | "--noautomute"
            | "--no-audio-processing"
            | "--screenshot"
            | "--screenshot-delay"
            | "--assets-dir"
            | "--disable-particles"
            | "--disable-mouse"
            | "--disable-parallax"
            | "--fit-render-to-output"
            | "--list-properties"
            | "--list-properties-json"
            | "--dump-structure"
    )
}

fn flag_takes_value(canonical: &str) -> bool {
    matches!(
        canonical,
        "--window"
            | "--screen-root"
            | "--screen-span"
            | "--bg"
            | "--playlist"
            | "--scaling"
            | "--clamp"
            | "--layer"
            | "--fps"
            | "--playback-speed"
            | "--render-scale"
            | "--control-socket"
            | "--audio-device"
            | "--fullscreen-pause-ignore-appid"
            | "--volume"
            | "--screenshot"
            | "--screenshot-delay"
            | "--assets-dir"
            | "--set-property"
            | "--render-debug"
            | "--gpu"
            | "--release-hidden-after"
            | "--battery-fps"
    )
}

enum Cursor {
    Window,
    Screen(usize),
}

pub fn parse(args: &[OsString]) -> Result<CompatArgs, ParseError> {
    let mut cache: Option<std::collections::BTreeMap<String, PlaylistDefinition>> = None;
    parse_with(args, &mut |name| {
        if cache.is_none() {
            cache = Some(playlist::load_config_playlists()?);
        }
        match cache.as_ref() {
            Some(map) => playlist::get(map, name).cloned(),
            None => unreachable!("playlist cache filled above"),
        }
    })
}

fn parse_with(
    args: &[OsString],
    load_playlist: &mut dyn FnMut(&str) -> Result<PlaylistDefinition, ParseError>,
) -> Result<CompatArgs, ParseError> {
    let argv0 = args
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "linux-wallpaperengine".to_owned());
    let mut out = CompatArgs {
        argv: args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
        ..CompatArgs::default()
    };
    let mut cursor = Cursor::Window;
    let mut seen: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let raw = &args[i];
        let token = raw.to_string_lossy();

        if !token.starts_with('-') || token.as_ref() == "-" {
            if !token.is_empty() {
                out.default_background = Some(resolve::translate_background(&token)?);
            }
            i += 1;
            continue;
        }

        let (name, inline): (String, Option<String>) = if token.starts_with("--") {
            match token.split_once('=') {
                Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                None => (token.into_owned(), None),
            }
        } else {
            (token.into_owned(), None)
        };

        let canonical = canonical_flag(&name);
        let Some(canonical) = canonical else {
            i += 1;
            continue;
        };

        if is_non_repeatable(canonical) && seen.iter().any(|s| s == canonical) {
            return Err(ParseError::single(format!(
                "Duplicate argument {canonical}. Use {argv0} --help for more information"
            )));
        }
        seen.push(canonical.to_owned());

        let mut consumed_next = false;
        let fetched: Result<String, ParseError> = if flag_takes_value(canonical) {
            if let Some(v) = inline {
                Ok(v)
            } else {
                match args.get(i + 1) {
                    Some(v) => {
                        consumed_next = true;
                        Ok(v.to_string_lossy().into_owned())
                    }
                    None => Err(ParseError::single(format!("{canonical}: expected one argument"))),
                }
            }
        } else {
            Ok(String::new())
        };
        let value = || fetched.clone();

        match canonical {
            "--help" => out.help = true,
            "--window" => {
                if out.mode == WindowMode::DesktopBackground {
                    return Err(ParseError::doubled(
                        "Cannot run in both background and window mode",
                    ));
                }
                if out.window.is_some() {
                    return Err(ParseError::doubled("Only one window at a time can be specified"));
                }
                out.window = Some(parse_geometry(&value()?)?);
                out.mode = WindowMode::ExplicitWindow;
            }
            "--screen-root" => {
                let name = value()?;
                apply_screen_root(&mut out, &mut cursor, name)?;
            }
            "--screen-span" => {
                let raw = value()?;
                apply_screen_span(&mut out, &mut cursor, raw)?;
            }
            "--bg" => {
                let resolved = resolve::translate_background(&value()?)?;
                out.default_background = Some(resolved.clone());
                if let Cursor::Screen(idx) = cursor {
                    out.screens[idx].background = Some(resolved);
                }
            }
            "--playlist" => {
                let name = value()?;
                let def = load_playlist(&name)?;
                let first = def.items.first().map(|p| p.to_string_lossy().into_owned());
                match cursor {
                    Cursor::Screen(idx) => {
                        if let Some(first) = &first {
                            out.screens[idx].background = Some(first.clone());
                        }
                        out.screens[idx].playlist = Some(def);
                        if out.default_background.is_none() {
                            out.default_background = first;
                        }
                    }
                    Cursor::Window => {
                        out.window_playlist = Some(def);
                        if out.default_background.is_none() {
                            out.default_background = first;
                        }
                    }
                }
            }
            "--scaling" => {
                let mode = choice(
                    &argv0,
                    &value()?,
                    &[
                        ("stretch", ScalingMode::Stretch),
                        ("fit", ScalingMode::Fit),
                        ("fill", ScalingMode::Fill),
                        ("default", ScalingMode::Default),
                    ],
                )?;
                match cursor {
                    Cursor::Screen(idx) => out.screens[idx].scaling = mode,
                    Cursor::Window => out.window_scaling = mode,
                }
            }
            "--clamp" => {
                let mode = choice(
                    &argv0,
                    &value()?,
                    &[
                        ("clamp", ClampMode::Clamp),
                        ("border", ClampMode::Border),
                        ("repeat", ClampMode::Repeat),
                    ],
                )?;
                match cursor {
                    Cursor::Screen(idx) => out.screens[idx].clamp = mode,
                    Cursor::Window => out.window_clamp = mode,
                }
            }
            "--layer" => {
                out.layer = choice(
                    &argv0,
                    &value()?,
                    &[
                        ("background", Layer::Background),
                        ("bottom", Layer::Bottom),
                        ("top", Layer::Top),
                        ("overlay", Layer::Overlay),
                    ],
                )?;
            }
            "--fps" => out.fps = scan_int("--fps", &value()?)?,
            "--playback-speed" => {
                out.playback_speed = scan_float("--playback-speed", &value()?)?;
            }
            "--render-scale" => {
                out.render_scale = scan_float("--render-scale", &value()?)?;
            }
            "--focus" => out.focus = scan_focus(&value()?)?,
            "--control-socket" => out.control_socket = Some(PathBuf::from(value()?)),
            "--audio-device" => out.audio_device = Some(value()?),
            "--no-fullscreen-pause" => out.no_fullscreen_pause = true,
            "--fullscreen-pause-only-active" => out.fullscreen_pause_only_active = true,
            "--fullscreen-pause-ignore-appid" => {
                let v = value()?;
                if !v.is_empty() {
                    out.fullscreen_pause_ignore_appid.push(v);
                }
            }
            "--volume" => out.volume = scan_int("--volume", &value()?)?,
            "--silent" => out.silent = true,
            "--noautomute" => out.noautomute = true,
            "--no-audio-processing" => out.no_audio_processing = true,
            "--screenshot" => out.screenshot = Some(PathBuf::from(value()?)),
            "--screenshot-delay" => {
                let n = scan_int("--screenshot-delay", &value()?)?;
                out.screenshot_delay = n.clamp(0, u32::MAX as i64) as u32;
            }
            "--assets-dir" => out.assets_dir = Some(PathBuf::from(value()?)),
            "--gpu" => drop(value()?),
            "--release-hidden-after" => {
                let n = scan_int("--release-hidden-after", &value()?)?;
                out.release_hidden_after = (n > 0).then_some(n as u64);
            }
            "--battery-fps" => {
                let n = scan_int("--battery-fps", &value()?)?;
                out.battery_fps = u32::try_from(n).unwrap_or(0);
            }
            "--disable-particles" => out.disable_particles = true,
            "--disable-mouse" => out.disable_mouse = true,
            "--disable-parallax" => out.disable_parallax = true,
            "--fit-render-to-output" => out.fit_render_to_output = true,
            "--list-properties" => out.list_properties = true,
            "--list-properties-json" => out.list_properties_json = true,
            "--set-property" => {
                let kv = value()?;
                out.set_properties.push(split_property(&kv));
            }
            "--dump-structure" => out.dump_structure = true,
            "--render-debug" => {
                let dbg = parse_render_debug(&value()?)?;
                out.render_debug.push(dbg);
            }
            other => unreachable!("unmapped canonical flag {other}"),
        }

        i += if consumed_next { 2 } else { 1 };
    }

    Ok(out)
}

fn split_property(kv: &str) -> (String, String) {
    match kv.split_once('=') {
        Some((k, v)) => (k.to_owned(), v.to_owned()),
        None => (kv.to_owned(), "1".to_owned()),
    }
}

fn apply_screen_root(out: &mut CompatArgs, cursor: &mut Cursor, name: String) -> Result<(), ParseError> {
    if out.mode == WindowMode::ExplicitWindow {
        return Err(ParseError::doubled(
            "Cannot run in both background and window mode",
        ));
    }
    if out.screens.iter().any(|s| !s.is_span && s.name == name) {
        return Err(ParseError::doubled(
            "Cannot specify the same screen more than once",
        ));
    }
    if out.screens.iter().any(|s| s.is_span && s.members.contains(&name)) {
        return Err(ParseError::doubled(format!(
            "Screen {name} is already part of a span group"
        )));
    }
    out.mode = WindowMode::DesktopBackground;
    out.screens.push(ScreenConfig {
        name,
        is_span: false,
        members: Vec::new(),
        background: None,
        scaling: out.window_scaling,
        clamp: out.window_clamp,
        playlist: None,
    });
    *cursor = Cursor::Screen(out.screens.len() - 1);
    Ok(())
}

fn apply_screen_span(out: &mut CompatArgs, cursor: &mut Cursor, raw: String) -> Result<(), ParseError> {
    if out.mode == WindowMode::ExplicitWindow {
        return Err(ParseError::doubled(
            "Cannot run in both background and window mode",
        ));
    }
    let members: Vec<String> = raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if members.len() < 2 {
        return Err(ParseError::doubled(
            "A span requires at least two comma-separated screen names",
        ));
    }
    for m in &members {
        if out.screens.iter().any(|s| !s.is_span && s.name == *m) {
            return Err(ParseError::doubled(format!(
                "Screen {m} is already configured individually"
            )));
        }
        if out
            .screens
            .iter()
            .any(|s| s.is_span && s.members.iter().any(|x| x == m))
        {
            return Err(ParseError::doubled(format!(
                "Screen {m} is already part of a span group"
            )));
        }
        if members.iter().filter(|x| *x == m).count() > 1 {
            return Err(ParseError::doubled(format!(
                "Screen {m} is duplicated in the span group"
            )));
        }
    }
    out.mode = WindowMode::DesktopBackground;
    out.screens.push(ScreenConfig {
        name: format!("span:{raw}"),
        is_span: true,
        members,
        background: None,
        scaling: out.window_scaling,
        clamp: out.window_clamp,
        playlist: None,
    });
    *cursor = Cursor::Screen(out.screens.len() - 1);
    Ok(())
}

fn canonical_flag(name: &str) -> Option<&'static str> {
    Some(match name {
        "-h" | "--help" => "--help",
        "-w" | "--window" => "--window",
        "-r" | "--screen-root" => "--screen-root",
        "--screen-span" => "--screen-span",
        "-b" | "--bg" => "--bg",
        "--playlist" => "--playlist",
        "--scaling" => "--scaling",
        "--clamp" => "--clamp",
        "--layer" => "--layer",
        "-f" | "--fps" => "--fps",
        "--playback-speed" | "--clock" => "--playback-speed",
        "--render-scale" => "--render-scale",
        "--focus" => "--focus",
        "--control-socket" => "--control-socket",
        "--audio-device" => "--audio-device",
        "--no-fullscreen-pause" => "--no-fullscreen-pause",
        "--fullscreen-pause-only-active" => "--fullscreen-pause-only-active",
        "--fullscreen-pause-ignore-appid" => "--fullscreen-pause-ignore-appid",
        "-v" | "--volume" => "--volume",
        "-s" | "--silent" => "--silent",
        "--noautomute" => "--noautomute",
        "--no-audio-processing" => "--no-audio-processing",
        "--screenshot" => "--screenshot",
        "--screenshot-delay" => "--screenshot-delay",
        "--assets-dir" => "--assets-dir",
        "--disable-particles" => "--disable-particles",
        "--disable-mouse" => "--disable-mouse",
        "--disable-parallax" => "--disable-parallax",
        "-l" | "--list-properties" => "--list-properties",
        "--list-properties-json" => "--list-properties-json",
        "--set-property" | "--property" => "--set-property",
        "-z" | "--dump-structure" => "--dump-structure",
        "--render-debug" => "--render-debug",
        "--gpu" => "--gpu",
        "--release-hidden-after" => "--release-hidden-after",
        "--fit-render-to-output" => "--fit-render-to-output",
        _ => return None,
    })
}

pub fn validate(mut args: CompatArgs) -> Result<CompatArgs, ParseError> {
    if args.default_background.is_none() {
        return Err(ParseError::doubled(
            "At least one background ID must be specified",
        ));
    }
    args.volume = args.volume.clamp(0, 128);
    args.screenshot_delay = args.screenshot_delay.min(600);
    Ok(args)
}

pub fn validate_screenshot_ext(path: &OsStr) -> Result<(), ParseError> {
    let name = path.to_string_lossy();
    let ok =
        name.ends_with(".bmp") || name.ends_with(".png") || name.ends_with(".jpeg") || name.ends_with(".jpg");
    if ok {
        Ok(())
    } else {
        Err(ParseError::doubled(format!(
            "Cannot determine screenshot format, unknown extension for {name}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn strtol_semantics() {
        assert_eq!(strtol("1920"), 1920);
        assert_eq!(strtol("-5"), -5);
        assert_eq!(strtol("3junk"), 3);
        assert_eq!(strtol("junk"), 0);
        assert_eq!(strtol(""), 0);
    }

    #[test]
    fn geometry_parses_and_ignores_extra_x() {
        assert_eq!(
            parse_geometry("0x0x1920x1080").unwrap(),
            WindowGeometry {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        assert_eq!(
            parse_geometry("1x2x3x4x5").unwrap(),
            WindowGeometry {
                x: 1,
                y: 2,
                w: 3,
                h: 4
            }
        );
        assert!(parse_geometry("1x2").is_err());
        assert!(parse_geometry("1920x1080x0").is_err());
    }

    #[test]
    fn help_flag_parses() {
        let args = parse(&os(&["linux-wallpaperengine", "--help"])).unwrap();
        assert!(args.help);
    }

    #[test]
    fn unknown_flags_are_ignored() {
        let args = parse(&os(&["kirie", "--bogus-flag", "--type=zygote"])).unwrap();
        assert!(args.default_background.is_none());
        assert!(validate(args).is_err());
    }

    #[test]
    fn duplicate_non_repeatable_is_fatal() {
        let err = parse(&os(&["kirie", "--fps", "30", "--fps", "60"])).unwrap_err();
        assert!(err.message.contains("Duplicate argument --fps"));
    }

    #[test]
    fn repeatable_flags_accumulate() {
        let args = parse(&os(&[
            "kirie",
            "--set-property",
            "a=1",
            "--set-property",
            "b=2",
            "/tmp/x",
        ]))
        .unwrap();
        assert_eq!(
            args.set_properties,
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
    }

    #[test]
    fn bare_property_key_defaults_to_one() {
        let args = parse(&os(&["kirie", "--set-property", "bloom", "/tmp/x"])).unwrap();
        assert_eq!(args.set_properties, vec![("bloom".into(), "1".into())]);
    }

    #[test]
    fn window_and_screen_root_conflict() {
        let err = parse(&os(&[
            "kirie",
            "--window",
            "0x0x100x100",
            "--screen-root",
            "HDMI-A-1",
        ]))
        .unwrap_err();
        assert!(
            err.message
                .contains("Cannot run in both background and window mode")
        );
        let err = parse(&os(&[
            "kirie",
            "--screen-root",
            "HDMI-A-1",
            "--window",
            "0x0x100x100",
        ]))
        .unwrap_err();
        assert!(
            err.message
                .contains("Cannot run in both background and window mode")
        );
    }

    #[test]
    fn same_screen_twice_is_fatal() {
        let err = parse(&os(&[
            "kirie",
            "--screen-root",
            "HDMI-A-1",
            "--screen-root",
            "HDMI-A-1",
        ]))
        .unwrap_err();
        assert!(
            err.message
                .contains("Cannot specify the same screen more than once")
        );
    }

    #[test]
    fn bad_choice_is_fatal_with_message() {
        let err = parse(&os(&["kirie", "--scaling", "wrong", "/tmp/x"])).unwrap_err();
        assert!(err.message.contains("allowed options"));
        assert!(err.message.contains("stretch"));
    }

    #[test]
    fn clock_alias_maps_to_playback_speed() {
        let args = parse(&os(&["kirie", "--clock", "0.5", "/tmp/x"])).unwrap();
        assert_eq!(args.playback_speed, 0.5);
    }

    #[test]
    fn property_alias_maps_to_set_property() {
        let args = parse(&os(&["kirie", "--property", "a=b", "/tmp/x"])).unwrap();
        assert_eq!(args.set_properties, vec![("a".into(), "b".into())]);
    }

    #[test]
    fn inline_equals_form_parses() {
        let args = parse(&os(&["kirie", "--fps=30", "--set-property=foo=bar", "/tmp/x"])).unwrap();
        assert_eq!(args.fps, 30);
        assert_eq!(args.set_properties, vec![("foo".into(), "bar".into())]);
    }

    #[test]
    fn per_screen_scaling_before_r_is_the_inherited_default() {
        let args = parse(&os(&[
            "kirie",
            "--scaling",
            "fill",
            "--screen-root",
            "HDMI-A-1",
            "--screen-root",
            "DP-1",
            "/tmp/x",
        ]))
        .unwrap();
        assert_eq!(args.window_scaling, ScalingMode::Fill);
        assert_eq!(args.screens[0].scaling, ScalingMode::Fill);
        assert_eq!(args.screens[1].scaling, ScalingMode::Fill);
    }

    #[test]
    fn per_screen_scaling_after_r_is_local() {
        let args = parse(&os(&[
            "kirie",
            "--screen-root",
            "HDMI-A-1",
            "--scaling",
            "fill",
            "--screen-root",
            "DP-1",
            "/tmp/x",
        ]))
        .unwrap();
        assert_eq!(args.window_scaling, ScalingMode::Default);
        assert_eq!(args.screens[0].scaling, ScalingMode::Fill);
        assert_eq!(args.screens[1].scaling, ScalingMode::Default);
    }

    #[test]
    fn last_bg_wins_as_default_background() {
        let args = parse(&os(&[
            "kirie",
            "--screen-root",
            "HDMI-A-1",
            "--bg",
            "/a",
            "--screen-root",
            "DP-1",
            "--bg",
            "/b",
        ]))
        .unwrap();
        assert_eq!(args.screens[0].background.as_deref(), Some("/a"));
        assert_eq!(args.screens[1].background.as_deref(), Some("/b"));
        assert_eq!(args.default_background.as_deref(), Some("/b"));
    }

    fn stub_loader(name: &str) -> Result<PlaylistDefinition, ParseError> {
        if name == "day" {
            Ok(PlaylistDefinition {
                name: "day".to_owned(),
                items: vec![PathBuf::from("/wp/one"), PathBuf::from("/wp/two")],
                settings: crate::compat::playlist::PlaylistSettings::default(),
            })
        } else {
            Err(ParseError::doubled(format!(
                "Playlist not found in config.json: {name}"
            )))
        }
    }

    #[test]
    fn playlist_before_any_screen_is_the_window_default() {
        let args = parse_with(&os(&["kirie", "--playlist", "day"]), &mut stub_loader).unwrap();
        let pl = args.window_playlist.as_ref().unwrap();
        assert_eq!(pl.name, "day");
        assert_eq!(args.default_background.as_deref(), Some("/wp/one"));
        assert!(args.screens.is_empty());
        assert!(
            validate(args).is_ok(),
            "a playlist satisfies the background check"
        );
    }

    #[test]
    fn playlist_after_screen_root_targets_that_screen() {
        let args = parse_with(
            &os(&["kirie", "--screen-root", "HDMI-A-1", "--playlist", "day"]),
            &mut stub_loader,
        )
        .unwrap();
        assert!(args.window_playlist.is_none());
        assert_eq!(args.screens[0].playlist.as_ref().unwrap().name, "day");
        assert_eq!(args.screens[0].background.as_deref(), Some("/wp/one"));
        assert_eq!(args.default_background.as_deref(), Some("/wp/one"));
    }

    #[test]
    fn playlist_does_not_override_an_explicit_default_background() {
        let args = parse_with(
            &os(&["kirie", "--bg", "/explicit", "--playlist", "day"]),
            &mut stub_loader,
        )
        .unwrap();
        assert_eq!(args.default_background.as_deref(), Some("/explicit"));
        assert!(args.window_playlist.is_some());
    }

    #[test]
    fn unknown_playlist_is_fatal_at_parse_time() {
        let err = parse_with(&os(&["kirie", "--playlist", "nope"]), &mut stub_loader).unwrap_err();
        assert!(err.message.contains("Playlist not found in config.json: nope"));
        assert!(err.doubled);
    }

    #[test]
    fn the_exact_live_cmdline_parses_to_the_expected_model() {
        let argv = os(&[
            "linux-wallpaperengine",
            "--control-socket",
            "/tmp/claude-1000/kirie-test.sock",
            "--screen-root",
            "HDMI-A-1",
            "--bg",
            "/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375",
            "--scaling",
            "fill",
            "--clamp",
            "clamp",
            "--fps",
            "30",
            "--render-scale",
            "1.06",
            "--volume",
            "0",
            "--set-property",
            "fov=48.333333333333336",
            "--set-property",
            "bloom=true",
            "--set-property",
            "radialblur=false",
            "--set-property",
            "huespeed=0.10555555555555556",
            "--set-property",
            "coloring1=2",
            "--set-property",
            "newproperty=0.025",
            "--set-property",
            "schemecolor=0.00000 0.00000 0.00000",
            "--set-property",
            "outline=0.36585 0.04268 0.43902",
            "--set-property",
            "bloomstrength=1.7916666666666665",
            "--set-property",
            "color1=0.00000 0.00000 1.00000",
            "--set-property",
            "color2=0.46951 0.00000 0.77439",
        ]);
        let args = validate(parse(&argv).unwrap()).unwrap();

        assert_eq!(args.mode, WindowMode::DesktopBackground);
        assert_eq!(
            args.control_socket.as_deref(),
            Some(std::path::Path::new("/tmp/claude-1000/kirie-test.sock"))
        );
        assert_eq!(args.screens.len(), 1);
        assert_eq!(args.screens[0].name, "HDMI-A-1");
        assert_eq!(
            args.screens[0].background.as_deref(),
            Some("/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375")
        );
        assert_eq!(args.screens[0].scaling, ScalingMode::Fill);
        assert_eq!(args.screens[0].clamp, ClampMode::Clamp);
        assert_eq!(args.fps, 30);
        assert!((args.render_scale - 1.06).abs() < 1e-12);
        assert_eq!(args.volume, 0);

        assert_eq!(args.set_properties.len(), 11);
        assert_eq!(
            args.set_properties[0],
            ("fov".to_owned(), "48.333333333333336".to_owned())
        );
        assert_eq!(
            args.set_properties[6],
            ("schemecolor".to_owned(), "0.00000 0.00000 0.00000".to_owned())
        );
        assert_eq!(
            args.set_properties[7],
            ("outline".to_owned(), "0.36585 0.04268 0.43902".to_owned())
        );
        assert_eq!(
            args.set_properties[10],
            ("color2".to_owned(), "0.46951 0.00000 0.77439".to_owned())
        );

        assert_eq!(
            args.default_background.as_deref(),
            Some("/home/aiko/.local/share/Steam/steamapps/workshop/content/431960/3047596375")
        );
    }
}
