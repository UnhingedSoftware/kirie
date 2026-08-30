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
}

impl Showing {
    #[must_use]
    pub fn new(names: &[String], background: Option<&Path>, speed: f32) -> Arc<Self> {
        let mut screens = BTreeMap::new();
        for name in names {
            screens.insert(name.clone(), background.map(Path::to_path_buf));
        }
        Arc::new(Self {
            screens: Mutex::new(screens),
            speed: Mutex::new(speed),
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
        if let Ok(mut screens) = self.screens.lock() {
            if screen.is_empty() {
                for held in screens.values_mut() {
                    *held = Some(wallpaper.to_path_buf());
                }
            } else if let Some(held) = screens.get_mut(screen) {
                *held = Some(wallpaper.to_path_buf());
            }
        }
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
        answer(&stream, &orders, &showing, &args);
    }
}

fn answer(stream: &UnixStream, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) {
    let reader = BufReader::new(stream);
    let mut writer = stream;
    for line in reader.lines() {
        let Ok(line) = line else { return };
        let reply = act(line.trim(), orders, showing, args);
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
        let _ = writer.flush();
    }
}

fn act(line: &str, orders: &Sender<RenderCommand>, showing: &Arc<Showing>, args: &CompatArgs) -> String {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "ping" => "pong\n".to_owned(),
        "status" => showing.report(),
        "bg" => put_up(rest, orders, showing, args),
        "property" => set_property(rest, orders, showing),
        "speed" => {
            let speed = rest.trim().parse::<f32>().unwrap_or(1.0);
            let speed = if speed > 0.0 { speed } else { 1.0 };
            if let Ok(mut held) = showing.speed.lock() {
                *held = speed;
            }
            let _ = orders.send(RenderCommand::SetSpeed(speed));
            "ok\n".to_owned()
        }
        "set" => set(rest, orders),
        "preload" => "ok\n".to_owned(),
        "volume" | "mute" => "unknown command\n".to_owned(),
        _ => "unknown command\n".to_owned(),
    }
}

fn set(rest: &str, orders: &Sender<RenderCommand>) -> String {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    match key {
        "fps" => {
            let fps = value.trim().parse::<u32>().unwrap_or(0);
            let _ = orders.send(RenderCommand::SetFps((fps > 0).then_some(fps)));
            "ok\n".to_owned()
        }
        "batteryfps" | "noautomute" | "disablemouse" | "nofullscreenpause" => "ok\n".to_owned(),
        _ => "unknown command\n".to_owned(),
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

    let screens = if screen.is_empty() {
        showing.names()
    } else {
        vec![screen.to_owned()]
    };
    for name in screens {
        let build_local = build_for(wallpaper.clone(), args);
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
    showing.put_up(screen, Path::new(path));
    "ok\n".to_owned()
}

fn set_property(rest: &str, orders: &Sender<RenderCommand>, showing: &Arc<Showing>) -> String {
    let mut parts = rest.splitn(3, ' ');
    let (Some(screen), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next()) else {
        return "error\n".to_owned();
    };
    let screens = if screen.is_empty() {
        showing.names()
    } else {
        vec![screen.to_owned()]
    };
    for name in screens {
        let _ = orders.send(RenderCommand::SetProperty {
            screen: name,
            key: key.to_owned(),
            value: value.to_owned(),
            structural: Arc::new(AtomicBool::new(false)),
        });
    }
    "ok\n".to_owned()
}

fn build_for(wallpaper: Wallpaper, args: &CompatArgs) -> kirie_platform::BuildLocalFn {
    let scaling: ScalingMode = args.window_scaling;
    let clamp: ClampMode = args.window_clamp;
    let properties = args.set_properties.clone();
    let sound = Sound {
        volume: args.volume,
        silent: args.silent,
    };

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
