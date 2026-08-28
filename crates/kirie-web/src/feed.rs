use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backend::WebBackend;
use crate::shim;

pub const AUDIO_INTERVAL: Duration = Duration::from_millis(33);

pub const MEDIA_INTERVAL: Duration = Duration::from_millis(100);

const MAX_BANDS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaChannel {
    Status,
    Properties,
    Playback,
    Timeline,
    Thumbnail,
}

impl MediaChannel {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPalette {
    pub primary: String,
    pub secondary: String,
    pub text: String,
    pub high_contrast: String,
}

impl MediaPalette {
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

#[derive(Debug, Clone)]
pub struct MediaSnapshot {
    pub available: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub state: i32,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub thumbnail: Option<Arc<str>>,
    pub palette: Option<MediaPalette>,
}

impl MediaSnapshot {
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

pub trait WebFeed: Send {
    fn audio(&self) -> Option<Vec<f32>>;

    fn media(&self) -> Option<MediaSnapshot>;
}

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

#[derive(Debug, Default)]
pub struct FeedPump {
    audio_at: Option<Instant>,
    media_at: Option<Instant>,
    power_save: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    sent: Option<SentMedia>,
}

impl FeedPump {
    pub fn set_power_save(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.power_save = Some(flag);
    }

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

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

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
        if first
            || self
                .sent
                .as_ref()
                .is_some_and(|s| !same_thumbnail(&s.thumbnail, &snap.thumbnail) || s.palette != snap.palette)
        {
            let palette = snap.palette.clone().unwrap_or_else(MediaPalette::neutral);
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

fn same_thumbnail(a: &Option<Arc<str>>, b: &Option<Arc<str>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y) || x == y,
        _ => false,
    }
}

#[must_use]
pub fn status_json(enabled: bool) -> String {
    format!("{{\"enabled\":{enabled}}}")
}

#[must_use]
pub fn playback_json(state: i32) -> String {
    format!("{{\"state\":{state}}}")
}

#[must_use]
pub fn timeline_json(position: f64, duration: f64) -> String {
    format!(
        "{{\"position\":{},\"duration\":{}}}",
        json_number(position),
        json_number(duration)
    )
}

#[must_use]
pub fn properties_json(snap: &MediaSnapshot) -> String {
    let mut out = String::with_capacity(160 + snap.title.len() + snap.artist.len() + snap.album.len());
    out.push_str("{\"title\":");
    push_json_string(&snap.title, &mut out);
    out.push_str(",\"artist\":");
    push_json_string(&snap.artist, &mut out);
    out.push_str(",\"album\":");
    push_json_string(&snap.album, &mut out);
    out.push_str(",\"albumTitle\":");
    push_json_string(&snap.album, &mut out);
    out.push_str(",\"albumArtist\":");
    push_json_string(&snap.artist, &mut out);
    out.push_str(",\"subTitle\":\"\",\"genres\":[],\"contentType\":\"music\"}");
    out
}

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

fn push_json_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_number(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.3}")
    } else {
        "0".to_owned()
    }
}

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

#[must_use]
pub fn parse_audio_bands(payload: &str) -> Vec<f32> {
    payload
        .split_whitespace()
        .filter_map(|t| t.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .take(MAX_BANDS)
        .collect()
}

#[must_use]
pub fn media_line(channel: MediaChannel, json: &str) -> Option<String> {
    if json.contains('\n') || json.contains('\r') {
        return None;
    }
    Some(format!("media {} {json}", channel.as_wire()))
}

#[must_use]
pub fn parse_media_payload(payload: &str) -> Option<(MediaChannel, &str)> {
    let (token, json) = payload.split_once(' ')?;
    Some((MediaChannel::from_wire(token)?, json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
        pump.media_at = None;
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.media.len(), after_first, "an unchanged snapshot re-sent");
    }

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

    #[test]
    fn audio_is_rate_limited_not_diffed() {
        let feed = StubFeed {
            audio: Some(vec![0.0; 64]),
            media: RefCell::new(None),
        };
        let mut backend = RecordingBackend::default();
        let mut pump = FeedPump::new();
        pump.pump(&feed, &mut backend);
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.audio, 1);
        pump.audio_at = None;
        pump.pump(&feed, &mut backend);
        assert_eq!(backend.audio, 2);
    }

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
