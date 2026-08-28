pub mod steam;

use std::path::PathBuf;

use steam::{APP_ID, Query, Session, Sort, SteamError};

const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub const HELPER_ARG: &str = "__steamhelper";

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

fn probe(args: &[String]) -> std::process::ExitCode {
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

fn id_and_roots(args: &[String]) -> Option<(u64, Vec<PathBuf>)> {
    let (id, rest) = args.split_first()?;
    let id = id.parse().ok()?;
    Some((id, rest.iter().map(PathBuf::from).collect()))
}

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

fn emit_error(message: &str) {
    println!("{}", serde_json::json!({ "error": message }));
}
