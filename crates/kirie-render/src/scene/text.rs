use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::fontdb;
use cosmic_text::{
    Align, Attrs, Buffer, Color as CtColor, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use kirie_scene::resolve::AssetSource;

use super::texture::GpuTexture;

const LINE_HEIGHT_RATIO: f32 = 1.2;

const MAX_EDGE: u32 = 4096;

pub struct TextFonts {
    font_system: FontSystem,
    swash: SwashCache,
    bundled: HashMap<String, Option<String>>,
}

impl TextFonts {
    #[must_use]
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash: SwashCache::new(),
            bundled: HashMap::new(),
        }
    }

    #[must_use]
    pub fn face_count(&self) -> usize {
        self.font_system.db().len()
    }

    pub fn bundled_family(&mut self, font: &str, source: &dyn AssetSource) -> Option<String> {
        let font = font.trim();
        let lower = font.to_ascii_lowercase();
        if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) {
            return None;
        }
        if let Some(cached) = self.bundled.get(font) {
            return cached.clone();
        }
        let family = source.load(font).and_then(|bytes| self.load_face(bytes));
        if family.is_none() {
            tracing::debug!(font, "bundled font not found/loadable; using system fallback");
        }
        self.bundled.insert(font.to_owned(), family.clone());
        family
    }

    fn load_face(&mut self, bytes: Vec<u8>) -> Option<String> {
        let db = self.font_system.db_mut();
        let ids = db.load_font_source(fontdb::Source::Binary(Arc::new(bytes)));
        let id = *ids.first()?;
        db.face(id)?.families.first().map(|(name, _)| name.clone())
    }
}

impl Default for TextFonts {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TextRaster {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub line_count: usize,
    pub any_coverage: bool,
}

fn h_align(s: &str) -> Align {
    match s {
        "left" => Align::Left,
        "right" => Align::Right,
        _ => Align::Center,
    }
}

fn v_align_factor(s: &str) -> f32 {
    match s {
        "top" => 0.0,
        "bottom" => 1.0,
        _ => 0.5,
    }
}

fn family_hint(font: &str) -> Option<String> {
    let font = font.trim();
    if font.is_empty() {
        return None;
    }
    if let Some(name) = font.strip_prefix("systemfont_") {
        let name = name.trim();
        return (!name.is_empty()).then(|| name.to_owned());
    }
    let file = font.rsplit(['/', '\\']).next().unwrap_or(font);
    let stem = file.rsplit_once('.').map_or(file, |(base, _ext)| base);
    let stem = stem.trim();
    (!stem.is_empty()).then(|| stem.to_owned())
}

#[must_use]
pub fn without_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let Some(end) = from.find('>') else {
            out.push_str(from);
            return unescape(&out);
        };
        let tag = &from[1..end];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .next()
            .unwrap_or("");
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            out.push('<');
            rest = &from[1..];
            continue;
        }
        if name.eq_ignore_ascii_case("br") {
            out.push('\n');
        }
        rest = &from[end + 1..];
    }
    out.push_str(rest);
    unescape(&out)
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn rasterize(
    fonts: &mut TextFonts,
    text: &str,
    font: &str,
    point_size: f32,
    box_size: [f32; 2],
    horizontalalign: &str,
    verticalalign: &str,
    padding: f32,
    bundled_family: Option<&str>,
) -> Option<TextRaster> {
    if text.is_empty() {
        return None;
    }
    let plain = without_markup(text);
    let text = plain.as_str();
    if text.is_empty() {
        return None;
    }
    let point_size = point_size.max(1.0);
    let pad = padding.max(0.0);
    let metrics = Metrics::new(point_size, point_size * LINE_HEIGHT_RATIO);

    let has_box_w = box_size[0] > 1.0;
    let has_box_h = box_size[1] > 1.0;
    let inner_w = has_box_w.then(|| (box_size[0] - 2.0 * pad).max(1.0));

    let hint = family_hint(font);
    let family_name = bundled_family.or(hint.as_deref());
    let family = family_name.map_or(Family::SansSerif, Family::Name);
    let attrs = Attrs::new().family(family);

    let mut buffer = {
        let fs = &mut fonts.font_system;
        let mut buffer = Buffer::new(fs, metrics);
        buffer.set_size(inner_w, None);
        buffer.set_wrap(if has_box_w { Wrap::WordOrGlyph } else { Wrap::None });
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(h_align(horizontalalign)));
        buffer.shape_until_scroll(fs, false);
        buffer
    };

    let mut text_w = 0.0f32;
    let mut text_h = 0.0f32;
    let mut line_count = 0usize;
    for run in buffer.layout_runs() {
        line_count += 1;
        text_w = text_w.max(run.line_w);
        text_h = text_h.max(run.line_top + run.line_height);
    }
    if line_count == 0 {
        return None;
    }

    let out_w = ceil_clamp(if has_box_w {
        box_size[0]
    } else {
        text_w + 2.0 * pad
    });
    let out_h = ceil_clamp(if has_box_h {
        box_size[1]
    } else {
        text_h + 2.0 * pad
    });

    let x_off = pad.round() as i32;
    let avail = out_h as f32 - 2.0 * pad - text_h;
    let y_off = (pad
        + if avail > 0.0 {
            avail * v_align_factor(verticalalign)
        } else {
            0.0
        })
    .round() as i32;

    let mut pixels = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    let mut any_coverage = false;
    buffer.draw(
        &mut fonts.font_system,
        &mut fonts.swash,
        CtColor::rgba(0xFF, 0xFF, 0xFF, 0xFF),
        |gx, gy, w, h, color| {
            let a = color.a();
            if a == 0 {
                return;
            }
            for dy in 0..h as i32 {
                for dx in 0..w as i32 {
                    let px = gx + dx + x_off;
                    let py = gy + dy + y_off;
                    if px < 0 || py < 0 || px >= out_w as i32 || py >= out_h as i32 {
                        continue;
                    }
                    let idx = ((py as u32 * out_w + px as u32) * 4) as usize;
                    if a > pixels[idx + 3] {
                        pixels[idx] = 0xFF;
                        pixels[idx + 1] = 0xFF;
                        pixels[idx + 2] = 0xFF;
                        pixels[idx + 3] = a;
                        any_coverage = true;
                    }
                }
            }
        },
    );

    Some(TextRaster {
        pixels,
        width: out_w,
        height: out_h,
        line_count,
        any_coverage,
    })
}

