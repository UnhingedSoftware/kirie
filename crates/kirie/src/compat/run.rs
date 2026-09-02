use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kirie_audio::{AudioCapture, AudioConfig, AutoMute};
use kirie_platform::{CommandSender, Platform, RenderCommand, RenderTarget, Renderer, SurfaceSize};
use kirie_render::{ImageContent, ImageOptions, ImageRenderer};
#[cfg(any(feature = "web-cef", feature = "web-webview"))]
use kirie_render::{MediaConfig, MediaSource};
use kirie_video::{VideoControl, VideoOptions, VideoPlayer, VideoRenderer};

use crate::compat::args::{ClampMode, CompatArgs, ScalingMode, WindowMode};
pub use crate::compat::common::*;
use crate::compat::ipc_app::{IpcApp, Register};
use crate::compat::playlist::{ActivePlaylist, PlaylistDefinition, Rng};
use crate::compat::power;
use crate::compat::resolve::{self, ClassifyError, Wallpaper};
use crate::compat::{list_props, screenshot, signals};

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
use crate::compat::webfeed::EngineWebFeed;
#[cfg(any(feature = "web-cef", feature = "web-webview"))]
use kirie_web::{WebBackend, WebRenderer, WebSize};

#[cfg(feature = "web-cef")]
type LiveWebBackend = kirie_web::hosted::HostedBackend;
#[cfg(all(feature = "web-webview", not(feature = "web-cef")))]
type LiveWebBackend = kirie_web::viewhost::ViewHostBackend;

fn wants_audio(spec: &RunSpec) -> bool {
    match spec {
        RunSpec::Scene { .. } => true,
        #[cfg(any(feature = "web-cef", feature = "web-webview"))]
        RunSpec::Web { .. } => true,
        _ => false,
    }
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
fn wants_media(spec: &RunSpec) -> bool {
    matches!(spec, RunSpec::Web { .. })
}

#[must_use]
pub fn audio_config(args: &CompatArgs) -> AudioConfig {
    let mut config = if args.no_audio_processing {
        AudioConfig::disabled()
    } else {
        AudioConfig::with_device(args.audio_device.clone())
    };
    config.power_save = Some(power_save_flag());
    config
}

pub(crate) struct LazySources {
    audio_config: AudioConfig,
    audio: std::sync::OnceLock<Arc<AudioCapture>>,
    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    media: std::sync::OnceLock<Arc<MediaSource>>,
}

impl LazySources {
    fn new(audio_config: AudioConfig) -> Self {
        Self {
            audio_config,
            audio: std::sync::OnceLock::new(),
            #[cfg(any(feature = "web-cef", feature = "web-webview"))]
            media: std::sync::OnceLock::new(),
        }
    }

    fn audio(&self) -> Arc<AudioCapture> {
        self.audio
            .get_or_init(|| {
                let cap = Arc::new(AudioCapture::start(self.audio_config.clone()));
                tracing::info!(status = ?cap.status(), device = ?cap.device(), "audio capture");
                cap
            })
            .clone()
    }

    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    fn media(&self) -> Arc<MediaSource> {
        self.media
            .get_or_init(|| {
                let src = Arc::new(MediaSource::start(MediaConfig::default()));
                tracing::info!(status = ?src.status(), "mpris media source");
                src
            })
            .clone()
    }

    fn audio_for(&self, spec: &RunSpec) -> Option<Arc<AudioCapture>> {
        wants_audio(spec).then(|| self.audio())
    }

    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    fn media_for(&self, spec: &RunSpec) -> Option<Arc<MediaSource>> {
        wants_media(spec).then(|| self.media())
    }
}

pub fn dispatch(args: CompatArgs) -> ExitCode {
    set_render_scale(args.render_scale as f32);
    kirie_render::set_focus(args.focus.0, args.focus.1);
    set_fit_render_to_output(args.fit_render_to_output);
    set_object_filter(&args.render_debug);

    if args.list_properties || args.list_properties_json {
        return match list_props::run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::FAILURE
            }
        };
    }

    if let Some(path) = args.screenshot.clone() {
        return run_screenshot(&args, &path);
    }

    run_wallpapers(args)
}

