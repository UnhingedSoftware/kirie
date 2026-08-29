use std::process::ExitCode;

use kirie_platform::{Platform, PresentOptions, RenderTarget, Renderer, RendererFactory, SurfaceSize};

use crate::compat::args::CompatArgs;
use crate::compat::resolve::{self, Wallpaper};
use crate::compat::screenshot::build_offscreen_renderer;

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

    tracing::info!(screens = platform.surface_count(), "presenting");
    match platform.run(None) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("the wallpaper stopped: {err}");
            ExitCode::FAILURE
        }
    }
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

    Box::new(move |target: &RenderTarget<'_>| {
        let size = SurfaceSize {
            width: target.size.0,
            height: target.size.1,
        };
        match build_offscreen_renderer(target, &wallpaper, scaling, clamp, size, None, &properties) {
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
