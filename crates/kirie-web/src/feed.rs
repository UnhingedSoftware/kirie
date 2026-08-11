//! Live audio + now-playing data for web wallpapers, and the rate-limited
//! diffing pump that delivers it.
//!
//! [`crate::shim`] defines *how* a page is spoken to (`window.__wpAudio`,
//! `window.__wpMedia*`, fired into whatever the page registered). This module
//! defines **what** is said and **when**:
//!
//! ```text
//!   engine-owned sources        WebRenderer::poll / ::render
//!   (AudioCapture, MPRIS)   ──►  WebFeed  ──►  FeedPump  ──►  WebBackend
//!        arc-swap reads          (trait)      diff + rate      push_audio /
//!                                              limit           push_media
//! ```
//!
//! The sources themselves are deliberately *not* named here. kirie-web is the
//! browser layer; the system-audio capture (`kirie-audio`) and the MPRIS client
//! (`kirie-render::media`) belong to the engine, and pulling either in would
//! drag the whole scene/video dependency tree into the crate that also builds
//! the standalone `kirie-webviewhost` binary. So the engine implements
//! [`WebFeed`] and hands it over with [`crate::WebRenderer::set_feed`] — the
//! same "app supplies the closure/impl, platform drives it" shape the renderer
//! factories already use (SPEC V1: no globals, state passed explicitly).
//!
//! # Why a pump rather than a push-per-frame
//!
//! The reference engine re-executes the bridge JavaScript every rendered frame
//! (docs/subsystems-misc.md §3.5). kirie cannot: the webview backend is
//! **passive** — webkit paints its own layer-shell window, so the platform
//! stops calling `render` entirely after the first frame
//! ([`kirie_platform::Renderer::is_passive`]) — and its bridge calls cross a
//! process boundary as pipe traffic. [`FeedPump`] therefore owns both halves of
//! the cost control:
//!
//! * **rate limits** — audio at [`AUDIO_INTERVAL`], media at
//!   [`MEDIA_INTERVAL`], so a 60 Hz `render` loop and a 30 Hz poll timer both
//!   produce the same traffic;
//! * **diffs** — every media channel carries its last-sent value and is only
//!   re-sent when it actually changed. That matters most for the thumbnail: it
//!   is a base64 cover image, hundreds of KB, and re-sending it ten times a
//!   second would swamp the very pipe the rest of the protocol uses.
//!
//! # Wire format
//!
//! The out-of-process backends ([`crate::viewhost`], [`crate::hosted`]) do not
//! evaluate JavaScript themselves; they forward to a host over a line-based
//! stdin protocol. [`audio_line`] / [`media_line`] and their parsers define
//! that encoding once, here, so the engine and host sides can never drift.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::WebBackend;
use crate::shim;

/// How often the audio spectrum is pushed to the page (~30 Hz).
///
/// The FFT worker republishes at ~60 Hz and the reference bridge pushes once
/// per rendered frame, so this is half the reference cadence — chosen because
/// the payload is a fixed 128 floats (~1 KB of JavaScript) that, on the
/// out-of-process backends, is written to a pipe from the render thread. At
/// 30 Hz that is ~30 KB/s, low enough to be irrelevant and fast enough that a
/// spectrum visualiser is indistinguishable from a per-frame feed (the bands
/// are already `move_towards`-smoothed, so they change slowly by construction).
pub const AUDIO_INTERVAL: Duration = Duration::from_millis(33);

/// How often the now-playing snapshot is re-examined for changes (~10 Hz).
///
/// Deliberately much slower than audio: the MPRIS worker only re-reads the
/// player once a second ([`MediaConfig::tick`]-driven), so polling faster than
/// this can discover nothing new — it would just burn diffs. 10 Hz keeps the
/// *reaction* to a track change well under the human-perceptible threshold
/// without pretending the source is fresher than it is.
///
/// [`MediaConfig::tick`]: https://docs.rs/kirie-render
pub const MEDIA_INTERVAL: Duration = Duration::from_millis(100);

