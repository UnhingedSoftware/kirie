use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use kirie_platform::{RenderCommand, RenderTarget, Renderer, SurfaceSize};

use crate::compat::args::{ClampMode, CompatArgs, ScalingMode};
use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::{Sound, build_presented_renderer};

pub struct Showing {
    screens: Mutex<BTreeMap<String, Option<PathBuf>>>,
    speed: Mutex<f32>,
    props: Mutex<BTreeMap<String, String>>,
    generation: std::sync::atomic::AtomicU64,
    sound: Mutex<Sound>,
}

impl Showing {
    #[must_use]
    pub fn new(names: &[String], background: Option<&Path>, speed: f32, sound: Sound) -> Arc<Self> {
        let mut screens = BTreeMap::new();
        for name in names {
            screens.insert(name.clone(), background.map(Path::to_path_buf));
        }
        Arc::new(Self {
            screens: Mutex::new(screens),
            speed: Mutex::new(speed),
            props: Mutex::new(BTreeMap::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
            sound: Mutex::new(sound),
        })
    }

    fn report(&self) -> String {
        let speed = self.speed.lock().map(|held| *held).unwrap_or(1.0);
        let mut out = format!("speed={speed}\n");
        if let Ok(screens) = self.screens.lock() {
            for (name, wallpaper) in screens.iter() {
                let path = wallpaper
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push_str(&format!("screen={name} bg={path}\n"));
            }
        }
        out
    }

    fn put_up(&self, screen: &str, wallpaper: &Path) {
        let wanted = self.meaning(screen);
        if let Ok(mut screens) = self.screens.lock() {
            for name in wanted {
                if let Some(held) = screens.get_mut(&name) {
                    *held = Some(wallpaper.to_path_buf());
                }
            }
        }
    }

    fn meaning(&self, screen: &str) -> Vec<String> {
        let names = self.names();
        if screen.is_empty() || !names.iter().any(|name| name == screen) {
            return names;
        }
        vec![screen.to_owned()]
    }

    fn wallpaper_on(&self, screen: &str) -> Option<PathBuf> {
        let screens = self.screens.lock().ok()?;
        screens
            .get(screen)
            .cloned()
            .flatten()
            .or_else(|| screens.values().find_map(Clone::clone))
    }

    fn remember(&self, key: &str, value: &str) -> Vec<(String, String)> {
        let mut props = match self.props.lock() {
            Ok(props) => props,
            Err(_) => return Vec::new(),
        };
        props.insert(key.to_owned(), value.to_owned());
        props.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn forget_props(&self) {
        if let Ok(mut props) = self.props.lock() {
            props.clear();
        }
    }

    fn sound(&self) -> Sound {
        self.sound.lock().map(|held| *held).unwrap_or(Sound {
            volume: 128,
            silent: false,
        })
    }

    fn properties(&self) -> Vec<(String, String)> {
        self.props
            .lock()
            .map(|props| props.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    fn names(&self) -> Vec<String> {
        self.screens
            .lock()
            .map(|screens| screens.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[must_use]
pub fn already_running(socket: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let mut writer = &stream;
    if writeln!(writer, "ping").is_err() {
        return false;
    }
    let _ = writer.flush();
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).is_ok() && answer.trim() == "pong"
}

pub fn serve(socket: PathBuf, orders: Sender<RenderCommand>, showing: Arc<Showing>, args: CompatArgs) {
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(path = %socket.display(), %err, "cannot open the control socket");
            return;
        }
    };
    tracing::info!(path = %socket.display(), "control socket listening");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            answer(&stream, &orders, &showing, &args);
        }));
        if handled.is_err() {
            tracing::error!("a control command panicked; the socket stays open");
        }
    }
    tracing::error!(path = %socket.display(), "the control socket stopped listening");
}