fn run_screenshot(args: &CompatArgs, path: &Path) -> ExitCode {
    let Some(bg) = args.default_background.clone() else {
        eprintln!("At least one background ID must be specified");
        return ExitCode::FAILURE;
    };
    let wallpaper = match resolve::classify(&bg) {
        Ok(w) => w,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(note) = resolve::refuse_without_assets(&wallpaper) {
        eprintln!("{note}");
        return ExitCode::FAILURE;
    }
    let audio = Arc::new(AudioCapture::start(audio_config(args)));
    match screenshot::capture(
        &wallpaper,
        args.window_scaling,
        args.window_clamp,
        args.screenshot_delay,
        path,
        Some(audio),
        &args.set_properties,
    ) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "screenshot written");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("screenshot failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone)]
enum RunSpec {
    Video {
        media: PathBuf,
        scaling: ScalingMode,
    },
    Image {
        file: PathBuf,
        scaling: ScalingMode,
        clamp: ClampMode,
    },
    Scene {
        dir: PathBuf,
        scaling: ScalingMode,
        clamp: ClampMode,
    },
    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    Web {
        url: String,
        dir: PathBuf,
    },
    Skip,
}

struct Target {
    screen: String,
    bg: PathBuf,
    spec: RunSpec,
    runnable: bool,
    app_fatal: bool,
}

fn run_wallpapers(args: CompatArgs) -> ExitCode {
    set_disable_parallax(args.disable_parallax);
    let window_mode = args.mode != WindowMode::DesktopBackground;
    let targets = build_targets(&args);
    if targets.is_empty() {
        eprintln!("At least one background ID must be specified");
        return ExitCode::FAILURE;
    }

    if resolve::we_assets_dir().is_none() && targets.iter().any(|t| matches!(t.spec, RunSpec::Scene { .. })) {
        eprintln!("{}", resolve::missing_assets_note());
        return ExitCode::FAILURE;
    }

    if targets.iter().any(|t| t.app_fatal) {
        eprintln!("Application wallpapers are not supported on this platform");
        eprintln!("Application wallpapers are not supported on this platform");
        return ExitCode::FAILURE;
    }

    if !targets.iter().any(|t| t.runnable) {
        for t in &targets {
            eprintln!("{}: {} — {}", t.screen, t.bg.display(), unrunnable_note(&t.bg));
        }
        return ExitCode::FAILURE;
    }
    for t in &targets {
        if !t.runnable {
            eprintln!(
                "{}: {} — {} (this output will stay black)",
                t.screen,
                t.bg.display(),
                unrunnable_note(&t.bg)
            );
        }
    }

    let seed: Vec<(String, Option<PathBuf>)> = targets
        .iter()
        .map(|t| (t.screen.clone(), Some(t.bg.clone())))
        .collect();
    let (socket, ipc_app) = setup_socket(&args, seed);
    let registrar = ipc_app.as_ref().map(IpcApp::registrar);

    signals::install_cleanup(args.control_socket.clone());

    let specs: Vec<(String, RunSpec)> = targets.into_iter().map(|t| (t.screen, t.spec)).collect();
    let prebake_root: Option<PathBuf> = specs.iter().find_map(|(_, spec)| match spec {
        RunSpec::Scene { dir, .. } => dir.parent().map(std::path::Path::to_path_buf),
        _ => None,
    });
    let volume = args.volume;
    let silent = args.silent;
    let properties = args.set_properties.clone();

    let active_playlists: Vec<(String, PlaylistDefinition, Option<PathBuf>)> = if window_mode {
        args.window_playlist
            .clone()
            .map(|p| {
                let current = p.items.first().cloned();
                ("default".to_owned(), p, current)
            })
            .into_iter()
            .collect()
    } else {
        args.screens
            .iter()
            .filter_map(|s| {
                s.playlist.clone().map(|p| {
                    let current = s.background.clone().map(PathBuf::from);
                    (s.name.clone(), p, current)
                })
            })
            .collect()
    };
    let rotation_properties = args.set_properties.clone();
    let playlist_stop = Arc::new(AtomicBool::new(false));
    let mut playlist_handle: Option<std::thread::JoinHandle<()>> = None;

    let sources = Arc::new(LazySources::new(audio_config(&args)));

    let has_video = specs
        .iter()
        .any(|(_, spec)| matches!(spec, RunSpec::Video { .. }));
    let automute = Arc::new(AutoMute::start(!args.noautomute && has_video));
    tracing::info!(enabled = automute.enabled(), "automute detector");

    let video_controls: Arc<Mutex<Vec<VideoControl>>> = Arc::new(Mutex::new(Vec::new()));
    let applier_stop = Arc::new(AtomicBool::new(false));
    let applier = spawn_automute_applier(&automute, &video_controls, &applier_stop);

    let screen_roots: Vec<String> = if window_mode {
        Vec::new()
    } else {
        specs.iter().map(|(name, _)| name.clone()).collect()
    };

    let (bg_scaling, bg_clamp) = args
        .screens
        .first()
        .map(|s| (s.scaling, s.clamp))
        .unwrap_or((args.window_scaling, args.window_clamp));
    let build_ctx = Arc::new(BuildContext {
        scaling: Mutex::new(bg_scaling),
        clamp: Mutex::new(bg_clamp),
        volume,
        silent,
        registrar: registrar.clone(),
        sources: sources.clone(),
        automute_controls: video_controls.clone(),
    });

    let initial_specs: std::collections::HashMap<String, RunSpec> =
        specs.iter().map(|(n, s)| (n.clone(), s.clone())).collect();
    let window_spec: Option<RunSpec> = specs.first().map(|(_, s)| s.clone());
    let ib_registrar = registrar.clone();
    let ib_sources = sources.clone();
    let ib_properties = properties.clone();
    let ib_controls = video_controls.clone();
    let initial_build: kirie_platform::InitialBuildFn = Box::new(move |output: &str| {
        let (screen_key, spec) = if window_mode {
            ("default".to_owned(), window_spec.clone()?)
        } else {
            (output.to_owned(), initial_specs.get(output).cloned()?)
        };
        match spec {
            RunSpec::Video { .. } | RunSpec::Image { .. } | RunSpec::Scene { .. } => {}
            _ => return None,
        }
        let registrar = ib_registrar.clone();
        let audio = ib_sources.audio_for(&spec);
        let properties = ib_properties.clone();
        let controls = ib_controls.clone();
        let build: kirie_platform::BuildFn = Box::new(move |device, queue, format, name, size, position| {
            let rt = RenderTarget {
                device,
                queue,
                format,
                output_name: name,
                size,
                position,
            };
            build_for_spec(
                &rt,
                screen_key,
                &spec,
                volume,
                silent,
                registrar.as_ref(),
                audio,
                &properties,
                &controls,
            )
        });
        Some(build)
    });

    let factory_controls = video_controls.clone();
    let factory_sources = sources.clone();
    let factory: kirie_platform::RendererFactory = Box::new(move |target: &RenderTarget<'_>| {
        build_renderer(
            target,
            &specs,
            window_mode,
            volume,
            silent,
            registrar.as_ref(),
            &factory_sources,
            &properties,
            &factory_controls,
        )
    });

    let fullscreen_paused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let present = kirie_platform::PresentOptions {
        screen_roots,
        activity_paused: Some(fullscreen_paused.clone()),
        fps: u32::try_from(args.fps).ok().filter(|f| *f > 0),
        playback_speed: args.playback_speed,
        fullscreen_pause: !args.no_fullscreen_pause,
        fullscreen_pause_only_active: args.fullscreen_pause_only_active,
        fullscreen_pause_ignore_appids: args.fullscreen_pause_ignore_appid.clone(),
        release_hidden_after: args.release_hidden_after.map(Duration::from_secs),
        ..Default::default()
    };

    let prebaker = if std::env::var_os("KIRIE_NO_PREBAKE").is_none() {
        let fullscreen = fullscreen_paused.clone();
        let power = power_save_flag();
        let pause: kirie_bake::PauseFn = std::sync::Arc::new(move || {
            fullscreen.load(std::sync::atomic::Ordering::Relaxed)
                || power.load(std::sync::atomic::Ordering::Relaxed)
        });
        prebake_root.as_deref().and_then(|root| {
            kirie_render::start_background_prebake(root, resolve::we_assets_dir().as_deref(), Some(pause))
        })
    } else {
        None
    }
    .map(std::sync::Arc::new);

    let power_save = power_save_flag();
    let normal_fps = u32::try_from(args.fps).ok().filter(|f| *f > 0);
    let battery_fps = battery_fps_target();
    battery_fps.store(args.battery_fps, std::sync::atomic::Ordering::Relaxed);
    let power_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut power_handle: Option<std::thread::JoinHandle<()>> = None;

    let asked_window = args
        .window
        .filter(|_| window_mode)
        .map(|w| kirie_platform::X11Mode::Window {
            width: u32::try_from(w.w.max(1)).unwrap_or(1),
            height: u32::try_from(w.h.max(1)).unwrap_or(1),
        });
    let connected = match asked_window {
        Some(mode) => Platform::connect_x11(mode, factory),
        None => Platform::connect_with(kirie_platform::Backend::from_env(), present, factory),
    };
    let exit = match connected {
        Ok(mut platform) => {
            if platform.output_count() > 0 && platform.surface_count() == 0 {
                crate::compat::autopin::recover_from_bad_pin();
            }
            platform.set_initial_build(initial_build);
            if let (Some(app), Some(cmd_tx)) = (ipc_app.as_ref(), platform.command_sender())
                && let Ok(mut slot) = app.swap_slot().lock()
            {
                *slot = Some(SwapCtx {
                    cmd_tx,
                    build: build_ctx.clone(),
                });
            }
            if !active_playlists.is_empty() {
                match platform.command_sender() {
                    Some(cmd_tx) => {
                        playlist_handle = spawn_playlist_rotator(
                            active_playlists,
                            window_mode,
                            cmd_tx,
                            build_ctx.clone(),
                            rotation_properties,
                            playlist_stop.clone(),
                        );
                    }
                    None => tracing::warn!(
                        "playlist rotation needs the live-swap command channel; disabled on this backend"
                    ),
                }
            }
            if let Some(cmd_tx) = platform.command_sender() {
                power_handle = power::spawn(power::PowerWatch {
                    cmd_tx,
                    normal_fps,
                    battery_fps: battery_fps.clone(),
                    power_save: power_save.clone(),
                    baker: prebaker.clone(),
                    stop: power_stop.clone(),
                });
            }
            let duration = std::env::var("KIRIE_RUN_SECONDS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            match platform.run(duration) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!(%err, "presentation layer stopped");
                    ExitCode::FAILURE
                }
            }
        }
        Err(err) => {
            eprintln!("cannot start the wayland presentation layer: {err}");
            ExitCode::FAILURE
        }
    };

    playlist_stop.store(true, Ordering::Relaxed);
    if let Some(h) = playlist_handle {
        h.thread().unpark();
        let _ = h.join();
    }

    power_stop.store(true, Ordering::Relaxed);
    if let Some(h) = power_handle {
        h.thread().unpark();
        let _ = h.join();
    }

    applier_stop.store(true, Ordering::Relaxed);
    if let Some(h) = applier {
        let _ = h.join();
    }
    drop(automute);

    drop(socket);
    drop(ipc_app);
    exit
}

