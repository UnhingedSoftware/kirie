use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use crate::audio::{AudioInit, AudioLink, CallbackCmd, DecodeCmd};
use crate::decode::{DecodedFrame, Decoder, FRAME_QUEUE_CAP, VideoInfo};
use crate::error::VideoError;
use crate::scaling::ScalingMode;

const RECYCLE_QUEUE_CAP: usize = FRAME_QUEUE_CAP + 4;

#[derive(Debug, Clone, Copy)]
pub(crate) enum RendererCmd {
    Pause(bool),
    Speed(f64),
    Scaling(ScalingMode),
}

#[derive(Debug, Clone, Copy)]
pub struct VideoOptions {
    pub volume: f64,
    pub mute: bool,
    pub silent: bool,
    pub paused: bool,
    pub nv12: bool,
    pub scaling: ScalingMode,
    pub enable_audio: bool,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            volume: 100.0,
            mute: false,
            silent: false,
            paused: false,
            scaling: ScalingMode::Default,
            nv12: false,
            enable_audio: true,
        }
    }
}

pub struct VideoPlayer {
    info: VideoInfo,
    frames_rx: Receiver<DecodedFrame>,
    recycle_tx: Sender<Vec<u8>>,
    commands_rx: Receiver<RendererCmd>,
    audio: Option<AudioLink>,
    scaling: ScalingMode,
    paused: bool,
    shutdown: Sender<()>,
}

pub(crate) struct PlayerParts {
    pub frames_rx: Receiver<DecodedFrame>,
    pub recycle_tx: Sender<Vec<u8>>,
    pub commands_rx: Receiver<RendererCmd>,
    pub audio: Option<AudioLink>,
    pub scaling: ScalingMode,
    pub paused: bool,
    pub shutdown: Sender<()>,
}

impl VideoPlayer {
    pub fn open(path: impl Into<PathBuf>, options: VideoOptions) -> Result<(Self, VideoControl), VideoError> {
        let path = path.into();

        let mut decoder = Decoder::open(&path)?;
        decoder.want_nv12 = options.nv12;
        let info = decoder.info();

        let (frames_tx, frames_rx) = bounded(FRAME_QUEUE_CAP);
        let (recycle_tx, recycle_rx) = bounded(RECYCLE_QUEUE_CAP);
        std::thread::Builder::new()
            .name("kirie-video-decode".into())
            .spawn(move || decoder.run(&frames_tx, &recycle_rx))?;

        let (renderer_tx, commands_rx) = unbounded();
        let (shutdown_tx, shutdown_rx) = bounded::<()>(1);

        let (audio, callback_tx, decode_tx) = if options.enable_audio {
            let (callback_tx, callback_rx) = unbounded();
            let (decode_tx, decode_rx) = unbounded();
            let init = AudioInit {
                volume: options.volume,
                mute: options.mute,
                silent: options.silent,
                paused: options.paused,
            };
            match crate::audio::spawn(path.clone(), init, callback_rx, decode_rx, shutdown_rx) {
                Ok(Some(link)) => (Some(link), Some(callback_tx), Some(decode_tx)),
                Ok(None) => {
                    tracing::debug!(path = %path.display(), "no audio stream; wall clock master");
                    (None, None, None)
                }
                Err(err) => {
                    tracing::warn!(%err, "audio unavailable; playing without sound");
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        };

        let player = Self {
            info,
            frames_rx,
            recycle_tx,
            commands_rx,
            audio,
            scaling: options.scaling,
            paused: options.paused,
            shutdown: shutdown_tx,
        };
        let control = VideoControl {
            renderer: renderer_tx,
            audio_callback: callback_tx,
            audio_decode: decode_tx,
        };
        Ok((player, control))
    }

    #[must_use]
    pub fn info(&self) -> VideoInfo {
        self.info
    }

    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    #[must_use]
    pub fn recv_frame_timeout(&self, timeout: Duration) -> Option<DecodedFrame> {
        self.frames_rx.recv_timeout(timeout).ok()
    }

    pub fn recycle_buffer(&self, buffer: Vec<u8>) {
        let _ = self.recycle_tx.try_send(buffer);
    }

    pub(crate) fn into_parts(self) -> PlayerParts {
        PlayerParts {
            frames_rx: self.frames_rx,
            recycle_tx: self.recycle_tx,
            commands_rx: self.commands_rx,
            audio: self.audio,
            scaling: self.scaling,
            paused: self.paused,
            shutdown: self.shutdown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoControl {
    renderer: Sender<RendererCmd>,
    audio_callback: Option<Sender<CallbackCmd>>,
    audio_decode: Option<Sender<DecodeCmd>>,
}

impl VideoControl {
    pub fn set_pause(&self, paused: bool) {
        let _ = self.renderer.send(RendererCmd::Pause(paused));
        if let Some(tx) = &self.audio_callback {
            let _ = tx.send(CallbackCmd::Pause(paused));
        }
    }

    pub fn set_speed(&self, speed: f64) {
        let speed = if speed > 0.0 && speed.is_finite() {
            speed
        } else {
            1.0
        };
        let _ = self.renderer.send(RendererCmd::Speed(speed));
        if let Some(tx) = &self.audio_decode {
            let _ = tx.send(DecodeCmd::Speed(speed));
        }
    }

    pub fn set_volume(&self, volume: f64) {
        if let Some(tx) = &self.audio_callback {
            let _ = tx.send(CallbackCmd::Volume(volume));
        }
    }

    pub fn set_mute(&self, mute: bool) {
        if let Some(tx) = &self.audio_callback {
            let _ = tx.send(CallbackCmd::Mute(mute));
        }
    }

    pub fn set_scaling(&self, mode: ScalingMode) {
        let _ = self.renderer.send(RendererCmd::Scaling(mode));
    }
}
