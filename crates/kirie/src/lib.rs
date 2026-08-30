#![forbid(unsafe_code)]

pub mod ask;
pub mod assets;
pub mod check;
pub mod compat;
pub mod detect;
pub mod extract;
pub mod gpus;
pub mod info;
pub mod list;
pub mod preview;
mod preview_render;
pub mod soak;
pub mod update;
pub mod workshop;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "kirie",
    version,
    about = "kirie (切り絵) — Wallpaper Engine compatible wallpaper renderer for Linux"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send one command to a running kirie and print what it says.
    Ask {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(required = true, trailing_var_arg = true)]
        words: Vec<String>,
    },
    Info {
        path: PathBuf,
    },
    Check {
        path: Option<PathBuf>,
    },
    Gpus {
        #[arg(long)]
        json: bool,
    },
    Assets {
        #[arg(long)]
        json: bool,
    },
    Update {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        force: bool,
    },
    Workshop {
        #[command(subcommand)]
        command: WorkshopCommand,
    },
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    Preview {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long = "bg")]
        background: String,
        #[arg(long)]
        fps: Option<u32>,
        #[arg(long)]
        size: Option<u32>,
    },
    Extract {
        path: PathBuf,
        #[arg(short = 'o', long = "output", default_value = ".")]
        output: PathBuf,
        #[arg(long)]
        tex_to_png: bool,
    },
}

#[derive(Subcommand, Debug)]
enum WorkshopCommand {
    Search {
        text: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "exclude-tag")]
        exclude_tags: Vec<String>,
        #[arg(long)]
        any_tag: bool,
        #[arg(long, default_value = "popular")]
        sort: String,
        #[arg(long)]
        days: Option<u32>,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    Subscribe {
        id: String,
        #[arg(long)]
        wait: bool,
        #[arg(long, value_name = "SCREEN")]
        apply: Option<String>,
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    #[cfg(feature = "tui")]
    Browse,
    Unsubscribe {
        id: String,
        #[arg(long)]
        json: bool,
    },
    State {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[must_use]
pub fn run(args: Vec<OsString>) -> ExitCode {
    if std::env::var_os("KIRIE_SOAK").is_some() {
        return soak::run_from_env();
    }
    if std::env::var_os("KIRIE_BENCH").is_some() {
        return soak::bench_from_env();
    }
    #[cfg(feature = "web-webview-inproc")]
    if args.get(1).is_some_and(|a| a == kirie_web::viewhost::HOST_ARG) {
        kirie_web::webview::host::run();
        return ExitCode::SUCCESS;
    }

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
                || sub == "update"
                || sub == "preview"
                || sub == "ask"
                || sub == "assets" =>
        {
            run_subcommand(args)
        }
        _ => compat::run(&args),
    }
}

fn default_control_socket() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    runtime.join("lwe.sock")
}

fn run_subcommand(args: Vec<OsString>) -> ExitCode {
    kirie_bake::limit_malloc_arenas(2);
    let cli = Cli::parse_from(args);
    if let Command::Ask { socket, words } = &cli.command {
        let path = socket.clone().unwrap_or_else(default_control_socket);
        return match ask::run(&path, &words.join(" ")) {
            Ok(said) => {
                print!("{said}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("{err}");
                ExitCode::FAILURE
            }
        };
    }

    if let Command::Assets { json } = &cli.command {
        return match assets::run(*json) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(err) => {
                eprintln!("error: {}", render_chain(&err));
                ExitCode::FAILURE
            }
        };
    }

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
        Command::Ask { .. } => unreachable!("answered above"),
        Command::Info { path } => info::run(&path),
        Command::Preview {
            socket,
            background,
            fps,
            size,
        } => preview::check_socket(&socket)
            .and_then(|()| preview::run(&socket, std::path::Path::new(&background), fps, size)),
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
        Command::Check { .. } | Command::Assets { .. } => unreachable!("handled above"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", render_chain(&err));
            ExitCode::FAILURE
        }
    }
}

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
