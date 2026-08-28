use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VideoError {
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),

    #[error("no video stream in {0}")]
    NoVideoStream(PathBuf),

    #[error("invalid video dimensions {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },

    #[error("audio output unavailable: {0}")]
    AudioOutput(String),

    #[error("unsupported audio device sample format: {0}")]
    UnsupportedSampleFormat(String),

    #[error("failed to spawn decode thread: {0}")]
    ThreadSpawn(#[from] std::io::Error),
}
