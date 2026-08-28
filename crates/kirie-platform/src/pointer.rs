use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct PointerButtons {
    left: Arc<AtomicBool>,
}

impl PointerButtons {
    #[must_use]
    pub fn left(&self) -> bool {
        self.left.load(Ordering::Relaxed)
    }

    pub(crate) fn set_left(&self, down: bool) {
        self.left.store(down, Ordering::Relaxed);
    }
}

#[derive(Clone, Default)]
pub struct PointerPoll {
    pos: Arc<RwLock<Option<(f64, f64)>>>,
    active: Arc<AtomicBool>,
}

impl PointerPoll {
    #[must_use]
    pub fn start() -> Self {
        let handle = PointerPoll {
            active: Arc::new(AtomicBool::new(true)),
            ..PointerPoll::default()
        };
        let Some(sock) = hypr_socket_path() else {
            return handle;
        };
        let slot = handle.pos.clone();
        let active = handle.active.clone();
        let _ = std::thread::Builder::new()
            .name("kirie-pointer-poll".into())
            .spawn(move || {
                loop {
                    if active.load(Ordering::Relaxed) {
                        let read = query_cursorpos(&sock);
                        if let Ok(mut w) = slot.write() {
                            *w = read;
                        }
                        std::thread::sleep(Duration::from_millis(16));
                    } else {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            });
        handle
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    #[must_use]
    pub fn get(&self) -> Option<(f64, f64)> {
        self.pos.read().ok().and_then(|g| *g)
    }
}

fn hypr_socket_path() -> Option<std::path::PathBuf> {
    let run = std::env::var_os("XDG_RUNTIME_DIR")?;
    let his = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let p = std::path::PathBuf::from(run)
        .join("hypr")
        .join(his)
        .join(".socket.sock");
    p.exists().then_some(p)
}

fn query_cursorpos(sock: &std::path::Path) -> Option<(f64, f64)> {
    let mut s = std::os::unix::net::UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(50))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(50))).ok()?;
    s.write_all(b"cursorpos").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    let (x, y) = buf.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}
