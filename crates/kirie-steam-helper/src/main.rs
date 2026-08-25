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
//! ```

mod steam;

use std::path::PathBuf;

use steam::{APP_ID, Session, SteamError};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (verb, rest) = match args.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => {
            emit_error("usage: kirie-steam-helper <verb> [args…]");
            return std::process::ExitCode::FAILURE;
        }
    };

    match verb {
        "probe" => probe(rest),
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
fn probe(steam_roots: &[String]) -> std::process::ExitCode {
    let roots: Vec<PathBuf> = steam_roots.iter().map(PathBuf::from).collect();

    let library = steam::find_library(&roots);
    let session = Session::open(&roots);

    let value = match session {
        Ok(session) => serde_json::json!({
            "appid": APP_ID,
            "library": library.as_ref().map(|p| p.display().to_string()),
            "running": true,
            "owned": session.owns_app(),
            "installed": session.app_installed(),
            "install_dir": session.app_install_dir().map(|p| p.display().to_string()),
        }),
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

/// A failure the caller should treat as "the helper could not answer".
fn emit_error(message: &str) {
    println!("{}", serde_json::json!({ "error": message }));
}
