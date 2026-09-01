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
    if let Some(reason) = wallpaper.unrunnable_reason() {
        eprintln!("cannot put this on a screen: {reason}");
        return ExitCode::FAILURE;
    }

    if let Some(socket) = control_socket(args)
        && crate::compat::mac_ipc::already_running(&socket)
    {
        eprintln!(
            "another kirie already owns {} — stop it first (pkill -x kirie), or pass a \
             different --control-socket",
            socket.display()
        );
        return ExitCode::FAILURE;
    }

    kirie_platform::set_battery_fps(args.battery_fps);

    let options = PresentOptions {
        screen_roots: args.screens.iter().map(|screen| screen.name.clone()).collect(),
        fps: u32::try_from(args.fps).ok().filter(|rate| *rate > 0),
        playback_speed: args.playback_speed,
        pointer: !args.disable_mouse,
        take_clicks: args.interactive,
        ..PresentOptions::default()
    };

    let mut platform = match Platform::connect_with(
        kirie_platform::Backend::Mac,
        options,
        factory(wallpaper.clone(), args),
    ) {
        Ok(platform) => platform,
        Err(err) => {
            eprintln!("cannot put a wallpaper on this desktop: {err}");
            return ExitCode::FAILURE;
        }
    };

    #[cfg(feature = "web-webview")]
    if let Wallpaper::Web { dir, file } = &wallpaper {
        let url = resolve::web_entry_url(dir, file);
        let level = crate::compat::mac_ipc::level_of(Sound {
            volume: args.volume,
            silent: args.silent,
        });
        for screen in platform.screen_names() {
            let wanted = url.clone();
            let make: kirie_platform::MakeViewFn = Box::new(move |size: SurfaceSize| {
                match kirie_web::wk::desktop_view(
                    &wanted,
                    kirie_web::WebSize {
                        width: size.width,
                        height: size.height,
                    },
                    level,
                ) {
                    Ok(view) => objc2::rc::Retained::into_super(view),
                    Err(err) => {
                        tracing::error!(%err, "cannot open the page");
                        let mtm = objc2::MainThreadMarker::new().expect("main thread");
                        objc2_app_kit::NSView::new(mtm)
                    }
                }
            });
            let _ = platform
                .orders()
                .send(kirie_platform::RenderCommand::SetView { screen, make });
        }
    }

    let showing = crate::compat::mac_ipc::Showing::new(
        &platform.screen_names(),
        Some(std::path::Path::new(&background)),
        args.playback_speed as f32,
        Sound {
            volume: args.volume,
            silent: args.silent,
        },
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
        match build_presented_renderer(target, &wallpaper, scaling, clamp, &properties, sound) {
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
