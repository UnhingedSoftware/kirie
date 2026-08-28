use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Empty,
    Ping,
    Status,
    GetProperties { screen: Option<String> },
    List,
    Workshop(WorkshopRequest),
    Command(Command),
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkshopRequest {
    Search(String),
    State(String),
    Subscribe(String),
    Unsubscribe(String),
    Job(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Speed(f32),
    Volume(i32),
    Mute(bool),
    Set(SetOption),
    Bg {
        screen: String,
        path: PathBuf,
    },
    Preload {
        path: PathBuf,
    },
    Property {
        screen: String,
        key: String,
        value: String,
    },
    Scaling {
        screen: String,
        mode: ScalingMode,
    },
    Clamp {
        screen: String,
        mode: ClampMode,
    },
    Screenshot {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetOption {
    Fps(i32),
    NoAutomute(bool),
    DisableMouse(bool),
    DisableParallax(bool),
    NoFullscreenPause(bool),
    RenderScale(f32),
    AudioDevice(String),
    BatteryFps(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    Stretch,
    Fit,
    Fill,
    Default,
}

impl ScalingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClampMode {
    Clamp,
    Border,
    Repeat,
}

impl ClampMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clamp => "clamp",
            Self::Border => "border",
            Self::Repeat => "repeat",
        }
    }
}

impl Command {
    pub fn is_fallible(&self) -> bool {
        matches!(
            self,
            Self::Bg { .. }
                | Self::Property { .. }
                | Self::Scaling { .. }
                | Self::Clamp { .. }
                | Self::Screenshot { .. }
        )
    }

    pub fn to_request_line(&self) -> Vec<u8> {
        fn join(parts: &[&[u8]]) -> Vec<u8> {
            let mut out = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(p);
            }
            out
        }
        let bool_str = |b: bool| if b { "true" } else { "false" };
        match self {
            Self::Speed(v) => format!("speed {v}").into_bytes(),
            Self::Volume(v) => format!("volume {v}").into_bytes(),
            Self::Mute(m) => format!("mute {}", i32::from(*m)).into_bytes(),
            Self::Set(opt) => match opt {
                SetOption::Fps(n) => format!("set fps {n}"),
                SetOption::BatteryFps(n) => format!("set batteryfps {n}"),
                SetOption::NoAutomute(b) => format!("set noautomute {}", bool_str(*b)),
                SetOption::DisableMouse(b) => format!("set disablemouse {}", bool_str(*b)),
                SetOption::DisableParallax(b) => format!("set disableparallax {}", bool_str(*b)),
                SetOption::NoFullscreenPause(b) => format!("set nofullscreenpause {}", bool_str(*b)),
                SetOption::RenderScale(v) => format!("set renderscale {v}"),
                SetOption::AudioDevice(s) => format!("set audiodevice {s}"),
            }
            .into_bytes(),
            Self::Bg { screen, path } => join(&[b"bg", screen.as_bytes(), path.as_os_str().as_bytes()]),
            Self::Preload { path } => join(&[b"preload", path.as_os_str().as_bytes()]),
            Self::Property { screen, key, value } => {
                join(&[b"property", screen.as_bytes(), key.as_bytes(), value.as_bytes()])
            }
            Self::Scaling { screen, mode } => {
                join(&[b"scaling", screen.as_bytes(), mode.as_str().as_bytes()])
            }
            Self::Clamp { screen, mode } => join(&[b"clamp", screen.as_bytes(), mode.as_str().as_bytes()]),
            Self::Screenshot { path } => join(&[b"screenshot", path.as_os_str().as_bytes()]),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn token(&mut self) -> Option<&'a [u8]> {
        while self.pos < self.bytes.len() && is_ws(self.bytes[self.pos]) {
            self.pos += 1;
        }
        let start = self.pos;
        while self.pos < self.bytes.len() && !is_ws(self.bytes[self.pos]) {
            self.pos += 1;
        }
        (self.pos > start).then(|| &self.bytes[start..self.pos])
    }

    fn rest(&mut self) -> &'a [u8] {
        let mut r = &self.bytes[self.pos..];
        self.pos = self.bytes.len();
        if r.first() == Some(&b' ') {
            r = &r[1..];
        }
        r
    }
}

