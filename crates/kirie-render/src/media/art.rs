//! Album-art decoding and the script/web-facing playback event.
//!
//! MPRIS delivers cover art as `mpris:artUrl`. The C++ reference resolves local
//! art (`file://` URL or a bare absolute path) into pixels a scene shader can
//! sample (the `$mediaThumbnail` virtual asset) and, for web wallpapers, into a
//! `data:` URL plus a computed palette (docs/subsystems-misc.md §3.5, §5,
//! `CWeb.cpp:64-152`). This module decodes local + `data:` art into an
//! [`AlbumArt`] RGBA image and derives that palette in [`MediaPlaybackEvent`].
//!
//! Remote (`http(s)://`) art is fetched via `curl`, as the reference does — a
//! streaming player publishes nothing else, so refusing would leave every such
//! wallpaper with an empty cover. The fetch runs on the MPRIS worker thread and
//! is cached by URL, so it costs one request per track change and never blocks
//! the render thread.

/// Longest edge a [`AlbumArt::png_data_uri`] thumbnail is downscaled to.
///
/// The data URI is delivered to a web wallpaper **inline**, as a base64 string
/// spliced into one `window.__wpMediaThumb({...})` JavaScript statement, and on
/// the out-of-process backends that statement's JSON crosses a line-based pipe
/// to the host. Album art off MPRIS is routinely 1000px+ (Spotify hands out
/// 640px, local files whatever the tagger embedded), which re-encodes to
/// megabytes of PNG and ~1.37× that as base64 — per track change, on the render
/// thread. 512 keeps a cover sharp on any realistic on-screen thumbnail (WE's
/// own media wallpapers draw them a few hundred pixels wide) while bounding the
/// payload to a few hundred KB.
pub const MAX_THUMBNAIL_EDGE: u32 = 512;

/// A decoded album-art image (RGBA8, row-major, top-left origin).
///
/// Cheap to share across snapshots via `Arc` — the render/script side binds
/// [`pixels`](Self::pixels) as a texture without re-decoding (SPEC §V4/§V5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumArt {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 pixels (`width * height * 4` bytes).
    pub pixels: Vec<u8>,
}

impl AlbumArt {
    /// Build from an already-decoded [`image::RgbaImage`].
    #[must_use]
    fn from_rgba(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self {
            width,
            height,
            pixels: img.into_raw(),
        }
    }

    /// Saturation·brightness-weighted dominant color, lifted out of darkness,
    /// matching the C++ `primaryColor` derivation (docs/subsystems-misc.md
    /// §3.5, `CWeb.cpp:64-152`): each pixel contributes with weight
    /// `saturation * brightness`; if the result's max channel is `< 170` it is
    /// scaled up so the max channel reaches 170 (avoids a near-black swatch).
    #[must_use]
    pub fn primary_color(&self) -> [u8; 3] {
        let mut acc = [0.0f64; 3];
        let mut weight_sum = 0.0f64;

        // Subsample large images so palette extraction stays O(1)-ish and never
        // stalls the worker: cap at ~64x64 sampled pixels.
        let step_x = (self.width / 64).max(1);
        let step_y = (self.height / 64).max(1);

        let mut y = 0;
        while y < self.height {
            let mut x = 0;
            while x < self.width {
                let idx = ((y * self.width + x) * 4) as usize;
                let r = self.pixels[idx] as f64;
                let g = self.pixels[idx + 1] as f64;
                let b = self.pixels[idx + 2] as f64;
                let a = self.pixels[idx + 3] as f64 / 255.0;
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                let brightness = max / 255.0;
                let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
                let weight = saturation * brightness * a;
                acc[0] += r * weight;
                acc[1] += g * weight;
                acc[2] += b * weight;
                weight_sum += weight;
                x += step_x;
            }
            y += step_y;
        }

        if weight_sum <= f64::EPSILON {
            // Fully desaturated (grayscale) art: fall back to mid-gray.
            return [128, 128, 128];
        }

        let mut color = [
            (acc[0] / weight_sum).round(),
            (acc[1] / weight_sum).round(),
            (acc[2] / weight_sum).round(),
        ];
        // Lift dark colors so the max channel reaches at least 170.
        let max = color[0].max(color[1]).max(color[2]);
        if max > 0.0 && max < 170.0 {
            let scale = 170.0 / max;
            for c in &mut color {
                *c = (*c * scale).min(255.0);
            }
        }
        [color[0] as u8, color[1] as u8, color[2] as u8]
    }

