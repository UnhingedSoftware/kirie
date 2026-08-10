//! The engine's implementation of [`kirie_web::WebFeed`]: system audio +
//! MPRIS now-playing, projected into the shapes a web page's
//! `wallpaperRegister*Listener` callbacks expect.
//!
//! Both sources already exist and already publish lock-free snapshots — the
//! system-audio FFT ([`kirie_audio::AudioCapture`], docs/subsystems-misc.md
//! §1.3) and the MPRIS D-Bus client ([`kirie_render::MediaSource`], §5). What
//! was missing was the adapter between them and the browser layer, because
//! kirie-web deliberately depends on neither: it also builds the standalone
//! `kirie-webviewhost`/`kirie-webhost` binaries, and pulling in the scene
//! renderer (and through it ffmpeg) to reach one D-Bus client would be a
//! ridiculous price. So kirie-web declares the trait and the engine — which
//! owns both sources anyway — implements it here.
//!
//! The one non-trivial job in this file is the **album-art cache**. The page
//! contract wants the cover as a base64 `data:` URI, which means a PNG encode
//! of up to a quarter-megapixel; the feed is polled ten times a second on the
//! render thread. Encoding per poll would be a self-inflicted stall, so the
//! encoded URI (and the palette derived from the same pixels) is cached against
//! the *identity* of the decoded art. The MPRIS worker already caches its
//! decode by art URL and hands back the same `Arc<AlbumArt>` until the URL
//! changes, so that identity is stable per track and the cache hits on every
//! poll but the first of each track.

use std::sync::{Arc, Mutex, PoisonError};

use kirie_audio::AudioCapture;
use kirie_render::media::{MediaPlaybackEvent, MediaSource, MediaState};
use kirie_web::feed::{MediaPalette, MediaSnapshot, WebFeed};

/// Cached cover encoding, keyed by the identity of the art it came from.
#[derive(Default)]
struct ArtCache {
    /// `Arc::as_ptr` of the [`AlbumArt`] the cached values were built from, or
    /// `0` for "no art". A pointer is a sound key here *because* the `Arc` is
    /// held alive by the published snapshot for as long as that art is current:
    /// the address cannot be recycled under us while it is still the answer.
    key: usize,
    /// The `data:image/png;base64,…` URI, or `None` when there was no art (or
    /// it failed to encode).
    uri: Option<Arc<str>>,
    /// Palette derived from the same pixels; kept in lockstep with `uri`.
    palette: Option<MediaPalette>,
}

/// The live audio + now-playing source handed to every web wallpaper.
///
/// Cheap to clone-share across outputs (both handles are `Arc`s over
/// worker-published state); one instance per web renderer keeps its own art
/// cache, which is what makes the per-poll cost a pointer comparison.
pub(crate) struct EngineWebFeed {
    /// System-audio spectrum. `None` with `--no-audio-processing` or when no
    /// output wanted capture — the page's audio listener then never fires,
    /// which is exactly what the reference does with audio processing off.
    audio: Option<Arc<AudioCapture>>,
    /// MPRIS now-playing. `None` when no web wallpaper was configured at
    /// launch (nothing else consumes it, so the D-Bus worker is not started).
    media: Option<Arc<MediaSource>>,
    /// See [`ArtCache`]. A `Mutex` rather than a `RefCell` because
    /// [`WebFeed`] is `Send` and takes `&self`.
    art: Mutex<ArtCache>,
}

impl EngineWebFeed {
    /// Wrap the engine's shared sources. Returns `None` when neither exists —
    /// there is nothing to feed, and attaching an always-empty feed would only
    /// cost the renderer a poll it can never use.
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

    /// The cover URI + palette for `state`, encoding only when the art changed.
    ///
    /// A poisoned lock is recovered from rather than propagated: a wallpaper
    /// must not die because some other thread panicked while holding a cache
    /// (SPEC V9), and the worst case is one extra encode.
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

/// Encode the snapshot's art into a `data:` URI and its palette.
///
/// The two are produced (and dropped) together on purpose: [`MediaSnapshot`]
/// documents `palette == None` exactly when `thumbnail == None`, so art that
/// decodes but fails to *encode* must not leave a palette behind describing an
/// image the page never received.
fn encode_art(state: &MediaState) -> (Option<Arc<str>>, Option<MediaPalette>) {
    let Some(art) = state.art.as_ref() else {
        return (None, None);
    };
    let Some(uri) = art.png_data_uri() else {
        return (None, None);
    };
    // `from_state` derives primaryColor/secondaryColor/textColor from the same
    // pixels with the reference's weighting (docs/subsystems-misc.md §3.5), so
    // the palette a page paints with always matches the cover it was sent.
    let event = MediaPlaybackEvent::from_state(state);
    let palette = palette_of(&event);
    (Some(Arc::from(uri.as_str())), palette)
}

/// Collect the four swatches, or `None` if the derivation produced none (which
/// only happens when there was no art — handled by the caller already, so this
/// is the defensive half of the invariant).
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
        // 64 bands is what the reference publishes to pages; `shim::audio_call`
        // mirrors it into the 128 floats (identical left+right) the listener
        // receives. The load is a lock-free arc-swap read (SPEC V4).
        Some(self.audio.as_ref()?.latest_spectrum().audio64.to_vec())
    }

    fn media(&self) -> Option<MediaSnapshot> {
        let state = self.media.as_ref()?.latest();
        // Hand the capture the player's PID so it can record the device that
        // player is actually feeding. Done here because this is the one place
        // holding both handles, and it re-runs as the adopted player changes —
        // a hint set once at startup would go stale the moment the user
        // switched apps.
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
            // The page contract is seconds; MPRIS is microseconds (§5).
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
        Arc::new(AlbumArt {
            width: 2,
            height: 1,
            pixels: vec![color, 20, 30, 255, 0, 0, 0, 255],
        })
    }

    fn feed() -> EngineWebFeed {
        // A disabled media handle is a real handle with an always-empty state;
        // the cache logic under test does not care which.
        EngineWebFeed::new(None, Some(Arc::new(MediaSource::start(MediaConfig::disabled()))))
            .expect("a media-only feed is still a feed")
    }

    #[test]
    fn no_sources_means_no_feed() {
        assert!(EngineWebFeed::new(None, None).is_none());
    }

    /// The same art must not be re-encoded, and different art must be.
    #[test]
    fn art_cache_keys_on_identity() {
        let feed = feed();
        let a = art(200);
        let mut state = MediaState {
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

        // Same `Arc` → same allocation handed back, no re-encode.
        let (uri2, _) = feed.thumbnail(&state);
        assert!(Arc::ptr_eq(&uri1, &uri2.expect("cached")));

        // Different art → a fresh encode.
        state.art = Some(art(10));
        let (uri3, _) = feed.thumbnail(&state);
        assert!(!Arc::ptr_eq(&uri1, &uri3.expect("re-encoded")));

        // No art → both halves cleared together.
        state.art = None;
        assert_eq!(feed.thumbnail(&state), (None, None));
    }

    /// A disabled source still yields a coherent snapshot (V9) — which is the
    /// snapshot that unsticks a page waiting for its first media event.
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
