use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use cef::{
    Browser, BrowserSettings, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, MouseButtonType,
    MouseEvent, Settings, WindowInfo, api_hash, args::Args, browser_host_create_browser_sync,
    do_message_loop_work, initialize, shutdown, sys::CEF_API_VERSION_LAST,
};

use crate::backend::{FrameBuffer, FrameSlot, PointerState, WebBackend, WebError, WebFrameRef, WebSize};

use super::client::{SharedSize, make_client};
use super::registry::{BrowserEntry, BrowserId, BrowserRegistry};

const FRAME_RATE: i32 = 60;

enum Command {
    Create(CreateRequest),
    Resize(BrowserId, i32, i32),
    Pointer(BrowserId, PointerState),
    Mute(BrowserId, bool),
    PowerSave(BrowserId, bool),
    ApplyProps(BrowserId, String),
    Audio(BrowserId, Vec<f32>),
    Media(BrowserId, crate::feed::MediaChannel, String),
    Close(BrowserId, Sender<()>),
    Quit,
}

struct CreateRequest {
    url: String,
    muted: bool,
    slot: FrameSlot,
    size: Arc<SharedSize>,
    reply: Sender<Result<BrowserId, WebError>>,
}

struct ThreadConfig {
    runtime_dir: PathBuf,
    helper_path: Option<PathBuf>,
    rx: Receiver<Command>,
}

struct Manager {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
    live: usize,
}

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);

fn manager_lock() -> std::sync::MutexGuard<'static, Option<Manager>> {
    MANAGER.lock().unwrap_or_else(PoisonError::into_inner)
}

