//! `kirie-steam-helper` — a short-lived bridge to the running Steam client.
//!
//! kirie itself never links or loads Steam. This helper does, for one request,
//! and exits. Two reasons, and they happen to demand the same shape:
//!
//! * **Presence.** Initialising Steamworks as an app tells the Steam client
//!   that app is running. The engine is a daemon that starts at login and never
//!   exits; if it held that connection the user would sit in "Playing Wallpaper
//!   Engine" permanently and accrue playtime for a wallpaper.
//! * **Licence.** This project is AGPL and the Steamworks SDK is proprietary.
//!   Keeping the seam in its own process — talking over stdout, redistributing
//!   nothing of Valve's — is the shape that keeps those apart.
//!
//! Every verb answers a single line of JSON on stdout and exits 0; a failure is
//! `{"error":"…"}` and a non-zero exit. Nothing is printed to stdout but that
//! one line, so a caller can parse it without framing.
//!
//! Verbs:
//!
//! ```text
//! kirie-steam-helper probe [steam-root]...
//! kirie-steam-helper search [key=value]... [steam-root]...
//! kirie-steam-helper state <id> [steam-root]...
//! kirie-steam-helper subscribe <id> [steam-root]...
//! kirie-steam-helper unsubscribe <id> [steam-root]...
//! ```

pub mod steam;

use std::path::PathBuf;

use steam::{APP_ID, Query, Session, Sort, SteamError};

/// Every verb but `probe` fails outright without Steam, so they share one wait.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The hidden argv kirie re-execs itself with to become this helper.
///
/// kirie ships as one file, so the helper is not a second binary beside it —
/// it is the same binary asked to be something else. The *process* is still
/// separate and still short-lived, which is the part that matters: holding a
/// Steamworks session open makes Steam count the process as playing Wallpaper
/// Engine and accrue playtime for it.
pub const HELPER_ARG: &str = "__steamhelper";

/// Run one helper verb. `args` is everything after the verb selector.
#[must_use]
pub fn run(args: &[String]) -> std::process::ExitCode {
    let (verb, rest) = match args.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => {
            emit_error("usage: kirie-steam-helper <verb> [args…]");
            return std::process::ExitCode::FAILURE;
        }
    };

    match verb {
        "probe" => probe(rest),
        "search" => search(rest),
        "state" => state(rest),
        "subscribe" => subscribe(rest),
        "unsubscribe" => unsubscribe(rest),
        other => {
            emit_error(&format!("unknown verb {other:?}"));
            std::process::ExitCode::FAILURE
        }
    }
}

/// Report what Steam can tell us: is it running, does this account own the app,
/// is the app installed, and where.
///
/// This is also the answer to "may this user use the Workshop features" — the
/// ownership question is Steam's to answer, and `Session::open` fails closed
/// when the answer is no.
fn probe(args: &[String]) -> std::process::ExitCode {
    // `--hold <seconds>` keeps the session open, which exists only to measure
    // what Steam does while an app is "running": presence in the friends list
    // and playtime accrual. Nothing in normal operation should use it — the
    // whole design rests on the session being momentary.
    let mut hold_secs = 0u64;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--hold" {
            hold_secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else {
            roots.push(PathBuf::from(arg));
        }
    }

    let library = steam::find_library(&roots);
    let session = Session::open(&roots);

    let value = match session {
        Ok(session) => {
            if hold_secs > 0 {
                eprintln!("holding the Steam session open for {hold_secs}s (measurement only)");
                std::thread::sleep(std::time::Duration::from_secs(hold_secs));
            }
            serde_json::json!({
            "appid": APP_ID,
            "library": library.as_ref().map(|p| p.display().to_string()),
            "running": true,
            "owned": session.owns_app(),
            "installed": session.app_installed(),
            "install_dir": session.app_install_dir().map(|p| p.display().to_string()),
            })
        }
        Err(err) => {
            // Not running / not owned / too-old client are ordinary answers
            // here, not crashes: the caller falls back to reading Steam's files
            // off disk. Only the shape of `reason` distinguishes them.
            let reason = match &err {
                SteamError::LibraryMissing => "library-missing",
                SteamError::LibraryUnusable(_) => "library-unusable",
                SteamError::NotRunning => "not-running",
                SteamError::InitFailed(_) => "init-failed",
            };
            serde_json::json!({
                "appid": APP_ID,
                "library": library.as_ref().map(|p| p.display().to_string()),
                "running": !matches!(err, SteamError::NotRunning),
                "owned": serde_json::Value::Null,
                "installed": serde_json::Value::Null,
                "install_dir": serde_json::Value::Null,
                "reason": reason,
                "detail": err.to_string(),
            })
        }
    };

    println!("{value}");
    std::process::ExitCode::SUCCESS
}

