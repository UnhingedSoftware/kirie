use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet as ContextFlags, State as ContextState};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::proplist::{Proplist, properties};
use libpulse_binding::volume::Volume;

use crate::AudioError;

const CONTEXT_NAME: &str = "wallpaperengine";

const POLL: Duration = Duration::from_millis(200);

pub struct AutoMute {
    playing: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    enabled: bool,
    thread: Option<JoinHandle<()>>,
}

impl AutoMute {
    #[must_use]
    pub fn start(enabled: bool) -> Self {
        let playing = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        if !enabled {
            return Self {
                playing,
                shutdown,
                enabled: false,
                thread: None,
            };
        }

        let thread = {
            let playing = playing.clone();
            let shutdown = shutdown.clone();
            std::thread::Builder::new()
                .name("kirie-automute".into())
                .spawn(move || {
                    if let Err(e) = run(&playing, &shutdown) {
                        playing.store(false, Ordering::Relaxed);
                        if !shutdown.load(Ordering::Relaxed) {
                            tracing::warn!(error = %e, "automute detector unavailable; wallpaper audio never auto-muted");
                        }
                    }
                })
                .expect("spawn automute thread")
        };

        Self {
            playing,
            shutdown,
            enabled: true,
            thread: Some(thread),
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::start(false)
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

impl Drop for AutoMute {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

fn pump(mainloop: &mut Mainloop) -> Result<(), AudioError> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) | IterateResult::Err(_) => Err(AudioError::Mainloop),
    }
}

fn iterate_until<T>(
    mainloop: &mut Mainloop,
    shutdown: &AtomicBool,
    mut check: impl FnMut() -> Option<Result<T, AudioError>>,
) -> Result<T, AudioError> {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(AudioError::Mainloop);
        }
        pump(mainloop)?;
        if let Some(res) = check() {
            return res;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn sleep_or_shutdown(shutdown: &AtomicBool, dur: Duration) {
    let step = Duration::from_millis(10);
    let mut slept = Duration::ZERO;
    while slept < dur {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(step);
        slept += step;
    }
}

fn run(playing: &AtomicBool, shutdown: &AtomicBool) -> Result<(), AudioError> {
    let mut mainloop = Mainloop::new().ok_or_else(|| AudioError::Connect("no mainloop".into()))?;

    let proplist = Proplist::new().ok_or_else(|| AudioError::Connect("no proplist".into()))?;
    let mut context = Context::new_with_proplist(&mainloop, CONTEXT_NAME, &proplist)
        .ok_or_else(|| AudioError::Connect("no context".into()))?;
    context
        .connect(None, ContextFlags::NOFLAGS, None)
        .map_err(|e| AudioError::Connect(format!("{e:?}")))?;

    iterate_until(&mut mainloop, shutdown, || match context.get_state() {
        ContextState::Ready => Some(Ok(())),
        ContextState::Failed | ContextState::Terminated => {
            Some(Err(AudioError::Connect("context failed".into())))
        }
        _ => None,
    })?;

    let own_pid = std::process::id();
    tracing::info!("automute detector running");

    let mut last: Option<bool> = None;
    while !shutdown.load(Ordering::Relaxed) {
        let found = Rc::new(Cell::new(false));
        let done = Rc::new(Cell::new(false));
        {
            let found = found.clone();
            let done = done.clone();
            let introspect = context.introspect();
            introspect.get_sink_input_info_list(move |res| match res {
                ListResult::Item(info) => {
                    let pid = info
                        .proplist
                        .get_str(properties::APPLICATION_PROCESS_ID)
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    let is_other = pid != Some(own_pid);
                    let audible = info.volume.avg() != Volume::MUTED;
                    if is_other && audible {
                        found.set(true);
                    }
                }
                ListResult::End | ListResult::Error => done.set(true),
            });
        }

        iterate_until(&mut mainloop, shutdown, || done.get().then_some(Ok(())))?;

        let now = found.get();
        if last != Some(now) {
            playing.store(now, Ordering::Relaxed);
            tracing::debug!(playing = now, "automute state changed");
            last = Some(now);
        }

        sleep_or_shutdown(shutdown, POLL);
    }

    Ok(())
}
