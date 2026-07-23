//! Out-of-process webview backend client: the webkit runtime and its
//! background-layer window live in a spawned `kirie-webviewhost` child; this
//! side is just a process handle and a command pipe, so it is `Send` and
//! carries zero gtk/webkit linkage.
//!
//! Unlike [`crate::hosted`] (CEF) there is **no frame channel**: webkit has no
//! off-screen path, so the child presents *itself* on the compositor's
//! background layer (gtk-layer-shell) above the engine's own layer surface.
//! [`WebBackend::latest_frame`] is therefore always `None` and the engine's
//! surface simply stays black underneath.
//!
//! ## Protocol (engine → child stdin, line-based)
//!
//! `props <single-line-json>`, `mute <0|1>`, `pointer <x> <y> <l> <r>`,
//! `resize <w> <h>` (informational — the layer window is anchored to the
//! output), `quit`. Child stdout: `ready` once the page is up; anything else
//! is ignored (forward-compatible). Killing the child tears down webkit and
//! its layer window deterministically.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use crate::backend::{PointerState, WebBackend, WebError, WebFrameRef, WebSize};

/// Locate the `kirie-webviewhost` binary: `KIRIE_WEBVIEWHOST` override, else
/// beside the current executable.
fn host_path() -> Result<std::path::PathBuf, WebError> {
    if let Some(p) = std::env::var_os("KIRIE_WEBVIEWHOST") {
        return Ok(std::path::PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|_| WebError::Init("current_exe".into()))?;
    let dir = exe.parent().ok_or_else(|| WebError::Init("exe dir".into()))?;
    let candidate = dir.join("kirie-webviewhost");
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(WebError::Init("kirie-webviewhost binary not found".into()))
    }
}

/// Spawn the host and wait for its `ready` line (webkit + layer-surface
/// bring-up dominates; a broken child is detected by pipe EOF/timeout).
fn spawn_host(url: &str, size: WebSize) -> Result<(Child, ChildStdin), WebError> {
    let host = host_path()?;
    let mut child = Command::new(&host)
        .arg("--url")
        .arg(url)
        .arg("--width")
        .arg(size.width.to_string())
        .arg("--height")
        .arg(size.height.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr inherits → the child's tracing lands in the engine log.
        .spawn()
        .map_err(|_| WebError::Init("kirie-webviewhost spawn".into()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| WebError::Init("webviewhost pipes".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WebError::Init("webviewhost pipes".into()))?;

    // Wait for `ready` on a helper thread so a hung child can't wedge the
    // engine: the reader sends the first lines over a channel we poll with a
    // deadline.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("kirie-webviewhost-io".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| WebError::Init("webviewhost spawn".into()))?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) if line.trim() == "ready" => break,
            Ok(_) => {}
            Err(_) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WebError::Init("webviewhost ready timeout".into()));
                }
                if let Ok(Some(status)) = child.try_wait() {
                    tracing::warn!(%status, "webviewhost exited during startup");
                    return Err(WebError::Init("webviewhost died during startup".into()));
                }
            }
        }
    }

    tracing::info!(host = %host.display(), pid = child.id(), "webview host process started");
    Ok((child, stdin))
}

/// The out-of-process webview backend handle. `Send`: a child process and a
/// pipe, no browser objects.
pub struct ViewHostBackend {
    child: Child,
    stdin: ChildStdin,
    /// Spawn parameters retained for crash auto-restart.
    url: String,
    size: WebSize,
    /// Restart budget + backoff, same policy as the CEF host client.
    restarts_left: u8,
    restart_after: Instant,
}

impl ViewHostBackend {
    fn send_line(&mut self, line: &str) {
        // A dead child means the wallpaper is being torn down or restarted.
        let _ = writeln!(self.stdin, "{line}");
        let _ = self.stdin.flush();
    }
}

impl WebBackend for ViewHostBackend {
    fn new(url: &str, size: WebSize) -> Result<Self, WebError> {
        let size = size.clamped();
        let (child, stdin) = spawn_host(url, size)?;
        Ok(Self {
            child,
            stdin,
            url: url.to_owned(),
            size,
            restarts_left: 3,
            restart_after: Instant::now(),
        })
    }

    fn tick(&mut self, _dt: f32) {
        // Crash auto-restart with a small budget + backoff; past the budget
        // the output stays black rather than crash-looping webkit.
        if let Ok(Some(status)) = self.child.try_wait()
            && self.restarts_left > 0
            && Instant::now() >= self.restart_after
        {
            self.restarts_left -= 1;
            self.restart_after = Instant::now() + Duration::from_secs(5);
            tracing::warn!(%status, left = self.restarts_left, "webview host died; restarting");
            if let Ok((child, stdin)) = spawn_host(&self.url, self.size) {
                self.child = child;
                self.stdin = stdin;
            }
        }
    }

    /// Always `None`: the child presents natively on its own layer surface.
    fn latest_frame(&self) -> Option<WebFrameRef<'_>> {
        None
    }

    fn resize(&mut self, size: WebSize) {
        let s = size.clamped();
        self.size = s;
        self.send_line(&format!("resize {} {}", s.width, s.height));
    }

    fn send_pointer(&mut self, pointer: PointerState) {
        self.send_line(&format!(
            "pointer {} {} {} {}",
            pointer.x,
            pointer.y,
            u8::from(pointer.left),
            u8::from(pointer.right)
        ));
    }

    fn set_muted(&mut self, muted: bool) {
        self.send_line(&format!("mute {}", u8::from(muted)));
    }

    fn apply_properties(&mut self, json: &str) {
        // The batch is single-line JSON by construction (serde output).
        if !json.contains('\n') {
            self.send_line(&format!("props {json}"));
        }
    }

    fn shutdown(&mut self) {
        self.send_line("quit");
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        tracing::info!("webview host process stopped");
    }
}

impl Drop for ViewHostBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}