/// Run one Workshop query.
///
/// Arguments are `key=value` so the caller does not have to quote a JSON blob
/// through two layers of shell: `text=`, `tag=` (repeatable), `nottag=`
/// (repeatable), `anytag=1`, `sort=`, `days=`, `page=`, and one or more Steam
/// roots as bare paths.
fn search(args: &[String]) -> std::process::ExitCode {
    let mut query = Query::default();
    let mut roots: Vec<PathBuf> = Vec::new();

    for arg in args {
        match arg.split_once('=') {
            Some(("text", v)) => query.text = Some(v.to_owned()),
            Some(("tag", v)) => query.required_tags.push(v.to_owned()),
            Some(("nottag", v)) => query.excluded_tags.push(v.to_owned()),
            Some(("anytag", v)) => query.match_any_tag = v != "0",
            Some(("days", v)) => query.trend_days = v.parse().ok(),
            Some(("page", v)) => query.page = v.parse().unwrap_or(1),
            Some(("sort", v)) => match Sort::parse(v) {
                Some(sort) => query.sort = sort,
                None => {
                    emit_error(&format!("unknown sort {v:?}"));
                    return std::process::ExitCode::FAILURE;
                }
            },
            _ => roots.push(PathBuf::from(arg)),
        }
    }

    let session = match Session::open(&roots) {
        Ok(session) => session,
        Err(err) => {
            emit_error(&err.to_string());
            return std::process::ExitCode::FAILURE;
        }
    };

    match session.search(&query, CALL_TIMEOUT) {
        Ok(found) => {
            let items: Vec<serde_json::Value> = found
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "id": f.id.to_string(),
                        "title": f.title,
                        "owner": f.owner.to_string(),
                        "created": f.created,
                        "updated": f.updated,
                        "size": f.size,
                        "votes_up": f.votes_up,
                        "votes_down": f.votes_down,
                        "score": f.score,
                        "preview_url": f.preview_url,
                        "tags": f.tags,
                    })
                })
                .collect();
            println!("{}", serde_json::Value::Array(items));
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            emit_error(&err.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

/// Split `<id> [steam-root]...` off an argument list.
fn id_and_roots(args: &[String]) -> Option<(u64, Vec<PathBuf>)> {
    let (id, rest) = args.split_first()?;
    let id = id.parse().ok()?;
    Some((id, rest.iter().map(PathBuf::from).collect()))
}

/// Report one item's state, plus where its files are and any download in
/// flight — everything a caller needs to decide whether to wait.
fn state(args: &[String]) -> std::process::ExitCode {
    let Some((id, roots)) = id_and_roots(args) else {
        emit_error("usage: kirie-steam-helper state <id> [steam-root]…");
        return std::process::ExitCode::FAILURE;
    };

    let session = match Session::open(&roots) {
        Ok(session) => session,
        Err(err) => {
            emit_error(&err.to_string());
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("{}", item_json(&session, id));
    std::process::ExitCode::SUCCESS
}

/// Subscribe to an item and ask Steam to fetch it.
///
/// Answers as soon as Steam accepts, reporting the same state block `state`
/// does. The caller waits for the files by watching the filesystem — this
/// process must not linger (see `steam.rs`'s module docs on playtime).
fn subscribe(args: &[String]) -> std::process::ExitCode {
    let Some((id, roots)) = id_and_roots(args) else {
        emit_error("usage: kirie-steam-helper subscribe <id> [steam-root]…");
        return std::process::ExitCode::FAILURE;
    };

    let session = match Session::open(&roots) {
        Ok(session) => session,
        Err(err) => {
            emit_error(&err.to_string());
            return std::process::ExitCode::FAILURE;
        }
    };

    if let Err(err) = session.subscribe(id, CALL_TIMEOUT) {
        emit_error(&err.to_string());
        return std::process::ExitCode::FAILURE;
    }

    println!("{}", item_json(&session, id));
    std::process::ExitCode::SUCCESS
}

/// Drop a subscription. Steam deletes the files on its own schedule, so the
/// answer is the item's state rather than a promise that they are gone.
fn unsubscribe(args: &[String]) -> std::process::ExitCode {
    let Some((id, roots)) = id_and_roots(args) else {
        emit_error("usage: kirie-steam-helper unsubscribe <id> [steam-root]…");
        return std::process::ExitCode::FAILURE;
    };

    let session = match Session::open(&roots) {
        Ok(session) => session,
        Err(err) => {
            emit_error(&err.to_string());
            return std::process::ExitCode::FAILURE;
        }
    };

    if let Err(err) = session.unsubscribe(id, CALL_TIMEOUT) {
        emit_error(&err.to_string());
        return std::process::ExitCode::FAILURE;
    }

    println!("{}", item_json(&session, id));
    std::process::ExitCode::SUCCESS
}

/// One item's state as JSON, shared by `state` and `subscribe`.
fn item_json(session: &Session, id: u64) -> serde_json::Value {
    let state = session.item_state(id);
    let install = session.item_install_info(id);
    let download = session.item_download_info(id);
    serde_json::json!({
        "id": id.to_string(),
        "subscribed": state.subscribed,
        "installed": state.installed,
        "needs_update": state.needs_update,
        "downloading": state.downloading,
        "download_pending": state.download_pending,
        "dir": install.as_ref().map(|i| i.folder.display().to_string()),
        "size": install.as_ref().map(|i| i.size),
        "updated": install.as_ref().map(|i| i.updated),
        "downloaded_bytes": download.as_ref().map(|d| d.downloaded),
        "total_bytes": download.as_ref().map(|d| d.total),
    })
}

/// A failure the caller should treat as "the helper could not answer".
fn emit_error(message: &str) {
    println!("{}", serde_json::json!({ "error": message }));
}