fn scan_float(t: &[u8]) -> Option<f32> {
    let mut i = usize::from(matches!(t.first(), Some(b'+' | b'-')));
    let d0 = i;
    while i < t.len() && t[i].is_ascii_digit() {
        i += 1;
    }
    let int_len = i - d0;
    let mut frac_len = 0;
    if t.get(i) == Some(&b'.') {
        let mut j = i + 1;
        while j < t.len() && t[j].is_ascii_digit() {
            j += 1;
        }
        frac_len = j - (i + 1);
        if int_len > 0 || frac_len > 0 {
            i = j;
        }
    }
    if int_len + frac_len == 0 {
        return None;
    }
    if matches!(t.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(t.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let e0 = j;
        while j < t.len() && t[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            i = j;
        }
    }
    std::str::from_utf8(&t[..i]).ok()?.parse::<f32>().ok()
}

fn scan_int(t: &[u8]) -> Option<i32> {
    let mut i = 0usize;
    let neg = match t.first() {
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
    let d0 = i;
    let mut v: i64 = 0;
    while i < t.len() && t[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add(i64::from(t[i] - b'0'));
        i += 1;
    }
    if i == d0 {
        return None;
    }
    if neg {
        v = -v;
    }
    Some(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

fn extract_f32(token: Option<&[u8]>, missing: f32) -> f32 {
    match token {
        None => missing,
        Some(t) => scan_float(t).unwrap_or(0.0),
    }
}

fn extract_i32(token: Option<&[u8]>, missing: i32) -> i32 {
    match token {
        None => missing,
        Some(t) => scan_int(t).unwrap_or(0),
    }
}

fn skip_leading_ws(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|&&c| is_ws(c)).count();
    &b[n..]
}

fn atoi(value: &[u8]) -> i32 {
    scan_int(skip_leading_ws(value)).unwrap_or(0)
}

fn atof(value: &[u8]) -> f32 {
    scan_float(skip_leading_ws(value)).unwrap_or(0.0)
}

fn set_bool(value: &[u8]) -> bool {
    value == b"true" || value == b"1"
}

fn token_string(cur: &mut Cursor<'_>) -> String {
    cur.token()
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .unwrap_or_default()
}

fn rest_string(cur: &mut Cursor<'_>) -> String {
    String::from_utf8_lossy(cur.rest()).into_owned()
}

fn rest_path(cur: &mut Cursor<'_>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(cur.rest()).to_os_string())
}

fn parse_workshop(cur: &mut Cursor<'_>) -> Request {
    let Some(verb) = cur.token() else {
        return Request::Unknown;
    };
    match verb {
        b"search" => Request::Workshop(WorkshopRequest::Search(rest_string(cur))),
        b"state" => match cur.token() {
            Some(id) => Request::Workshop(WorkshopRequest::State(String::from_utf8_lossy(id).into_owned())),
            None => Request::Rejected,
        },
        b"unsubscribe" => match cur.token() {
            Some(id) => Request::Workshop(WorkshopRequest::Unsubscribe(
                String::from_utf8_lossy(id).into_owned(),
            )),
            None => Request::Rejected,
        },
        b"subscribe" => match cur.token() {
            Some(id) => Request::Workshop(WorkshopRequest::Subscribe(
                String::from_utf8_lossy(id).into_owned(),
            )),
            None => Request::Rejected,
        },
        b"job" => match cur
            .token()
            .and_then(|t| std::str::from_utf8(t).ok()?.parse().ok())
        {
            Some(job) => Request::Workshop(WorkshopRequest::Job(job)),
            None => Request::Rejected,
        },
        _ => Request::Unknown,
    }
}

fn parse_set(cur: &mut Cursor<'_>) -> Request {
    let Some(key) = cur.token() else {
        return Request::Rejected;
    };
    let opt = match key {
        b"fps" => SetOption::Fps(atoi(cur.rest()).max(1)),
        b"noautomute" => SetOption::NoAutomute(set_bool(cur.rest())),
        b"disablemouse" => SetOption::DisableMouse(set_bool(cur.rest())),
        b"disableparallax" => SetOption::DisableParallax(set_bool(cur.rest())),
        b"nofullscreenpause" => SetOption::NoFullscreenPause(set_bool(cur.rest())),
        b"renderscale" => SetOption::RenderScale(atof(cur.rest()).clamp(0.5, 2.0)),
        b"batteryfps" => SetOption::BatteryFps(u32::try_from(atoi(cur.rest())).unwrap_or(0)),
        b"audiodevice" => {
            let v = rest_string(cur);
            SetOption::AudioDevice(if v == "default" { String::new() } else { v })
        }
        _ => return Request::Rejected,
    };
    Request::Command(Command::Set(opt))
}

pub fn parse_request(line: &[u8]) -> Request {
    if line.is_empty() {
        return Request::Empty;
    }
    let mut cur = Cursor::new(line);
    let Some(cmd) = cur.token() else {
        return Request::Unknown;
    };
    match cmd {
        b"ping" => Request::Ping,
        b"status" => Request::Status,
        b"list" => Request::List,
        b"workshop" => parse_workshop(&mut cur),
        b"getproperties" => Request::GetProperties {
            screen: cur.token().map(|t| String::from_utf8_lossy(t).into_owned()),
        },
        b"speed" => {
            let v = extract_f32(cur.token(), 1.0);
            Request::Command(Command::Speed(if v <= 0.0 { 1.0 } else { v }))
        }
        b"volume" => Request::Command(Command::Volume(extract_i32(cur.token(), 128))),
        b"mute" => Request::Command(Command::Mute(extract_i32(cur.token(), 0) != 0)),
        b"set" => parse_set(&mut cur),
        b"bg" => {
            let screen = token_string(&mut cur);
            let path = rest_path(&mut cur);
            Request::Command(Command::Bg { screen, path })
        }
        b"preload" => Request::Command(Command::Preload {
            path: rest_path(&mut cur),
        }),
        b"property" => {
            let screen = token_string(&mut cur);
            let key = token_string(&mut cur);
            let value = rest_string(&mut cur);
            Request::Command(Command::Property { screen, key, value })
        }
        b"stage" => {
            let key = token_string(&mut cur);
            let value = rest_string(&mut cur);
            Request::Command(Command::Property {
                screen: String::new(),
                key,
                value,
            })
        }
        b"scaling" => {
            let screen = token_string(&mut cur);
            let mode = match cur.token() {
                Some(b"stretch") => ScalingMode::Stretch,
                Some(b"fit") => ScalingMode::Fit,
                Some(b"fill") => ScalingMode::Fill,
                Some(b"default") => ScalingMode::Default,
                _ => return Request::Rejected,
            };
            Request::Command(Command::Scaling { screen, mode })
        }
        b"clamp" => {
            let screen = token_string(&mut cur);
            let mode = match cur.token() {
                Some(b"clamp") => ClampMode::Clamp,
                Some(b"border") => ClampMode::Border,
                Some(b"repeat") => ClampMode::Repeat,
                _ => return Request::Rejected,
            };
            Request::Command(Command::Clamp { screen, mode })
        }
        b"screenshot" => Request::Command(Command::Screenshot {
            path: rest_path(&mut cur),
        }),
        _ => Request::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &[u8]) -> Command {
        match parse_request(line) {
            Request::Command(c) => c,
            other => panic!(
                "expected Command for {:?}, got {other:?}",
                String::from_utf8_lossy(line)
            ),
        }
    }

    #[test]
    fn empty_and_whitespace_lines() {
        assert_eq!(parse_request(b""), Request::Empty);
        assert_eq!(parse_request(b"   "), Request::Unknown);
        assert_eq!(parse_request(b"\r"), Request::Unknown);
    }

    #[test]
    fn ping_and_status() {
        assert_eq!(parse_request(b"ping"), Request::Ping);
        assert_eq!(parse_request(b"ping with extra args"), Request::Ping);
        assert_eq!(parse_request(b"ping\r"), Request::Ping);
        assert_eq!(parse_request(b"  ping"), Request::Ping);
        assert_eq!(parse_request(b"status"), Request::Status);
        assert_eq!(parse_request(b"PING"), Request::Unknown);
        assert_eq!(parse_request(b"frobnicate"), Request::Unknown);
    }

    #[test]
    fn getproperties_optional_screen() {
        assert_eq!(
            parse_request(b"getproperties"),
            Request::GetProperties { screen: None }
        );
        assert_eq!(
            parse_request(b"getproperties HDMI-A-1"),
            Request::GetProperties {
                screen: Some("HDMI-A-1".into())
            }
        );
        assert_eq!(
            parse_request(b"getproperties DP-2 extra"),
            Request::GetProperties {
                screen: Some("DP-2".into())
            }
        );
        assert_eq!(parse_request(b"GetProperties"), Request::Unknown);
    }

    #[test]
    fn speed_coercions() {
        assert_eq!(cmd(b"speed"), Command::Speed(1.0));
        assert_eq!(cmd(b"speed abc"), Command::Speed(1.0));
        assert_eq!(cmd(b"speed 0"), Command::Speed(1.0));
        assert_eq!(cmd(b"speed -2.5"), Command::Speed(1.0));
        assert_eq!(cmd(b"speed 0.5"), Command::Speed(0.5));
        assert_eq!(cmd(b"speed 2"), Command::Speed(2.0));
        assert_eq!(cmd(b"speed 1.5x"), Command::Speed(1.5));
        assert_eq!(cmd(b"speed .5"), Command::Speed(0.5));
        assert_eq!(cmd(b"speed 1e1"), Command::Speed(10.0));
    }

    #[test]
    fn volume_coercions() {
        assert_eq!(cmd(b"volume"), Command::Volume(128));
        assert_eq!(cmd(b"volume abc"), Command::Volume(0));
        assert_eq!(cmd(b"volume 15"), Command::Volume(15));
        assert_eq!(cmd(b"volume -5"), Command::Volume(-5));
        assert_eq!(cmd(b"volume 500"), Command::Volume(500));
        assert_eq!(cmd(b"volume 99999999999999999999"), Command::Volume(i32::MAX));
        assert_eq!(cmd(b"volume -99999999999999999999"), Command::Volume(i32::MIN));
    }

    #[test]
    fn mute_coercions() {
        assert_eq!(cmd(b"mute"), Command::Mute(false));
        assert_eq!(cmd(b"mute 0"), Command::Mute(false));
        assert_eq!(cmd(b"mute 1"), Command::Mute(true));
        assert_eq!(cmd(b"mute 2"), Command::Mute(true));
        assert_eq!(cmd(b"mute -1"), Command::Mute(true));
        assert_eq!(cmd(b"mute x"), Command::Mute(false));
    }

    #[test]
    fn set_keys_and_coercions() {
        assert_eq!(parse_request(b"set"), Request::Rejected);
        assert_eq!(parse_request(b"set bogus 1"), Request::Rejected);
        assert_eq!(cmd(b"set fps 30"), Command::Set(SetOption::Fps(30)));
        assert_eq!(cmd(b"set fps abc"), Command::Set(SetOption::Fps(1)));
        assert_eq!(cmd(b"set fps -5"), Command::Set(SetOption::Fps(1)));
        assert_eq!(cmd(b"set fps 0"), Command::Set(SetOption::Fps(1)));
        assert_eq!(cmd(b"set fps  60"), Command::Set(SetOption::Fps(60)));
        assert_eq!(
            cmd(b"set disablemouse true"),
            Command::Set(SetOption::DisableMouse(true))
        );
        assert_eq!(
            cmd(b"set disablemouse 1"),
            Command::Set(SetOption::DisableMouse(true))
        );
        assert_eq!(
            cmd(b"set disablemouse TRUE"),
            Command::Set(SetOption::DisableMouse(false))
        );
        assert_eq!(
            cmd(b"set disablemouse yes"),
            Command::Set(SetOption::DisableMouse(false))
        );
        assert_eq!(
            cmd(b"set disablemouse true "),
            Command::Set(SetOption::DisableMouse(false))
        );
        assert_eq!(
            cmd(b"set noautomute true"),
            Command::Set(SetOption::NoAutomute(true))
        );
        assert_eq!(
            cmd(b"set disableparallax 1"),
            Command::Set(SetOption::DisableParallax(true))
        );
        assert_eq!(
            cmd(b"set nofullscreenpause true"),
            Command::Set(SetOption::NoFullscreenPause(true))
        );
        assert_eq!(
            cmd(b"set renderscale 1.06"),
            Command::Set(SetOption::RenderScale(1.06))
        );
        assert_eq!(
            cmd(b"set renderscale 5"),
            Command::Set(SetOption::RenderScale(2.0))
        );
        assert_eq!(
            cmd(b"set renderscale 0.1"),
            Command::Set(SetOption::RenderScale(0.5))
        );
        assert_eq!(
            cmd(b"set renderscale abc"),
            Command::Set(SetOption::RenderScale(0.5))
        );
        assert_eq!(
            cmd(b"set audiodevice default"),
            Command::Set(SetOption::AudioDevice(String::new()))
        );
        assert_eq!(
            cmd(b"set audiodevice alsa_output.pci 0000"),
            Command::Set(SetOption::AudioDevice("alsa_output.pci 0000".into()))
        );
    }

    #[test]
    fn rest_of_line_semantics() {
        assert_eq!(
            cmd(b"bg HDMI-A-1 /path/with spaces"),
            Command::Bg {
                screen: "HDMI-A-1".into(),
                path: PathBuf::from("/path/with spaces")
            }
        );
        assert_eq!(
            cmd(b"bg HDMI-A-1  /padded"),
            Command::Bg {
                screen: "HDMI-A-1".into(),
                path: PathBuf::from(" /padded")
            }
        );
        assert_eq!(
            cmd(b"bg HDMI-A-1 /a\r"),
            Command::Bg {
                screen: "HDMI-A-1".into(),
                path: PathBuf::from("/a\r")
            }
        );
        assert_eq!(
            cmd(b"bg"),
            Command::Bg {
                screen: String::new(),
                path: PathBuf::new()
            }
        );
    }

    #[test]
    fn property_values() {
        assert_eq!(
            cmd(b"property HDMI-A-1 outline 0.36585 0.04268 0.43902"),
            Command::Property {
                screen: "HDMI-A-1".into(),
                key: "outline".into(),
                value: "0.36585 0.04268 0.43902".into(),
            }
        );
        assert_eq!(
            cmd(b"property HDMI-A-1 bloom true"),
            Command::Property {
                screen: "HDMI-A-1".into(),
                key: "bloom".into(),
                value: "true".into()
            }
        );
        assert_eq!(
            cmd(b"property"),
            Command::Property {
                screen: String::new(),
                key: String::new(),
                value: String::new()
            }
        );
    }

    #[test]
    fn scaling_and_clamp_modes() {
        for (s, m) in [
            ("stretch", ScalingMode::Stretch),
            ("fit", ScalingMode::Fit),
            ("fill", ScalingMode::Fill),
            ("default", ScalingMode::Default),
        ] {
            assert_eq!(
                cmd(format!("scaling HDMI-A-1 {s}").as_bytes()),
                Command::Scaling {
                    screen: "HDMI-A-1".into(),
                    mode: m
                }
            );
        }
        for (s, m) in [
            ("clamp", ClampMode::Clamp),
            ("border", ClampMode::Border),
            ("repeat", ClampMode::Repeat),
        ] {
            assert_eq!(
                cmd(format!("clamp HDMI-A-1 {s}").as_bytes()),
                Command::Clamp {
                    screen: "HDMI-A-1".into(),
                    mode: m
                }
            );
        }
        assert_eq!(parse_request(b"scaling HDMI-A-1 bogusmode"), Request::Rejected);
        assert_eq!(parse_request(b"scaling HDMI-A-1"), Request::Rejected);
        assert_eq!(parse_request(b"clamp HDMI-A-1 nope"), Request::Rejected);
        assert_eq!(parse_request(b"clamp"), Request::Rejected);
    }

    #[test]
    fn preload_and_screenshot() {
        assert_eq!(
            cmd(b"preload /w/dir"),
            Command::Preload {
                path: PathBuf::from("/w/dir")
            }
        );
        assert_eq!(
            cmd(b"screenshot /tmp/a b.png"),
            Command::Screenshot {
                path: PathBuf::from("/tmp/a b.png")
            }
        );
        assert_eq!(cmd(b"screenshot"), Command::Screenshot { path: PathBuf::new() });
    }

    #[test]
    fn non_utf8_paths_survive() {
        let line = b"bg HDMI-A-1 /weird/\xff\xfe/dir";
        let Command::Bg { path, .. } = cmd(line) else {
            panic!("expected bg")
        };
        assert_eq!(path.as_os_str().as_bytes(), b"/weird/\xff\xfe/dir");
    }

    #[test]
    fn round_trip_every_variant() {
        let all = [
            Command::Speed(0.5),
            Command::Speed(1.0),
            Command::Speed(2.25),
            Command::Volume(0),
            Command::Volume(64),
            Command::Volume(-3),
            Command::Volume(500),
            Command::Mute(true),
            Command::Mute(false),
            Command::Set(SetOption::Fps(30)),
            Command::Set(SetOption::NoAutomute(true)),
            Command::Set(SetOption::NoAutomute(false)),
            Command::Set(SetOption::DisableMouse(true)),
            Command::Set(SetOption::DisableParallax(false)),
            Command::Set(SetOption::NoFullscreenPause(true)),
            Command::Set(SetOption::RenderScale(1.06)),
            Command::Set(SetOption::BatteryFps(10)),
            Command::Set(SetOption::BatteryFps(0)),
            Command::Set(SetOption::AudioDevice(String::new())),
            Command::Set(SetOption::AudioDevice("alsa_output.pci 0000_00.analog".into())),
            Command::Bg {
                screen: "HDMI-A-1".into(),
                path: PathBuf::from("/path/with spaces/dir"),
            },
            Command::Bg {
                screen: "span:DP-1".into(),
                path: PathBuf::from(" /leading-space"),
            },
            Command::Preload {
                path: PathBuf::from("/w/431960/3047596375"),
            },
            Command::Property {
                screen: "HDMI-A-1".into(),
                key: "outline".into(),
                value: "0.36585 0.04268 0.43902".into(),
            },
            Command::Property {
                screen: "default".into(),
                key: "bloom".into(),
                value: String::new(),
            },
            Command::Scaling {
                screen: "HDMI-A-1".into(),
                mode: ScalingMode::Stretch,
            },
            Command::Scaling {
                screen: "HDMI-A-1".into(),
                mode: ScalingMode::Fit,
            },
            Command::Scaling {
                screen: "HDMI-A-1".into(),
                mode: ScalingMode::Fill,
            },
            Command::Scaling {
                screen: "HDMI-A-1".into(),
                mode: ScalingMode::Default,
            },
            Command::Clamp {
                screen: "HDMI-A-1".into(),
                mode: ClampMode::Clamp,
            },
            Command::Clamp {
                screen: "HDMI-A-1".into(),
                mode: ClampMode::Border,
            },
            Command::Clamp {
                screen: "HDMI-A-1".into(),
                mode: ClampMode::Repeat,
            },
            Command::Screenshot {
                path: PathBuf::from("/tmp/shot.png"),
            },
        ];
        for c in all {
            let line = c.to_request_line();
            assert_eq!(
                parse_request(&line),
                Request::Command(c.clone()),
                "round-trip failed for line {:?}",
                String::from_utf8_lossy(&line)
            );
        }
    }
}