fn ceil_clamp(v: f32) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        return 1;
    }
    (v.ceil() as u32).clamp(1, MAX_EDGE)
}

#[must_use]
pub fn upload(device: &wgpu::Device, queue: &wgpu::Queue, raster: &TextRaster) -> GpuTexture {
    let width = raster.width.max(1);
    let height = raster.height.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kirie-scene-text-atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let need = (width * height * 4) as usize;
    if raster.pixels.len() >= need {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &raster.pixels[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kirie-scene-text-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..wgpu::SamplerDescriptor::default()
    });
    GpuTexture {
        texture,
        view,
        sampler,
        width,
        height,
        uv_crop: [1.0, 1.0],
        real_size: [width as f32, height as f32],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_read_as_text_not_shown() {
        assert_eq!(without_markup("<u>Notepad Settings:</u>"), "Notepad Settings:");
        assert_eq!(without_markup("a<br>b"), "a\nb");
        assert_eq!(without_markup("<sup style=\"x\">hi</sup>"), "hi");
    }

    #[test]
    fn ordinary_angle_brackets_survive() {
        assert_eq!(without_markup("a < b > c"), "a < b > c");
        assert_eq!(without_markup("5 <3"), "5 <3");
    }

    #[test]
    fn entities_come_back_as_characters() {
        assert_eq!(without_markup("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(without_markup("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn multiline_line_count_matches_newlines() {
        let mut fonts = TextFonts::new();
        let r = rasterize(
            &mut fonts,
            "line one\nline two\nline three",
            "",
            32.0,
            [0.0, 0.0],
            "left",
            "top",
            0.0,
            None,
        )
        .expect("non-empty text rasterizes");
        assert_eq!(r.line_count, 3, "three newline-separated lines");
        assert!(
            r.height >= (32.0 * LINE_HEIGHT_RATIO * 2.0) as u32,
            "height {} spans multiple lines",
            r.height
        );
        assert!(r.width > 0 && r.height > 0, "non-degenerate bounds");
    }

    #[test]
    fn single_line_bounds() {
        let mut fonts = TextFonts::new();
        let r = rasterize(
            &mut fonts,
            "Hello",
            "",
            24.0,
            [0.0, 0.0],
            "center",
            "center",
            0.0,
            None,
        )
        .expect("non-empty text rasterizes");
        assert_eq!(r.line_count, 1);
        assert!(r.height > 0);
        if fonts.face_count() > 0 {
            assert!(r.width > 1, "measured a non-empty advance");
            assert!(r.any_coverage, "rasterized real glyph coverage");
        }
    }

    #[test]
    fn box_size_sets_bitmap_dims() {
        let mut fonts = TextFonts::new();
        let r = rasterize(
            &mut fonts,
            "x",
            "",
            16.0,
            [200.0, 120.0],
            "center",
            "center",
            0.0,
            None,
        )
        .expect("rasterizes");
        assert_eq!(r.width, 200);
        assert_eq!(r.height, 120);
    }

    #[test]
    fn empty_text_is_none() {
        let mut fonts = TextFonts::new();
        assert!(
            rasterize(
                &mut fonts,
                "",
                "any",
                32.0,
                [0.0, 0.0],
                "center",
                "center",
                0.0,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn oversize_is_clamped() {
        assert_eq!(ceil_clamp(f32::INFINITY), 1);
        assert_eq!(ceil_clamp(-5.0), 1);
        assert_eq!(ceil_clamp(1_000_000.0), MAX_EDGE);
        assert_eq!(ceil_clamp(63.2), 64);
    }

    #[test]
    fn family_hint_mapping() {
        assert_eq!(family_hint(""), None);
        assert_eq!(family_hint("   "), None);
        assert_eq!(family_hint("systemfont_arial").as_deref(), Some("arial"));
        assert_eq!(
            family_hint("fonts/VCR_OSD_MONO.ttf").as_deref(),
            Some("VCR_OSD_MONO")
        );
        assert_eq!(
            family_hint("workshop/123/My Font.otf").as_deref(),
            Some("My Font")
        );
    }
}