/// Upper bound on how many audio bands survive the wire encoding.
///
/// The spectrum is 16/32/64 bands by construction, so this only exists to keep
/// a malformed or hostile value from turning one pipe write into an unbounded
/// one (SPEC V9: never trust an externally supplied length).
const MAX_BANDS: usize = 512;

/// Which `wallpaperRegisterMedia*Listener` an update is aimed at.
///
/// WE splits the now-playing event across five independent listeners so a page
/// can subscribe to only what it draws (docs/subsystems-misc.md §3.5). Keeping
/// them separate here is what makes the thumbnail diff worth anything: a
/// position tick must not drag the cover image along with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaChannel {
    /// `wallpaperRegisterMediaStatusListener` — is media integration live at
    /// all (`{enabled}`).
    Status,
    /// `wallpaperRegisterMediaPropertiesListener` — track title/artist/album.
    Properties,
    /// `wallpaperRegisterMediaPlaybackListener` — the playback state integer.
    Playback,
    /// `wallpaperRegisterMediaTimelineListener` — position/duration in seconds.
    Timeline,
    /// `wallpaperRegisterMediaThumbnailListener` — the cover `data:` URI plus
    /// its derived palette.
    Thumbnail,
}

impl MediaChannel {
    /// The short token used on the host protocol line (`media <token> <json>`).
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Properties => "props",
            Self::Playback => "playback",
            Self::Timeline => "timeline",
            Self::Thumbnail => "thumb",
        }
    }

    /// Parse a token produced by [`Self::as_wire`]; `None` for anything else.
    ///
    /// Named `from_wire` rather than `from_str` on purpose: this is a private
    /// protocol token, not a user-facing `FromStr` parse, and an unknown token
    /// is a forward-compatible no-op rather than an error.
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "status" => Some(Self::Status),
            "props" => Some(Self::Properties),
            "playback" => Some(Self::Playback),
            "timeline" => Some(Self::Timeline),
            "thumb" => Some(Self::Thumbnail),
            _ => None,
        }
    }

    /// Build the one-line JavaScript statement that fires this channel's
    /// listeners with `json` (already a single-line JSON object literal).
    #[must_use]
    pub fn call(self, json: &str) -> String {
        match self {
            Self::Status => shim::media_status_call(json),
            Self::Properties => shim::media_properties_call(json),
            Self::Playback => shim::media_playback_call(json),
            Self::Timeline => shim::media_timeline_call(json),
            Self::Thumbnail => shim::media_thumbnail_call(json),
        }
    }
}

/// The `#rrggbb` swatches a page paints its media UI with.
///
/// Derived from the cover art by the engine (`primaryColor` weights every pixel
/// by `saturation × brightness`, docs/subsystems-misc.md §3.5) and passed
/// through verbatim — pages assign these straight into CSS, so they must always
/// be valid colour strings, never empty and never `undefined`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPalette {
    /// Dominant colour of the cover.
    pub primary: String,
    /// `primary × 0.4`, for gradient tails.
    pub secondary: String,
    /// Readable text colour over `primary`.
    pub text: String,
    /// Pure black/white — maximum contrast against `primary`.
    pub high_contrast: String,
}

impl MediaPalette {
    /// The palette sent when there is no decodable cover.
    ///
    /// A page with no art still has to paint *something*, and WE's own
    /// behaviour is to keep delivering the thumbnail event with an empty image
    /// rather than to go silent. Handing over neutral grey keeps every
    /// `css('color', event.textColor)` call in a page valid instead of writing
    /// `undefined` into the stylesheet and losing the element.
    #[must_use]
    pub fn neutral() -> Self {
        Self {
            primary: "#808080".to_owned(),
            secondary: "#333333".to_owned(),
            text: "#ffffff".to_owned(),
            high_contrast: "#ffffff".to_owned(),
        }
    }
}

