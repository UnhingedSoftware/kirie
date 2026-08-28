use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::backend::{FrameBuffer, PixelFormat, PointerState, WebBackend, WebError, WebFrameRef, WebSize};
use crate::feed::{MediaChannel, audio_line, media_line};

pub const SHM_HEADER: usize = 24;
pub const SHM_PIXELS: usize = 4096 * 2304 * 4;

fn webhost_path() -> Result<std::path::PathBuf, WebError> {
    if let Some(p) = std::env::var_os("KIRIE_WEBHOST") {
        return Ok(std::path::PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|_| WebError::Init("current_exe".into()))?;
    let dir = exe.parent().ok_or_else(|| WebError::Init("exe dir".into()))?;
    let candidate = dir.join("kirie-webhost");
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(WebError::Init("kirie-webhost binary not found".into()))
    }
}

pub struct HostedBackend {
    child: Child,
    stdin: ChildStdin,
    shm: FrameShm,
    last_seq: u64,
    cached: Option<FrameBuffer>,
    status_rx: Receiver<String>,
    url: String,
    size: WebSize,
    restarts_left: u8,
    restart_after: Instant,
}

impl HostedBackend {
    fn send_line(&mut self, line: &str) {
        let _ = writeln!(self.stdin, "{line}");
        let _ = self.stdin.flush();
    }

    fn poll_frame(&mut self) {
        let shm = (*self.shm).as_ref();
        if shm.len() < SHM_HEADER {
            return;
        }
        for _ in 0..3 {
            let seq0 = u64::from_le_bytes(shm[0..8].try_into().unwrap_or_default());
            if seq0 % 2 != 0 || seq0 == self.last_seq {
                return;
            }
            let w = u32::from_le_bytes(shm[8..12].try_into().unwrap_or_default());
            let h = u32::from_le_bytes(shm[12..16].try_into().unwrap_or_default());
            let len = (w as usize) * (h as usize) * 4;
            if w == 0 || h == 0 || SHM_HEADER + len > shm.len() {
                return;
            }
            let pixels = &shm[SHM_HEADER..SHM_HEADER + len];
            let buf = match self.cached.as_mut() {
                Some(b) => {
                    b.data.clear();
                    b.data.extend_from_slice(pixels);
                    b.width = w;
                    b.height = h;
                    b.format = PixelFormat::Bgra8;
                    None
                }
                None => Some(FrameBuffer {
                    data: pixels.to_vec(),
                    width: w,
                    height: h,
                    format: PixelFormat::Bgra8,
                }),
            };
            let seq1 = u64::from_le_bytes(shm[0..8].try_into().unwrap_or_default());
            if seq1 == seq0 {
                if let Some(b) = buf {
                    self.cached = Some(b);
                }
                self.last_seq = seq0;
                return;
            }
            if let Some(b) = buf {
                self.cached = Some(b);
            }
        }
    }
}

type FrameShm = Box<dyn AsRef<[u8]> + Send + Sync>;

type SpawnedHost = (Child, ChildStdin, FrameShm, Receiver<String>);

fn spawn_host(url: &str, size: WebSize) -> Result<SpawnedHost, WebError> {
    {
        let host = webhost_path()?;
        let mut child = Command::new(&host)
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
            .map_err(|_| WebError::Init("webhost spawn".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WebError::Init("webhost pipes".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WebError::Init("webhost pipes".into()))?;

        let (tx, status_rx) = channel();
        std::thread::Builder::new()
            .name("kirie-webhost-io".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| WebError::Init("webhost spawn".into()))?;

        let deadline = Instant::now() + Duration::from_secs(20);
        let shm = loop {
            match status_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let mut parts = line.split_whitespace();
                    if parts.next() == Some("shm")
                        && let Some(path) = parts.next()
                        && let Ok(map) = kirie_bake::map_readonly(std::path::Path::new(path))
                    {
                        break map;
                    }
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(WebError::Init("webhost shm announcement timeout".into()));
                    }
                }
            }
        };

        tracing::info!(host = %host.display(), pid = child.id(), "web host process started");
        Ok((child, stdin, shm, status_rx))
    }
}

impl WebBackend for HostedBackend {
    fn new(url: &str, size: WebSize) -> Result<Self, WebError> {
        let (child, stdin, shm, status_rx) = spawn_host(url, size.clamped())?;
        Ok(Self {
            child,
            stdin,
            shm,
            last_seq: 0,
            cached: None,
            status_rx,
            url: url.to_owned(),
            size: size.clamped(),
            restarts_left: 3,
            restart_after: Instant::now(),
        })
    }

    fn tick(&mut self, _dt: f32) {
        while self.status_rx.try_recv().is_ok() {}
        if let Ok(Some(status)) = self.child.try_wait()
            && self.restarts_left > 0
            && Instant::now() >= self.restart_after
        {
            {
                self.restarts_left -= 1;
                self.restart_after = Instant::now() + Duration::from_secs(5);
                tracing::warn!(%status, left = self.restarts_left, "web host died; restarting");
                if let Ok((child, stdin, shm, status_rx)) = spawn_host(&self.url, self.size) {
                    self.child = child;
                    self.stdin = stdin;
                    self.shm = shm;
                    self.status_rx = status_rx;
                    self.last_seq = 0;
                }
            }
        }
        self.poll_frame();
    }

    fn latest_frame(&self) -> Option<WebFrameRef<'_>> {
        self.cached.as_ref().map(|f| WebFrameRef {
            data: &f.data,
            width: f.width,
            height: f.height,
            format: f.format,
        })
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

    fn set_power_save(&mut self, on: bool) {
        self.send_line(&format!("powersave {}", u8::from(on)));
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
        tracing::info!("web host process stopped; browser runtime fully reclaimed");
    }
}

impl Drop for HostedBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}
