use std::process::ExitCode;

use kirie_platform::{Platform, PresentOptions, RenderTarget, Renderer, RendererFactory, SurfaceSize};

use crate::compat::args::CompatArgs;
use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::{Sound, build_presented_renderer};

pub fn present(args: &CompatArgs) -> ExitCode {
    let Some(background) = background_of(args) else {
        eprintln!("At least one background ID must be specified");
        return ExitCode::FAILURE;
    };

    let wallpaper = match resolve::classify(&background) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(note) = resolve::refuse_without_assets(&wallpaper) {
        eprintln!("{note}");
        return ExitCode::FAILURE;
    }
    #[cfg(feature = "web-webview")]
    if let Wallpaper::Web { dir, file } = &wallpaper {
        return present_web(dir, file, args);
    }
    if let Some(reason) = wallpaper.unrunnable_reason() {
        eprintln!("cannot put this on a screen: {reason}");
        return ExitCode::FAILURE;
    }

    let options = PresentOptions {
        screen_roots: args.screens.iter().map(|screen| screen.name.clone()).collect(),
        fps: u32::try_from(args.fps).ok().filter(|rate| *rate > 0),
        playback_speed: args.playback_speed,
        ..PresentOptions::default()
    };

    let mut platform =
        match Platform::connect_with(kirie_platform::Backend::Mac, options, factory(wallpaper, args)) {
            Ok(platform) => platform,
            Err(err) => {
                eprintln!("cannot put a wallpaper on this desktop: {err}");
                return ExitCode::FAILURE;
            }
        };

    let showing = crate::compat::mac_ipc::Showing::new(
        &platform.screen_names(),
        Some(std::path::Path::new(&background)),
        args.playback_speed as f32,
    );
    if let Some(socket) = control_socket(args) {
        let orders = platform.orders();
        let spoken = args.clone();
        let held = std::sync::Arc::clone(&showing);
        let started = std::thread::Builder::new()
            .name("kirie-control".to_owned())
            .spawn(move || crate::compat::mac_ipc::serve(socket, orders, held, spoken));
        if let Err(err) = started {
            tracing::warn!(%err, "no control socket thread");
        }
    }

    tracing::info!(screens = platform.surface_count(), "presenting");
    match platform.run(None) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("the wallpaper stopped: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "web-webview")]
fn present_web(dir: &std::path::Path, file: &str, args: &CompatArgs) -> ExitCode {
    use kirie_web::WebSize;

    let url = resolve::web_entry_url(dir, file);
    let roots: Vec<String> = args.screens.iter().map(|screen| screen.name.clone()).collect();
    let surfaces = match kirie_platform::open_desktop(&roots) {
        Ok(surfaces) => surfaces,
        Err(err) => {
            eprintln!("cannot put a wallpaper on this desktop: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut views = Vec::with_capacity(surfaces.len());
    for surface in &surfaces {
        let size = WebSize {
            width: surface.size().width,
            height: surface.size().height,
        };
        match kirie_web::wk::desktop_view(&url, size) {
            Ok(view) => {
                surface.show(&view);
                views.push(view);
            }
            Err(err) => {
                eprintln!("{}: cannot open the page: {err}", surface.name());
                return ExitCode::FAILURE;
            }
        }
    }

    let level = if args.silent {
        0.0
    } else {
        (args.volume as f32 / 128.0).clamp(0.0, 1.0)
    };
    tracing::info!(screens = views.len(), url, "presenting a web wallpaper");

    let mut hushed = std::time::Instant::now() - std::time::Duration::from_secs(1);
    loop {
        kirie_platform::pump_desktop_events();
        if hushed.elapsed() >= std::time::Duration::from_millis(500) {
            for view in &views {
                kirie_web::wk::hush(view, level);
            }
            hushed = std::time::Instant::now();
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn control_socket(args: &CompatArgs) -> Option<std::path::PathBuf> {
    args.control_socket.clone().or_else(|| {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| Some(std::env::temp_dir()))
            .map(|dir| dir.join("lwe.sock"))
    })
}

fn background_of(args: &CompatArgs) -> Option<String> {
    args.default_background
        .clone()
        .or_else(|| args.screens.iter().find_map(|screen| screen.background.clone()))
}

fn factory(wallpaper: Wallpaper, args: &CompatArgs) -> RendererFactory {
    let scaling = args.window_scaling;
    let clamp = args.window_clamp;
    let properties = args.set_properties.clone();
    let sound = Sound {
        volume: args.volume,
        silent: args.silent,
    };

    Box::new(move |target: &RenderTarget<'_>| {
        let size = SurfaceSize {
            width: target.size.0,
            height: target.size.1,
        };
        match build_presented_renderer(target, &wallpaper, scaling, clamp, size, &properties, sound) {
            Ok(renderer) => renderer,
            Err(err) => {
                tracing::error!(output = target.output_name, "cannot build the wallpaper: {err:#}");
                Box::new(Blank)
            }
        }
    })
}

struct Blank;

impl Renderer for Blank {
    fn render(&mut self, _view: &wgpu::TextureView, _size: SurfaceSize, _dt: f32) {}

    fn is_passive(&self) -> bool {
        true
    }
}