pub fn serve_relaunching(socket: PathBuf, showing: Arc<Showing>) {
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(path = %socket.display(), %err, "cannot open the control socket");
            return;
        }
    };
    tracing::info!(path = %socket.display(), "control socket listening (web wallpaper)");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            continue;
        }

        let line = line.trim().to_owned();
        let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        tracing::debug!(verb, rest, "control command (web wallpaper)");

        let reply = match verb {
            "ping" => "pong\n".to_owned(),
            "status" => showing.report(),
            "bg" => {
                let (_, path) = rest.split_once(' ').unwrap_or(("", rest));
                let path = path.trim().to_owned();
                if path.is_empty() {
                    "error\n".to_owned()
                } else {
                    let mut writer = &stream;
                    let _ = writer.write_all(b"ok\n");
                    let _ = writer.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    restart_with(&path);
                    return;
                }
            }
            _ => "unknown command\n".to_owned(),
        };

        let mut writer = &stream;
        let _ = writer.write_all(reply.as_bytes());
        let _ = writer.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn restart_with(wallpaper: &str) {
    use std::os::unix::process::CommandExt as _;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut kept = keep_without_background(std::env::args_os().skip(1));
    kept.push(std::ffi::OsString::from(format!("--bg={wallpaper}")));

    tracing::info!(wallpaper, "restarting to change the web wallpaper");
    let failed = std::process::Command::new(exe).args(kept).exec();
    tracing::error!(%failed, "could not restart with the new wallpaper");
}

#[must_use]
pub fn keep_without_background<I>(argv: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut kept = Vec::new();
    let mut skip_value = false;
    for argument in argv {
        if skip_value {
            skip_value = false;
            continue;
        }
        let text = argument.to_string_lossy();
        if text == "--bg" {
            skip_value = true;
            continue;
        }
        if text.starts_with("--bg=") {
            continue;
        }
        kept.push(argument);
    }
    kept
}

fn answer(stream: &UnixStream, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }

    let (reply, restart) = act(line.trim(), orders, showing, args);
    let mut writer = stream;
    if writer.write_all(reply.as_bytes()).is_ok() {
        let _ = writer.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);

    if let Some(wallpaper) = restart {
        restart_with(&wallpaper);
    }
}

fn act(
    line: &str,
    orders: &Sender<RenderCommand>,
    showing: &Arc<Showing>,
    args: &CompatArgs,
) -> (String, Option<String>) {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    tracing::debug!(verb, rest, "control command");
    if verb == "bg" {
        return put_up(rest, orders, showing, args);
    }
    let reply = match verb {
        "ping" => "pong\n".to_owned(),
        "status" => showing.report(),
        "property" => set_property(rest, orders, showing, args),
        "speed" => {
            let speed = rest.trim().parse::<f32>().unwrap_or(1.0);
            let speed = if speed > 0.0 { speed } else { 1.0 };
            if let Ok(mut held) = showing.speed.lock() {
                *held = speed;
            }
            let _ = orders.send(RenderCommand::SetSpeed(speed));
            "ok\n".to_owned()
        }
        "set" => set(rest, orders, showing, args),
        "preload" => "ok\n".to_owned(),
        "volume" => {
            let wanted = rest.trim().parse::<i64>().unwrap_or(100).clamp(0, 100);
            if let Ok(mut sound) = showing.sound.lock() {
                sound.volume = wanted * 128 / 100;
            }
            rebuild_all(orders, showing, args);
            "ok\n".to_owned()
        }
        "mute" => {
            let quiet = matches!(rest.trim(), "1" | "true" | "on" | "yes");
            if let Ok(mut sound) = showing.sound.lock() {
                sound.silent = quiet;
            }
            rebuild_all(orders, showing, args);
            "ok\n".to_owned()
        }
        _ => "unknown command\n".to_owned(),
    };
    (reply, None)
}

fn set(rest: &str, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) -> String {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let on = matches!(value.trim(), "1" | "true" | "on" | "yes");
    match key {
        "fps" => {
            let fps = value.trim().parse::<u32>().unwrap_or(0);
            let _ = orders.send(RenderCommand::SetFps((fps > 0).then_some(fps)));
            "ok\n".to_owned()
        }
        "renderscale" => {
            let scale = value.trim().parse::<f32>().unwrap_or(1.0);
            super::common::set_render_scale(scale);
            rebuild_all(orders, showing, args);
            "ok\n".to_owned()
        }
        "disableparallax" => {
            super::common::set_disable_parallax(on);
            rebuild_all(orders, showing, args);
            "ok\n".to_owned()
        }
        "batteryfps" => {
            #[cfg(target_os = "macos")]
            kirie_platform::set_battery_fps(value.trim().parse::<u32>().unwrap_or(0));
            "ok\n".to_owned()
        }
        "noautomute" | "disablemouse" | "nofullscreenpause" => "ok\n".to_owned(),
        _ => "unknown command\n".to_owned(),
    }
}

