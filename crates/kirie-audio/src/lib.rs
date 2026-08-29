#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod automute;
#[cfg(target_os = "linux")]
mod capture;
pub mod dsp;
mod spectrum;
#[cfg(target_os = "linux")]
mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
#[cfg(target_os = "linux")]
use ringbuf::HeapRb;
#[cfg(target_os = "linux")]
use ringbuf::traits::Split;

#[cfg(target_os = "linux")]
pub use automute::AutoMute;
pub use dsp::{
    BANDS_16, BANDS_32, BANDS_64, DEFAULT_GATE, DEFAULT_LEVEL, SAMPLE_RATE, SMOOTH_RATE, WAVE_BUFFER_SIZE,
};
pub use spectrum::AudioSpectrum;

#[cfg(target_os = "linux")]
const RING_CAPACITY: usize = SAMPLE_RATE as usize / 4;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("failed to connect to PulseAudio server: {0}")]
    Connect(String),
    #[error("no monitor source available")]
    NoMonitor,
    #[error("failed to connect record stream to source {source_name:?}: {reason}")]
    StreamConnect { source_name: String, reason: String },
    #[error("PulseAudio mainloop error")]
    Mainloop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureStatus {
    Disabled,
    Starting,
    Running,
    Failed,
}

impl CaptureStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Disabled,
            1 => Self::Starting,
            2 => Self::Running,
            _ => Self::Failed,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Starting => 1,
            Self::Running => 2,
            Self::Failed => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioConfig {
    pub enabled: bool,
    pub device: Option<String>,
    pub gate: Option<f32>,
    pub tick: Duration,
    pub power_save: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            device: None,
            gate: None,
            tick: Duration::from_millis(16),
            power_save: None,
        }
    }
}

impl AudioConfig {
    #[must_use]
    pub fn with_device(device: Option<String>) -> Self {
        Self {
            device: device.filter(|d| !d.is_empty()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn disabled_on(config: Self) -> Self {
        Self {
            enabled: false,
            ..config
        }
    }

    #[cfg(target_os = "linux")]
    fn resolved_gate(&self) -> f32 {
        if let Some(g) = self.gate {
            return g;
        }
        match std::env::var("WPE_AUDIO_GATE") {
            Ok(v) => v.trim().parse::<f32>().unwrap_or(DEFAULT_GATE),
            Err(_) => DEFAULT_GATE,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerHint {
    pub pid: Option<u32>,
    pub name: Option<String>,
}

impl PlayerHint {
    #[must_use]
    pub fn from_bus_name(bus_name: &str, pid: Option<u32>) -> Self {
        let name = bus_name
            .strip_prefix("org.mpris.MediaPlayer2.")
            .and_then(|rest| rest.split('.').next())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
        Self { pid, name }
    }
}

pub(crate) type PlayerSlot = Arc<arc_swap::ArcSwapOption<PlayerHint>>;

pub struct AudioCapture {
    shared: Arc<ArcSwap<AudioSpectrum>>,
    status: Arc<AtomicU8>,
    shutdown: Arc<AtomicBool>,
    device: Option<String>,
    player: PlayerSlot,
    capture_thread: Option<JoinHandle<()>>,
    worker_thread: Option<JoinHandle<()>>,
}

impl AudioCapture {
    #[must_use]
    pub fn start(config: AudioConfig) -> Self {
        #[cfg(not(target_os = "linux"))]
        let config = AudioConfig::disabled_on(config);
        let shared = Arc::new(ArcSwap::from_pointee(AudioSpectrum::silent()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let player = PlayerSlot::default();

        #[cfg(not(target_os = "linux"))]
        {
            let status = Arc::new(AtomicU8::new(CaptureStatus::Disabled.as_u8()));
            return Self {
                shared,
                status,
                shutdown,
                device: config.device.clone(),
                player,
                capture_thread: None,
                worker_thread: None,
            };
        }

        #[cfg(target_os = "linux")]
        if !config.enabled {
            let status = Arc::new(AtomicU8::new(CaptureStatus::Disabled.as_u8()));
            return Self {
                shared,
                status,
                shutdown,
                device: config.device.clone(),
                player,
                capture_thread: None,
                worker_thread: None,
            };
        }

        #[cfg(target_os = "linux")]
        {
        let status = Arc::new(AtomicU8::new(CaptureStatus::Starting.as_u8()));
        let device = config.device.clone();
        let gate = config.resolved_gate();
        let level: f32 = std::env::var("KIRIE_AUDIO_BOOST")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|b: &f32| b.is_finite() && *b >= 0.0)
            .unwrap_or(0.12)
            .min(64.0);
        let tick = config.tick;
        let power_save = config.power_save.clone();

        let (prod, cons) = HeapRb::<u8>::new(RING_CAPACITY).split();

        let worker_thread = {
            let shared = shared.clone();
            let shutdown = shutdown.clone();
            Some(
                std::thread::Builder::new()
                    .name("kirie-audio-fft".into())
                    .spawn(move || {
                        worker::run(
                            cons,
                            shared,
                            shutdown,
                            worker::WorkerParams {
                                level,
                                gate,
                                tick,
                                power_save,
                            },
                        );
                    })
                    .expect("spawn fft worker"),
            )
        };

        let capture_thread = {
            let status = status.clone();
            let shutdown = shutdown.clone();
            let device = device.clone();
            let player_cap = player.clone();
            Some(
                std::thread::Builder::new()
                    .name("kirie-audio-capture".into())
                    .spawn(move || {
                        if let Err(e) = capture::run(device, prod, &status, &shutdown, &player_cap) {
                            status.store(CaptureStatus::Failed.as_u8(), Ordering::Relaxed);
                            tracing::warn!(error = %e, "audio capture unavailable; spectrum silent");
                        }
                    })
                    .expect("spawn capture thread"),
            )
        };

        Self {
            shared,
            status,
            shutdown,
            device,
            player,
            capture_thread,
            worker_thread,
        }
    }
        }

    pub fn set_player(&self, hint: Option<PlayerHint>) {
        self.player.store(hint.map(Arc::new));
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::start(AudioConfig::disabled())
    }

    #[must_use]
    pub fn latest_spectrum(&self) -> Arc<AudioSpectrum> {
        self.shared.load_full()
    }

    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        CaptureStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.capture_thread.take() {
            let _ = h.join();
        }
    }
}
