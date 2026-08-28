use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::backend::{FrameBuffer, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize};
use crate::feed::{MediaChannel, audio_line, media_line};

const SNAP_TIMEOUT: Duration = Duration::from_millis(1500);

pub const HOST_ARG: &str = "__webviewhost";

enum HostCommand {
    SelfExec(std::path::PathBuf),
    Sibling(std::path::PathBuf),
}

static EMBEDDED_HOST: std::sync::OnceLock<&'static [u8]> = std::sync::OnceLock::new();

pub fn set_embedded_host(bytes: &'static [u8]) {
    if !bytes.is_empty() {
        let _ = EMBEDDED_HOST.set(bytes);
    }
}

fn extracted_host() -> Option<std::path::PathBuf> {
    extract_host(*EMBEDDED_HOST.get()?)
}

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

    if path.is_file() {
        return Some(path);
    }

    std::fs::create_dir_all(&dir).ok()?;
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

fn host_command() -> Result<HostCommand, WebError> {
    if let Some(p) = std::env::var_os("KIRIE_WEBVIEWHOST") {
        return Ok(HostCommand::Sibling(std::path::PathBuf::from(p)));
    }
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

fn snap_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("kirie-webview-still-{}-{seq}.bgra", std::process::id()))
}

pub struct ViewHostBackend {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<String>,
    url: String,
    size: WebSize,
    restarts_left: u8,
    restart_after: Instant,
}

impl ViewHostBackend {
    fn send_line(&mut self, line: &str) {
        let _ = writeln!(self.stdin, "{line}");
        let _ = self.stdin.flush();
    }

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
                (Some("snap"), _) => return None,
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
                self.stdout = stdout;
            }
        }
    }

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
        if !json.contains('\n') {
            self.send_line(&format!("props {json}"));
        }
    }

    fn push_audio(&mut self, bands: &[f32]) {
        self.send_line(&audio_line(bands));
    }

    fn push_media(&mut self, channel: MediaChannel, json: &str) {
        if let Some(line) = media_line(channel, json) {
            self.send_line(&line);
        }
    }

    fn snapshot(&mut self) -> Option<FrameBuffer> {
        while self.stdout.try_recv().is_ok() {}

        let path = snap_path();
        let Some(arg) = path.to_str() else {
            return None;
        };
        self.send_line(&format!("snap {arg}"));

        let reply = self.await_snap_reply();
        let data = reply.and_then(|_| std::fs::read(&path).ok());
        let _ = std::fs::remove_file(&path);

        let (width, height) = reply?;
        let data = data?;
        let frame = FrameBuffer {
            data,
            width,
            height,
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

    #[test]
    fn different_bytes_get_a_different_path() {
        let a = extract_host(b"host A").expect("a");
        let b = extract_host(b"host B").expect("b");
        assert_ne!(a, b, "a rebuilt host must not reuse the old extraction");
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }
}
