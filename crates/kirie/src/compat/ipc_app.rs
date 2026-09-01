use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kirie_platform::{CaptureFn, RenderCommand};

use super::run::SwapCtx;
use crossbeam_channel::{Receiver, Sender, select};
use kirie_ipc::{
    ClampMode as IpcClamp, Command, CommandOutcome, IpcEvent, ScalingMode as IpcScaling, ScreenStatus,
    SetOption, StatusSnapshot,
};
use kirie_video::{ScalingMode as VideoScaling, VideoControl};

pub enum Register {
    Video { screen: String, control: VideoControl },
    Background { screen: String, bg: PathBuf },
}

struct ScreenEntry {
    bg: Option<PathBuf>,
    control: Option<VideoControl>,
}

struct AppState {
    screens: BTreeMap<String, ScreenEntry>,
    workshop_jobs: Arc<crate::workshop::Jobs>,
    speed: f32,
    volume: i32,
    muted: bool,
    properties: BTreeMap<String, String>,
    staged: BTreeMap<String, String>,
    swap: Arc<Mutex<Option<SwapCtx>>>,
    prop_gen: Arc<std::sync::atomic::AtomicU64>,
}

pub struct IpcApp {
    register: Sender<Register>,
    swap: Arc<Mutex<Option<SwapCtx>>>,
}