fn spawn_playlist_rotator(
    playlists: Vec<(String, PlaylistDefinition, Option<PathBuf>)>,
    window_mode: bool,
    cmd_tx: CommandSender,
    build: Arc<BuildContext>,
    properties: Vec<(String, String)>,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let mut rng = Rng::seeded();
    let now = Instant::now();
    let mut active: Vec<(String, ActivePlaylist)> = playlists
        .into_iter()
        .filter_map(|(screen, def, current)| {
            ActivePlaylist::start(def, current.as_deref(), now, &mut rng).map(|state| (screen, state))
        })
        .collect();
    if active.is_empty() {
        return None;
    }
    for (screen, state) in &active {
        tracing::info!(
            %screen,
            playlist = state.name(),
            items = state.item_count(),
            "playlist registered"
        );
    }
    std::thread::Builder::new()
        .name("kirie-playlist".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let now = Instant::now();
                let wait = active
                    .iter()
                    .filter_map(|(_, s)| s.next_due())
                    .map(|due| due.saturating_duration_since(now))
                    .min()
                    .unwrap_or(Duration::from_secs(300))
                    .clamp(Duration::from_millis(50), Duration::from_secs(300));
                std::thread::park_timeout(wait);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let now = Instant::now();
                for (screen, state) in &mut active {
                    if !state.due(now) {
                        continue;
                    }
                    let swap_screen = if window_mode { "*" } else { screen.as_str() };
                    let screen_key = screen.clone();
                    state.advance(
                        screen,
                        now,
                        &mut rng,
                        |path| playlist_preflight(&build, &screen_key, path, &properties),
                        |path| playlist_show(&cmd_tx, &build, &screen_key, swap_screen, path, &properties),
                    );
                }
            }
        })
        .ok()
}

