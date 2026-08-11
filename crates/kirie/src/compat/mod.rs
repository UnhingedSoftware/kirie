//! The `linux-wallpaperengine` compatibility surface (docs/compat-cli.md).
//!
//! [`run`] is the compat entry point: it parses the full C++ flag surface
//! ([`args`]), handles `--help`/`--list-properties`/`--screenshot` exit modes,
//! prints the `Running with:` banner (doc §1.2), and otherwise dispatches to
//! per-screen wallpaper rendering with the control socket wired in ([`run`]
//! module, [`ipc_app`], [`screenshot`]).

pub mod args;
pub mod ipc_app;
pub mod list_props;
pub mod playlist;
pub mod power;
pub mod resolve;
pub mod run;
pub mod screenshot;
pub mod signals;
/// The engine's [`kirie_web::WebFeed`]: system audio + MPRIS now-playing,
/// adapted to the shapes a web page's `wallpaperRegister*Listener` callbacks
/// expect. Only exists in a build with a web backend — nothing else consumes
/// it, and it is the only place the browser layer and the media/audio
/// pipelines meet.
#[cfg(any(feature = "web-cef", feature = "web-webview"))]
pub mod webfeed;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::ExitCode;
use std::sync::Once;

use args::ParseError;

/// Run the compat surface for a full argv (`argv[0]` is the program name).
///
/// Exit codes follow doc §5: `0` for `--help` and successful runs/clean stops,
/// `1` for any parse/startup fatal or abnormal termination.
pub fn run(argv: &[OsString]) -> ExitCode {
    // Keep glibc to two malloc arenas so per-swap build threads reuse them and
    // `trim_heap` after each build/drop actually returns the pages — without
    // this, every fresh worker thread can land in a new arena the trims never
    // reach and RSS ratchets across wallpaper switches.
    kirie_bake::limit_malloc_arenas(2);
    // Pin the Vulkan loader to one vendor's driver (re-execs; see `pin_gpu`).
    // On a multi-GPU box this is the single biggest memory saving available —
    // the loader otherwise keeps BOTH vendors' userspace stacks resident.
    // `--gpu` wins over `KIRIE_GPU`.
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

    // `--help`: print the synopsis and exit 0 before validation/banner
    // (doc §5, main.cpp:68-71).
    if parsed.help {
        print!("{}", args::HELP_TEXT);
        return ExitCode::SUCCESS;
    }

    let validated = match args::validate(parsed) {
        Ok(a) => a,
        Err(e) => return fail(&argv0, &e),
    };

    // The `Running with:` banner always prints on a successful parse
    // (doc §1.2, §4.8 step 4) — before running, and even for the list modes.
    // It must precede the screenshot-extension check (§4.8 step 6) so that a
    // bad `--screenshot` extension still emits the banner on stdout first,
    // reproducing the C++ post-parse validation order exactly.
    print_banner(&validated);

    // Screenshot extension validation (doc §3.6, §4.8 step 6 — after the
    // banner).
    if let Some(path) = &validated.screenshot
        && let Err(e) = args::validate_screenshot_ext(path.as_os_str())
    {
        return fail(&argv0, &e);
    }

    run::dispatch(validated)
}

/// Honor `--gpu`/`KIRIE_GPU` by **re-executing** with the chosen Vulkan ICD in
/// the environment, then never returning (the new image takes over).
///
/// The Vulkan loader opens every installed ICD, so a two-GPU machine keeps both
/// vendors' userspace resident: measured, one scene wallpaper is ~233MB RSS
/// (~142MB of driver pages) unpinned versus ~93MB (~29MB driver pages) with
/// `VK_DRIVER_FILES` pinned — the largest single memory item there is, and it
/// also makes adapter choice deterministic.
///
/// Setting the variables in-process does not work (measured: no change at all);
/// the loader only honors what it inherited. Re-exec is therefore the
/// mechanism, guarded by a sentinel so it happens at most once, and it uses
/// `Command::exec` so this crate keeps `forbid(unsafe_code)`.
///
/// Every failure path is non-fatal: kirie continues on the loader default.
fn pin_gpu(argv: &[OsString]) {
    const SENTINEL: &str = "KIRIE_GPU_PINNED";
    if std::env::var_os(SENTINEL).is_some() {
        return; // already re-executed
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
    // Already pointed at exactly this driver (e.g. the daemon set it) — the
    // loader has what it needs. Still re-exec once if `KIRIE_GPU` is missing:
    // the web hosts derive their GL offload environment from it, and skipping
    // here would leave a daemon-launched engine rendering web wallpapers on
    // the default GPU while scenes obey the selection.
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
        .env("VK_ICD_FILENAMES", &manifest) // older loaders read this spelling
        // The token itself, for the web host spawns: a browser renders through
        // GL, not through kirie's Vulkan device, so kirie-web translates this
        // into the matching GL offload environment for its child process —
        // without it a web wallpaper silently renders on the default GPU no
        // matter what --gpu said.
        .env("KIRIE_GPU", &sel)
        .env(SENTINEL, "1")
        .exec();
    // `exec` only returns on failure; carry on with the loader default.
    eprintln!("kirie: could not re-exec to pin {}: {err}", manifest.display());
}

/// The `--gpu <vendor>` / `--gpu=<vendor>` value, scanned straight off argv.
///
/// Read before the real parser runs because the driver pin has to be in place
/// before any Vulkan instance exists (see [`pin_gpu`]).
/// The parser knows the flag too, so its value is never mistaken for a
/// background path.
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

/// Print a fatal parse error with the doc §4.7 doubling: the bare message,
/// then (for `sLog.exception` fatals) the message again with the
/// `. Use <argv0> --help for more information` suffix. Returns exit 1 (doc §5).
fn fail(argv0: &str, err: &ParseError) -> ExitCode {
    eprintln!("{}", err.message);
    if err.doubled {
        eprintln!("{}. Use {argv0} --help for more information", err.message);
    }
    ExitCode::FAILURE
}

/// Print the `Running with: <argv...> ` banner (doc §1.2): every argv element
/// space-separated with a trailing space, then a newline.
///
/// Goes to **stdout** normally (reference parity), but to **stderr** for
/// `--list-properties-json`: that mode's stdout is machine-readable JSON a tool
/// parses (e.g. the ArchEclipse properties UI), and a banner line prefixed to
/// it breaks the parse.
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

/// Initialize a stderr tracing subscriber once (best-effort). The C++ engine
/// logs to stderr; kirie routes kirie-video/platform diagnostics the same way.
fn init_tracing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Respect `RUST_LOG` (the hardcoded INFO cap silently swallowed every
        // debug/trace diagnostic); default stays INFO when unset.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_writer(std::io::stderr)
            .try_init();
    });
}