impl IpcApp {
    pub fn spawn(
        events: Receiver<IpcEvent>,
        seed_screens: Vec<(String, Option<PathBuf>)>,
        speed: f32,
        volume: i32,
        muted: bool,
    ) -> Self {
        let (register_tx, register_rx) = crossbeam_channel::unbounded::<Register>();
        let swap: Arc<Mutex<Option<SwapCtx>>> = Arc::new(Mutex::new(None));
        let mut state = AppState {
            screens: seed_screens
                .into_iter()
                .map(|(name, bg)| (name, ScreenEntry { bg, control: None }))
                .collect(),
            workshop_jobs: Arc::default(),
            speed,
            volume,
            muted,
            properties: BTreeMap::new(),
            staged: BTreeMap::new(),
            swap: swap.clone(),
            prop_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        std::thread::Builder::new()
            .name("kirie-ipc-app".into())
            .spawn(move || run(&mut state, &events, &register_rx))
            .expect("spawn ipc-app thread");
        Self {
            register: register_tx,
            swap,
        }
    }

    #[must_use]
    pub fn registrar(&self) -> Sender<Register> {
        self.register.clone()
    }

    #[must_use]
    pub(crate) fn swap_slot(&self) -> Arc<Mutex<Option<SwapCtx>>> {
        self.swap.clone()
    }
}

fn run(state: &mut AppState, events: &Receiver<IpcEvent>, register: &Receiver<Register>) {
    loop {
        select! {
            recv(events) -> msg => match msg {
                Ok(event) => handle_event(state, event),
                Err(_) => {
                    while register.try_recv().is_ok() {}
                    return;
                }
            },
            recv(register) -> msg => match msg {
                Ok(reg) => handle_register(state, reg),
                Err(_) => {
                    while let Ok(event) = events.recv() {
                        handle_event(state, event);
                    }
                    return;
                }
            },
        }
    }
}

fn handle_register(state: &mut AppState, reg: Register) {
    match reg {
        Register::Video { screen, control } => {
            control.set_speed(f64::from(state.speed));
            control.set_volume(f64::from(state.volume) * 100.0 / 128.0);
            control.set_mute(state.muted);
            let entry = state.screens.entry(screen).or_insert(ScreenEntry {
                bg: None,
                control: None,
            });
            entry.control = Some(control);
        }
        Register::Background { screen, bg } => {
            let entry = state.screens.entry(screen).or_insert(ScreenEntry {
                bg: None,
                control: None,
            });
            entry.bg = Some(bg);
        }
    }
}

fn remember_background(state: &mut AppState, screen: &str, path: PathBuf) {
    if screen == "*" {
        if state.screens.is_empty() {
            state.screens.insert(
                screen.to_owned(),
                ScreenEntry {
                    bg: Some(path),
                    control: None,
                },
            );
            return;
        }
        for entry in state.screens.values_mut() {
            entry.bg = Some(path.clone());
        }
        return;
    }
    state
        .screens
        .entry(screen.to_owned())
        .or_insert_with(|| ScreenEntry {
            bg: None,
            control: None,
        })
        .bg = Some(path);
}

fn handle_event(state: &mut AppState, event: IpcEvent) {
    match event {
        IpcEvent::Status { reply } => {
            let snapshot = StatusSnapshot {
                speed: state.speed,
                screens: state
                    .screens
                    .iter()
                    .map(|(name, entry)| ScreenStatus {
                        screen: name.clone(),
                        bg: entry.bg.clone(),
                    })
                    .collect(),
            };
            let _ = reply.send(snapshot);
        }
        IpcEvent::Workshop { request, reply } => {
            crate::workshop::serve_socket(&state.workshop_jobs, request, reply);
        }
        IpcEvent::List { reply } => {
            let _ = reply.send(crate::list::to_json(&crate::list::scan(None)));
        }
        IpcEvent::GetProperties { screen, reply } => {
            let source = match &screen {
                Some(name) => state.screens.get(name).and_then(|e| e.bg.clone()),
                None => state.screens.values().find_map(|e| e.bg.clone()),
            };
            let body = match source {
                Some(dir) => super::list_props::properties_json_string(&dir, &state.properties),
                None => "[]".to_string(),
            };
            let _ = reply.send(body);
        }
        IpcEvent::Command { command, reply } => {
            let outcome = apply_command(state, command);
            let _ = reply.send(outcome);
        }
    }
}

fn apply_command(state: &mut AppState, command: Command) -> CommandOutcome {
    match command {
        Command::Speed(s) => {
            state.speed = s;
            for entry in state.screens.values() {
                if let Some(c) = &entry.control {
                    c.set_speed(f64::from(s));
                }
            }
            if let Some(cmd_tx) = cmd_sender(state) {
                let _ = cmd_tx.send(RenderCommand::SetSpeed(s));
            }
            CommandOutcome::Ok
        }
        Command::Volume(v) => {
            state.volume = v;
            let mapped = f64::from(v) * 100.0 / 128.0;
            for entry in state.screens.values() {
                if let Some(c) = &entry.control {
                    c.set_volume(mapped);
                }
            }
            CommandOutcome::Ok
        }
        Command::Mute(m) => {
            state.muted = m;
            for entry in state.screens.values() {
                if let Some(c) = &entry.control {
                    c.set_mute(m);
                }
            }
            CommandOutcome::Ok
        }
        Command::Set(opt) => {
            match opt {
                SetOption::Fps(n) => {
                    if let Some(cmd_tx) = cmd_sender(state) {
                        let fps = u32::try_from(n).ok().filter(|f| *f > 0);
                        let _ = cmd_tx.send(RenderCommand::SetFps(fps));
                    }
                }
                SetOption::RenderScale(v) => {
                    let clamped = if v.is_finite() { v.clamp(0.25, 4.0) } else { 1.0 };
                    if (clamped - super::run::render_scale()).abs() > f32::EPSILON {
                        super::run::set_render_scale(v);
                        rebuild_current(state);
                    }
                }
                SetOption::BatteryFps(n) => {
                    super::run::battery_fps_target().store(n, std::sync::atomic::Ordering::Relaxed);
                }
                SetOption::DisableParallax(on) if on != super::run::disable_parallax() => {
                    super::run::set_disable_parallax(on);
                    rebuild_current(state);
                }
                _ => {}
            }
            CommandOutcome::Ok
        }
        Command::Bg { screen, path } => {
            state.prop_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let staged: Vec<(String, String)> = std::mem::take(&mut state.staged).into_iter().collect();
            state.properties = super::saved_props::with_saved(&path, &staged).into_iter().collect();
            if !staged.is_empty() {
                let all: Vec<(String, String)> = state
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                super::saved_props::write(&path, &all);
            }
            let sc = state
                .swap
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| (s.cmd_tx.clone(), s.build.clone())));
            let Some((cmd_tx, build_ctx)) = sc else {
                return CommandOutcome::Refused("the renderer is not ready yet".to_owned());
            };
            let props: Vec<(String, String)> = state
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            #[cfg(any(feature = "web-cef", feature = "web-webview"))]
            let props_web = props.clone();
            if let Some(build) = build_ctx.build_fn(screen.clone(), &path, props) {
                let _ = cmd_tx.send(RenderCommand::Swap {
                    screen: screen.clone(),
                    key: path.to_string_lossy().into_owned(),
                    build,
                });
                remember_background(state, &screen, path);
                return CommandOutcome::Ok;
            }
            #[cfg(any(feature = "web-cef", feature = "web-webview"))]
            if let Some(build_local) = build_ctx.build_local_fn(screen.clone(), &path, props_web) {
                let _ = cmd_tx.send(RenderCommand::SwapLocal {
                    screen: screen.clone(),
                    build_local,
                });
                remember_background(state, &screen, path);
                return CommandOutcome::Ok;
            }
            CommandOutcome::Refused(why_not(&path))
        }
        Command::Preload { path } => {
            if let Some((cmd_tx, build_ctx)) = state
                .swap
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| (s.cmd_tx.clone(), s.build.clone())))
            {
                let props: Vec<(String, String)> = state
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if let Some(build) = build_ctx.build_fn("*".to_owned(), &path, props) {
                    let _ = cmd_tx.send(RenderCommand::Build {
                        screen: "*".to_owned(),
                        stash: Some(path.to_string_lossy().into_owned()),
                        build,
                    });
                }
            }
            CommandOutcome::Ok
        }
        Command::Property { screen, key, value } => {
            if screen.is_empty() {
                state.staged.insert(key.clone(), value.clone());
                return CommandOutcome::Ok;
            }
            state.properties.insert(key.clone(), value.clone());
            if let Some(showing) = state.screens.get(&screen).and_then(|e| e.bg.clone()) {
                super::saved_props::remember(&showing, &key, &value);
            }
            let sc = state
                .swap
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| (s.cmd_tx.clone(), s.build.clone())));
            if let Some((cmd_tx, build_ctx)) = sc {
                let structural = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let _ = cmd_tx.send(RenderCommand::SetProperty {
                    screen: screen.clone(),
                    key,
                    value,
                    structural: structural.clone(),
                });
                if !screen.is_empty()
                    && let Some(path) = state.screens.get(&screen).and_then(|e| e.bg.clone())
                {
                    use std::sync::atomic::Ordering;
                    let props: Vec<(String, String)> = state
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let generation = state.prop_gen.fetch_add(1, Ordering::SeqCst) + 1;
                    let gen_slot = state.prop_gen.clone();
                    let screen_c = screen.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(350));
                        if gen_slot.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        if !structural.load(Ordering::SeqCst) {
                            return;
                        }
                        if let Some(build) = build_ctx.build_fn(screen_c.clone(), &path, props) {
                            let key = format!("{}#props", path.to_string_lossy());
                            let _ = cmd_tx.send(RenderCommand::Swap {
                                screen: screen_c,
                                key,
                                build,
                            });
                        }
                    });
                }
            }
            if state.screens.get(&screen).is_some_and(|e| e.bg.is_some()) {
                CommandOutcome::Ok
            } else {
                CommandOutcome::Error
            }
        }
        Command::Scaling { screen, mode } => match state.screens.get(&screen) {
            Some(entry) if entry.bg.is_some() => {
                if let Some(c) = &entry.control {
                    c.set_scaling(map_scaling(mode));
                }
                if let Some((_, build_ctx)) = swap_parts(state)
                    && build_ctx.set_scaling(scaling_to_args(mode))
                {
                    rebuild_current(state);
                }
                CommandOutcome::Ok
            }
            _ => CommandOutcome::Error,
        },
        Command::Clamp { screen, mode } => match state.screens.get(&screen) {
            Some(entry) if entry.bg.is_some() => {
                if let Some((_, build_ctx)) = swap_parts(state)
                    && build_ctx.set_clamp(clamp_to_args(mode))
                {
                    rebuild_current(state);
                }
                CommandOutcome::Ok
            }
            _ => CommandOutcome::Error,
        },
        Command::Screenshot { path } => {
            #[cfg(all(feature = "web-webview", not(feature = "web-cef")))]
            {
                let web_active = state.screens.values().any(|e| {
                    e.bg.as_deref().is_some_and(|b| {
                        matches!(
                            super::resolve::classify(&b.to_string_lossy()),
                            Ok(super::resolve::Wallpaper::Web { .. })
                        )
                    })
                });
                if web_active {
                    return CommandOutcome::Error;
                }
            }
            let cmd_tx = state
                .swap
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| s.cmd_tx.clone()));
            let Some(cmd_tx) = cmd_tx else {
                return CommandOutcome::Error;
            };
            let capture: CaptureFn = Box::new(move |device, queue, renderer, size, format| {
                if let Err(e) = super::screenshot::capture_live(device, queue, renderer, size, format, &path)
                {
                    tracing::warn!(error = format!("{e:#}"), "socket screenshot failed");
                }
            });
            let _ = cmd_tx.send(RenderCommand::Screenshot {
                screen: "*".to_owned(),
                capture,
            });
            CommandOutcome::Ok
        }
    }
}