    /// Encode this art as a `data:image/png;base64,…` URI, the exact shape a WE
    /// web wallpaper expects in `event.thumbnail`.
    ///
    /// The reference hands pages the cover as a base64 `data:` URL
    /// (docs/subsystems-misc.md §3.5, `CWeb.cpp:64-152`) and real workshop
    /// wallpapers depend on the `image/png` spelling: the popular media players
    /// strip the literal `"data:image/png;base64,"` prefix off `event.thumbnail`
    /// to test for "no cover", and assign the whole string to an `<img>` `src`.
    /// PNG (not JPEG) is therefore part of the contract, not a choice.
    ///
    /// Downscaled to [`MAX_THUMBNAIL_EDGE`] on the longest edge first — see that
    /// constant for why the size bound is load-bearing rather than cosmetic.
    /// Returns `None` only if the pixel buffer is inconsistent with its declared
    /// size or the encoder fails (V9: never panics on odd art).
    #[must_use]
    pub fn png_data_uri(&self) -> Option<String> {
        let img = image::RgbaImage::from_raw(self.width, self.height, self.pixels.clone())?;
        let longest = self.width.max(self.height);
        let img = if longest > MAX_THUMBNAIL_EDGE {
            // Triangle: cheap, and a smooth downscale matters here because the
            // page usually scales the thumbnail up again for a blurred
            // background layer.
            let scale = f64::from(MAX_THUMBNAIL_EDGE) / f64::from(longest);
            let w = ((f64::from(self.width) * scale).round() as u32).max(1);
            let h = ((f64::from(self.height) * scale).round() as u32).max(1);
            image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
        } else {
            img
        };

        let mut buf = std::io::Cursor::new(Vec::new());
        if let Err(e) = image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png) {
            tracing::debug!(error = %e, "album art PNG encode failed; page gets no thumbnail");
            return None;
        }
        let mut uri = String::from("data:image/png;base64,");
        base64_encode_into(&buf.into_inner(), &mut uri);
        Some(uri)
    }
}

/// Standard-alphabet, padded base64 encoder appending into `out`.
///
/// Hand-rolled for the same reason [`base64_decode`] is: the workspace carries
/// no base64 crate, and this is the one direction the web bridge needs (an
/// album cover on its way into a `data:` URI). Appending rather than returning
/// lets the caller pre-seed the `data:` prefix and grow one allocation.
fn base64_encode_into(data: &[u8], out: &mut String) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.reserve(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
}

/// The script/web-facing now-playing event.
///
/// This is the aggregate a `wallpaperRegisterMedia*Listener` callback receives
/// (docs/subsystems-misc.md §3.5): typed props, an integer playback state, a
/// seconds-based timeline, and — when art is available — a thumbnail plus a
/// derived `#rrggbb` palette. Built from a [`super::MediaState`] snapshot via
/// [`super::MediaState`]'s data with [`MediaPlaybackEvent::from_state`].
#[derive(Clone, Debug, PartialEq)]
pub struct MediaPlaybackEvent {
    /// Whether a player is present (mirrors `MediaState::available`).
    pub available: bool,
    /// `xesam:title`.
    pub title: String,
    /// First `xesam:artist`.
    pub artist: String,
    /// `xesam:album`.
    pub album: String,
    /// Playback state integer (`0` Stopped / `1` Playing / `2` Paused) — the
    /// `__wpMediaPlayback` contract.
    pub state: i32,
    /// Playback position in seconds (`__wpMediaTimeline.position`).
    pub position_secs: f64,
    /// Track duration in seconds (`__wpMediaTimeline.duration`), `0.0` unknown.
    pub duration_secs: f64,
    /// Decoded album art, when local/`data:` art was loadable.
    pub thumbnail: Option<std::sync::Arc<AlbumArt>>,
    /// The raw `mpris:artUrl` (useful for remote art the renderer may fetch).
    pub art_url: Option<String>,
    /// Dominant color `#rrggbb` (`__wpMediaThumb.primaryColor`), or `None` when
    /// no art is decoded.
    pub primary_color: Option<String>,
    /// `primary × 0.4` `#rrggbb` (`secondaryColor`), or `None`.
    pub secondary_color: Option<String>,
    /// `#ffffff` when the primary's luma `< 150`, else `#101010`
    /// (`textColor`), or `None` when no art is decoded.
    pub text_color: Option<String>,
    /// Pure `#ffffff`/`#000000` — whichever contrasts the primary hardest
    /// (`highContrastColor` in the WE thumbnail event). Distinct from
    /// [`text_color`](Self::text_color), which softens its dark end to
    /// `#101010`; this one never does, so a page can use it for hairlines and
    /// focus rings that must stay legible. `None` when no art is decoded.
    pub high_contrast_color: Option<String>,
}