fn playlist_preflight(
    build: &Arc<BuildContext>,
    screen: &str,
    path: &Path,
    properties: &[(String, String)],
) -> bool {
    if build
        .build_fn(screen.to_owned(), path, properties.to_vec())
        .is_some()
    {
        return true;
    }
    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    if build
        .build_local_fn(screen.to_owned(), path, properties.to_vec())
        .is_some()
    {
        return true;
    }
    false
}

fn playlist_show(
    cmd_tx: &CommandSender,
    build: &Arc<BuildContext>,
    screen: &str,
    swap_screen: &str,
    path: &Path,
    properties: &[(String, String)],
) -> bool {
    if let Some(build_fn) = build.build_fn(screen.to_owned(), path, properties.to_vec()) {
        let sent = cmd_tx
            .send(RenderCommand::Swap {
                screen: swap_screen.to_owned(),
                key: path.to_string_lossy().into_owned(),
                build: build_fn,
            })
            .is_ok();
        if sent {
            build.notify_background(screen, path);
        }
        return sent;
    }
    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    if let Some(build_local) = build.build_local_fn(screen.to_owned(), path, properties.to_vec()) {
        let sent = cmd_tx
            .send(RenderCommand::SwapLocal {
                screen: swap_screen.to_owned(),
                build_local,
            })
            .is_ok();
        if sent {
            build.notify_background(screen, path);
        }
        return sent;
    }
    false
}

