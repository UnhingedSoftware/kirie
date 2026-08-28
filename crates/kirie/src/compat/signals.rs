use std::path::PathBuf;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level;

pub fn install_cleanup(socket_path: Option<PathBuf>) {
    let mut signals = match Signals::new([SIGTERM, SIGINT]) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "could not install SIGTERM handler; socket cleanup on signal disabled");
            return;
        }
    };

    let spawn = std::thread::Builder::new()
        .name("kirie-signals".into())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                if let Some(path) = &socket_path {
                    match std::fs::remove_file(path) {
                        Ok(()) => tracing::info!(path = %path.display(), signal, "signal received; control socket unlinked"),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => tracing::warn!(path = %path.display(), error = %e, "failed to unlink control socket on signal"),
                    }
                } else {
                    tracing::info!(signal, "signal received; shutting down");
                }
                if low_level::emulate_default_handler(signal).is_err() {
                    low_level::exit(128 + signal);
                }
                low_level::exit(128 + signal);
            }
        });

    if let Err(err) = spawn {
        tracing::warn!(%err, "could not spawn signal-handler thread; socket cleanup on signal disabled");
    }
}
