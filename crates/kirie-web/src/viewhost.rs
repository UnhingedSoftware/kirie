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
//! output), `audio <f> <f> …`, `media <channel> <single-line-json>`,
//! `snap <path>`, `quit`. Child stdout: `ready` once the page is up,
//! `snap ok <w> <h>` / `snap fail` in reply to a `snap`; anything else is
//! ignored (forward-compatible). Killing the child tears down webkit and its
//! layer window deterministically.
//!
//! ### Why the cover image rides the same line protocol
//!
//! `media thumb …` carries a base64 PNG and is by far the largest command —
//! hundreds of KB against a few dozen bytes for everything else. It is still
//! sent inline, un-chunked, because the two things that could go wrong do not:
//!
//! * **Truncation** — `BufRead::lines` grows its `String` to whatever arrives,
//!   so there is no line-length ceiling to exceed. The engine side guarantees
//!   the payload is single-line ([`crate::feed::media_line`] refuses anything
//!   else), which is the only property the framing actually depends on.
//! * **Blocking the render thread** — the pipe buffer is 64 KB, so a large
//!   write does wait for the child to drain. The child's stdin reader is a
//!   dedicated thread whose only job is to push lines into an unbounded
//!   channel, so it drains at memcpy speed and never stalls on the GTK loop;
//!   and a *dead* child fails the write with `EPIPE` rather than blocking
//!   (Rust ignores `SIGPIPE`). The engine side additionally bounds the cover to
//!   `kirie_render::media::MAX_THUMBNAIL_EDGE` and only re-sends it when the
//!   art genuinely changes, so this is a per-track-change event, not traffic.
//!
//! The alternative — the temp-file handoff [`ViewHostBackend::snapshot`] uses —
//! was rejected here for the opposite reason it was chosen there: a still is
//! ~14 MB and single-shot, while a cover is ~0.3 MB and arrives whenever a
//! track changes, so a file per track would trade a bounded pipe write for
//! filesystem litter that leaks whenever the host dies mid-read.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::backend::{FrameBuffer, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize};
use crate::feed::{MediaChannel, audio_line, media_line};

/// How long to wait for the host's `snap` reply before giving up on the still.
///
/// The host services stdin off a 15ms GTK timeout and `gtk_widget_draw` on a
/// full-screen page is a few tens of milliseconds, so a healthy round trip is
/// well inside this. The bound exists for the unhealthy case: this runs on the
/// **render thread**, so a host stuck in a page's JS (or gone but not yet
/// reaped) must cost a bounded pause and then degrade to "no still", never wedge
/// the engine. Releasing without a stand-in is exactly today's behaviour.
const SNAP_TIMEOUT: Duration = Duration::from_millis(1500);

/// The argument that makes a `kirie` built with the host embedded run as the
/// host instead of the engine. Underscored so it cannot collide with the
/// reference CLI, which this binary otherwise mirrors exactly.
pub const HOST_ARG: &str = "__webviewhost";

/// How to start the host process.
enum HostCommand {
    /// Re-execute this binary as the host (`web-webview`: one file to ship).
    SelfExec(std::path::PathBuf),
    /// Spawn a separate `kirie-webviewhost` binary (`web-webview-client`, or
    /// the `KIRIE_WEBVIEWHOST` override).
    Sibling(std::path::PathBuf),
}

/// The host binary carried inside the engine, if the build embedded one.
///
/// The engine deliberately does not *link* gtk: doing so maps the whole gtk /
/// gdk / cairo / pango / fontconfig chain at exec for every wallpaper,
/// including scene-only ones that never open a browser (measured: +10 MB peak
/// and +265 mappings). Carrying the host as bytes keeps the install one file
/// without paying that.
static EMBEDDED_HOST: std::sync::OnceLock<&'static [u8]> = std::sync::OnceLock::new();

/// Offer the embedded host bytes. Called once at startup by the binary that
/// embedded them; an empty slice means "this build embedded none".
pub fn set_embedded_host(bytes: &'static [u8]) {
    if !bytes.is_empty() {
        let _ = EMBEDDED_HOST.set(bytes);
    }
}

/// Materialise the embedded host into the cache and return its path.
///
/// Named by a hash of its own bytes, so a rebuilt engine writes a new file
/// rather than reusing a stale host — the exact footgun that cost two days
/// when a sibling `kirie-webviewhost` silently stayed behind.
fn extracted_host() -> Option<std::path::PathBuf> {
    extract_host(*EMBEDDED_HOST.get()?)
}

