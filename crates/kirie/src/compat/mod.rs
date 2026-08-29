pub mod args;
pub mod autopin;
pub mod common;
#[cfg(target_os = "linux")]
pub mod ipc_app;
pub mod list_props;
pub mod playlist;
pub mod power;
pub mod resolve;
#[cfg(target_os = "linux")]
pub mod run;
pub mod screenshot;
pub mod signals;
pub mod steam;
#[cfg(any(feature = "web-cef", feature = "web-webview"))]
pub mod webfeed;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::ExitCode;
use std::sync::Once;

use args::ParseError;

pub fn run(argv: &[OsString]) -> ExitCode {
    kirie_bake::limit_malloc_arenas(2);
    #[cfg(feature = "web-webview")]
    kirie_web::viewhost::set_embedded_host(include_bytes!(env!("KIRIE_WEBVIEWHOST_BLOB")));

    autopin::auto_pin(argv);
    pin_gpu(argv);
    init_tracing();
    let argv0 = argv
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "linux-wallpaperengine".to_owned());

    let parsed = match args::parse(argv) {
        Ok(a) => a,
        Err(e) => return fail(&argv0, &e),
    };

    if parsed.help {
        print!("{}", args::HELP_TEXT);
        return ExitCode::SUCCESS;
    }

    let validated = match args::validate(parsed) {
        Ok(a) => a,
        Err(e) => return fail(&argv0, &e),
    };

    print_banner(&validated);

    if let Some(path) = &validated.screenshot
        && let Err(e) = args::validate_screenshot_ext(path.as_os_str())
    {
        return fail(&argv0, &e);
    }

    #[cfg(target_os = "linux")]
    {
        run::dispatch(validated)
    }
    #[cfg(not(target_os = "linux"))]
    {
        offscreen_only(validated)
    }
}

fn pin_gpu(argv: &[OsString]) {
    const SENTINEL: &str = "KIRIE_GPU_PINNED";
    if std::env::var_os(SENTINEL).is_some() {
        return;
    }
    let Some(sel) = gpu_selector(argv).or_else(|| std::env::var("KIRIE_GPU").ok()) else {
        return;
    };
    if sel.trim().is_empty() || sel.eq_ignore_ascii_case("auto") {
        return;
    }
    let Some(manifest) = kirie_bake::resolve_vulkan_icd(&sel) else {
        eprintln!("kirie: no Vulkan ICD matched --gpu {sel}; using the loader default");
        return;
    };
    if std::env::var_os("VK_DRIVER_FILES").is_some_and(|v| v == manifest)
        && std::env::var_os("KIRIE_GPU").is_some()
    {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let err = std::process::Command::new(exe)
        .args(argv.iter().skip(1))
        .env("VK_DRIVER_FILES", &manifest)
        .env("VK_ICD_FILENAMES", &manifest)
        .env("KIRIE_GPU", &sel)
        .env(SENTINEL, "1")
        .exec();
    eprintln!("kirie: could not re-exec to pin {}: {err}", manifest.display());
}

fn gpu_selector(argv: &[OsString]) -> Option<String> {
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if let Some(v) = s.strip_prefix("--gpu=") {
            return Some(v.to_owned());
        }
        if s == "--gpu" {
            return it.next().map(|v| v.to_string_lossy().into_owned());
        }
    }
    None
}

fn fail(argv0: &str, err: &ParseError) -> ExitCode {
    eprintln!("{}", err.message);
    if err.doubled {
        eprintln!("{}. Use {argv0} --help for more information", err.message);
    }
    ExitCode::FAILURE
}

fn print_banner(args: &args::CompatArgs) {
    let mut line = String::from("Running with: ");
    for a in &args.argv {
        line.push_str(a);
        line.push(' ');
    }
    if args.list_properties_json {
        eprintln!("{line}");
        return;
    }
    println!("{line}");
}

fn init_tracing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_writer(std::io::stderr)
            .try_init();
    });
}

#[cfg(not(target_os = "linux"))]
fn offscreen_only(args: args::CompatArgs) -> ExitCode {
    common::set_render_scale(args.render_scale as f32);
    common::set_fit_render_to_output(args.fit_render_to_output);
    common::set_object_filter(&args.render_debug);

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
        let Some(bg) = args.default_background.clone() else {
            eprintln!("At least one background ID must be specified");
            return ExitCode::FAILURE;
        };
        let wallpaper = match resolve::classify(&bg) {
            Ok(found) => found,
            Err(err) => {
                eprintln!("{err}");
                return ExitCode::FAILURE;
            }
        };
        return match screenshot::capture(
            &wallpaper,
            args.window_scaling,
            args.window_clamp,
            args.screenshot_delay,
            &path,
            None,
            &args.set_properties,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("screenshot failed: {err:#}");
                ExitCode::FAILURE
            }
        };
    }

    eprintln!(
        "this build renders off-screen only: --screenshot, `preview` and `list` work, putting a wallpaper on a screen does not"
    );
    ExitCode::FAILURE
}
