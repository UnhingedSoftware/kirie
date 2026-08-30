#![forbid(unsafe_code)]

mod content;
mod error;
pub mod media;
pub mod particle;
mod renderer;
mod scaling;
pub mod scene;
mod schedule;

pub use content::{FramePlacement, ImageContent, ImagePage, SamplerSpec};
pub use error::RenderError;
pub use media::{
    AlbumArt, MediaConfig, MediaPlaybackEvent, MediaSource, MediaState, MediaStatus, PlaybackState,
    TrackMetadata,
};
pub use renderer::{ImageOptions, ImageRenderer};
pub use scaling::{ClampMode, ScalingMode, UvWindow, focus, set_focus};
pub use scene::{
    SceneError, SceneLoadError, SceneOptions, SceneRenderer, load_workshop_scene, start_background_prebake,
};
pub use schedule::FrameSchedule;
