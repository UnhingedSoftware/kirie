#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::{Arc, Mutex, PoisonError};

use kirie_audio::AudioCapture;
use kirie_render::media::{MediaPlaybackEvent, MediaSource, MediaState};
use kirie_web::feed::{MediaPalette, MediaSnapshot, WebFeed};

#[derive(Default)]
struct ArtCache {
    key: usize,
    uri: Option<Arc<str>>,
    palette: Option<MediaPalette>,
}

pub(crate) struct EngineWebFeed {
    audio: Option<Arc<AudioCapture>>,
    media: Option<Arc<MediaSource>>,
    art: Mutex<ArtCache>,
}

impl EngineWebFeed {
    pub(crate) fn new(audio: Option<Arc<AudioCapture>>, media: Option<Arc<MediaSource>>) -> Option<Self> {
        if audio.is_none() && media.is_none() {
            return None;
        }
        Some(Self {
            audio,
            media,
            art: Mutex::new(ArtCache::default()),
        })
    }

    fn thumbnail(&self, state: &MediaState) -> (Option<Arc<str>>, Option<MediaPalette>) {
        let key = state.art.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize);
        let mut cache = self.art.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.key != key {
            cache.key = key;
            let (uri, palette) = encode_art(state);
            cache.uri = uri;
            cache.palette = palette;
        }
        (cache.uri.clone(), cache.palette.clone())
    }
}

fn encode_art(state: &MediaState) -> (Option<Arc<str>>, Option<MediaPalette>) {
    let Some(art) = state.art.as_ref() else {
        return (None, None);
    };
    let Some(uri) = art.png_data_uri() else {
        return (None, None);
    };
    let event = MediaPlaybackEvent::from_state(state);
    let palette = palette_of(&event);
    (Some(Arc::from(uri.as_str())), palette)
}

fn palette_of(event: &MediaPlaybackEvent) -> Option<MediaPalette> {
    Some(MediaPalette {
        primary: event.primary_color.clone()?,
        secondary: event.secondary_color.clone()?,
        text: event.text_color.clone()?,
        high_contrast: event.high_contrast_color.clone()?,
    })
}

impl WebFeed for EngineWebFeed {
    fn audio(&self) -> Option<Vec<f32>> {
        Some(self.audio.as_ref()?.latest_spectrum().audio64.to_vec())
    }

    fn media(&self) -> Option<MediaSnapshot> {
        let state = self.media.as_ref()?.latest();
        if let Some(audio) = &self.audio {
            audio.set_player(
                state
                    .player
                    .as_deref()
                    .map(|bus| kirie_audio::PlayerHint::from_bus_name(bus, state.player_pid)),
            );
        }
        let (thumbnail, palette) = self.thumbnail(&state);
        Some(MediaSnapshot {
            available: state.available,
            title: state.metadata.title.clone(),
            artist: state.metadata.artist.clone(),
            album: state.metadata.album.clone(),
            state: state.playback.as_i32(),
            position_secs: state.position_secs(),
            duration_secs: state.duration_secs(),
            thumbnail,
            palette,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirie_render::media::{AlbumArt, MediaConfig, TrackMetadata};

    fn art(color: u8) -> Arc<AlbumArt> {
        Arc::new(AlbumArt::new(2, 1, vec![color, 20, 30, 255, 0, 0, 0, 255]))
    }

    fn feed() -> EngineWebFeed {
        EngineWebFeed::new(None, Some(Arc::new(MediaSource::start(MediaConfig::disabled()))))
            .expect("a media-only feed is still a feed")
    }

    #[test]
    fn no_sources_means_no_feed() {
        assert!(EngineWebFeed::new(None, None).is_none());
    }

    #[test]
    fn art_cache_keys_on_identity() {
        let feed = feed();
        let a = art(200);
        let mut state = MediaState {
            player_pid: None,
            available: true,
            player: None,
            playback: kirie_render::PlaybackState::Playing,
            metadata: TrackMetadata::default(),
            position_us: 0,
            art: Some(a.clone()),
        };

        let (uri1, palette1) = feed.thumbnail(&state);
        let uri1 = uri1.expect("art encodes");
        assert!(uri1.starts_with("data:image/png;base64,"));
        assert!(palette1.is_some());

        let (uri2, _) = feed.thumbnail(&state);
        assert!(Arc::ptr_eq(&uri1, &uri2.expect("cached")));

        state.art = Some(art(10));
        let (uri3, _) = feed.thumbnail(&state);
        assert!(!Arc::ptr_eq(&uri1, &uri3.expect("re-encoded")));

        state.art = None;
        assert_eq!(feed.thumbnail(&state), (None, None));
    }

    #[test]
    fn empty_media_projects_to_an_empty_snapshot() {
        let snap = feed().media().expect("media handle present");
        assert!(!snap.available);
        assert_eq!(snap.state, 0);
        assert!(snap.title.is_empty());
        assert!(snap.thumbnail.is_none());
        assert!(snap.palette.is_none());
    }

    #[test]
    fn absent_audio_yields_no_frame() {
        assert!(feed().audio().is_none());
    }
}
