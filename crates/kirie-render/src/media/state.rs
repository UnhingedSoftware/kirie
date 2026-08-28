use std::sync::Arc;

use super::art::AlbumArt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PlaybackState {
    #[default]
    Stopped = 0,
    Playing = 1,
    Paused = 2,
}

impl PlaybackState {
    #[must_use]
    pub fn from_mpris(status: &str) -> Self {
        match status {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }

    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub length_us: Option<i64>,
}

impl TrackMetadata {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_empty()
            && self.artist.is_empty()
            && self.album.is_empty()
            && self.art_url.is_none()
            && self.length_us.is_none()
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaState {
    pub available: bool,
    pub player: Option<String>,
    pub player_pid: Option<u32>,
    pub playback: PlaybackState,
    pub metadata: TrackMetadata,
    pub position_us: i64,
    pub art: Option<Arc<AlbumArt>>,
}

impl MediaState {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn position_secs(&self) -> f64 {
        self.position_us as f64 / 1_000_000.0
    }

    #[must_use]
    pub fn duration_secs(&self) -> f64 {
        self.metadata.length_us.unwrap_or(0) as f64 / 1_000_000.0
    }
}