/// One immutable now-playing snapshot as the page-facing bridge sees it.
///
/// Deliberately a flat, dependency-free mirror of the engine's MPRIS state:
/// seconds instead of microseconds, an integer playback state, and the cover
/// already encoded as a `data:` URI (encoding is the expensive step and belongs
/// to whoever can cache it across ticks — see [`WebFeed::media`]).
#[derive(Debug, Clone)]
pub struct MediaSnapshot {
    /// Whether any MPRIS player is currently adopted. Drives the status
    /// channel; when `false` every other field is at its default.
    pub available: bool,
    /// `xesam:title`.
    pub title: String,
    /// First `xesam:artist`.
    pub artist: String,
    /// `xesam:album`.
    pub album: String,
    /// Playback state integer: `0` stopped, `1` playing, `2` paused
    /// (docs/subsystems-misc.md §5 — these exact integers are the page
    /// contract).
    pub state: i32,
    /// Playback position in seconds.
    pub position_secs: f64,
    /// Track duration in seconds; `0.0` when unknown.
    pub duration_secs: f64,
    /// The cover as a complete `data:image/png;base64,…` URI, or `None` when
    /// the track has no decodable art.
    ///
    /// Shared rather than owned so the pump's "has the cover changed?" test is
    /// a pointer comparison instead of a multi-hundred-KB `memcmp` ten times a
    /// second.
    pub thumbnail: Option<Arc<str>>,
    /// Palette derived from the cover; `None` exactly when `thumbnail` is.
    pub palette: Option<MediaPalette>,
}

impl MediaSnapshot {
    /// The "no player" snapshot — what a page is told when MPRIS has nothing.
    ///
    /// Sending this is the point: a media wallpaper that has received *nothing*
    /// sits on its initial "Loading…" markup forever, whereas one told
    /// `state = 0` renders its idle state.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            available: false,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            state: 0,
            position_secs: 0.0,
            duration_secs: 0.0,
            thumbnail: None,
            palette: None,
        }
    }
}

/// The engine-supplied source of live data for a web wallpaper.
///
/// Both methods are called from the render thread at up to
/// `1 / AUDIO_INTERVAL`, so both must be cheap and non-blocking (SPEC V4) —
/// the intended implementation is a pair of `arc-swap` loads over data
/// published by worker threads. Returning `None` means "this build has no such
/// source", and nothing is pushed at all.
///
/// `Send` because the renderer that owns it may be built on a worker thread
/// before being installed on the render thread.
pub trait WebFeed: Send {
    /// The latest FFT band magnitudes, or `None` when audio processing is off.
    ///
    /// A 64-entry slice is what the reference produces; [`shim::audio_call`]
    /// mirrors it into the 128 floats (identical left+right channels) a page's
    /// audio listener receives.
    fn audio(&self) -> Option<Vec<f32>>;

    /// The latest now-playing snapshot, or `None` when media integration is off.
    ///
    /// The implementation owns the cover→`data:` URI encoding and **must**
    /// cache it: this is called at [`MEDIA_INTERVAL`], and re-encoding a PNG
    /// every 100 ms on the render thread would be a self-inflicted stall.
    fn media(&self) -> Option<MediaSnapshot>;
}

/// What was last delivered to the page, per channel.
///
/// Split by channel rather than kept as a whole snapshot because that is the
/// granularity of the diff: a moving position must not re-send the cover.
#[derive(Debug)]
struct SentMedia {
    available: bool,
    title: String,
    artist: String,
    album: String,
    state: i32,
    position_secs: f64,
    duration_secs: f64,
    thumbnail: Option<Arc<str>>,
    palette: Option<MediaPalette>,
}

