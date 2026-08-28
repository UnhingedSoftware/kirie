pub const MAX_THUMBNAIL_EDGE: u32 = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumArt {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    primary: [u8; 3],
}

impl AlbumArt {
    #[must_use]
    fn from_rgba(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self::new(width, height, img.into_raw())
    }

    #[must_use]
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        let primary = compute_primary_color(width, height, &pixels);
        Self {
            width,
            height,
            pixels,
            primary,
        }
    }

    #[must_use]
    pub fn primary_color(&self) -> [u8; 3] {
        self.primary
    }
}

fn compute_primary_color(width: u32, height: u32, pixels: &[u8]) -> [u8; 3] {
    {
        let mut acc = [0.0f64; 3];
        let mut weight_sum = 0.0f64;

        let step_x = (width / 64).max(1);
        let step_y = (height / 64).max(1);

        let mut y = 0;
        while y < height {
            let mut x = 0;
            while x < width {
                let idx = ((y * width + x) * 4) as usize;
                let Some(px) = pixels.get(idx..idx + 4) else {
                    x += step_x;
                    continue;
                };
                let r = px[0] as f64;
                let g = px[1] as f64;
                let b = px[2] as f64;
                let a = px[3] as f64 / 255.0;
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
            return [128, 128, 128];
        }

        let mut color = [
            (acc[0] / weight_sum).round(),
            (acc[1] / weight_sum).round(),
            (acc[2] / weight_sum).round(),
        ];
        let max = color[0].max(color[1]).max(color[2]);
        if max > 0.0 && max < 170.0 {
            let scale = 170.0 / max;
            for c in &mut color {
                *c = (*c * scale).min(255.0);
            }
        }
        [color[0] as u8, color[1] as u8, color[2] as u8]
    }
}

impl AlbumArt {
    #[must_use]
    pub fn png_data_uri(&self) -> Option<String> {
        let img = image::RgbaImage::from_raw(self.width, self.height, self.pixels.clone())?;
        let longest = self.width.max(self.height);
        let img = if longest > MAX_THUMBNAIL_EDGE {
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

#[derive(Clone, Debug, PartialEq)]
pub struct MediaPlaybackEvent {
    pub available: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub state: i32,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub thumbnail: Option<std::sync::Arc<AlbumArt>>,
    pub art_url: Option<String>,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub text_color: Option<String>,
    pub high_contrast_color: Option<String>,
}

impl MediaPlaybackEvent {
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
                let luma = 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
                let text = if luma < 150.0 {
                    [255, 255, 255]
                } else {
                    [0x10, 0x10, 0x10]
                };
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

#[must_use]
fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

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
        url.to_owned()
    } else {
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

const REMOTE_TIMEOUT_SECS: &str = "6";
const REMOTE_MAX_BYTES: &str = "16777216";

fn load_remote(url: &str) -> Option<AlbumArt> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--max-redirs",
            "3",
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

fn load_data_uri(rest: &str) -> Option<AlbumArt> {
    let comma = rest.find(',')?;
    let (meta, data) = rest.split_at(comma);
    let data = &data[1..];
    if !meta.contains("base64") {
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

    #[test]
    fn primary_color_prefers_saturated_pixel() {
        let pixels = vec![
            255, 0, 0, 255, 128, 128, 128, 255, 128, 128, 128, 255, 0, 0, 0, 255,
        ];
        let art = AlbumArt::new(2, 2, pixels);
        let c = art.primary_color();
        assert!(c[0] > c[1] && c[0] > c[2], "got {c:?}");
    }

    #[test]
    fn primary_color_grayscale_fallback() {
        let art = AlbumArt::new(2, 1, vec![100, 100, 100, 255, 40, 40, 40, 255]);
        assert_eq!(art.primary_color(), [128, 128, 128]);
    }

    #[test]
    fn primary_color_lifts_dark() {
        let art = AlbumArt::new(1, 1, vec![0, 0, 60, 255]);
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
        assert_eq!(percent_decode("/a%2"), "/a%2");
        assert_eq!(percent_decode("/plain/path.png"), "/plain/path.png");
    }

    #[test]
    fn base64_decode_roundtrip_known_vector() {
        assert_eq!(base64_decode("TWFu").as_deref(), Some(&b"Man"[..]));
        assert_eq!(base64_decode("TW E=").as_deref(), Some(&b"Ma"[..]));
        assert_eq!(base64_decode("****"), None);
    }

    #[test]
    fn load_art_unknown_scheme_returns_none() {
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

    #[test]
    fn png_data_uri_round_trips_through_load_art() {
        let art = AlbumArt::new(2, 1, vec![10, 200, 30, 255, 0, 0, 0, 255]);
        let uri = art.png_data_uri().expect("encode");
        assert!(uri.starts_with("data:image/png;base64,"), "{}", &uri[..32]);
        let back = load_art(&uri).expect("decode");
        assert_eq!((back.width, back.height), (2, 1));
        assert_eq!(&back.pixels[..4], &[10, 200, 30, 255]);
    }

    #[test]
    fn png_data_uri_downscales_to_the_edge_cap() {
        let w = MAX_THUMBNAIL_EDGE * 2;
        let art = AlbumArt::new(w, w / 2, vec![128; (w as usize) * (w as usize / 2) * 4]);
        let back = load_art(&art.png_data_uri().expect("encode")).expect("decode");
        assert_eq!(back.width, MAX_THUMBNAIL_EDGE);
        assert_eq!(back.height, MAX_THUMBNAIL_EDGE / 2);
    }

    #[test]
    fn png_data_uri_rejects_inconsistent_pixels() {
        let art = AlbumArt::new(4, 4, vec![0; 8]);
        assert!(art.png_data_uri().is_none());
    }
}