/// [`extracted_host`] over explicit bytes, so the extraction is testable
/// without setting the process-wide slot.
fn extract_host(bytes: &[u8]) -> Option<std::path::PathBuf> {
    let base = if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        std::path::PathBuf::from(x).join("kirie")
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".cache")
            .join("kirie")
    };
    let dir = base.join("host");
    let digest = blake3::hash(bytes).to_hex();
    let path = dir.join(format!("kirie-webviewhost-{}", &digest[..16]));

    // Already extracted, and by content hash it is the right one.
    if path.is_file() {
        return Some(path);
    }

    std::fs::create_dir_all(&dir).ok()?;
    // Write to a unique temp name and rename: two engines starting at once
    // must not read a half-written host.
    let tmp = dir.join(format!(
        ".kirie-webviewhost-{}.{}",
        &digest[..16],
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).ok()?;
    let mut perms = std::fs::metadata(&tmp).ok()?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&tmp, perms).ok()?;
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Some(path),
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            path.is_file().then_some(path)
        }
    }
}

/// Decide how to start the host.
///
/// An explicit `KIRIE_WEBVIEWHOST` always wins — it is the escape hatch for a
/// host built elsewhere. Otherwise a binary with the host compiled in hosts
/// itself, which also means a stale `kirie-webviewhost` left in `~/.local/bin`
/// by an older install can no longer shadow the engine it sits beside.
fn host_command() -> Result<HostCommand, WebError> {
    if let Some(p) = std::env::var_os("KIRIE_WEBVIEWHOST") {
        return Ok(HostCommand::Sibling(std::path::PathBuf::from(p)));
    }
    // A host compiled into this same binary (dev builds with the `webview`
    // feature) hosts itself; a release carries it as bytes instead.
    let exe = std::env::current_exe().map_err(|_| WebError::Init("current_exe".into()))?;
    if cfg!(feature = "webview") {
        return Ok(HostCommand::SelfExec(exe));
    }
    if let Some(path) = extracted_host() {
        return Ok(HostCommand::Sibling(path));
    }
    let dir = exe.parent().ok_or_else(|| WebError::Init("exe dir".into()))?;
    let candidate = dir.join("kirie-webviewhost");
    if candidate.is_file() {
        Ok(HostCommand::Sibling(candidate))
    } else {
        Err(WebError::Init("kirie-webviewhost binary not found".into()))
    }
}

/// Spawn the host and wait for its `ready` line (webkit + layer-surface
/// bring-up dominates; a broken child is detected by pipe EOF/timeout).
///
/// The stdout receiver is handed back rather than dropped at the end of the
/// handshake: the reader thread stops the moment nobody is listening, and the
/// `snap` reply has to arrive through it later.
fn spawn_host(url: &str, size: WebSize) -> Result<(Child, ChildStdin, Receiver<String>), WebError> {
    let (host, mut child) = match host_command()? {
        HostCommand::SelfExec(exe) => {
            let mut cmd = Command::new(&exe);
            cmd.arg(HOST_ARG);
            (exe, cmd)
        }
        HostCommand::Sibling(path) => {
            let cmd = Command::new(&path);
            (path, cmd)
        }
    };
    let mut child = child
        .arg("--url")
        .arg(url)
        .arg("--width")
        .arg(size.width.to_string())
        .arg("--height")
        .arg(size.height.to_string())
        .envs(crate::backend::gpu_offload_env())
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
    Ok((child, stdin, rx))
}

/// A private path for the host to drop one raw still into.
///
/// `$XDG_RUNTIME_DIR` first: it is a per-user tmpfs (mode 0700), so a
/// full-screen BGRA buffer never touches disk and no other user can read the
/// page's contents. `/tmp` is the fallback for the odd session that has no
/// runtime dir. The pid + counter keep two outputs (or a retried capture) from
/// racing on the same name; the caller deletes the file as soon as it has read
/// it.
fn snap_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("kirie-webview-still-{}-{seq}.bgra", std::process::id()))
}

/// The out-of-process webview backend handle. `Send`: a child process and a
/// pipe, no browser objects.
pub struct ViewHostBackend {
    child: Child,
    stdin: ChildStdin,
    /// Lines the host printed, fed by the reader thread `spawn_host` started.
    /// Only `snap` replies are meaningful after startup; everything else is
    /// drained and discarded.
    stdout: Receiver<String>,
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

    /// Wait up to [`SNAP_TIMEOUT`] for the host's answer to a `snap`, returning
    /// the reported `(width, height)`.
    ///
    /// Anything that is not a `snap` line is skipped rather than treated as the
    /// answer, so a log line or a future protocol addition slipping onto stdout
    /// cannot be mistaken for a reply. The deadline is absolute, so a chatty
    /// host cannot extend the wait one line at a time.
    fn await_snap_reply(&self) -> Option<(u32, u32)> {
        let deadline = Instant::now() + SNAP_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::debug!("webview host did not answer `snap` in time; no still");
                return None;
            }
            let line = self.stdout.recv_timeout(left).ok()?;
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("snap"), Some("ok")) => {
                    let w = parts.next()?.parse().ok()?;
                    let h = parts.next()?.parse().ok()?;
                    return Some((w, h));
                }
                (Some("snap"), _) => return None, // `snap fail`: nothing to draw
                _ => {}
            }
        }
    }
}