impl MediaPlaybackEvent {
    /// Project a [`super::MediaState`] snapshot into the page/script event,
    /// converting µs → seconds and deriving the palette from decoded art
    /// (docs/subsystems-misc.md §3.5).
    #[must_use]
    pub fn from_state(state: &super::MediaState) -> Self {
        let (primary, secondary, text, contrast) = match &state.art {
            Some(art) => {
                let p = art.primary_color();
                let sec = [
                    (f64::from(p[0]) * 0.4) as u8,
                    (f64::from(p[1]) * 0.4) as u8,
                    (f64::from(p[2]) * 0.4) as u8,
                ];
                // Rec.601 luma.
                let luma = 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
                let text = if luma < 150.0 {
                    [255, 255, 255]
                } else {
                    [0x10, 0x10, 0x10]
                };
                // Same split point, but never softened: this is the "always
                // legible" swatch, so it stays at the extremes.
                let contrast = if luma < 150.0 { [255, 255, 255] } else { [0, 0, 0] };
                (Some(hex(p)), Some(hex(sec)), Some(hex(text)), Some(hex(contrast)))
            }
            None => (None, None, None, None),
        };

        Self {
            available: state.available,
            title: state.metadata.title.clone(),
            artist: state.metadata.artist.clone(),
            album: state.metadata.album.clone(),
            state: state.playback.as_i32(),
            position_secs: state.position_secs(),
            duration_secs: state.duration_secs(),
            thumbnail: state.art.clone(),
            art_url: state.metadata.art_url.clone(),
            primary_color: primary,
            secondary_color: secondary,
            text_color: text,
            high_contrast_color: contrast,
        }
    }
}

/// Format an RGB triple as `#rrggbb`.
#[must_use]
fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Resolve an `mpris:artUrl` into decoded [`AlbumArt`].
///
/// Handles `file://` URLs (percent-decoded), bare absolute paths, and
/// `data:...;base64,...` URIs. `http(s)://` and anything unrecognized return
/// `None` (never fetched here — the worker must not block on the network, V4).
/// Decode failures also return `None` (V9: no panic on a broken/oversized
/// image).
#[must_use]
pub fn load_art(url: &str) -> Option<AlbumArt> {
    if let Some(rest) = url.strip_prefix("data:") {
        return load_data_uri(rest);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return load_remote(url);
    }
    let path = if let Some(rest) = url.strip_prefix("file://") {
        percent_decode(rest)
    } else if url.starts_with('/') {
        // Bare absolute path (some players emit this).
        url.to_owned()
    } else {
        // Unknown scheme — nothing safe to open.
        return None;
    };

    match image::open(&path) {
        Ok(img) => Some(AlbumArt::from_rgba(img.to_rgba8())),
        Err(e) => {
            tracing::debug!(path = %path, error = %e, "album art decode failed");
            None
        }
    }
}

/// Longest a remote fetch may take before the cover is given up on.
const REMOTE_TIMEOUT_SECS: &str = "6";
/// Hard ceiling on a downloaded cover, handed to curl so an oversized body is
/// refused rather than streamed into memory and discarded.
const REMOTE_MAX_BYTES: &str = "16777216";

