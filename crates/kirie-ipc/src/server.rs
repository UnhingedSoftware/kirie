use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{fs, thread};

use crossbeam_channel::{Sender, bounded};

use crate::command::{Request, parse_request};
use crate::error::IpcError;
use crate::event::{CommandOutcome, IpcEvent};
use crate::status::format_status;

const READ_TIMEOUT: Duration = Duration::from_millis(50);

const READ_CHUNK: usize = 1024;

const CONNECTION_DEADLINE: Duration = Duration::from_secs(2);

const WORKSHOP_DEADLINE: Duration = Duration::from_secs(30);

const RESP_PONG: &[u8] = b"pong\n";
const RESP_OK: &[u8] = b"ok\n";
const RESP_ERROR: &[u8] = b"error\n";
const RESP_UNKNOWN: &[u8] = b"unknown command\n";

#[derive(Debug)]
pub struct ControlSocket {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlSocket {
    pub fn bind(path: impl Into<PathBuf>, events: Sender<IpcEvent>) -> Result<Self, IpcError> {
        let path = path.into();
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != ErrorKind::NotFound
        {
            // A shared temp dir plus a fixed name means the socket can belong to
            // a different account; /tmp's sticky bit then refuses the unlink and
            // the bind below fails with a bare EADDRINUSE. Say what is wrong.
            if e.kind() == ErrorKind::PermissionDenied && socket_is_foreign(&path) {
                return Err(IpcError::ForeignSocket { path });
            }
            tracing::debug!(path = %path.display(), error = %e, "stale socket unlink failed");
        }
        let listener = UnixListener::bind(&path).map_err(|source| IpcError::Bind {
            path: path.clone(),
            source,
        })?;
        tracing::info!(path = %path.display(), "ControlSocket listening");
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("kirie-ipc".into())
            .spawn({
                let shutdown = Arc::clone(&shutdown);
                move || serve(&listener, &events, shutdown.as_ref())
            })
            .map_err(IpcError::Spawn)?;
        Ok(Self {
            path,
            shutdown,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn shutdown(&mut self) {
        let Some(handle) = self.thread.take() else {
            return;
        };
        self.shutdown.store(true, Ordering::Release);
        match UnixStream::connect(&self.path) {
            Ok(stream) => {
                drop(stream);
                let _ = handle.join();
            }
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e,
                    "could not wake control-socket thread; detaching it");
            }
        }
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn socket_is_foreign(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(socket) = fs::metadata(path) else {
        return false;
    };
    let ours = std::env::var_os("HOME")
        .and_then(|home| fs::metadata(home).ok())
        .map(|home| home.uid());
    ours.is_some_and(|uid| uid != socket.uid())
}

fn serve(listener: &UnixListener, events: &Sender<IpcEvent>, shutdown: &AtomicBool) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                handle_connection(stream, events);
            }
            Err(e) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                tracing::warn!(error = %e, "control-socket accept failed");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn handle_connection(mut stream: UnixStream, events: &Sender<IpcEvent>) {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let deadline = Instant::now() + CONNECTION_DEADLINE;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let has_newline = chunk[..n].contains(&b'\n');
                buf.extend_from_slice(&chunk[..n]);
                if has_newline {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let line = match buf.iter().position(|&b| b == b'\n') {
        Some(i) => &buf[..i],
        None => &buf[..],
    };
    let request = parse_request(line);

    if matches!(request, Request::Workshop(_)) {
        let events = events.clone();
        thread::Builder::new()
            .name("kirie-ipc-workshop".to_owned())
            .spawn(move || {
                if let Some(response) = respond(request, &events)
                    && let Err(e) = stream.write_all(&response)
                {
                    tracing::debug!(error = %e, "control-socket workshop response write failed");
                }
            })
            .map_or_else(
                |e| tracing::warn!(error = %e, "could not spawn a workshop responder"),
                |_handle| (),
            );
        return;
    }

    if let Some(response) = respond(request, events)
        && let Err(e) = stream.write_all(&response)
    {
        tracing::debug!(error = %e, "control-socket response write failed");
    }
}

fn respond(request: Request, events: &Sender<IpcEvent>) -> Option<Vec<u8>> {
    match request {
        Request::Empty => None,
        Request::Ping => Some(RESP_PONG.to_vec()),
        Request::Unknown => Some(RESP_UNKNOWN.to_vec()),
        Request::Rejected => Some(RESP_ERROR.to_vec()),
        Request::Status => {
            let (tx, rx) = bounded(1);
            events.send(IpcEvent::Status { reply: tx }).ok()?;
            let snapshot = rx.recv().ok()?;
            Some(format_status(&snapshot))
        }
        Request::List => {
            let (tx, rx) = bounded(1);
            events.send(IpcEvent::List { reply: tx }).ok()?;
            let mut body = rx.recv().ok()?.into_bytes();
            body.push(b'\n');
            Some(body)
        }
        Request::GetProperties { screen } => {
            let (tx, rx) = bounded(1);
            events.send(IpcEvent::GetProperties { screen, reply: tx }).ok()?;
            let mut body = rx.recv().ok()?.into_bytes();
            body.push(b'\n');
            Some(body)
        }
        Request::Workshop(request) => {
            let (tx, rx) = bounded(1);
            events.send(IpcEvent::Workshop { request, reply: tx }).ok()?;
            let mut body = rx
                .recv_timeout(WORKSHOP_DEADLINE)
                .unwrap_or_else(|_| r#"{"error":"the Workshop request did not finish in time"}"#.to_owned())
                .into_bytes();
            body.push(b'\n');
            Some(body)
        }
        Request::Command(command) => {
            let fallible = command.is_fallible();
            let (tx, rx) = bounded(1);
            events.send(IpcEvent::Command { command, reply: tx }).ok()?;
            let outcome = rx.recv().ok()?;
            Some(match (fallible, outcome) {
                (false, _) | (true, CommandOutcome::Ok) => RESP_OK.to_vec(),
                (true, CommandOutcome::Error) => RESP_ERROR.to_vec(),
                (true, CommandOutcome::Refused(why)) => format!("error {why}\n").into_bytes(),
            })
        }
    }
}