static TEARDOWN: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn teardown_lock() -> std::sync::MutexGuard<'static, Option<JoinHandle<()>>> {
    TEARDOWN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn stop_thread(guard: &mut Option<Manager>) {
    if let Some(mut mgr) = guard.take() {
        let _ = mgr.tx.send(Command::Quit);
        if let Some(thread) = mgr.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct CefBackend {
    id: BrowserId,
    tx: Sender<Command>,
    slot: FrameSlot,
    cached: Option<Arc<FrameBuffer>>,
    closed: bool,
}

impl CefBackend {
    fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl WebBackend for CefBackend {
    fn new(url: &str, size: WebSize) -> Result<Self, WebError> {
        let size = size.clamped();

        let slot: FrameSlot = Arc::new(ArcSwapOption::empty());
        let shared_size = SharedSize::new(size.width as i32, size.height as i32);
        let (reply_tx, reply_rx) = channel();
        let request = CreateRequest {
            url: url.to_string(),
            muted: false,
            slot: slot.clone(),
            size: shared_size,
            reply: reply_tx,
        };

        if let Some(handle) = teardown_lock().take() {
            let _ = handle.join();
        }

        let mut guard = manager_lock();

        let tx = match guard.as_ref() {
            Some(mgr) => mgr.tx.clone(),
            None => {
                let runtime_dir = resolve_runtime_dir().ok_or_else(|| {
                    WebError::Init("could not locate the CEF runtime dir (icudtl.dat)".into())
                })?;
                let helper_path = resolve_helper_path(&runtime_dir);
                let (tx, rx) = channel();
                let config = ThreadConfig {
                    runtime_dir,
                    helper_path,
                    rx,
                };
                let thread = std::thread::Builder::new()
                    .name("kirie-cef".into())
                    .spawn(move || cef_thread_main(config))
                    .map_err(|e| WebError::Thread(e.to_string()))?;
                *guard = Some(Manager {
                    tx: tx.clone(),
                    thread: Some(thread),
                    live: 0,
                });
                tx
            }
        };

        if tx.send(Command::Create(request)).is_err() {
            stop_thread(&mut guard);
            return Err(WebError::Init("the CEF thread is gone".into()));
        }

        let outcome = reply_rx.recv().unwrap_or_else(|_| {
            Err(WebError::Init(
                "the CEF thread exited during browser creation".into(),
            ))
        });

        match outcome {
            Ok(id) => {
                if let Some(mgr) = guard.as_mut() {
                    mgr.live += 1;
                }
                Ok(Self {
                    id,
                    tx,
                    slot,
                    cached: None,
                    closed: false,
                })
            }
            Err(e) => {
                if guard.as_ref().is_some_and(|mgr| mgr.live == 0) {
                    stop_thread(&mut guard);
                }
                Err(e)
            }
        }
    }

    fn tick(&mut self, _dt: f32) {
        if let Some(frame) = self.slot.load_full() {
            self.cached = Some(frame);
        }
    }

    fn latest_frame(&self) -> Option<WebFrameRef<'_>> {
        let frame = self.cached.as_ref()?;
        if !frame.is_consistent() {
            return None;
        }
        Some(WebFrameRef {
            data: &frame.data,
            width: frame.width,
            height: frame.height,
            format: frame.format,
        })
    }

    fn resize(&mut self, size: WebSize) {
        let size = size.clamped();
        self.send(Command::Resize(self.id, size.width as i32, size.height as i32));
    }

    fn send_pointer(&mut self, pointer: PointerState) {
        self.send(Command::Pointer(self.id, pointer));
    }

    fn set_muted(&mut self, muted: bool) {
        self.send(Command::Mute(self.id, muted));
    }

    fn set_power_save(&mut self, on: bool) {
        self.send(Command::PowerSave(self.id, on));
    }

    fn apply_properties(&mut self, json: &str) {
        self.send(Command::ApplyProps(self.id, json.to_owned()));
    }

    fn push_audio(&mut self, bands: &[f32]) {
        self.send(Command::Audio(self.id, bands.to_vec()));
    }

    fn push_media(&mut self, channel: crate::feed::MediaChannel, json: &str) {
        self.send(Command::Media(self.id, channel, json.to_owned()));
    }

    fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;

        let (done_tx, _done_rx) = channel();
        let _ = self.tx.send(Command::Close(self.id, done_tx));

        let mut guard = manager_lock();
        if let Some(mgr) = guard.as_mut() {
            mgr.live = mgr.live.saturating_sub(1);
            if mgr.live == 0
                && let Some(mut mgr) = guard.take()
            {
                let handle = std::thread::spawn(move || {
                    let _ = mgr.tx.send(Command::Quit);
                    if let Some(thread) = mgr.thread.take() {
                        let _ = thread.join();
                    }
                    kirie_bake::trim_heap();
                    kirie_bake::pageout_cold_libs();
                });
                *teardown_lock() = Some(handle);
            }
        }
    }
}

impl Drop for CefBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn cef_thread_main(config: ThreadConfig) {
    let _ = api_hash(CEF_API_VERSION_LAST, 0);

    let ThreadConfig {
        runtime_dir,
        helper_path,
        rx,
    } = config;

    let cache_dir = throwaway_cache_dir();

    let mut settings = Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        disable_signal_handlers: 1,
        command_line_args_disabled: 0,
        root_cache_path: CefString::from(cache_dir.to_string_lossy().as_ref()),
        resources_dir_path: CefString::from(runtime_dir.to_string_lossy().as_ref()),
        locales_dir_path: CefString::from(runtime_dir.join("locales").to_string_lossy().as_ref()),
        ..Default::default()
    };
    if let Some(helper) = &helper_path {
        settings.browser_subprocess_path = CefString::from(helper.to_string_lossy().as_ref());
    }

    let args = Args::new();
    let mut app = super::app::make_app();

    let init_ok = initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if init_ok != 1 {
        fail_pending(&rx);
        return;
    }

    let mut registry: BrowserRegistry<Browser> = BrowserRegistry::new();
    let frame_dt = Duration::from_secs_f64(1.0 / f64::from(FRAME_RATE));
    let audio_zero = [0.0f32; 128];

    let mut settle: u32 = 0;

    'pump: loop {
        let frame_start = Instant::now();

        while settle == 0 {
            match rx.try_recv() {
                Ok(Command::Create(req)) => match create_browser(&req) {
                    Some(browser) => {
                        let id = registry.insert(browser, req.size.clone(), req.slot.clone());
                        let _ = req.reply.send(Ok(id));
                    }
                    None => {
                        let _ = req.reply.send(Err(WebError::BrowserCreation));
                    }
                },
                Ok(Command::Resize(id, w, h)) => {
                    if let Some(entry) = registry.get_mut(id) {
                        entry.size.set(w, h);
                        if let Some(host) = entry.browser.host() {
                            host.was_resized();
                        }
                    }
                }
                Ok(Command::Pointer(id, p)) => {
                    if let Some(entry) = registry.get_mut(id) {
                        entry.set_pointer(p);
                    }
                }
                Ok(Command::Mute(id, m)) => {
                    if let Some(entry) = registry.get_mut(id)
                        && let Some(host) = entry.browser.host()
                    {
                        host.set_audio_muted(i32::from(m));
                    }
                }
                Ok(Command::PowerSave(id, on)) => {
                    if let Some(entry) = registry.get_mut(id)
                        && let Some(host) = entry.browser.host()
                    {
                        host.set_windowless_frame_rate(if on { 10 } else { FRAME_RATE });
                    }
                }
                Ok(Command::ApplyProps(id, json)) => {
                    if let Some(entry) = registry.get_mut(id) {
                        entry.push_props(json);
                    }
                }
                Ok(Command::Audio(id, bands)) => {
                    if let Some(entry) = registry.get_mut(id) {
                        entry.set_audio(bands);
                    }
                }
                Ok(Command::Media(id, channel, json)) => {
                    if let Some(entry) = registry.get_mut(id) {
                        entry.push_media(channel, json);
                    }
                }
                Ok(Command::Close(id, done)) => {
                    if let Some(entry) = registry.remove(id) {
                        close_browser(entry.browser);
                        settle = 4;
                    }
                    let _ = done.send(());
                }
                Ok(Command::Quit) => break 'pump,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'pump,
            }
        }

        for (_, entry) in registry.iter_mut() {
            drive_browser(entry, &audio_zero);
        }

        do_message_loop_work();
        settle = settle.saturating_sub(1);

        if let Some(rem) = frame_dt.checked_sub(frame_start.elapsed()) {
            std::thread::sleep(rem);
        }
    }

    for (_, entry) in registry.drain() {
        close_browser(entry.browser);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while super::client::LIVE_BROWSERS.load(std::sync::atomic::Ordering::SeqCst) > 0
        && Instant::now() < deadline
    {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..10 {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(5));
    }
    tracing::info!(
        remaining = super::client::LIVE_BROWSERS.load(std::sync::atomic::Ordering::SeqCst),
        "cef context shutting down"
    );
    shutdown();
    tracing::info!("cef context shut down; browser runtime released");
    let _ = std::fs::remove_dir_all(&cache_dir);
}