/// Fetch cover art served over HTTP(S) and decode it.
///
/// Every streaming player publishes `mpris:artUrl` as a remote URL — Spotify
/// only ever does — so refusing to fetch means those wallpapers show an empty
/// cover no matter what else works. The reference solves this the same way,
/// by shelling out to `curl` (docs/subsystems-misc.md §5); doing likewise keeps
/// a TLS stack out of a wallpaper renderer, and a machine without curl simply
/// gets the behaviour it has today: no cover, nothing else disturbed.
///
/// This runs on the MPRIS worker thread, never the render thread, and the whole
/// result is cached by URL upstream, so a track change costs one fetch.
///
/// The URL comes from whichever media player happens to be running, so it is
/// constrained rather than trusted: the scheme is pinned to http/https on both
/// the initial request and any redirect, redirects are bounded, and the
/// response is capped in both time and size.
fn load_remote(url: &str) -> Option<AlbumArt> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--max-redirs",
            "3",
            // Pin the scheme on the first request *and* every redirect, so a
            // redirect cannot walk the fetch onto file:// or another protocol.
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-time",
            REMOTE_TIMEOUT_SECS,
            "--max-filesize",
            REMOTE_MAX_BYTES,
            "--url",
        ])
        .arg(url)
        .stdin(std::process::Stdio::null())
        .output();

    let output = match output {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!(error = %e, "cover art fetch needs curl; page gets no thumbnail");
            return None;
        }
    };
    if !output.status.success() {
        tracing::debug!(
            url = %url,
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "cover art fetch failed"
        );
        return None;
    }

    match image::load_from_memory(&output.stdout) {
        Ok(img) => Some(AlbumArt::from_rgba(img.to_rgba8())),
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "remote album art decode failed");
            None
        }
    }
}

/// Decode the body of a `data:[<mime>][;base64],<data>` URI (base64 only).
fn load_data_uri(rest: &str) -> Option<AlbumArt> {
    let comma = rest.find(',')?;
    let (meta, data) = rest.split_at(comma);
    let data = &data[1..]; // skip the comma
    if !meta.contains("base64") {
        // Non-base64 (percent-encoded) data URIs are not expected for images.
        return None;
    }
    let bytes = base64_decode(data)?;
    match image::load_from_memory(&bytes) {
        Ok(img) => Some(AlbumArt::from_rgba(img.to_rgba8())),
        Err(e) => {
            tracing::debug!(error = %e, "data: album art decode failed");
            None
        }
    }
}