fn spawn_automute_applier(
    automute: &Arc<AutoMute>,
    controls: &Arc<Mutex<Vec<VideoControl>>>,
    stop: &Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    if !automute.enabled() {
        return None;
    }
    let automute = automute.clone();
    let controls = controls.clone();
    let stop = stop.clone();
    Some(
        std::thread::Builder::new()
            .name("kirie-automute-apply".into())
            .spawn(move || {
                let mut last: Option<bool> = None;
                let mut applied_len = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let playing = automute.is_playing();
                    if let Ok(guard) = controls.lock()
                        && (last != Some(playing) || guard.len() != applied_len)
                    {
                        for c in guard.iter() {
                            c.set_mute(playing);
                        }
                        last = Some(playing);
                        applied_len = guard.len();
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .expect("spawn automute applier"),
    )
}

fn build_targets(args: &CompatArgs) -> Vec<Target> {
    let default_bg = args.default_background.clone();
    if args.mode != WindowMode::DesktopBackground {
        let bg = args
            .window_playlist
            .as_ref()
            .and_then(|p| p.items.first())
            .map(|p| p.to_string_lossy().into_owned())
            .or(default_bg);
        let Some(bg) = bg else {
            return Vec::new();
        };
        return vec![make_target(
            "default".to_owned(),
            bg,
            args.window_scaling,
            args.window_clamp,
        )];
    }

    args.screens
        .iter()
        .map(|screen| {
            let bg = screen
                .background
                .clone()
                .or_else(|| default_bg.clone())
                .unwrap_or_default();
            make_target(screen.name.clone(), bg, screen.scaling, screen.clamp)
        })
        .collect()
}

fn make_target(screen: String, bg: String, scaling: ScalingMode, clamp: ClampMode) -> Target {
    let bg_path = PathBuf::from(&bg);
    match resolve::classify(&bg) {
        Ok(Wallpaper::Video { media }) => Target {
            screen,
            bg: bg_path,
            spec: RunSpec::Video { media, scaling },
            runnable: true,
            app_fatal: false,
        },
        Ok(Wallpaper::Image { file }) => Target {
            screen,
            bg: bg_path,
            spec: RunSpec::Image { file, scaling, clamp },
            runnable: true,
            app_fatal: false,
        },
        Ok(Wallpaper::Scene { dir }) => Target {
            screen,
            bg: bg_path,
            spec: RunSpec::Scene { dir, scaling, clamp },
            runnable: true,
            app_fatal: false,
        },
        Ok(Wallpaper::Web { dir, file }) => make_web_target(screen, bg_path, &dir, &file),
        Ok(Wallpaper::Unsupported { kind }) => {
            tracing::warn!(%screen, kind, "wallpaper type not supported");
            Target {
                screen,
                bg: bg_path,
                spec: RunSpec::Skip,
                runnable: false,
                app_fatal: kind == "application",
            }
        }
        Ok(Wallpaper::Asset) => {
            tracing::warn!(%screen, "background is a non-renderable asset (effect preset)");
            Target {
                screen,
                bg: bg_path,
                spec: RunSpec::Skip,
                runnable: false,
                app_fatal: false,
            }
        }
        Err(err) => {
            let reason = classify_reason(&err);
            tracing::warn!(%screen, %reason, "cannot load wallpaper");
            Target {
                screen,
                bg: bg_path,
                spec: RunSpec::Skip,
                runnable: false,
                app_fatal: false,
            }
        }
    }
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
fn make_web_target(screen: String, bg: PathBuf, dir: &Path, file: &str) -> Target {
    let url = resolve::web_entry_url(dir, file);
    #[cfg(feature = "web-cef")]
    tracing::info!(%screen, url, "web wallpaper (CEF off-screen backend)");
    #[cfg(all(feature = "web-webview", not(feature = "web-cef")))]
    tracing::info!(%screen, url, "web wallpaper (webview native-layer backend)");
    Target {
        screen,
        bg,
        spec: RunSpec::Web {
            url,
            dir: dir.to_path_buf(),
        },
        runnable: true,
        app_fatal: false,
    }
}

#[cfg(not(any(feature = "web-cef", feature = "web-webview")))]
fn make_web_target(screen: String, bg: PathBuf, _dir: &Path, _file: &str) -> Target {
    tracing::warn!(%screen, "web wallpaper not runnable in this build");
    Target {
        screen,
        bg,
        spec: RunSpec::Skip,
        runnable: false,
        app_fatal: false,
    }
}

fn classify_reason(err: &ClassifyError) -> String {
    err.to_string()
}

fn web_unrunnable_note() -> String {
    #[cfg(not(any(feature = "web-cef", feature = "web-webview")))]
    {
        "web wallpapers are not supported by this build; rebuild with --features web-cef \
         (composited) or --features web-webview (system webkit)"
            .to_owned()
    }
    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    {
        "web wallpapers are supported on this build".to_owned()
    }
}

fn unrunnable_note(bg: &Path) -> String {
    match resolve::classify(&bg.to_string_lossy()) {
        Ok(Wallpaper::Web { .. }) => web_unrunnable_note(),
        Ok(w) => w
            .unrunnable_reason()
            .unwrap_or_else(|| "not yet supported by kirie".to_owned()),
        Err(err) => classify_reason(&err),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_renderer(
    target: &RenderTarget<'_>,
    specs: &[(String, RunSpec)],
    window_mode: bool,
    volume: i64,
    silent: bool,
    registrar: Option<&crossbeam_channel::Sender<Register>>,
    sources: &LazySources,
    properties: &[(String, String)],
    automute_controls: &Arc<Mutex<Vec<VideoControl>>>,
) -> Box<dyn Renderer> {
    let (screen_key, spec) = if window_mode {
        match specs.first() {
            Some((_, spec)) => ("default".to_owned(), spec),
            None => return black(target),
        }
    } else {
        match specs.iter().find(|(name, _)| name == target.output_name) {
            Some((name, spec)) => (name.clone(), spec),
            None => return black(target),
        }
    };

    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    if let RunSpec::Web { url, dir } = spec {
        return build_web(
            target,
            url,
            dir,
            silent,
            properties,
            sources.audio_for(spec),
            sources.media_for(spec),
        );
    }
    build_for_spec(
        target,
        screen_key,
        spec,
        volume,
        silent,
        registrar,
        sources.audio_for(spec),
        properties,
        automute_controls,
    )
}

#[cfg(any(feature = "web-cef", feature = "web-webview"))]
fn build_web(
    target: &RenderTarget<'_>,
    url: &str,
    dir: &Path,
    silent: bool,
    properties: &[(String, String)],
    audio: Option<Arc<AudioCapture>>,
    media: Option<Arc<MediaSource>>,
) -> Box<dyn Renderer> {
    let size = WebSize {
        width: 1920,
        height: 1080,
    };
    match <LiveWebBackend as WebBackend>::new_on_output(
        url,
        size,
        Some(target.output_name),
        Some(target.position),
    ) {
        Ok(mut backend) => {
            if silent {
                backend.set_muted(true);
            }
            let props = web_props_json(dir, properties);
            if props != "{}" {
                backend.apply_properties(&props);
            }
            tracing::info!(output = %target.output_name, url, "web wallpaper ready");
            let mut renderer = WebRenderer::new(target, Box::new(backend));
            renderer.set_power_save(power_save_flag());
            if let Some(feed) = EngineWebFeed::new(audio, media) {
                renderer.set_feed(Box::new(feed));
            } else {
                tracing::info!(
                    output = %target.output_name,
                    "no audio capture and no MPRIS source; page gets no live audio/media data"
                );
            }
            Box::new(renderer)
        }
        Err(err) => {
            eprintln!("{}: failed to start web backend: {err}", target.output_name);
            black(target)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spec_background(spec: &RunSpec) -> Option<&Path> {
    match spec {
        RunSpec::Scene { dir, .. } => Some(dir),
        RunSpec::Video { media, .. } => Some(media),
        RunSpec::Image { file, .. } => Some(file),
        #[cfg(any(feature = "web-cef", feature = "web-webview"))]
        RunSpec::Web { dir, .. } => Some(dir),
        RunSpec::Skip => None,
    }
}

fn build_for_spec(
    target: &RenderTarget<'_>,
    screen_key: String,
    spec: &RunSpec,
    volume: i64,
    silent: bool,
    registrar: Option<&crossbeam_channel::Sender<Register>>,
    audio: Option<Arc<AudioCapture>>,
    properties: &[(String, String)],
    automute_controls: &Arc<Mutex<Vec<VideoControl>>>,
) -> Box<dyn Renderer + Send> {
    struct TrimOnExit;
    impl Drop for TrimOnExit {
        fn drop(&mut self) {
            kirie_bake::trim_heap();
        }
    }
    let _trim = TrimOnExit;
    let saved = spec_background(spec).map(|bg| super::saved_props::with_saved(bg, properties));
    let properties: &[(String, String)] = saved.as_deref().unwrap_or(properties);
    match spec {
        RunSpec::Video { media, scaling } => {
            let options = VideoOptions {
                volume: volume as f64 * 100.0 / 128.0,
                mute: false,
                silent,
                paused: false,
                scaling: to_video_scaling(*scaling),
                nv12: false,
                enable_audio: true,
            };
            match VideoPlayer::open(media, options) {
                Ok((player, control)) => {
                    if let Ok(mut guard) = automute_controls.lock() {
                        guard.push(control.clone());
                    }
                    if let Some(reg) = registrar {
                        let _ = reg.send(Register::Video {
                            screen: screen_key,
                            control,
                        });
                    }
                    let info = player.info();
                    tracing::info!(
                        output = %target.output_name,
                        width = info.width,
                        height = info.height,
                        audio = player.has_audio(),
                        "video wallpaper ready"
                    );
                    Box::new(VideoRenderer::new(target, player))
                }
                Err(err) => {
                    eprintln!("{}: failed to open video: {err}", target.output_name);
                    black(target)
                }
            }
        }
        RunSpec::Image { file, scaling, clamp } => match ImageContent::from_path(file) {
            Ok(content) => {
                let options = ImageOptions {
                    scaling: to_render_scaling(*scaling),
                    clamp: to_render_clamp(*clamp),
                };
                match ImageRenderer::new(target, &content, options) {
                    Ok(renderer) => {
                        tracing::info!(output = %target.output_name, "image wallpaper ready");
                        Box::new(renderer)
                    }
                    Err(err) => {
                        eprintln!("{}: failed to build image renderer: {err}", target.output_name);
                        black(target)
                    }
                }
            }
            Err(err) => {
                eprintln!("{}: failed to load image: {err}", target.output_name);
                black(target)
            }
        },
        RunSpec::Scene { dir, scaling, clamp } => {
            let options = kirie_render::SceneOptions {
                render_scale: render_scale(),
                scaling: to_render_scaling(*scaling),
                clamp: to_render_clamp(*clamp),
                disable_parallax: disable_parallax(),
                fit_render_to_output: fit_render_to_output(),
                only_objects: object_filter().0,
                skip_objects: object_filter().1,
            };
            match kirie_render::load_workshop_scene(
                target,
                dir,
                resolve::we_assets_dir_or_warn().as_deref(),
                options,
                audio,
                properties,
            ) {
                Ok(renderer) => {
                    tracing::info!(output = %target.output_name, "scene wallpaper ready");
                    renderer
                }
                Err(err) => {
                    eprintln!("{}: failed to build scene renderer: {err}", target.output_name);
                    black(target)
                }
            }
        }
        #[cfg(any(feature = "web-cef", feature = "web-webview"))]
        RunSpec::Web { .. } => black(target),
        RunSpec::Skip => black(target),
    }
}

pub(crate) struct BuildContext {
    scaling: Mutex<ScalingMode>,
    clamp: Mutex<ClampMode>,
    volume: i64,
    silent: bool,
    registrar: Option<crossbeam_channel::Sender<Register>>,
    sources: Arc<LazySources>,
    automute_controls: Arc<Mutex<Vec<VideoControl>>>,
}

impl BuildContext {
    fn scaling(&self) -> ScalingMode {
        self.scaling.lock().map(|g| *g).unwrap_or_default()
    }

    fn clamp(&self) -> ClampMode {
        self.clamp.lock().map(|g| *g).unwrap_or_default()
    }

    pub(crate) fn set_scaling(&self, mode: ScalingMode) -> bool {
        match self.scaling.lock() {
            Ok(mut g) if *g != mode => {
                *g = mode;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn set_clamp(&self, mode: ClampMode) -> bool {
        match self.clamp.lock() {
            Ok(mut g) if *g != mode => {
                *g = mode;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn notify_background(&self, screen: &str, path: &Path) {
        if let Some(reg) = &self.registrar {
            let _ = reg.send(Register::Background {
                screen: screen.to_owned(),
                bg: path.to_path_buf(),
            });
        }
    }

    pub(crate) fn build_fn(
        self: &Arc<Self>,
        screen: String,
        path: &Path,
        properties: Vec<(String, String)>,
    ) -> Option<kirie_platform::BuildFn> {
        let target = make_target(
            screen.clone(),
            path.to_string_lossy().into_owned(),
            self.scaling(),
            self.clamp(),
        );
        match &target.spec {
            RunSpec::Video { .. } | RunSpec::Image { .. } | RunSpec::Scene { .. } => {}
            _ => return None,
        }
        let ctx = self.clone();
        let spec = target.spec;
        let build: kirie_platform::BuildFn = Box::new(move |device, queue, format, name, size, position| {
            let rt = RenderTarget {
                device,
                queue,
                format,
                output_name: name,
                size,
                position,
            };
            build_for_spec(
                &rt,
                screen,
                &spec,
                ctx.volume,
                ctx.silent,
                ctx.registrar.as_ref(),
                ctx.sources.audio_for(&spec),
                &properties,
                &ctx.automute_controls,
            )
        });
        Some(build)
    }

    #[cfg(any(feature = "web-cef", feature = "web-webview"))]
    pub(crate) fn build_local_fn(
        self: &Arc<Self>,
        screen: String,
        path: &Path,
        properties: Vec<(String, String)>,
    ) -> Option<kirie_platform::BuildLocalFn> {
        let target = make_target(
            screen,
            path.to_string_lossy().into_owned(),
            self.scaling(),
            self.clamp(),
        );
        let RunSpec::Web { url, dir } = target.spec else {
            return None;
        };
        let silent = self.silent;
        let audio = Some(self.sources.audio());
        let media = Some(self.sources.media());
        let build: kirie_platform::BuildLocalFn =
            Box::new(move |device, queue, format, name, size, position| {
                let rt = RenderTarget {
                    device,
                    queue,
                    format,
                    output_name: name,
                    size,
                    position,
                };
                build_web(&rt, &url, &dir, silent, &properties, audio, media)
            });
        Some(build)
    }
}

pub(crate) struct SwapCtx {
    pub cmd_tx: kirie_platform::CommandSender,
    pub build: Arc<BuildContext>,
}

fn setup_socket(
    args: &CompatArgs,
    seed: Vec<(String, Option<PathBuf>)>,
) -> (Option<kirie_ipc::ControlSocket>, Option<IpcApp>) {
    let Some(path) = &args.control_socket else {
        return (None, None);
    };
    let (events_tx, events_rx) = crossbeam_channel::unbounded();
    match kirie_ipc::ControlSocket::bind(path.clone(), events_tx) {
        Ok(socket) => {
            let app = IpcApp::spawn(
                events_rx,
                seed,
                args.playback_speed as f32,
                args.volume as i32,
                false,
            );
            (Some(socket), Some(app))
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "control socket unavailable; running without live control");
            (None, None)
        }
    }
}

struct BlackRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn black(target: &RenderTarget<'_>) -> Box<dyn Renderer + Send> {
    Box::new(BlackRenderer {
        device: target.device.clone(),
        queue: target.queue.clone(),
    })
}

impl Renderer for BlackRenderer {
    fn render(&mut self, view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kirie-black-encoder"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kirie-black-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
    }
}