fn rebuild_all(orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) {
    let props = showing.properties();
    let sound = showing.sound();
    for name in showing.names() {
        let Some(path) = showing.wallpaper_on(&name) else {
            continue;
        };
        let Ok(wallpaper) = resolve::classify(&path.to_string_lossy()) else {
            continue;
        };
        let _ = orders.send(RenderCommand::SwapLocal {
            screen: name,
            build_local: build_with(wallpaper, args, props.clone(), sound),
        });
    }
}

fn put_up(
    rest: &str,
    orders: &Sender<RenderCommand>,
    showing: &Arc<Showing>,
    args: &CompatArgs,
) -> (String, Option<String>) {
    let (screen, path) = rest.split_once(' ').unwrap_or(("", rest));
    let path = path.trim();
    if path.is_empty() {
        return ("error\n".to_owned(), None);
    }
    let wallpaper = match resolve::classify(path) {
        Ok(found) => found,
        Err(err) => return refused(path, &err.to_string()),
    };
    if let Some(note) = resolve::refuse_without_assets(&wallpaper) {
        return refused(path, &note.replace('\n', " "));
    }
    if let Some(reason) = wallpaper.unrunnable_reason() {
        return refused(path, &reason);
    }
    #[cfg(all(target_os = "macos", feature = "web-webview"))]
    if let Wallpaper::Web { dir, file } = &wallpaper {
        return hand_to_web(dir, file, path, orders, showing);
    }
    #[cfg(not(all(target_os = "macos", feature = "web-webview")))]
    if matches!(wallpaper, Wallpaper::Web { .. }) {
        return ("ok\n".to_owned(), Some(path.to_owned()));
    }

    for name in showing.meaning(screen) {
        let build_local = build_with(wallpaper.clone(), args, showing.properties(), showing.sound());
        if orders
            .send(RenderCommand::SwapLocal {
                screen: name,
                build_local,
            })
            .is_err()
        {
            return ("error\n".to_owned(), None);
        }
    }
    showing.forget_props();
    showing.put_up(screen, Path::new(path));
    showing
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ("ok\n".to_owned(), None)
}

#[cfg(all(target_os = "macos", feature = "web-webview"))]
fn hand_to_web(
    dir: &Path,
    file: &str,
    path: &str,
    orders: &Sender<RenderCommand>,
    showing: &Arc<Showing>,
) -> (String, Option<String>) {
    let url = resolve::web_entry_url(dir, file);
    let level = level_of(showing.sound());
    for name in showing.meaning("") {
        let wanted = url.clone();
        let make: kirie_platform::MakeViewFn = Box::new(move |size: SurfaceSize| {
            let view = kirie_web::wk::desktop_view(
                &wanted,
                kirie_web::WebSize {
                    width: size.width,
                    height: size.height,
                },
                level,
            );
            match view {
                Ok(view) => objc2::rc::Retained::into_super(view),
                Err(err) => {
                    tracing::error!(%err, "cannot open the page");
                    let mtm = objc2::MainThreadMarker::new().expect("main thread");
                    objc2_app_kit::NSView::new(mtm)
                }
            }
        });
        if orders
            .send(RenderCommand::SetView { screen: name, make })
            .is_err()
        {
            return ("error the renderer stopped listening\n".to_owned(), None);
        }
    }
    showing.forget_props();
    showing.put_up("", Path::new(path));
    showing
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ("ok\n".to_owned(), None)
}

#[must_use]
pub fn level_of(sound: Sound) -> f32 {
    if sound.silent {
        0.0
    } else {
        (sound.volume as f32 / 128.0).clamp(0.0, 1.0)
    }
}

// The reply the C++ engine gives is a bare `error`. Saying why is a kirie
// extension: a client that only knows the original protocol still sees `error`
// as the first word.
fn refused(path: &str, reason: &str) -> (String, Option<String>) {
    tracing::warn!(path, reason, "cannot put that wallpaper up");
    (format!("error {reason}\n"), None)
}