/// Rate-limited, diffing delivery of a [`WebFeed`] into a [`WebBackend`].
///
/// Owned by the renderer (one per web wallpaper) and driven from both
/// `Renderer::render` (composited backends) and `Renderer::poll` (passive
/// backends), which is exactly why the pacing lives here and not in either
/// caller: whichever one runs, the page sees the same cadence.
#[derive(Debug, Default)]
pub struct FeedPump {
    /// When the last audio frame went out; `None` = nothing sent yet.
    audio_at: Option<Instant>,
    /// When the media snapshot was last examined.
    media_at: Option<Instant>,
    /// On-battery flag (the engine's power watcher): while set, both
    /// intervals double — the listener still fires (pages use it as a
    /// clock), just at half the rate.
    power_save: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Last values delivered per channel; `None` = the page has been told
    /// nothing, so the next pump sends every channel.
    sent: Option<SentMedia>,
}

impl FeedPump {
    /// Wire the engine's power-save flag (see the field doc).
    pub fn set_power_save(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.power_save = Some(flag);
    }

    /// The current interval scale: 2 in power-save, 1 otherwise.
    fn scale(&self) -> u32 {
        if self
            .power_save
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
        {
            2
        } else {
            1
        }
    }

    /// A pump that has delivered nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver whatever is due and changed. Cheap when nothing is.
    ///
    /// Safe to call at any rate — the two interval checks collapse a 144 Hz
    /// render loop and a 30 Hz poll timer onto the same output cadence.
    pub fn pump(&mut self, feed: &dyn WebFeed, backend: &mut dyn WebBackend) {
        let now = Instant::now();
        self.pump_audio(feed, backend, now);
        self.pump_media(feed, backend, now);
    }

    fn pump_audio(&mut self, feed: &dyn WebFeed, backend: &mut dyn WebBackend, now: Instant) {
        if self
            .audio_at
            .is_some_and(|at| now.duration_since(at) < AUDIO_INTERVAL * self.scale())
        {
            return;
        }
        let Some(bands) = feed.audio() else {
            return;
        };
        // Unconditional — deliberately *not* diffed the way media is. Two
        // reasons, and the first one bit during bring-up:
        //
        // 1. Pages use the audio listener as a clock. The workshop media
        //    players drive their whole animation off it (scaling the cover,
        //    rotating a gradient), so a listener that stops firing when the
        //    spectrum happens to repeat freezes the wallpaper rather than
        //    saving anything. The reference pushes the bands every rendered
        //    frame for exactly this reason (docs/subsystems-misc.md §3.5).
        // 2. Delivery is best-effort below this point: the webview host drops
        //    spectrum frames that arrive before the page's first paint (they
        //    are worthless by the time it commits). A "skip if identical to the
        //    last one I *sent*" rule cannot see that drop, so a silent desktop
        //    with a slow-loading page would suppress every frame after one that
        //    was never delivered — the listener would then never fire at all.
        //
        // The cost of not diffing is one ~1 KB JS call every AUDIO_INTERVAL.
        backend.push_audio(&bands);
        self.audio_at = Some(now);
    }

