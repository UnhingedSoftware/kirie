// Layer-shell and X11 are Linux-only; elsewhere this example only says so.
#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use kirie_platform::{
        Backend, Platform, PresentOptions, RenderTarget, Renderer, RendererFactory, TestPattern, X11Mode,
    };

    struct Args {
        seconds: u64,
        backend: Option<Backend>,
        window: Option<(u32, u32)>,
        namespace: Option<String>,
        screen_roots: Vec<String>,
    }

    fn parse_args() -> Result<Args, String> {
        let mut args = std::env::args().skip(1);
        let mut out = Args {
            seconds: 3,
            backend: None,
            window: None,
            namespace: None,
            screen_roots: Vec::new(),
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--seconds" => {
                    let value = args.next().ok_or("--seconds requires a value")?;
                    out.seconds = value
                        .parse()
                        .map_err(|err| format!("invalid --seconds value {value:?}: {err}"))?;
                }
                "--namespace" => {
                    out.namespace = Some(args.next().ok_or("--namespace requires a value")?);
                }
                "--screen-root" => {
                    out.screen_roots
                        .push(args.next().ok_or("--screen-root requires an output name")?);
                }
                "--backend" => {
                    let value = args.next().ok_or("--backend requires wayland|x11")?;
                    out.backend = Some(match value.as_str() {
                        "wayland" => Backend::Wayland,
                        "x11" => Backend::X11,
                        other => return Err(format!("unknown backend {other:?} (wayland|x11)")),
                    });
                }
                "--window" => {
                    let value = args.next().ok_or("--window requires WxH")?;
                    let (w, h) = value
                        .split_once('x')
                        .ok_or_else(|| format!("invalid --window value {value:?} (expected WxH)"))?;
                    let w = w.parse().map_err(|e| format!("bad width in {value:?}: {e}"))?;
                    let h = h.parse().map_err(|e| format!("bad height in {value:?}: {e}"))?;
                    out.window = Some((w, h));
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(out)
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .init();

        let args = parse_args()?;

        fn make_factory() -> RendererFactory {
            Box::new(|target: &RenderTarget<'_>| {
                tracing::info!(output = %target.output_name, "creating test-pattern renderer");
                Box::new(TestPattern::new(target)) as Box<dyn Renderer>
            })
        }

        let options = PresentOptions {
            layer_namespace: args
                .namespace
                .unwrap_or_else(|| PresentOptions::default().layer_namespace),
            screen_roots: args.screen_roots,
            ..PresentOptions::default()
        };

        let mut platform = match (args.backend, args.window) {
            (Some(Backend::Wayland), Some(_)) => {
                return Err("--window is only supported on the X11 backend".into());
            }
            (_, Some((w, h))) => {
                Platform::connect_x11(X11Mode::Window { width: w, height: h }, make_factory())?
            }
            (Some(Backend::Wayland), None) => {
                Platform::connect_with(Backend::Wayland, options, make_factory())?
            }
            (None, None) if Backend::from_env() == Backend::Wayland => {
                Platform::connect_with(Backend::Wayland, options, make_factory())?
            }
            (Some(backend), None) => Platform::connect_backend(backend, make_factory())?,
            (None, None) => Platform::connect(make_factory())?,
        };

        platform.run(Some(Duration::from_secs(args.seconds)))?;

        tracing::info!(outputs = platform.output_count(), "clean exit; dropping surfaces");
        Ok(())
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    if let Err(err) = linux::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("this example needs a Linux compositor");
}
