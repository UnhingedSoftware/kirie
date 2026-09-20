use std::path::PathBuf;

/// Unlink the control socket when the session asks kirie to stop.
///
/// A socket file left behind is a socket the next run has to decide whether to
/// trust, so it is worth removing on the way out even though nothing guarantees
/// we get the chance.
#[cfg(unix)]
pub fn install_cleanup(socket_path: Option<PathBuf>) {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    use signal_hook::low_level;

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
                unlink(socket_path.as_deref(), signal);
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

/// Windows has no signals to wait on, but it does deliver Ctrl-C, Ctrl-Break
/// and the console-close event to a handler, which is the same moment and the
/// same job. `ctrlc` is the safe wrapper around `SetConsoleCtrlHandler`;
/// signal-hook's iterator, which the Unix arm uses, is Unix-only.
///
/// The handler runs on a thread the OS creates for it and has a few seconds
/// before Windows ends the process anyway, which is plenty to remove one file.
#[cfg(windows)]
pub fn install_cleanup(socket_path: Option<PathBuf>) {
    // SIGINT here is Ctrl-C; ctrlc also covers Ctrl-Break and the console
    // closing, which is what a user quitting kirie from the tray would do.
    const SIGINT: i32 = 2;

    let installed = ctrlc::set_handler(move || {
        unlink(socket_path.as_deref(), SIGINT);
        std::process::exit(128 + SIGINT);
    });

    if let Err(err) = installed {
        tracing::warn!(%err, "could not install the console handler; socket cleanup on exit disabled");
    }
}

fn unlink(socket_path: Option<&std::path::Path>, signal: i32) {
    let Some(path) = socket_path else {
        tracing::info!(signal, "signal received; shutting down");
        return;
    };
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), signal, "signal received; control socket unlinked");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to unlink control socket on signal");
        }
    }
}