    fn pump_media(&mut self, feed: &dyn WebFeed, backend: &mut dyn WebBackend, now: Instant) {
        if self
            .media_at
            .is_some_and(|at| now.duration_since(at) < MEDIA_INTERVAL * self.scale())
        {
            return;
        }
        self.media_at = Some(now);
        let Some(snap) = feed.media() else {
            return;
        };

        // First delivery: the page has heard nothing, so every channel goes out
        // even if the snapshot is the empty one. That empty delivery is what
        // moves a media wallpaper off its "Loading…" placeholder when no player
        // is running.
        let first = self.sent.is_none();

        if first || self.sent.as_ref().is_some_and(|s| s.available != snap.available) {
            backend.push_media(MediaChannel::Status, &status_json(snap.available));
        }
        if first
            || self
                .sent
                .as_ref()
                .is_some_and(|s| s.title != snap.title || s.artist != snap.artist || s.album != snap.album)
        {
            backend.push_media(MediaChannel::Properties, &properties_json(&snap));
        }
        if first || self.sent.as_ref().is_some_and(|s| s.state != snap.state) {
            backend.push_media(MediaChannel::Playback, &playback_json(snap.state));
        }
        if first
            || self.sent.as_ref().is_some_and(|s| {
                s.position_secs != snap.position_secs || s.duration_secs != snap.duration_secs
            })
        {
            backend.push_media(
                MediaChannel::Timeline,
                &timeline_json(snap.position_secs, snap.duration_secs),
            );
        }
        // The expensive one, hence the strictest gate: identity first, contents
        // only as a fallback for a feed that rebuilt an equal URI.
        if first
            || self
                .sent
                .as_ref()
                .is_some_and(|s| !same_thumbnail(&s.thumbnail, &snap.thumbnail) || s.palette != snap.palette)
        {
            let palette = snap.palette.clone().unwrap_or_else(MediaPalette::neutral);
            // WE keeps the field present and empty when there is no cover; the
            // popular workshop media players test for exactly this string
            // rather than for a missing property.
            let uri = snap.thumbnail.as_deref().unwrap_or("data:image/png;base64,");
            backend.push_media(MediaChannel::Thumbnail, &thumbnail_json(uri, &palette));
        }

        self.sent = Some(SentMedia {
            available: snap.available,
            title: snap.title,
            artist: snap.artist,
            album: snap.album,
            state: snap.state,
            position_secs: snap.position_secs,
            duration_secs: snap.duration_secs,
            thumbnail: snap.thumbnail,
            palette: snap.palette,
        });
    }
}

/// Cover-change test: pointer identity first (the expected hit — a caching feed
/// hands back the same `Arc` until the art URL changes), contents as the
/// fallback.
fn same_thumbnail(a: &Option<Arc<str>>, b: &Option<Arc<str>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y) || x == y,
        _ => false,
    }
}

/// `{"enabled":true}` — the media-status event.
#[must_use]
pub fn status_json(enabled: bool) -> String {
    format!("{{\"enabled\":{enabled}}}")
}

/// `{"state":1}` — the playback event (`0` stopped / `1` playing / `2` paused).
#[must_use]
pub fn playback_json(state: i32) -> String {
    format!("{{\"state\":{state}}}")
}

/// `{"position":12.34,"duration":210.00}` — the timeline event, in seconds.
#[must_use]
pub fn timeline_json(position: f64, duration: f64) -> String {
    format!(
        "{{\"position\":{},\"duration\":{}}}",
        json_number(position),
        json_number(duration)
    )
}

/// The media-properties event.
///
/// `title` / `artist` / `album` are what kirie's MPRIS source can actually
/// answer (docs/subsystems-misc.md §5 lists the exact `xesam:` keys read).
/// `albumTitle` / `albumArtist` / `subTitle` / `genres` / `contentType` are
/// carried alongside because that is the property set the reference's own
/// listener documents, and a page reading `event.genres.join()` would throw on
/// a missing field rather than degrade. Every one of them is filled from real
/// data or an honest empty, never invented.
#[must_use]
pub fn properties_json(snap: &MediaSnapshot) -> String {
    let mut out = String::with_capacity(160 + snap.title.len() + snap.artist.len() + snap.album.len());
    out.push_str("{\"title\":");
    push_json_string(&snap.title, &mut out);
    out.push_str(",\"artist\":");
    push_json_string(&snap.artist, &mut out);
    out.push_str(",\"album\":");
    push_json_string(&snap.album, &mut out);
    // WE spellings for the same two values, so a page written against either
    // naming finds them.
    out.push_str(",\"albumTitle\":");
    push_json_string(&snap.album, &mut out);
    out.push_str(",\"albumArtist\":");
    push_json_string(&snap.artist, &mut out);
    // MPRIS has no equivalent of these; empty is the truthful answer.
    out.push_str(",\"subTitle\":\"\",\"genres\":[],\"contentType\":\"music\"}");
    out
}

