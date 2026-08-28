#![deny(unsafe_code)]

mod audio;
mod clock;
mod decode;
mod error;
#[cfg(feature = "vaapi")]
mod hw;
mod pacing;
mod player;
mod renderer;
mod scaling;

pub use decode::{DecodedFrame, FRAME_QUEUE_CAP, FramePixels, VideoInfo};
pub use error::VideoError;
pub use pacing::{LoopTimeline, Pacer, PacerStats, Timed};
pub use player::{VideoControl, VideoOptions, VideoPlayer};
pub use renderer::VideoRenderer;
pub use scaling::{ScalingMode, UvRect, compute_uvs};