fn create_browser(req: &CreateRequest) -> Option<Browser> {
    let mut client = make_client(req.slot.clone(), req.size.clone());
    let window_info = WindowInfo::default().set_as_windowless(0);
    let browser_settings = BrowserSettings {
        windowless_frame_rate: FRAME_RATE,
        ..Default::default()
    };
    let url_str = CefString::from(req.url.as_str());

    let browser = browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&url_str),
        Some(&browser_settings),
        None,
        None,
    )?;

    if req.muted
        && let Some(host) = browser.host()
    {
        host.set_audio_muted(1);
    }
    Some(browser)
}

fn close_browser(browser: Browser) {
    if let Some(host) = browser.host() {
        host.close_browser(1);
    }
    drop(browser);
}

fn drive_browser(entry: &mut BrowserEntry<Browser>, audio_zero: &[f32]) {
    let pointer = entry.pointer();
    let left_edge = entry.left_edge();
    let right_edge = entry.right_edge();
    if let Some(host) = entry.browser.host() {
        let event = MouseEvent {
            x: pointer.x,
            y: pointer.y,
            modifiers: 0,
        };
        host.send_mouse_move_event(Some(&event), 0);
        if let Some(down) = left_edge {
            host.send_mouse_click_event(Some(&event), MouseButtonType::LEFT, i32::from(!down), 1);
        }
        if let Some(down) = right_edge {
            host.send_mouse_click_event(Some(&event), MouseButtonType::RIGHT, i32::from(!down), 1);
        }
    }
    if let Some(frame) = entry.browser.main_frame() {
        let bands = if entry.audio().is_empty() {
            audio_zero
        } else {
            entry.audio()
        };
        let js = CefString::from(crate::shim::audio_call(bands).as_str());
        frame.execute_java_script(Some(&js), None, 0);
        for json in entry.drain_props_if_painted() {
            let call = CefString::from(crate::shim::apply_user_properties_call(&json).as_str());
            frame.execute_java_script(Some(&call), None, 0);
        }
        for (channel, json) in entry.drain_media_if_painted() {
            let call = CefString::from(channel.call(&json).as_str());
            frame.execute_java_script(Some(&call), None, 0);
        }
    }
}

fn fail_pending(rx: &Receiver<Command>) {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Command::Create(req) => {
                let _ = req.reply.send(Err(WebError::Init(
                    "cef_initialize returned failure (missing libcef runtime files?)".into(),
                )));
            }
            Command::Close(_, done) => {
                let _ = done.send(());
            }
            _ => {}
        }
    }
}

fn resolve_runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KIRIE_CEF_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join("icudtl.dat").exists() {
            return Some(dir);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    candidates.into_iter().find(|dir| dir.join("icudtl.dat").exists())
}

fn resolve_helper_path(runtime_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KIRIE_CEF_HELPER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let named = runtime_dir.join("kirie-cef-helper");
    if named.exists() {
        return Some(named);
    }
    let exe = std::env::current_exe().ok()?;
    let beside = exe.parent()?.join("kirie-cef-helper");
    beside.exists().then_some(beside)
}

fn throwaway_cache_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("kirie-cef-{}-{nanos}", std::process::id()))
}