/// Minimal percent-decoder for `file://` paths (`%20` → space, etc.). Invalid
/// escapes are left verbatim.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard-alphabet base64 decode (no external crate). Ignores ASCII
/// whitespace; returns `None` on any invalid symbol.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some(u32::from(b - b'A')),
            b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in input.as_bytes() {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = val(b)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 image with one saturated red pixel dominates the palette.
    #[test]
    fn primary_color_prefers_saturated_pixel() {
        // Pixels: red (saturated), gray, gray, black.
        let pixels = vec![
            255, 0, 0, 255, // red
            128, 128, 128, 255, // gray (0 saturation)
            128, 128, 128, 255, // gray
            0, 0, 0, 255, // black
        ];
        let art = AlbumArt {
            width: 2,
            height: 2,
            pixels,
        };
        let c = art.primary_color();
        // Red channel dominates strongly.
        assert!(c[0] > c[1] && c[0] > c[2], "got {c:?}");
    }

    /// A fully-gray image (zero saturation) falls back to mid-gray, no NaN.
    #[test]
    fn primary_color_grayscale_fallback() {
        let art = AlbumArt {
            width: 2,
            height: 1,
            pixels: vec![100, 100, 100, 255, 40, 40, 40, 255],
        };
        assert_eq!(art.primary_color(), [128, 128, 128]);
    }

    /// A dark dominant color is lifted so its max channel reaches 170.
    #[test]
    fn primary_color_lifts_dark() {
        // Single dark-blue saturated pixel.
        let art = AlbumArt {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 60, 255],
        };
        let c = art.primary_color();
        assert_eq!(c.iter().copied().max(), Some(170));
    }

    #[test]
    fn hex_formats_lowercase_padded() {
        assert_eq!(hex([0, 16, 255]), "#0010ff");
    }

    #[test]
    fn percent_decode_spaces_and_literals() {
        assert_eq!(percent_decode("/tmp/My%20Cover.jpg"), "/tmp/My Cover.jpg");
        // Bad escape left verbatim.
        assert_eq!(percent_decode("/a%2"), "/a%2");
        assert_eq!(percent_decode("/plain/path.png"), "/plain/path.png");
    }

    #[test]
    fn base64_decode_roundtrip_known_vector() {
        // "Man" → "TWFu"
        assert_eq!(base64_decode("TWFu").as_deref(), Some(&b"Man"[..]));
        // With padding + whitespace: "Ma" → "TWE="
        assert_eq!(base64_decode("TW E=").as_deref(), Some(&b"Ma"[..]));
        // Invalid symbol.
        assert_eq!(base64_decode("****"), None);
    }

    #[test]
    fn load_art_unknown_scheme_returns_none() {
        // Remote URLs are covered by `load_remote`, which is deliberately not
        // exercised here: it would make the unit suite depend on the network.
        assert!(load_art("weird:thing").is_none());
        assert!(load_art("ftp://example.com/cover.jpg").is_none());
    }

    #[test]
    fn load_art_missing_file_returns_none_no_panic() {
        assert!(load_art("file:///nonexistent/kirie-test-cover.png").is_none());
        assert!(load_art("/nonexistent/kirie-test-cover.png").is_none());
    }

    #[test]
    fn load_art_decodes_data_uri_png() {
        // Encode a 1x1 red PNG in-memory, wrap as a data: URI, decode back.
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([200, 10, 10, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode png");
        let b64 = base64_encode(&buf.into_inner());
        let uri = format!("data:image/png;base64,{b64}");
        let art = load_art(&uri).expect("decode data uri");
        assert_eq!((art.width, art.height), (1, 1));
        assert_eq!(&art.pixels[..4], &[200, 10, 10, 255]);
    }

    /// Wrapper over the production encoder, so the round-trip test exercises
    /// the encoder the web bridge actually ships.
    fn base64_encode(data: &[u8]) -> String {
        let mut out = String::new();
        base64_encode_into(data, &mut out);
        out
    }

    #[test]
    fn base64_encode_known_vectors_and_padding() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b""), "");
    }

    /// The thumbnail URI must carry the exact prefix workshop pages strip, and
    /// must decode back to an image (round-trip through `load_art`).
    #[test]
    fn png_data_uri_round_trips_through_load_art() {
        let art = AlbumArt {
            width: 2,
            height: 1,
            pixels: vec![10, 200, 30, 255, 0, 0, 0, 255],
        };
        let uri = art.png_data_uri().expect("encode");
        assert!(uri.starts_with("data:image/png;base64,"), "{}", &uri[..32]);
        let back = load_art(&uri).expect("decode");
        assert_eq!((back.width, back.height), (2, 1));
        assert_eq!(&back.pixels[..4], &[10, 200, 30, 255]);
    }

    /// Oversized art is downscaled so the inline payload stays bounded.
    #[test]
    fn png_data_uri_downscales_to_the_edge_cap() {
        let w = MAX_THUMBNAIL_EDGE * 2;
        let art = AlbumArt {
            width: w,
            height: w / 2,
            pixels: vec![128; (w as usize) * (w as usize / 2) * 4],
        };
        let back = load_art(&art.png_data_uri().expect("encode")).expect("decode");
        assert_eq!(back.width, MAX_THUMBNAIL_EDGE);
        assert_eq!(back.height, MAX_THUMBNAIL_EDGE / 2);
    }

    /// A pixel buffer that disagrees with its declared size yields `None`
    /// rather than panicking (V9).
    #[test]
    fn png_data_uri_rejects_inconsistent_pixels() {
        let art = AlbumArt {
            width: 4,
            height: 4,
            pixels: vec![0; 8],
        };
        assert!(art.png_data_uri().is_none());
    }
}