/// The media-thumbnail event: the cover `data:` URI plus its palette.
#[must_use]
pub fn thumbnail_json(data_uri: &str, palette: &MediaPalette) -> String {
    let mut out = String::with_capacity(data_uri.len() + 160);
    out.push_str("{\"thumbnail\":");
    push_json_string(data_uri, &mut out);
    out.push_str(",\"primaryColor\":");
    push_json_string(&palette.primary, &mut out);
    out.push_str(",\"secondaryColor\":");
    push_json_string(&palette.secondary, &mut out);
    out.push_str(",\"textColor\":");
    push_json_string(&palette.text, &mut out);
    out.push_str(",\"highContrastColor\":");
    push_json_string(&palette.high_contrast, &mut out);
    out.push('}');
    out
}

/// Append `value` as a quoted, escaped JSON string.
///
/// Hand-rolled because kirie-web carries no serde (the crate also builds the
/// standalone host binaries and is kept dependency-light on purpose). Control
/// characters are escaped rather than dropped, which is what guarantees the
/// result is **single-line** — the host protocol is line-based, so a raw
/// newline inside a track title would otherwise split one command into two.
fn push_json_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below 0x20 (and U+007F, which some taggers leave in)
            // must not reach the JS parser raw.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Format a float for JSON, mapping non-finite values to `0` (V9: a NaN
/// duration from a broken player must not emit invalid JSON that kills the
/// whole bridge call).
fn json_number(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.3}")
    } else {
        "0".to_owned()
    }
}

/// Encode one audio frame as the host protocol's `audio …` line.
///
/// Space-separated fixed-point rather than JSON: the host side has no JSON
/// parser (and should not grow one for 64 numbers), whereas
/// `split_whitespace().parse()` is exact and allocation-light. Four decimals is
/// the same precision the reference formats the bands with (`"%.4f"`,
/// docs/subsystems-misc.md §1.3).
#[must_use]
pub fn audio_line(bands: &[f32]) -> String {
    let bands = &bands[..bands.len().min(MAX_BANDS)];
    let mut out = String::with_capacity(6 + bands.len() * 7);
    out.push_str("audio");
    for b in bands {
        let b = if b.is_finite() { *b } else { 0.0 };
        out.push(' ');
        out.push_str(&format!("{b:.4}"));
    }
    out
}

