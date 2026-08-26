//! kirie (切り絵) — a Wallpaper Engine compatible wallpaper renderer.
//!
//! This crate is both a library and the `kirie` binary. The library exposes:
//!
//! * the existing `info` / `extract` subcommands (SPEC.md §I) via
//!   [`info`] / [`extract`], and
//! * the drop-in `linux-wallpaperengine` compatibility surface via
//!   [`compat`] — the full C++ flag parser (docs/compat-cli.md), per-screen
//!   wallpaper dispatch, the control socket (docs/compat-socket.md, via
//!   `kirie-ipc`), and offscreen `--screenshot` capture.
//!
//! [`run`] is the single entry point the binary calls: it picks the
//! `info`/`extract` subcommands when `argv[1]` names one, and otherwise
//! parses the full compat flag surface exactly like the C++ engine
//! (docs/compat-cli.md §1: `linux-wallpaperengine [options...] [background]`).

#![forbid(unsafe_code)]

pub mod check;
pub mod compat;
pub mod detect;
pub mod extract;
pub mod gpus;
pub mod info;
pub mod list;
pub mod soak;
pub mod update;
pub mod workshop;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The `info` / `extract` subcommand surface (SPEC.md §I). Kept separate from
/// the compat flag surface: only `argv[1] ∈ {info, extract}` reaches clap.
#[derive(Parser)]
#[command(
    name = "kirie",
    version,
    about = "kirie (切り絵) — Wallpaper Engine compatible wallpaper renderer for Linux"
)]
struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// The kirie-native subcommands (SPEC.md §I).
#[derive(Subcommand)]
enum Command {
    /// Summarize a workshop item directory, project.json, scene.pkg, or .tex
    Info {
        /// Workshop item directory, project.json, scene.pkg, or .tex file
        path: PathBuf,
    },
    /// Check that everything needed to build and run a wallpaper is present
    Check {
        /// Optional wallpaper to validate (workshop item dir, or a media file).
        /// Omit to check only the environment (GPU, WE base assets, web backend).
        path: Option<PathBuf>,
    },
    /// List the GPUs kirie can render on (values accepted by `--gpu`)
    Gpus {
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
    /// Update this binary to the newest release
    Update {
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
        /// Replace a locally built binary with the release anyway
        #[arg(long)]
        force: bool,
    },
    /// Browse the Steam Workshop, subscribe to items, and check their state
    Workshop {
        #[command(subcommand)]
        command: WorkshopCommand,
    },
    /// List the Wallpaper Engine items installed on this machine
    List {
        /// Workshop content directory to read instead of probing Steam's
        /// standard locations
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
    /// Extract a scene.pkg's entries, or decode a .tex to PNG(s)
    Extract {
        /// scene.pkg or .tex file to extract
        path: PathBuf,
        /// Output directory (created if missing)
        #[arg(short = 'o', long = "output", default_value = ".")]
        output: PathBuf,
        /// For a pkg input: also decode every contained .tex entry to
        /// PNG(s) next to the extracted file (video textures are skipped
        /// with a warning)
        #[arg(long)]
        tex_to_png: bool,
    },
}

/// `kirie workshop <…>` — the Workshop surface, which needs a running Steam
/// client and an account that owns Wallpaper Engine (Steam enforces both).
#[derive(Subcommand, Debug)]
enum WorkshopCommand {
    /// Search the Workshop for wallpapers, installed or not
    Search {
        /// Free-text search
        text: Option<String>,
        /// Only items carrying this tag (repeatable, e.g. Scene, Video, Web)
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Skip items carrying this tag (repeatable, e.g. Mature, Questionable)
        #[arg(long = "exclude-tag")]
        exclude_tags: Vec<String>,
        /// Match any --tag rather than all of them
        #[arg(long)]
        any_tag: bool,
        /// popular | trend | recent | rated
        #[arg(long, default_value = "popular")]
        sort: String,
        /// Days --sort trend ranks over
        #[arg(long)]
        days: Option<u32>,
        /// 1-based page; Steam answers 50 items per page
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Keep at most this many results
        #[arg(long)]
        limit: Option<usize>,
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
    /// Subscribe to an item, so Steam downloads it where kirie will find it
    Subscribe {
        /// Workshop id
        id: String,
        /// Wait for Steam to finish downloading it
        #[arg(long)]
        wait: bool,
        /// Wait, then show it on this screen through a running engine
        #[arg(long, value_name = "SCREEN")]
        apply: Option<String>,
        /// Control socket of the running engine (--apply)
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
    /// Browse the Workshop interactively
    #[cfg(feature = "tui")]
    Browse,
    /// Drop a subscription; Steam removes the files on its own schedule
    Unsubscribe {
        /// Workshop id
        id: String,
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
    /// Report whether an item is subscribed, installed or downloading
    State {
        /// Workshop id
        id: String,
        /// Emit JSON (for shells, panels and other tooling)
        #[arg(long)]
        json: bool,
    },
}

/// Run the kirie CLI from a full argv (`args[0]` is the program name).
///
/// Dispatch (docs/compat-cli.md §1):
///
/// * no arguments → print the version line (pre-subcommand behavior kept so
///   `kirie` alone stays a harmless probe);
/// * `argv[1] ∈ {info, extract}` → the kirie-native subcommands;
/// * anything else → the `linux-wallpaperengine` compat surface
///   ([`compat::run`]), which owns its own exit code.
#[must_use]
pub fn run(args: Vec<OsString>) -> ExitCode {
    // Out-of-band leak/stability soak (release hardening) — never part of the
    // compat CLI surface, so it can only be reached deliberately via the env.
    if std::env::var_os("KIRIE_SOAK").is_some() {
        return soak::run_from_env();
    }
    // Frame-time benchmark for one wallpaper (see `soak::bench_from_env`).
    if std::env::var_os("KIRIE_BENCH").is_some() {
        return soak::bench_from_env();
    }
    // Only the in-process build answers to the host argument; the shipped one
    // carries the host as an embedded binary instead, so that it does not have
    // to link gtk (see crates/kirie/build.rs). Checked before anything else:
    // the host must not touch engine setup.
    #[cfg(feature = "web-webview-inproc")]
    if args.get(1).is_some_and(|a| a == kirie_web::viewhost::HOST_ARG) {
        kirie_web::webview::host::run();
        return ExitCode::SUCCESS;
    }

    // The Workshop bridge, run as this same binary under a hidden verb. Checked
    // before anything else, exactly like the webview host: the helper must
    // touch no engine setup, and its process has to stay short-lived — a held
    // Steamworks session makes Steam count it as playing Wallpaper Engine.
    if args.get(1).is_some_and(|a| a == kirie_steam_helper::HELPER_ARG) {
        let rest: Vec<String> = args
            .iter()
            .skip(2)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        return kirie_steam_helper::run(&rest);
    }

    match args.get(1).map(|s| s.to_string_lossy()) {
        None => {
            // Bare `kirie`: keep the version probe (a real engine would error
            // with "At least one background ID must be specified", but the
            // daemon never invokes kirie without arguments).
            println!(concat!("kirie ", env!("CARGO_PKG_VERSION")));
            ExitCode::SUCCESS
        }
        Some(sub)
            if sub == "info"
                || sub == "extract"
                || sub == "check"
                || sub == "list"
                || sub == "gpus"
                || sub == "workshop"
                || sub == "update" =>
        {
            run_subcommand(args)
        }
        _ => compat::run(&args),
    }
}

/// Where a running engine is expected to be listening.
///
/// kirie's `--control-socket` has no default — the daemon is always told where
/// to listen — so this is the path the shells that drive it use, and the one a
/// user typing `--apply` by hand almost certainly means.
fn default_control_socket() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    runtime.join("lwe.sock")
}

/// Run the clap-driven `info` / `extract` subcommands.
fn run_subcommand(args: Vec<OsString>) -> ExitCode {
    // The engine path caps arenas as its first act (`compat::run`); the
    // subcommands never did, so they ran with glibc's default of 8x cores —
    // 256 on a 32-thread box — and paid the fragmentation for it.
    kirie_bake::limit_malloc_arenas(2);
    let cli = Cli::parse_from(args);
    // `check` owns its exit code: it reports prerequisites as a checklist and
    // exits nonzero when a required check failed (not via an error).
    if let Command::Check { path } = &cli.command {
        return match check::run(path.as_deref()) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(err) => {
                eprintln!("error: {}", render_chain(&err));
                ExitCode::FAILURE
            }
        };
    }
    let result = match cli.command {
        Command::Info { path } => info::run(&path),
        Command::Extract {
            path,
            output,
            tex_to_png,
        } => extract::run(&path, &output, tex_to_png),
        Command::List { dir, json } => list::run(dir.as_deref(), json),
        Command::Gpus { json } => gpus::run(json),
        Command::Update { check, force } => update::run(check, force),
        Command::Workshop { command } => match command {
            WorkshopCommand::Search {
                text,
                tags,
                exclude_tags,
                any_tag,
                sort,
                days,
                page,
                limit,
                json,
            } => workshop::run_search(
                &workshop::Query {
                    text,
                    tags,
                    excluded_tags: exclude_tags,
                    match_any_tag: any_tag,
                    sort,
                    trend_days: days,
                    page,
                    limit,
                },
                json,
            ),
            WorkshopCommand::Subscribe {
                id,
                wait,
                apply,
                socket,
                json,
            } => workshop::run_subscribe(
                &id,
                wait,
                apply.as_deref(),
                &socket.unwrap_or_else(default_control_socket),
                json,
            ),
            WorkshopCommand::Unsubscribe { id, json } => workshop::run_unsubscribe(&id, json),
            WorkshopCommand::State { id, json } => workshop::run_state(&id, json),
            #[cfg(feature = "tui")]
            WorkshopCommand::Browse => workshop::tui::run(),
        },
        Command::Check { .. } => unreachable!("handled above"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", render_chain(&err));
            ExitCode::FAILURE
        }
    }
}

/// Render an error chain like `anyhow`'s `{:#}` but without repeating causes:
/// the kirie-formats errors already include their source in their `Display`
/// (self-contained messages), so blindly appending every `source()` prints
/// e.g. `invalid JSON: expected value…: expected value…`.
pub(crate) fn render_chain(err: &anyhow::Error) -> String {
    let mut message = String::new();
    for cause in err.chain() {
        let text = cause.to_string();
        if message.is_empty() {
            message = text;
        } else if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
    }
    message
}
