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

fn answer(stream: &UnixStream, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }

    let reply = act(line.trim(), orders, showing, args);
    let mut writer = stream;
    if writer.write_all(reply.as_bytes()).is_ok() {
        let _ = writer.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn act(line: &str, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) -> String {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "ping" => "pong\n".to_owned(),
        "status" => showing.report(),
        "bg" => put_up(rest, orders, showing, args),
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
    }
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
        "batteryfps" | "noautomute" | "disablemouse" | "nofullscreenpause" => "ok\n".to_owned(),
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

fn put_up(rest: &str, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) -> String {
    let (screen, path) = rest.split_once(' ').unwrap_or(("", rest));
    let path = path.trim();
    if path.is_empty() {
        return "error\n".to_owned();
    }
    let Ok(wallpaper) = resolve::classify(path) else {
        return "error\n".to_owned();
    };
    if resolve::refuse_without_assets(&wallpaper).is_some() || wallpaper.unrunnable_reason().is_some() {
        return "error\n".to_owned();
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
            return "error\n".to_owned();
        }
    }
    showing.forget_props();
    showing.put_up(screen, Path::new(path));
    showing
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    "ok\n".to_owned()
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
}