/// Decode the payload of an `audio …` line (everything after the keyword).
///
/// Unparsable tokens are skipped rather than failing the whole frame: a partial
/// spectrum is still a usable one, and a host must never die on a malformed
/// line (SPEC V9).
#[must_use]
pub fn parse_audio_bands(payload: &str) -> Vec<f32> {
    payload
        .split_whitespace()
        .filter_map(|t| t.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .take(MAX_BANDS)
        .collect()
}

/// Encode one media update as the host protocol's `media <channel> <json>`
/// line.
///
/// Returns `None` when `json` is not single-line, which would corrupt the
/// protocol. By construction it always is (see [`push_json_string`]); the check
/// is the belt to that braces.
#[must_use]
pub fn media_line(channel: MediaChannel, json: &str) -> Option<String> {
    if json.contains('\n') || json.contains('\r') {
        return None;
    }
    Some(format!("media {} {json}", channel.as_wire()))
}

/// Split the payload of a `media …` line into its channel and JSON halves.
#[must_use]
pub fn parse_media_payload(payload: &str) -> Option<(MediaChannel, &str)> {
    let (token, json) = payload.split_once(' ')?;
    Some((MediaChannel::from_wire(token)?, json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records everything a pump pushed, so the diffing can be asserted on.
    #[derive(Default)]
    struct RecordingBackend {
        audio: usize,
        media: Vec<(MediaChannel, String)>,
    }

    impl crate::backend::WebBackend for RecordingBackend {
        fn new(_url: &str, _size: crate::backend::WebSize) -> Result<Self, crate::backend::WebError> {
            Ok(Self::default())
        }
        fn tick(&mut self, _dt: f32) {}
        fn latest_frame(&self) -> Option<crate::backend::WebFrameRef<'_>> {
            None
        }
        fn resize(&mut self, _size: crate::backend::WebSize) {}
        fn send_pointer(&mut self, _pointer: crate::backend::PointerState) {}
        fn set_muted(&mut self, _muted: bool) {}
        fn push_audio(&mut self, _bands: &[f32]) {
            self.audio += 1;
        }
        fn push_media(&mut self, channel: MediaChannel, json: &str) {
            self.media.push((channel, json.to_owned()));
        }
        fn shutdown(&mut self) {}
    }

    struct StubFeed {
        audio: Option<Vec<f32>>,
        media: RefCell<Option<MediaSnapshot>>,
    }

    // SAFETY-free: the stub is only ever touched from the test thread; `Send`
    // is required by the trait bound, and `RefCell` is `Send` when its contents
    // are.
    impl WebFeed for StubFeed {
        fn audio(&self) -> Option<Vec<f32>> {
            self.audio.clone()
        }
        fn media(&self) -> Option<MediaSnapshot> {
            self.media.borrow().clone()
        }
    }

    fn snapshot(title: &str, state: i32, pos: f64) -> MediaSnapshot {
        MediaSnapshot {
            available: true,
            title: title.to_owned(),
            artist: "A".to_owned(),
            album: "B".to_owned(),
            state,
            position_secs: pos,
            duration_secs: 100.0,
            thumbnail: Some(Arc::from("data:image/png;base64,AAA")),
            palette: Some(MediaPalette::neutral()),
        }
    }

    #[test]
    fn first_pump_sends_every_media_channel() {
        let feed = StubFeed {
            audio: None,
            media: RefCell::new(Some(snapshot("t", 1, 0.0))),
        };
        let mut backend = RecordingBackend::default();
        let mut pump = FeedPump::new();
        pump.pump(&feed, &mut backend);
        let channels: Vec<_> = backend.media.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            channels,
            vec![
                MediaChannel::Status,
                MediaChannel::Properties,
                MediaChannel::Playback,
                MediaChannel::Timeline,
                MediaChannel::Thumbnail,
            ]
        );
    }

    /// The empty snapshot is still delivered — that is what unsticks a page
    /// waiting on its first media event.
    #[test]
    fn empty_snapshot_is_still_delivered() {
        let feed = StubFeed {
            audio: None,
            media: RefCell::new(Some(MediaSnapshot::empty())),
        };
        let mut backend = RecordingBackend::default();
        FeedPump::new().pump(&feed, &mut backend);
        assert_eq!(backend.media.len(), 5);
        assert!(backend.media[0].1.contains("\"enabled\":false"));
        assert!(
            backend.media[4]
                .1
                .contains("\"thumbnail\":\"data:image/png;base64,\"")
        );
    }

    #[test]
    fn unchanged_media_sends_nothing_more() {
        let feed = StubFeed {
            audio: None,
            media: RefCell::new(Some(snapshot("t", 1, 0.0))),
        };
        let mut backend = RecordingBackend::default();
        let mut pump = FeedPump::new();
        pump.pump(&feed, &mut backend);
        let after_first = backend.media.len();
        // Defeat the interval so the second pump really examines the snapshot.
        pump.media_at = None;
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.media.len(), after_first, "an unchanged snapshot re-sent");
    }

    /// A moving position must not drag the (large) thumbnail along with it.
    #[test]
    fn position_change_sends_only_the_timeline() {
        let feed = StubFeed {
            audio: None,
            media: RefCell::new(Some(snapshot("t", 1, 0.0))),
        };
        let mut backend = RecordingBackend::default();
        let mut pump = FeedPump::new();
        pump.pump(&feed, &mut backend);
        backend.media.clear();

        *feed.media.borrow_mut() = Some(snapshot("t", 1, 5.0));
        pump.media_at = None;
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.media.len(), 1);
        assert_eq!(backend.media[0].0, MediaChannel::Timeline);
        assert!(backend.media[0].1.contains("\"position\":5.000"));
    }

    /// Audio is rate-limited but never suppressed: pages clock their animation
    /// off the listener, so a silent spectrum must keep arriving.
    #[test]
    fn audio_is_rate_limited_not_diffed() {
        let feed = StubFeed {
            audio: Some(vec![0.0; 64]),
            media: RefCell::new(None),
        };
        let mut backend = RecordingBackend::default();
        let mut pump = FeedPump::new();
        pump.pump(&feed, &mut backend);
        // Immediately again: inside the interval, so nothing more goes out.
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.audio, 1);
        // Interval elapsed: the identical silent frame is sent again.
        pump.audio_at = None;
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.audio, 2);
    }

    /// No audio source at all means no calls — a build with
    /// `--no-audio-processing` leaves the listener silent rather than feeding
    /// it fabricated zeroes forever.
    #[test]
    fn absent_audio_source_pushes_nothing() {
        let feed = StubFeed {
            audio: None,
            media: RefCell::new(None),
        };
        let mut backend = RecordingBackend::default();
        FeedPump::new().pump(&feed, &mut backend);
        assert_eq!(backend.audio, 0);
    }

    #[test]
    fn json_strings_stay_single_line_and_escaped() {
        let mut out = String::new();
        push_json_string("a\"b\\c\nd\te\u{1}", &mut out);
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\te\\u0001\"");
        assert!(!out.contains('\n'));
    }

    #[test]
    fn non_finite_numbers_never_emit_invalid_json() {
        let js = timeline_json(f64::NAN, f64::INFINITY);
        assert_eq!(js, "{\"position\":0,\"duration\":0}");
    }

    #[test]
    fn audio_line_round_trips() {
        let bands: Vec<f32> = (0..64).map(|i| i as f32 / 64.0).collect();
        let line = audio_line(&bands);
        let payload = line.strip_prefix("audio ").expect("keyword");
        let back = parse_audio_bands(payload);
        assert_eq!(back.len(), 64);
        assert!((back[32] - bands[32]).abs() < 1e-4);
    }

    #[test]
    fn audio_line_survives_non_finite_and_overlong_input() {
        let line = audio_line(&[f32::NAN, f32::INFINITY, 0.5]);
        assert_eq!(line, "audio 0.0000 0.0000 0.5000");
        let huge = vec![1.0f32; MAX_BANDS * 2];
        assert_eq!(parse_audio_bands(&audio_line(&huge)[6..]).len(), MAX_BANDS);
    }

    #[test]
    fn media_line_round_trips_and_rejects_multiline() {
        let line = media_line(MediaChannel::Thumbnail, "{\"a\":1}").expect("encoded");
        assert_eq!(line, "media thumb {\"a\":1}");
        let payload = line.strip_prefix("media ").expect("keyword");
        assert_eq!(
            parse_media_payload(payload),
            Some((MediaChannel::Thumbnail, "{\"a\":1}"))
        );
        assert!(media_line(MediaChannel::Status, "{\n}").is_none());
        assert!(parse_media_payload("nosuch {}").is_none());
    }

    /// A large cover survives the encoding intact — no truncation anywhere in
    /// the line protocol (the whole reason the thumbnail is diffed rather than
    /// chunked).
    #[test]
    fn large_thumbnail_line_is_not_truncated() {
        let uri = format!("data:image/png;base64,{}", "A".repeat(600_000));
        let json = thumbnail_json(&uri, &MediaPalette::neutral());
        let line = media_line(MediaChannel::Thumbnail, &json).expect("encoded");
        let (channel, back) =
            parse_media_payload(line.strip_prefix("media ").expect("keyword")).expect("parsed");
        assert_eq!(channel, MediaChannel::Thumbnail);
        assert_eq!(back, json);
        assert!(back.len() > 600_000);
        assert!(!back.contains('\n'));
    }
}