impl WebBackend for ViewHostBackend {
    fn new(url: &str, size: WebSize) -> Result<Self, WebError> {
        let size = size.clamped();
        let (child, stdin, stdout) = spawn_host(url, size)?;
        Ok(Self {
            child,
            stdin,
            stdout,
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
            if let Ok((child, stdin, stdout)) = spawn_host(&self.url, self.size) {
                self.child = child;
                self.stdin = stdin;
                // The old reader thread died with its pipe; replies must come
                // from the new child's channel or `snap` would wait on a
                // receiver nobody feeds.
                self.stdout = stdout;
            }
        }
    }

    /// Always `None`: the child presents natively on its own layer surface.
    /// Never: webkit paints its own layer-shell window, so the engine has no
    /// frame to composite and must not keep presenting (`Renderer::is_passive`).
    fn produces_frames(&self) -> bool {
        false
    }

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

    fn push_audio(&mut self, bands: &[f32]) {
        self.send_line(&audio_line(bands));
    }

    fn push_media(&mut self, channel: MediaChannel, json: &str) {
        // `None` only for a payload that would break the framing; the builder
        // never produces one, so dropping it is strictly better than corrupting
        // the command stream.
        if let Some(line) = media_line(channel, json) {
            self.send_line(&line);
        }
    }

    /// Have the host draw the live page for us, so the engine can leave that
    /// still on the output when this wallpaper is released.
    ///
    /// This backend is the whole reason [`WebBackend::snapshot`] exists: webkit
    /// paints into the host's own layer-shell window, so the engine's surface
    /// holds nothing but black and killing the host would uncover it for the
    /// entire release. The host can still see its own widget, though —
    /// `gtk_widget_draw` into a cairo surface — which is what `snap` asks for.
    ///
    /// The pixels come back through a file rather than the pipe: a 1440p frame
    /// is ~14MB, and streaming that down a line-based stdin/stdout protocol
    /// would mean framing binary data through a channel built for commands. The
    /// file lives in `$XDG_RUNTIME_DIR` (tmpfs, 0700 — the page's contents never
    /// touch disk) and is deleted here the moment it has been read, on every
    /// path including the failures.
    fn snapshot(&mut self) -> Option<FrameBuffer> {
        // A reply from an earlier round (a timeout that landed late) would
        // otherwise be read as the answer to this one.
        while self.stdout.try_recv().is_ok() {}

        let path = snap_path();
        let Some(arg) = path.to_str() else {
            return None; // non-UTF-8 runtime dir: not expressible on this line protocol
        };
        self.send_line(&format!("snap {arg}"));

        let reply = self.await_snap_reply();
        let data = reply.and_then(|_| std::fs::read(&path).ok());
        // Unconditional: the host may have written the file and then failed, or
        // answered after we stopped waiting. Leaving multi-MB files behind in
        // the runtime dir on every release is not an option.
        let _ = std::fs::remove_file(&path);

        let (width, height) = reply?;
        let data = data?;
        let frame = FrameBuffer {
            data,
            width,
            height,
            // The host writes cairo `ARGB32`, which on little-endian is B,G,R,A
            // in memory — matched by the texture format rather than swizzled on
            // the CPU (that would be a ~14MB per-pixel pass for nothing).
            format: PixelFormat::Bgra8,
        };
        if !frame.is_consistent() {
            tracing::debug!(
                width,
                height,
                got = frame.data.len(),
                "webview still file did not match its reported size; ignoring"
            );
            return None;
        }
        tracing::debug!(width, height, "captured a webview still for the release");
        Some(frame)
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

#[cfg(test)]
mod embedded_host_tests {
    use super::*;

    /// The carried host must land on disk executable, and a second call must
    /// reuse it rather than rewrite it.
    #[test]
    fn extracts_once_and_is_executable() {
        let bytes: &[u8] = b"#!/bin/true\n-- pretend host --";
        let first = extract_host(bytes).expect("extraction");
        assert!(first.is_file(), "host was not written: {}", first.display());

        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&first).expect("metadata").permissions(),
        );
        assert_eq!(mode & 0o111, 0o111, "extracted host is not executable");
        assert_eq!(std::fs::read(&first).expect("read"), bytes);

        let second = extract_host(bytes).expect("second extraction");
        assert_eq!(first, second, "same bytes must resolve to the same path");

        let _ = std::fs::remove_file(&first);
    }

    /// Different bytes must not reuse the previous file — this is the
    /// stale-host footgun that once cost two days of phantom webview fixes.
    #[test]
    fn different_bytes_get_a_different_path() {
        let a = extract_host(b"host A").expect("a");
        let b = extract_host(b"host B").expect("b");
        assert_ne!(a, b, "a rebuilt host must not reuse the old extraction");
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }
}