fn set_property(
    rest: &str,
    orders: &Sender<RenderCommand>,
    showing: &Arc<Showing>,
    args: &CompatArgs,
) -> String {
    let mut parts = rest.splitn(3, ' ');
    let (Some(screen), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next()) else {
        return "error\n".to_owned();
    };

    let props = showing.remember(key, value);
    let generation = showing
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;

    for name in showing.meaning(screen) {
        let structural = Arc::new(AtomicBool::new(true));
        if orders
            .send(RenderCommand::SetProperty {
                screen: name.clone(),
                key: key.to_owned(),
                value: value.to_owned(),
                structural: Arc::clone(&structural),
            })
            .is_err()
        {
            return "error\n".to_owned();
        }
        rebuild_if_needed(
            &name,
            structural,
            generation,
            orders,
            showing,
            args,
            props.clone(),
        );
    }
    "ok\n".to_owned()
}

fn rebuild_if_needed(
    screen: &str,
    structural: Arc<AtomicBool>,
    generation: u64,
    orders: &Sender<RenderCommand>,
    showing: &Arc<Showing>,
    args: &CompatArgs,
    props: Vec<(String, String)>,
) {
    if showing.wallpaper_on(screen).is_none() {
        return;
    }
    let orders = orders.clone();
    let sound = showing.sound();
    let showing = Arc::clone(showing);
    let args = args.clone();
    let screen = screen.to_owned();

    let started = std::thread::Builder::new()
        .name("kirie-property".to_owned())
        .spawn(move || {
            std::thread::sleep(SETTLE);
            let now = showing.generation.load(std::sync::atomic::Ordering::SeqCst);
            if now != generation || !structural.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let Some(path) = showing.wallpaper_on(&screen) else {
                return;
            };
            let Ok(wallpaper) = resolve::classify(&path.to_string_lossy()) else {
                return;
            };
            let _ = orders.send(RenderCommand::SwapLocal {
                screen,
                build_local: build_with(wallpaper, &args, props, sound),
            });
        });
    if let Err(err) = started {
        tracing::warn!(%err, "cannot rebuild after a property change");
    }
}

const SETTLE: std::time::Duration = std::time::Duration::from_millis(350);

fn build_with(
    wallpaper: Wallpaper,
    args: &CompatArgs,
    properties: Vec<(String, String)>,
    sound: Sound,
) -> kirie_platform::BuildLocalFn {
    let scaling: ScalingMode = args.window_scaling;
    let clamp: ClampMode = args.window_clamp;

    Box::new(
        move |device: &wgpu::Device,
              queue: &wgpu::Queue,
              format: wgpu::TextureFormat,
              name: &str,
              size: (u32, u32)| {
            let target = RenderTarget {
                device,
                queue,
                format,
                output_name: name,
                size,
            };
            let surface = SurfaceSize {
                width: size.0,
                height: size.1,
            };
            match build_presented_renderer(&target, &wallpaper, scaling, clamp, surface, &properties, sound) {
                Ok(renderer) => renderer,
                Err(err) => {
                    tracing::error!(screen = name, "cannot build the wallpaper: {err:#}");
                    Box::new(Blank)
                }
            }
        },
    )
}

struct Blank;

impl Renderer for Blank {
    fn render(&mut self, _view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {}

    fn is_placeholder(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Read as _;
    use std::sync::mpsc::Receiver;

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|part| OsString::from(*part)).collect()
    }

    #[test]
    fn the_old_wallpaper_is_dropped_in_both_spellings() {
        let kept = keep_without_background(argv(&["--bg=/old", "--fps=30"]));
        assert_eq!(kept, argv(&["--fps=30"]));

        let kept = keep_without_background(argv(&["--bg", "/old", "--fps=30"]));
        assert_eq!(kept, argv(&["--fps=30"]));
    }

    #[test]
    fn everything_else_survives_the_restart() {
        let kept = keep_without_background(argv(&[
            "--control-socket=/tmp/lwe.sock",
            "--scaling=fill",
            "--bg",
            "/old",
            "--silent",
        ]));
        assert_eq!(
            kept,
            argv(&["--control-socket=/tmp/lwe.sock", "--scaling=fill", "--silent"])
        );
    }

    struct Served {
        socket: PathBuf,
        orders: Receiver<RenderCommand>,
        showing: Arc<Showing>,
        _scratch: Scratch,
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kirie-ipc-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            Self(dir)
        }