// The bg failed to build. Say which of the ordinary reasons it was, rather
// than leaving the caller with a bare `error`.
fn why_not(path: &std::path::Path) -> String {
    match super::resolve::classify(&path.to_string_lossy()) {
        Err(err) => err.to_string(),
        Ok(wallpaper) => {
            if let Some(note) = super::resolve::refuse_without_assets(&wallpaper) {
                return note.replace('\n', " ");
            }
            wallpaper
                .unrunnable_reason()
                .unwrap_or_else(|| "the renderer could not build it".to_owned())
        }
    }
}

fn swap_parts(state: &AppState) -> Option<(kirie_platform::CommandSender, Arc<super::run::BuildContext>)> {
    state
        .swap
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| (s.cmd_tx.clone(), s.build.clone())))
}

fn cmd_sender(state: &AppState) -> Option<kirie_platform::CommandSender> {
    swap_parts(state).map(|(tx, _)| tx)
}

fn rebuild_current(state: &mut AppState) {
    let Some((cmd_tx, build_ctx)) = swap_parts(state) else {
        return;
    };
    state.prop_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let props: Vec<(String, String)> = state
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let screens: Vec<(String, std::path::PathBuf)> = state
        .screens
        .iter()
        .filter_map(|(s, e)| e.bg.clone().map(|b| (s.clone(), b)))
        .collect();
    for (screen, path) in screens {
        #[cfg(any(feature = "web-cef", feature = "web-webview"))]
        let props_web = props.clone();
        if let Some(build) = build_ctx.build_fn(screen.clone(), &path, props.clone()) {
            let _ = cmd_tx.send(RenderCommand::Build {
                screen,
                stash: None,
                build,
            });
            continue;
        }
        #[cfg(any(feature = "web-cef", feature = "web-webview"))]
        if let Some(build_local) = build_ctx.build_local_fn(screen.clone(), &path, props_web) {
            let _ = cmd_tx.send(RenderCommand::SwapLocal { screen, build_local });
        }
    }
}

fn scaling_to_args(mode: IpcScaling) -> super::args::ScalingMode {
    match mode {
        IpcScaling::Stretch => super::args::ScalingMode::Stretch,
        IpcScaling::Fit => super::args::ScalingMode::Fit,
        IpcScaling::Fill => super::args::ScalingMode::Fill,
        IpcScaling::Default => super::args::ScalingMode::Default,
    }
}

fn clamp_to_args(mode: IpcClamp) -> super::args::ClampMode {
    match mode {
        IpcClamp::Clamp => super::args::ClampMode::Clamp,
        IpcClamp::Border => super::args::ClampMode::Border,
        IpcClamp::Repeat => super::args::ClampMode::Repeat,
    }
}

fn map_scaling(mode: IpcScaling) -> VideoScaling {
    match mode {
        IpcScaling::Stretch => VideoScaling::Stretch,
        IpcScaling::Fit => VideoScaling::Fit,
        IpcScaling::Fill => VideoScaling::Fill,
        IpcScaling::Default => VideoScaling::Default,
    }
}
