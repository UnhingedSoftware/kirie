mod art;
mod metadata;
mod state;
mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;

pub use art::{AlbumArt, MAX_THUMBNAIL_EDGE, MediaPlaybackEvent, load_art};
pub use metadata::parse_metadata;
pub use state::{MediaState, PlaybackState, TrackMetadata};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaStatus {
    Disabled,
    Starting,
    Connected,
    Failed,
}

impl MediaStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Disabled,
            1 => Self::Starting,
            2 => Self::Connected,
            _ => Self::Failed,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Starting => 1,
            Self::Connected => 2,
            Self::Failed => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MediaConfig {
    pub enabled: bool,
    pub tick: Duration,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick: Duration::from_secs(1),
        }
    }
}

impl MediaConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }
}

struct WorkerParams {
    tick: Duration,
}

pub struct MediaSource {
    shared: Arc<ArcSwap<MediaState>>,
    status: Arc<AtomicU8>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl MediaSource {
    #[must_use]
    pub fn start(config: MediaConfig) -> Self {
        let shared = Arc::new(ArcSwap::from_pointee(MediaState::empty()));
        let shutdown = Arc::new(AtomicBool::new(false));

        if !config.enabled {
            return Self {
                shared,
                status: Arc::new(AtomicU8::new(MediaStatus::Disabled.as_u8())),
                shutdown,
                worker: None,
            };
        }

        let status = Arc::new(AtomicU8::new(MediaStatus::Starting.as_u8()));
        let worker = {
            let shared = shared.clone();
            let status = status.clone();
            let shutdown = shutdown.clone();
            let params = WorkerParams { tick: config.tick };
            Some(
                std::thread::Builder::new()
                    .name("kirie-mpris".into())
                    .spawn(move || {
                        worker::run(shared, status, shutdown, params);
                    })
                    .expect("spawn mpris worker"),
            )
        };

        Self {
            shared,
            status,
            shutdown,
            worker,
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::start(MediaConfig::disabled())
    }

    #[must_use]
    pub fn latest(&self) -> Arc<MediaState> {
        self.shared.load_full()
    }

    #[must_use]
    pub fn event(&self) -> MediaPlaybackEvent {
        MediaPlaybackEvent::from_state(&self.latest())
    }

    #[must_use]
    pub fn status(&self) -> MediaStatus {
        MediaStatus::from_u8(self.status.load(Ordering::Relaxed))
    }
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_source_is_empty_and_never_spawns() {
        let media = MediaSource::disabled();
        assert_eq!(media.status(), MediaStatus::Disabled);
        let state = media.latest();
        assert!(!state.available);
        assert!(state.metadata.is_empty());
        let ev = media.event();
        assert!(!ev.available);
        assert_eq!(ev.state, PlaybackState::Stopped.as_i32());
        assert!(ev.primary_color.is_none());
    }

    #[test]
    fn config_builders() {
        let c = MediaConfig::default().with_tick(Duration::from_millis(250));
        assert!(c.enabled);
        assert_eq!(c.tick, Duration::from_millis(250));
        assert!(!MediaConfig::disabled().enabled);
    }

    #[test]
    fn live_session_bus_no_panic() {
        if std::env::var("KIRIE_MPRIS_LIVE").as_deref() != Ok("1") {
            eprintln!("skipping live MPRIS test (set KIRIE_MPRIS_LIVE=1 to run)");
            return;
        }
        let media = MediaSource::start(MediaConfig::default().with_tick(Duration::from_millis(200)));
        std::thread::sleep(Duration::from_millis(600));
        let state = media.latest();
        eprintln!(
            "live media: status={:?} available={} player={:?} playback={:?} title={:?} artist={:?} pos={:.2}s/{:.2}s art={:?}",
            media.status(),
            state.available,
            state.player,
            state.playback,
            state.metadata.title,
            state.metadata.artist,
            state.position_secs(),
            state.duration_secs(),
            state.art.as_ref().map(|a| (a.width, a.height)),
        );
        assert!(matches!(
            media.status(),
            MediaStatus::Connected | MediaStatus::Failed
        ));
        if !state.available {
            assert!(state.metadata.is_empty());
        }
        let _ = media.event();
    }
}