        fn wallpaper(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("a.png"), [0_u8; 16]);
            let _ = std::fs::write(
                dir.join("project.json"),
                r#"{"type":"image","file":"a.png","title":"t"}"#,
            );
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn serving(name: &str) -> Served {
        let scratch = Scratch::new(name);
        let socket = scratch.0.join("lwe.sock");
        let (orders, received) = std::sync::mpsc::channel();
        let showing = Showing::new(
            &["Desktop".to_owned()],
            None,
            1.0,
            Sound {
                volume: 128,
                silent: false,
            },
        );

        let held = Arc::clone(&showing);
        let path = socket.clone();
        std::thread::spawn(move || serve(path, orders, held, CompatArgs::default()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Served {
            socket,
            orders: received,
            showing,
            _scratch: scratch,
        }
    }

    fn ask(socket: &Path, line: &str) -> String {
        let stream = UnixStream::connect(socket).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let mut writing = &stream;
        writeln!(writing, "{line}").expect("write");
        writing.flush().ok();

        let mut said = String::new();
        let mut reading = &stream;
        reading.read_to_string(&mut said).expect("read");
        said
    }

    #[test]
    fn it_answers_a_ping() {
        let served = serving("ping");
        assert_eq!(ask(&served.socket, "ping"), "pong\n");
    }

    #[test]
    fn every_command_gets_its_own_connection_answered() {
        let served = serving("framing");
        for _ in 0..3 {
            assert_eq!(ask(&served.socket, "ping"), "pong\n");
        }
        assert_eq!(ask(&served.socket, "nonsense"), "unknown command\n");
        assert_eq!(ask(&served.socket, "ping"), "pong\n");
    }

    #[test]
    fn a_wallpaper_path_with_spaces_is_taken_whole() {
        let served = serving("spaces");
        let dir = served._scratch.wallpaper("Application Support");
        let reply = ask(&served.socket, &format!("bg Desktop {}", dir.display()));
        assert_eq!(reply, "ok\n");

        match served.orders.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(RenderCommand::SwapLocal { screen, .. }) => assert_eq!(screen, "Desktop"),
            other => panic!("expected a swap, got {:?}", other.is_ok()),
        }
        assert!(served.showing.report().contains(&dir.display().to_string()));
    }

    #[test]
    fn a_screen_kirie_does_not_know_still_gets_the_wallpaper() {
        let served = serving("screen");
        let dir = served._scratch.wallpaper("wall");
        assert_eq!(
            ask(&served.socket, &format!("bg NotAScreen {}", dir.display())),
            "ok\n"
        );
        match served.orders.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(RenderCommand::SwapLocal { screen, .. }) => assert_eq!(screen, "Desktop"),
            other => panic!("expected a swap, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn a_wallpaper_that_is_not_there_is_refused_with_a_reason() {
        let served = serving("missing");
        let said = ask(&served.socket, "bg Desktop /nowhere/at/all");
        assert!(said.starts_with("error "), "{said}");
        assert!(said.contains("does not exist"), "{said}");
        assert!(
            served
                .orders
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err()
        );
    }

    #[test]
    fn a_property_reaches_the_render_loop() {
        let served = serving("property");
        assert_eq!(ask(&served.socket, "property Desktop colour 1 0 0"), "ok\n");
        match served.orders.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(RenderCommand::SetProperty { key, value, .. }) => {
                assert_eq!(key, "colour");
                assert_eq!(value, "1 0 0");
            }
            other => panic!("expected a property, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn a_frame_rate_reaches_the_render_loop() {
        let served = serving("fps");
        assert_eq!(ask(&served.socket, "set fps 30"), "ok\n");
        match served.orders.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(RenderCommand::SetFps(fps)) => assert_eq!(fps, Some(30)),
            other => panic!("expected a frame rate, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn status_reports_the_speed_and_the_screens() {
        let served = serving("status");
        let said = ask(&served.socket, "status");
        assert!(said.starts_with("speed=1"), "{said}");
        assert!(said.contains("screen=Desktop bg="), "{said}");
    }

    #[test]
    fn a_new_wallpaper_cancels_a_pending_property_rebuild() {
        let served = serving("generation");
        let first = served._scratch.wallpaper("first");
        assert_eq!(
            ask(&served.socket, &format!("bg Desktop {}", first.display())),
            "ok\n"
        );
        let before = served
            .showing
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);

        assert_eq!(ask(&served.socket, "property Desktop k v"), "ok\n");
        let second = served._scratch.wallpaper("second");
        assert_eq!(
            ask(&served.socket, &format!("bg Desktop {}", second.display())),
            "ok\n"
        );

        let after = served
            .showing
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(after > before, "{before} -> {after}");
    }
}
