use std::borrow::Cow;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TexError {
    #[error("bad {what} magic: expected {expected:?}, got {found:?}")]
    BadMagic {
        what: &'static str,
        expected: &'static str,
        found: String,
    },

    #[error(
        "truncated texture: need {needed} byte(s) for {what} at offset {offset}, \
         only {available} available"
    )]
    Truncated {
        what: &'static str,
        offset: usize,
        needed: usize,
        available: usize,
    },

    #[error("unknown texture format {value} (0x{value:08x})")]
    UnknownFormat { value: u32 },

    #[error("unknown FreeImage format id {value}")]
    UnknownFif { value: i32 },

    #[error("unsupported mip compression mode {value} (only 0=stored, 1=LZ4 exist)")]
    UnsupportedCompression { value: u32 },

    #[error("negative mip {what}: {value}")]
    NegativeSize { what: &'static str, value: i32 },

    #[error("LZ4 decompression failed: {source}")]
    Lz4 {
        #[source]
        source: lz4_flex::block::DecompressError,
    },

    #[error("LZ4 mip decompressed to {actual} byte(s), header says {expected}")]
    Lz4SizeMismatch { expected: usize, actual: usize },

    #[error(
        "payload of {format:?} mip {width}x{height} is {actual} byte(s), \
         format rule says {expected} (docs/format-tex.md §7.1)"
    )]
    WrongPayloadSize {
        format: TextureFormat,
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },

    #[error("no RGBA8 decoder for texture format {format:?}")]
    UnsupportedFormat { format: TextureFormat },

    #[error("mip dimensions {width}x{height} overflow the address space")]
    Oversized { width: u32, height: u32 },

    #[error("texture is a video; use video_payload() for the raw MP4 bytes")]
    IsVideo,

    #[error("texture is not a video (docs/format-tex.md §7.3)")]
    NotVideo,

    #[error("no image {index} (texture has {count})")]
    NoSuchImage { index: usize, count: usize },

    #[error("no mipmap {index} (image has {count})")]
    NoSuchMipmap { index: usize, count: usize },

    #[error("embedded image decode failed: {source}")]
    ImageDecode {
        #[source]
        source: Box<image::ImageError>,
    },

    #[error("{format:?} block decode failed: {reason}")]
    BlockDecode {
        format: TextureFormat,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Unknown,
    Argb8888,
    Rgb888,
    Rgb565,
    Dxt5,
    Dxt3,
    Dxt1,
    Rg88,
    R8,
    Rg1616f,
    R16f,
    Bc7,
    Rgba1010102,
    Rgba16161616f,
    Rgb161616f,
}

impl TextureFormat {
    fn from_u32(value: u32) -> Result<Self, TexError> {
        Ok(match value {
            0xFFFF_FFFF => Self::Unknown,
            0 => Self::Argb8888,
            1 => Self::Rgb888,
            2 => Self::Rgb565,
            4 => Self::Dxt5,
            6 => Self::Dxt3,
            7 => Self::Dxt1,
            8 => Self::Rg88,
            9 => Self::R8,
            10 => Self::Rg1616f,
            11 => Self::R16f,
            12 => Self::Bc7,
            13 => Self::Rgba1010102,
            14 => Self::Rgba16161616f,
            15 => Self::Rgb161616f,
            other => return Err(TexError::UnknownFormat { value: other }),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureFlags(pub u32);

impl TextureFlags {
    pub const NO_INTERPOLATION: u32 = 0x1;
    pub const CLAMP_UVS: u32 = 0x2;
    pub const IS_GIF: u32 = 0x4;
    pub const CLAMP_UVS_BORDER: u32 = 0x8;
    pub const VIDEO: u32 = 0x20;
    pub const ALPHA_CHANNEL_PRIORITY: u32 = 0x8_0000;

    #[must_use]
    pub fn no_interpolation(self) -> bool {
        self.0 & Self::NO_INTERPOLATION != 0
    }

    #[must_use]
    pub fn clamp_uvs(self) -> bool {
        self.0 & Self::CLAMP_UVS != 0
    }

    #[must_use]
    pub fn is_gif(self) -> bool {
        self.0 & Self::IS_GIF != 0
    }

    #[must_use]
    pub fn clamp_uvs_border(self) -> bool {
        self.0 & Self::CLAMP_UVS_BORDER != 0
    }

    #[must_use]
    pub fn video(self) -> bool {
        self.0 & Self::VIDEO != 0
    }

    #[must_use]
    pub fn alpha_channel_priority(self) -> bool {
        self.0 & Self::ALPHA_CHANNEL_PRIORITY != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeImageFormat(pub i32);

impl FreeImageFormat {
    pub const UNKNOWN: Self = Self(-1);
    pub const JPEG: Self = Self(2);
    pub const PNG: Self = Self(13);
    pub const MP4: Self = Self(35);

    fn from_i32(value: i32) -> Result<Self, TexError> {
        if (-1..=36).contains(&value) {
            Ok(Self(value))
        } else {
            Err(TexError::UnknownFif { value })
        }
    }

    #[must_use]
    pub fn is_raw(self) -> bool {
        self == Self::UNKNOWN
    }

    #[must_use]
    pub fn is_mp4(self) -> bool {
        self == Self::MP4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerVersion {
    Texb0001,
    Texb0002,
    Texb0003,
    Texb0004,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationVersion {
    Texs0001,
    Texs0002,
    Texs0003,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Stored,
    Lz4,
}

#[derive(Debug, Clone)]
pub struct Mipmap<'a> {
    pub width: u32,
    pub height: u32,
    pub compression: Compression,
    pub uncompressed_size: usize,
    pub payload: &'a [u8],
}

impl<'a> Mipmap<'a> {
    pub fn data(&self) -> Result<Cow<'a, [u8]>, TexError> {
        match self.compression {
            Compression::Stored => Ok(Cow::Borrowed(self.payload)),
            Compression::Lz4 => {
                let bound = self.payload.len().saturating_mul(255).saturating_add(64);
                if self.uncompressed_size > bound {
                    return Err(TexError::Lz4SizeMismatch {
                        expected: self.uncompressed_size,
                        actual: bound,
                    });
                }
                let out = lz4_flex::block::decompress(self.payload, self.uncompressed_size)
                    .map_err(|source| TexError::Lz4 { source })?;
                if out.len() != self.uncompressed_size {
                    return Err(TexError::Lz4SizeMismatch {
                        expected: self.uncompressed_size,
                        actual: out.len(),
                    });
                }
                Ok(Cow::Owned(out))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TexImage<'a> {
    pub mipmaps: Vec<Mipmap<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub frame_number: u32,
    pub frametime: f32,
    pub x: f32,
    pub y: f32,
    pub width1: f32,
    pub width2: f32,
    pub height2: f32,
    pub height1: f32,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub version: AnimationVersion,
    pub gif_width: u32,
    pub gif_height: u32,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone)]
pub struct Rgba8Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Tex<'a> {
    pub format: TextureFormat,
    pub flags: TextureFlags,
    pub texture_width: u32,
    pub texture_height: u32,
    pub width: u32,
    pub height: u32,
    pub unknown: u32,
    pub container: ContainerVersion,
    pub fif: FreeImageFormat,
    pub is_video_mp4: bool,
    pub images: Vec<TexImage<'a>>,
    pub animation: Option<Animation>,
}

impl<'a> Tex<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, TexError> {
        let mut r = Reader { data, pos: 0 };

        r.expect_magic(b"TEXV0005\0", "outer container", "TEXV0005")?;
        r.expect_magic(b"TEXI0001\0", "header sub-block", "TEXI0001")?;

        let format = TextureFormat::from_u32(r.read_u32("format")?)?;
        let flags = TextureFlags(r.read_u32("flags")?);
        let texture_width = r.read_u32("textureWidth")?;
        let texture_height = r.read_u32("textureHeight")?;
        let width = r.read_u32("width")?;
        let height = r.read_u32("height")?;
        let unknown = r.read_u32("header word +0x2a")?;

        let magic = r.take(9, "TEXB magic")?;
        let container = match magic {
            b"TEXB0001\0" => ContainerVersion::Texb0001,
            b"TEXB0002\0" => ContainerVersion::Texb0002,
            b"TEXB0003\0" => ContainerVersion::Texb0003,
            b"TEXB0004\0" => ContainerVersion::Texb0004,
            other => {
                return Err(TexError::BadMagic {
                    what: "image container",
                    expected: "TEXB0001..TEXB0004",
                    found: String::from_utf8_lossy(other).into_owned(),
                });
            }
        };

        let image_count = r.read_u32("imageCount")?;

        let mut fif = FreeImageFormat::UNKNOWN;
        if matches!(container, ContainerVersion::Texb0003 | ContainerVersion::Texb0004) {
            fif = FreeImageFormat::from_i32(r.read_i32("freeImageFormat")?)?;
        }
        let mut is_video_mp4 = false;
        if container == ContainerVersion::Texb0004 {
            is_video_mp4 = r.read_u32("isVideoMp4")? == 1;
            if fif.is_raw() && is_video_mp4 {
                fif = FreeImageFormat::MP4;
            }
        }
        let effective = if container == ContainerVersion::Texb0004 && !fif.is_mp4() {
            ContainerVersion::Texb0003
        } else {
            container
        };

        let remaining = data.len().saturating_sub(r.pos);
        let mut images = Vec::with_capacity((image_count as usize).min(remaining / 4));
        for _ in 0..image_count {
            let mip_count = r.read_u32("mipmapCount")?;
            let remaining = data.len().saturating_sub(r.pos);
            let mut mipmaps = Vec::with_capacity((mip_count as usize).min(remaining / 12));
            for _ in 0..mip_count {
                mipmaps.push(parse_mipmap(&mut r, effective)?);
            }
            images.push(TexImage { mipmaps });
        }

        let animation = if flags.is_gif() {
            Some(parse_animation(&mut r)?)
        } else {
            None
        };

        Ok(Self {
            format,
            flags,
            texture_width,
            texture_height,
            width,
            height,
            unknown,
            container,
            fif,
            is_video_mp4,
            images,
            animation,
        })
    }

    #[must_use]
    pub fn effective_container(&self) -> ContainerVersion {
        if self.container == ContainerVersion::Texb0004 && !self.fif.is_mp4() {
            ContainerVersion::Texb0003
        } else {
            self.container
        }
    }

    #[must_use]
    pub fn is_video(&self) -> bool {
        self.is_video_mp4 || self.flags.video()
    }

    pub fn video_payload(&self) -> Result<Cow<'a, [u8]>, TexError> {
        if !self.is_video() {
            return Err(TexError::NotVideo);
        }
        let image = self
            .images
            .first()
            .ok_or(TexError::NoSuchImage { index: 0, count: 0 })?;
        let mip = image
            .mipmaps
            .first()
            .ok_or(TexError::NoSuchMipmap { index: 0, count: 0 })?;
        mip.data()
    }

    pub fn decode_rgba8(&self, image_index: usize, mip_index: usize) -> Result<Rgba8Image, TexError> {
        if self.is_video() || self.fif.is_mp4() {
            return Err(TexError::IsVideo);
        }
        let image = self.images.get(image_index).ok_or(TexError::NoSuchImage {
            index: image_index,
            count: self.images.len(),
        })?;
        let mip = image.mipmaps.get(mip_index).ok_or(TexError::NoSuchMipmap {
            index: mip_index,
            count: image.mipmaps.len(),
        })?;
        let data = mip.data()?;

        if !self.fif.is_raw() {
            let decoded = image::load_from_memory(&data)
                .map_err(|source| TexError::ImageDecode {
                    source: Box::new(source),
                })?
                .to_rgba8();
            let (width, height) = (decoded.width(), decoded.height());
            return Ok(Rgba8Image {
                width,
                height,
                pixels: decoded.into_raw(),
            });
        }

        decode_raw_rgba8(self.format, mip.width, mip.height, &data)
    }
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0f32 };
    let exp = (h >> 10) & 0x1f;
    let mant = (h & 0x3ff) as f32;
    match exp {
        0 => sign * mant * 2.0f32.powi(-24),
        0x1f => {
            if mant == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mant / 1024.0) * 2.0f32.powi(exp as i32 - 15),
    }
}

fn half_to_unorm8(h: u16) -> u8 {
    let v = half_to_f32(h);
    let c = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    (c * 255.0 + 0.5) as u8
}

fn pixel_count(width: u32, height: u32) -> Result<usize, TexError> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or(TexError::Oversized { width, height })
}

fn block_count(width: u32, height: u32) -> Result<usize, TexError> {
    (width as usize)
        .div_ceil(4)
        .checked_mul((height as usize).div_ceil(4))
        .ok_or(TexError::Oversized { width, height })
}

fn expected_payload_len(format: TextureFormat, width: u32, height: u32) -> Result<usize, TexError> {
    let px = pixel_count(width, height)?;
    let len = match format {
        TextureFormat::Argb8888 => px.checked_mul(4),
        TextureFormat::Rgb888 => px.checked_mul(3),
        TextureFormat::Rgb565 => px.checked_mul(2),
        TextureFormat::Rg88 => px.checked_mul(2),
        TextureFormat::R8 => Some(px),
        TextureFormat::Rg1616f => px.checked_mul(4),
        TextureFormat::R16f => px.checked_mul(2),
        TextureFormat::Rgba1010102 => px.checked_mul(4),
        TextureFormat::Rgba16161616f => px.checked_mul(8),
        TextureFormat::Rgb161616f => px.checked_mul(6),
        TextureFormat::Dxt5 | TextureFormat::Dxt3 => block_count(width, height)?.checked_mul(16),
        TextureFormat::Bc7 => block_count(width, height)?.checked_mul(16),
        TextureFormat::Dxt1 => block_count(width, height)?.checked_mul(8),
        TextureFormat::Unknown => return Err(TexError::UnsupportedFormat { format }),
    };
    len.ok_or(TexError::Oversized { width, height })
}

fn decode_raw_rgba8(
    format: TextureFormat,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Rgba8Image, TexError> {
    let expected = expected_payload_len(format, width, height)?;
    if data.len() != expected {
        return Err(TexError::WrongPayloadSize {
            format,
            width,
            height,
            expected,
            actual: data.len(),
        });
    }
    let px = pixel_count(width, height)?;
    let out_len = px.checked_mul(4).ok_or(TexError::Oversized { width, height })?;

    let pixels = match format {
        TextureFormat::Argb8888 => data.to_vec(),
        TextureFormat::Rg88 => {
            let mut out = Vec::with_capacity(out_len);
            for &[r, g] in data.as_chunks::<2>().0 {
                out.extend_from_slice(&[r, g, 0, 255]);
            }
            out
        }
        TextureFormat::R8 => {
            let mut out = Vec::with_capacity(out_len);
            for &r in data {
                out.extend_from_slice(&[r, 0, 0, 255]);
            }
            out
        }
        TextureFormat::Rgb888 => {
            let mut out = Vec::with_capacity(out_len);
            for &[r, g, b] in data.as_chunks::<3>().0 {
                out.extend_from_slice(&[r, g, b, 255]);
            }
            out
        }
        TextureFormat::Rgb565 => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1] in data.as_chunks::<2>().0 {
                let v = u16::from_le_bytes([b0, b1]);
                let r5 = ((v >> 11) & 0x1f) as u8;
                let g6 = ((v >> 5) & 0x3f) as u8;
                let b5 = (v & 0x1f) as u8;
                let r = (r5 << 3) | (r5 >> 2);
                let g = (g6 << 2) | (g6 >> 4);
                let b = (b5 << 3) | (b5 >> 2);
                out.extend_from_slice(&[r, g, b, 255]);
            }
            out
        }
        TextureFormat::Rg1616f => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1, b2, b3] in data.as_chunks::<4>().0 {
                let r = half_to_unorm8(u16::from_le_bytes([b0, b1]));
                let g = half_to_unorm8(u16::from_le_bytes([b2, b3]));
                out.extend_from_slice(&[r, g, 0, 255]);
            }
            out
        }
        TextureFormat::R16f => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1] in data.as_chunks::<2>().0 {
                let r = half_to_unorm8(u16::from_le_bytes([b0, b1]));
                out.extend_from_slice(&[r, 0, 0, 255]);
            }
            out
        }
        TextureFormat::Rgba1010102 => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1, b2, b3] in data.as_chunks::<4>().0 {
                let v = u32::from_le_bytes([b0, b1, b2, b3]);
                let r = ((v >> 2) & 0xff) as u8;
                let g = ((v >> 12) & 0xff) as u8;
                let b = ((v >> 22) & 0xff) as u8;
                let a2 = ((v >> 30) & 0x3) as u8;
                let a = a2 * 0x55;
                out.extend_from_slice(&[r, g, b, a]);
            }
            out
        }
        TextureFormat::Rgba16161616f => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1, b2, b3, b4, b5, b6, b7] in data.as_chunks::<8>().0 {
                let r = half_to_unorm8(u16::from_le_bytes([b0, b1]));
                let g = half_to_unorm8(u16::from_le_bytes([b2, b3]));
                let b = half_to_unorm8(u16::from_le_bytes([b4, b5]));
                let a = half_to_unorm8(u16::from_le_bytes([b6, b7]));
                out.extend_from_slice(&[r, g, b, a]);
            }
            out
        }
        TextureFormat::Rgb161616f => {
            let mut out = Vec::with_capacity(out_len);
            for &[b0, b1, b2, b3, b4, b5] in data.as_chunks::<6>().0 {
                let r = half_to_unorm8(u16::from_le_bytes([b0, b1]));
                let g = half_to_unorm8(u16::from_le_bytes([b2, b3]));
                let b = half_to_unorm8(u16::from_le_bytes([b4, b5]));
                out.extend_from_slice(&[r, g, b, 255]);
            }
            out
        }
        TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 | TextureFormat::Bc7 => {
            let mut words = vec![0u32; px];
            let (w, h) = (width as usize, height as usize);
            let result = match format {
                TextureFormat::Dxt1 => texture2ddecoder::decode_bc1(data, w, h, &mut words),
                TextureFormat::Dxt3 => texture2ddecoder::decode_bc2(data, w, h, &mut words),
                TextureFormat::Dxt5 => texture2ddecoder::decode_bc3(data, w, h, &mut words),
                _ => texture2ddecoder::decode_bc7(data, w, h, &mut words),
            };
            result.map_err(|reason| TexError::BlockDecode { format, reason })?;
            let mut out = Vec::with_capacity(out_len);
            for word in words {
                let [b, g, r, a] = word.to_le_bytes();
                out.extend_from_slice(&[r, g, b, a]);
            }
            out
        }
        other => return Err(TexError::UnsupportedFormat { format: other }),
    };

    Ok(Rgba8Image {
        width,
        height,
        pixels,
    })
}

fn parse_mipmap<'a>(r: &mut Reader<'a>, effective: ContainerVersion) -> Result<Mipmap<'a>, TexError> {
    if effective == ContainerVersion::Texb0004 {
        r.read_u32("mip editor int 1")?;
        r.read_u32("mip editor int 2")?;
        r.read_cstr("mip JSON string")?;
        r.read_u32("mip editor int 3")?;
    }

    let width = r.read_u32("mip width")?;
    let height = r.read_u32("mip height")?;

    let (compression_word, uncompressed_field) = if effective == ContainerVersion::Texb0001 {
        (0u32, 0i32)
    } else {
        (
            r.read_u32("mip compression")?,
            r.read_i32("mip uncompressedSize")?,
        )
    };

    let compressed_size = r.read_i32("mip compressedSize")?;
    let compressed_size = usize::try_from(compressed_size).map_err(|_| TexError::NegativeSize {
        what: "compressedSize",
        value: compressed_size,
    })?;

    let (compression, uncompressed_size) = match compression_word {
        0 => (Compression::Stored, compressed_size),
        1 => {
            let size = usize::try_from(uncompressed_field).map_err(|_| TexError::NegativeSize {
                what: "uncompressedSize",
                value: uncompressed_field,
            })?;
            (Compression::Lz4, size)
        }
        other => return Err(TexError::UnsupportedCompression { value: other }),
    };

    let payload = r.take(compressed_size, "mip payload")?;
    Ok(Mipmap {
        width,
        height,
        compression,
        uncompressed_size,
        payload,
    })
}

fn parse_animation(r: &mut Reader<'_>) -> Result<Animation, TexError> {
    let magic = r.take(9, "TEXS magic")?;
    let version = match magic {
        b"TEXS0001\0" => AnimationVersion::Texs0001,
        b"TEXS0002\0" => AnimationVersion::Texs0002,
        b"TEXS0003\0" => AnimationVersion::Texs0003,
        other => {
            return Err(TexError::BadMagic {
                what: "animation block",
                expected: "TEXS0001..TEXS0003",
                found: String::from_utf8_lossy(other).into_owned(),
            });
        }
    };

    let frame_count = r.read_u32("frameCount")?;

    let (mut gif_width, mut gif_height) = (0u32, 0u32);
    if version == AnimationVersion::Texs0003 {
        gif_width = r.read_u32("gifWidth")?;
        gif_height = r.read_u32("gifHeight")?;
    }

    let remaining = r.data.len().saturating_sub(r.pos);
    let mut frames = Vec::with_capacity((frame_count as usize).min(remaining / 32));
    for _ in 0..frame_count {
        frames.push(match version {
            AnimationVersion::Texs0001 => parse_frame_v1(r)?,
            _ => parse_frame(r)?,
        });
    }

    if version != AnimationVersion::Texs0003
        && let Some(frame0) = frames.first()
    {
        gif_width = frame0.width1 as u32;
        gif_height = frame0.height1 as u32;
    }

    Ok(Animation {
        version,
        gif_width,
        gif_height,
        frames,
    })
}

fn parse_frame(r: &mut Reader<'_>) -> Result<Frame, TexError> {
    Ok(Frame {
        frame_number: r.read_u32("frame frameNumber")?,
        frametime: r.read_f32("frame frametime")?,
        x: r.read_f32("frame x")?,
        y: r.read_f32("frame y")?,
        width1: r.read_f32("frame width1")?,
        width2: r.read_f32("frame width2")?,
        height2: r.read_f32("frame height2")?,
        height1: r.read_f32("frame height1")?,
    })
}

fn parse_frame_v1(r: &mut Reader<'_>) -> Result<Frame, TexError> {
    let frame_number = r.read_u32("frame frameNumber")?;
    let frametime = r.read_f32("frame frametime")?;
    let x = r.read_u32("frame x")?;
    let y = r.read_u32("frame y")?;
    let width1 = r.read_u32("frame width1")?;
    r.read_u32("frame unused field 5")?;
    r.read_u32("frame unused field 6")?;
    let height1 = r.read_u32("frame height1")?;
    Ok(Frame {
        frame_number,
        frametime,
        x: x as f32,
        y: y as f32,
        width1: width1 as f32,
        width2: 0.0,
        height2: 0.0,
        height1: height1 as f32,
    })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], TexError> {
        let truncated = || TexError::Truncated {
            what,
            offset: self.pos,
            needed: len,
            available: self.data.len().saturating_sub(self.pos),
        };
        let end = self.pos.checked_add(len).ok_or_else(truncated)?;
        let bytes = self.data.get(self.pos..end).ok_or_else(truncated)?;
        self.pos = end;
        Ok(bytes)
    }

    fn expect_magic(
        &mut self,
        magic: &'static [u8; 9],
        what: &'static str,
        expected: &'static str,
    ) -> Result<(), TexError> {
        let bytes = self.take(9, what)?;
        if bytes == magic {
            Ok(())
        } else {
            Err(TexError::BadMagic {
                what,
                expected,
                found: String::from_utf8_lossy(bytes).into_owned(),
            })
        }
    }

    fn read_u32(&mut self, what: &'static str) -> Result<u32, TexError> {
        Ok(u32::from_le_bytes(self.read_4(what)?))
    }

    fn read_i32(&mut self, what: &'static str) -> Result<i32, TexError> {
        Ok(i32::from_le_bytes(self.read_4(what)?))
    }

    fn read_f32(&mut self, what: &'static str) -> Result<f32, TexError> {
        Ok(f32::from_le_bytes(self.read_4(what)?))
    }

    fn read_4(&mut self, what: &'static str) -> Result<[u8; 4], TexError> {
        let offset = self.pos;
        let bytes = self.take(4, what)?;
        match bytes.first_chunk::<4>() {
            Some(arr) => Ok(*arr),
            None => Err(TexError::Truncated {
                what,
                offset,
                needed: 4,
                available: bytes.len(),
            }),
        }
    }

    fn read_cstr(&mut self, what: &'static str) -> Result<&'a [u8], TexError> {
        let start = self.pos;
        let rest = self.data.get(start..).unwrap_or(&[]);
        match rest.iter().position(|&b| b == 0) {
            Some(n) => {
                let bytes = rest.get(..n).unwrap_or(&[]);
                self.pos = start.saturating_add(n).saturating_add(1);
                Ok(bytes)
            }
            None => Err(TexError::Truncated {
                what,
                offset: start,
                needed: rest.len().saturating_add(1),
                available: rest.len(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn header(format: u32, flags: u32, tex_w: u32, tex_h: u32, w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"TEXV0005\0");
        v.extend_from_slice(b"TEXI0001\0");
        for word in [format, flags, tex_w, tex_h, w, h, 0xFF00_0000] {
            v.extend_from_slice(&word.to_le_bytes());
        }
        v
    }

    fn mip_v3(w: u32, h: u32, compression: u32, uncompressed: i32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&compression.to_le_bytes());
        v.extend_from_slice(&uncompressed.to_le_bytes());
        v.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn simple_tex(format: u32, flags: u32, w: u32, h: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = header(format, flags, w, h, w, h);
        v.extend_from_slice(b"TEXB0003\0");
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&(-1i32).to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&mip_v3(w, h, 0, 0, payload));
        v
    }

    #[test]
    fn parses_minimal_argb_texture() {
        let payload: Vec<u8> = (0..16).collect();
        let data = simple_tex(0, 2, 2, 2, &payload);
        let tex = Tex::parse(&data).unwrap();
        assert_eq!(tex.format, TextureFormat::Argb8888);
        assert_eq!(tex.flags.0, 2);
        assert!(tex.flags.clamp_uvs());
        assert!(!tex.flags.is_gif());
        assert_eq!((tex.texture_width, tex.texture_height), (2, 2));
        assert_eq!((tex.width, tex.height), (2, 2));
        assert_eq!(tex.unknown, 0xFF00_0000);
        assert_eq!(tex.container, ContainerVersion::Texb0003);
        assert_eq!(tex.effective_container(), ContainerVersion::Texb0003);
        assert!(tex.fif.is_raw());
        assert!(!tex.is_video());
        assert!(tex.animation.is_none());
        assert_eq!(tex.images.len(), 1);
        let mip = &tex.images[0].mipmaps[0];
        assert_eq!((mip.width, mip.height), (2, 2));
        assert_eq!(mip.compression, Compression::Stored);
        assert_eq!(mip.uncompressed_size, 16);
        assert_eq!(mip.payload, &payload[..]);
        assert_eq!(mip.data().unwrap().as_ref(), &payload[..]);

        let img = tex.decode_rgba8(0, 0).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.pixels, payload);
    }

    #[test]
    fn lz4_mip_roundtrips() {
        let raw: Vec<u8> = std::iter::repeat_n([1u8, 2, 3, 4], 4).flatten().collect();
        let compressed = lz4_flex::block::compress(&raw);
        let mut data = header(0, 0, 2, 2, 2, 2);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(2, 2, 1, raw.len() as i32, &compressed));

        let tex = Tex::parse(&data).unwrap();
        let mip = &tex.images[0].mipmaps[0];
        assert_eq!(mip.compression, Compression::Lz4);
        assert_eq!(mip.uncompressed_size, 16);
        assert_eq!(mip.data().unwrap().as_ref(), &raw[..]);
        assert_eq!(tex.decode_rgba8(0, 0).unwrap().pixels, raw);
    }

    #[test]
    fn rejects_bad_outer_and_inner_magic() {
        let mut data = simple_tex(0, 0, 1, 1, &[0; 4]);
        data[0..9].copy_from_slice(b"TEXV0004\0");
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::BadMagic {
                what: "outer container",
                ..
            })
        ));

        let mut data = simple_tex(0, 0, 1, 1, &[0; 4]);
        data[9..18].copy_from_slice(b"TEXI0002\0");
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::BadMagic {
                what: "header sub-block",
                ..
            })
        ));
    }

    #[test]
    fn rejects_unknown_formats() {
        for bad in [3u32, 5, 16, 0xFFFF_FFFE] {
            let data = simple_tex(bad, 0, 1, 1, &[0; 4]);
            assert!(
                matches!(Tex::parse(&data), Err(TexError::UnknownFormat { value }) if value == bad),
                "format {bad} must be rejected"
            );
        }
        for good in [0u32, 1, 2, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0xFFFF_FFFF] {
            let data = simple_tex(good, 0, 1, 1, &[0; 4]);
            assert!(Tex::parse(&data).is_ok(), "format {good} must parse");
        }
    }

    #[test]
    fn rejects_unknown_container_magic() {
        let mut data = header(0, 0, 1, 1, 1, 1);
        data.extend_from_slice(b"TEXB0005\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::BadMagic {
                what: "image container",
                ..
            })
        ));
    }

    #[test]
    fn rejects_out_of_range_fif() {
        for bad in [-2i32, 37, i32::MIN, i32::MAX] {
            let mut data = header(0, 0, 1, 1, 1, 1);
            data.extend_from_slice(b"TEXB0003\0");
            data.extend_from_slice(&1u32.to_le_bytes());
            data.extend_from_slice(&bad.to_le_bytes());
            assert!(
                matches!(Tex::parse(&data), Err(TexError::UnknownFif { value }) if value == bad),
                "fif {bad} must be rejected"
            );
        }
    }

    #[test]
    fn never_panics_and_errors_on_any_truncation() {
        let payload: Vec<u8> = (0..16).collect();
        let data = simple_tex(0, 0, 2, 2, &payload);
        for len in 0..data.len() {
            let prefix = data.get(..len).unwrap();
            assert!(Tex::parse(prefix).is_err(), "prefix of {len} bytes must fail");
        }
        assert!(Tex::parse(&data).is_ok());
    }

    #[test]
    fn truncated_mip_payload_is_a_typed_error() {
        let mut data = header(0, 0, 4, 4, 4, 4);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&100i32.to_le_bytes());
        data.extend_from_slice(&[0u8; 10]);
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::Truncated {
                what: "mip payload",
                needed: 100,
                available: 10,
                ..
            })
        ));
    }

    #[test]
    fn negative_mip_sizes_are_typed_errors() {
        let mut data = header(0, 0, 1, 1, 1, 1);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(1, 1, 0, 0, &[]));
        let n = data.len();
        data[n - 4..].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::NegativeSize {
                what: "compressedSize",
                value: -1
            })
        ));

        let mut data = header(0, 0, 1, 1, 1, 1);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(1, 1, 1, -5, &[0; 4]));
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::NegativeSize {
                what: "uncompressedSize",
                value: -5
            })
        ));
    }

    #[test]
    fn unsupported_compression_is_a_typed_error() {
        let mut data = header(0, 0, 1, 1, 1, 1);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(1, 1, 2, 4, &[0; 4]));
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::UnsupportedCompression { value: 2 })
        ));
    }

    #[test]
    fn lz4_size_mismatch_and_corruption_are_typed_errors() {
        let raw = [7u8; 64];
        let compressed = lz4_flex::block::compress(&raw);

        let mut data = header(0, 0, 8, 8, 8, 8);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(8, 8, 1, 128, &compressed));
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.images[0].mipmaps[0].data(),
            Err(TexError::Lz4SizeMismatch {
                expected: 128,
                actual: 64
            })
        ));

        let mut data = header(0, 0, 8, 8, 8, 8);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(8, 8, 1, 32, &compressed));
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.images[0].mipmaps[0].data(),
            Err(TexError::Lz4 { .. })
        ));

        let mut data = header(0, 0, 8, 8, 8, 8);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(8, 8, 1, i32::MAX, &[0u8; 8]));
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.images[0].mipmaps[0].data(),
            Err(TexError::Lz4SizeMismatch { .. })
        ));
    }

    #[test]
    fn texb0004_without_mp4_downgrades_to_texb0003_layout() {
        let payload: Vec<u8> = (0..16).collect();
        let mut data = header(0, 0, 2, 2, 2, 2);
        data.extend_from_slice(b"TEXB0004\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(2, 2, 0, 0, &payload));

        let tex = Tex::parse(&data).unwrap();
        assert_eq!(tex.container, ContainerVersion::Texb0004);
        assert_eq!(tex.effective_container(), ContainerVersion::Texb0003);
        assert!(tex.fif.is_raw());
        assert!(!tex.is_video());
        assert_eq!(tex.images[0].mipmaps[0].payload, &payload[..]);
    }

    #[test]
    fn texb0004_mp4_uses_v4_mip_layout_and_is_video() {
        let mp4 = b"\x00\x00\x00\x20ftypisom-fake-video-bytes";
        let mut data = header(0, 0, 2, 2, 2, 2);
        data.extend_from_slice(b"TEXB0004\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&(-1i32).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(b"{}\0");
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(2, 2, 0, 0, mp4));

        let tex = Tex::parse(&data).unwrap();
        assert_eq!(tex.container, ContainerVersion::Texb0004);
        assert_eq!(tex.effective_container(), ContainerVersion::Texb0004);
        assert!(tex.fif.is_mp4());
        assert!(tex.is_video_mp4);
        assert!(tex.is_video());
        assert_eq!(tex.video_payload().unwrap().as_ref(), &mp4[..]);
        assert!(matches!(tex.decode_rgba8(0, 0), Err(TexError::IsVideo)));
    }

    #[test]
    fn video_flag_marks_texture_as_video() {
        let mp4 = b"\x00\x00\x00\x20ftypisomvideo";
        let data = simple_tex(0, 0x22, 2, 2, mp4);
        let tex = Tex::parse(&data).unwrap();
        assert!(tex.flags.video());
        assert!(!tex.is_video_mp4);
        assert!(tex.is_video());
        assert_eq!(tex.video_payload().unwrap().as_ref(), &mp4[..]);
        assert!(matches!(tex.decode_rgba8(0, 0), Err(TexError::IsVideo)));

        let data = simple_tex(0, 2, 1, 1, &[0; 4]);
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(tex.video_payload(), Err(TexError::NotVideo)));
    }

    #[test]
    fn texb0001_mip_layout_has_no_compression_fields() {
        let payload = [9u8; 4];
        let mut data = header(0, 0, 1, 1, 1, 1);
        data.extend_from_slice(b"TEXB0001\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(&payload);

        let tex = Tex::parse(&data).unwrap();
        assert_eq!(tex.container, ContainerVersion::Texb0001);
        assert!(tex.fif.is_raw());
        let mip = &tex.images[0].mipmaps[0];
        assert_eq!(mip.compression, Compression::Stored);
        assert_eq!(mip.data().unwrap().as_ref(), &payload[..]);
    }

    fn gif_tex(texs: &[u8]) -> Vec<u8> {
        let mut data = simple_tex(0, TextureFlags::IS_GIF, 2, 2, &[0u8; 16]);
        data.extend_from_slice(texs);
        data
    }

    fn frame_v23(frame: &Frame) -> Vec<u8> {
        let mut v = frame.frame_number.to_le_bytes().to_vec();
        for f in [
            frame.frametime,
            frame.x,
            frame.y,
            frame.width1,
            frame.width2,
            frame.height2,
            frame.height1,
        ] {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v
    }

    fn plain_frame(frame_number: u32, frametime: f32, x: f32, y: f32, w: f32, h: f32) -> Frame {
        Frame {
            frame_number,
            frametime,
            x,
            y,
            width1: w,
            width2: 0.0,
            height2: 0.0,
            height1: h,
        }
    }

    #[test]
    fn parses_texs0003_animation_block() {
        let mut texs = b"TEXS0003\0".to_vec();
        texs.extend_from_slice(&2u32.to_le_bytes());
        texs.extend_from_slice(&201u32.to_le_bytes());
        texs.extend_from_slice(&201u32.to_le_bytes());
        texs.extend_from_slice(&frame_v23(&plain_frame(0, 0.5, 0.0, 0.0, 201.0, 201.0)));
        texs.extend_from_slice(&frame_v23(&plain_frame(0, 0.5, 201.0, 0.0, 201.0, 201.0)));

        let data = gif_tex(&texs);
        let tex = Tex::parse(&data).unwrap();
        let anim = tex.animation.as_ref().unwrap();
        assert_eq!(anim.version, AnimationVersion::Texs0003);
        assert_eq!((anim.gif_width, anim.gif_height), (201, 201));
        assert_eq!(anim.frames.len(), 2);
        assert_eq!(anim.frames[1], plain_frame(0, 0.5, 201.0, 0.0, 201.0, 201.0));
    }

    #[test]
    fn texs0002_backfills_gif_dims_from_frame_zero() {
        let mut texs = b"TEXS0002\0".to_vec();
        texs.extend_from_slice(&1u32.to_le_bytes());
        texs.extend_from_slice(&frame_v23(&Frame {
            frame_number: 3,
            frametime: 0.1,
            x: 4.0,
            y: 8.0,
            width1: 64.0,
            width2: 1.0,
            height2: 2.0,
            height1: 32.0,
        }));
        let data = gif_tex(&texs);
        let tex = Tex::parse(&data).unwrap();
        let anim = tex.animation.as_ref().unwrap();
        assert_eq!(anim.version, AnimationVersion::Texs0002);
        assert_eq!((anim.gif_width, anim.gif_height), (64, 32));
        assert_eq!(anim.frames[0].frame_number, 3);
        assert_eq!(anim.frames[0].width2, 1.0);
        assert_eq!(anim.frames[0].height2, 2.0);
    }

    #[test]
    fn texs0001_frames_use_integer_coords_with_unused_middle_fields() {
        let mut texs = b"TEXS0001\0".to_vec();
        texs.extend_from_slice(&1u32.to_le_bytes());
        texs.extend_from_slice(&7u32.to_le_bytes());
        texs.extend_from_slice(&0.25f32.to_le_bytes());
        for word in [10u32, 20, 30, 999, 888, 40] {
            texs.extend_from_slice(&word.to_le_bytes());
        }
        let data = gif_tex(&texs);
        let tex = Tex::parse(&data).unwrap();
        let anim = tex.animation.as_ref().unwrap();
        assert_eq!(anim.version, AnimationVersion::Texs0001);
        assert_eq!(
            anim.frames[0],
            Frame {
                frame_number: 7,
                frametime: 0.25,
                x: 10.0,
                y: 20.0,
                width1: 30.0,
                width2: 0.0,
                height2: 0.0,
                height1: 40.0,
            }
        );
        assert_eq!((anim.gif_width, anim.gif_height), (30, 40));
    }

    #[test]
    fn rejects_bad_or_missing_texs_block() {
        let mut texs = b"TEXS0004\0".to_vec();
        texs.extend_from_slice(&0u32.to_le_bytes());
        let data = gif_tex(&texs);
        assert!(matches!(
            Tex::parse(&data),
            Err(TexError::BadMagic {
                what: "animation block",
                ..
            })
        ));

        let data = simple_tex(0, TextureFlags::IS_GIF, 2, 2, &[0u8; 16]);
        assert!(matches!(Tex::parse(&data), Err(TexError::Truncated { .. })));
    }

    #[test]
    fn expands_rg88_and_r8() {
        let data = simple_tex(8, 0, 1, 2, &[1, 2, 3, 4]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [1, 2, 0, 255, 3, 4, 0, 255]);

        let data = simple_tex(9, 0, 3, 1, &[10, 20, 30]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255]);
    }

    #[test]
    fn decodes_dxt_blocks() {
        let dxt1_white = [0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0];
        let data = simple_tex(7, 0, 4, 4, &dxt1_white);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 64);
        assert!(
            img.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [255, 255, 255, 255])
        );

        let mut dxt5_white = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0];
        dxt5_white.extend_from_slice(&dxt1_white);
        let data = simple_tex(4, 0, 4, 4, &dxt5_white);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert!(
            img.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [255, 255, 255, 255])
        );

        let mut dxt3_white = vec![0xFF; 8];
        dxt3_white.extend_from_slice(&dxt1_white);
        let data = simple_tex(6, 0, 4, 4, &dxt3_white);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert!(
            img.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [255, 255, 255, 255])
        );
    }

    #[test]
    fn decodes_fif_png_payload() {
        let mut png = Vec::new();
        let rgba = image::RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8 * 10, y as u8 * 10, 7, 255]));
        image::DynamicImage::ImageRgba8(rgba.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let mut data = header(0, 0, 4, 4, 3, 2);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&13i32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(9, 9, 0, 0, &png));

        let tex = Tex::parse(&data).unwrap();
        assert_eq!(tex.fif, FreeImageFormat::PNG);
        let img = tex.decode_rgba8(0, 0).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.pixels, rgba.into_raw());

        let mut data = header(0, 0, 4, 4, 3, 2);
        data.extend_from_slice(b"TEXB0003\0");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&13i32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&mip_v3(1, 1, 0, 0, b"not a png"));
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.decode_rgba8(0, 0),
            Err(TexError::ImageDecode { .. })
        ));
    }

    #[test]
    fn decode_rejects_wrong_sizes_and_unsupported_formats() {
        let data = simple_tex(0, 0, 2, 2, &[0u8; 15]);
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.decode_rgba8(0, 0),
            Err(TexError::WrongPayloadSize {
                expected: 16,
                actual: 15,
                ..
            })
        ));

        let data = simple_tex(0xFFFF_FFFF, 0, 1, 1, &[0u8; 4]);
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.decode_rgba8(0, 0),
            Err(TexError::UnsupportedFormat {
                format: TextureFormat::Unknown
            })
        ));

        let data = simple_tex(0, 0, 1, 1, &[0u8; 4]);
        let tex = Tex::parse(&data).unwrap();
        assert!(matches!(
            tex.decode_rgba8(1, 0),
            Err(TexError::NoSuchImage { index: 1, count: 1 })
        ));
        assert!(matches!(
            tex.decode_rgba8(0, 9),
            Err(TexError::NoSuchMipmap { index: 9, count: 1 })
        ));
    }

    #[test]
    fn half_to_unorm8_handles_normals_subnormals_and_specials() {
        assert_eq!(half_to_unorm8(0x0000), 0);
        assert_eq!(half_to_unorm8(0x8000), 0);
        assert_eq!(half_to_unorm8(0x3C00), 255);
        assert_eq!(half_to_unorm8(0x3800), 128);
        assert_eq!(half_to_unorm8(0x3400), 64);
        assert_eq!(half_to_unorm8(0x7C00), 255);
        assert_eq!(half_to_unorm8(0xFC00), 0);
        assert_eq!(half_to_unorm8(0x7E00), 0);
        assert_eq!(half_to_unorm8(0xBC00), 0);
        assert_eq!(half_to_unorm8(0x0001), 0);
    }

    #[test]
    fn decodes_all_added_raw_formats() {
        let data = simple_tex(1, 0, 2, 1, &[10, 20, 30, 40, 50, 60]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4 * 2);
        assert_eq!(img.pixels, [10, 20, 30, 255, 40, 50, 60, 255]);

        let data = simple_tex(2, 0, 1, 1, &[0x00, 0xF8]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [255, 0, 0, 255]);
        let data = simple_tex(2, 0, 2, 1, &[0xE0, 0x07, 0x1F, 0x00]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [0, 255, 0, 255, 0, 0, 255, 255]);

        let data = simple_tex(10, 0, 1, 1, &[0x00, 0x3C, 0x00, 0x38]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4);
        assert_eq!(img.pixels, [255, 128, 0, 255]);

        let data = simple_tex(11, 0, 1, 1, &[0x00, 0x3C]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [255, 0, 0, 255]);

        let bc7_white = [
            0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0, 0, 0, 0, 0, 0, 0,
        ];
        let data = simple_tex(12, 0, 4, 4, &bc7_white);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4 * 16);
        assert!(
            img.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [255, 255, 255, 255]),
            "BC7 mode-6 solid block must decode to opaque white"
        );

        let data = simple_tex(13, 0, 1, 1, &[0xFF, 0x03, 0x00, 0xC0]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4);
        assert_eq!(img.pixels, [255, 0, 0, 255]);
        let data = simple_tex(13, 0, 1, 1, &[0x00, 0xFC, 0x0F, 0x00]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels, [0, 255, 0, 0]);

        let data = simple_tex(14, 0, 1, 1, &[0x00, 0x3C, 0x00, 0x38, 0x00, 0x00, 0x00, 0x3C]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4);
        assert_eq!(img.pixels, [255, 128, 0, 255]);

        let data = simple_tex(15, 0, 1, 1, &[0x00, 0x3C, 0x00, 0x38, 0x00, 0x34]);
        let img = Tex::parse(&data).unwrap().decode_rgba8(0, 0).unwrap();
        assert_eq!(img.pixels.len(), 4);
        assert_eq!(img.pixels, [255, 128, 64, 255]);
    }

    #[test]
    fn added_formats_reject_wrong_payload_sizes() {
        for (value, w, h, good_len) in [
            (1u32, 2u32, 1u32, 6usize),
            (2, 2, 1, 4),
            (10, 1, 1, 4),
            (11, 1, 1, 2),
            (12, 4, 4, 16),
            (13, 1, 1, 4),
            (14, 1, 1, 8),
            (15, 1, 1, 6),
        ] {
            let data = simple_tex(value, 0, w, h, &vec![0u8; good_len - 1]);
            let tex = Tex::parse(&data).unwrap();
            assert!(
                matches!(tex.decode_rgba8(0, 0), Err(TexError::WrongPayloadSize { .. })),
                "format {value} must reject a short payload"
            );
        }
    }

    #[test]
    fn flag_accessors_match_bit_values() {
        let f = TextureFlags(0x1 | 0x2 | 0x4 | 0x8 | 0x20 | 0x8_0000);
        assert!(f.no_interpolation());
        assert!(f.clamp_uvs());
        assert!(f.is_gif());
        assert!(f.clamp_uvs_border());
        assert!(f.video());
        assert!(f.alpha_channel_priority());
        let none = TextureFlags(0x10);
        assert!(!none.no_interpolation() && !none.video() && !none.is_gif());
    }

    const CORPUS_DIR: &str = "/home/aiko/.steam/steam/steamapps/workshop/content/431960";
    const CORPUS_TEX_COUNT: usize = 190;
    const CORPUS_VIDEO_COUNT: usize = 3;

    fn corpus_dir() -> Option<PathBuf> {
        let dir = std::env::var_os("KIRIE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(CORPUS_DIR));
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!(
                "skipping corpus test: {} not found (set KIRIE_CORPUS to override)",
                dir.display()
            );
            None
        }
    }

    fn corpus_scene_pkgs(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|item| item.path().join("scene.pkg"))
            .filter(|p| p.is_file())
            .filter(|p| {
                std::fs::read(p)
                    .map(|bytes| {
                        bytes.starts_with(b"\x08PKGV") || bytes.windows(4).take(16).any(|w| w == b"PKGV")
                    })
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();
        paths
    }

    fn for_each_corpus_tex(dir: &Path, mut visit: impl FnMut(&Path, &str, &[u8])) {
        for path in corpus_scene_pkgs(dir) {
            let pkg =
                crate::pkg::OwnedPkg::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            for entry in pkg.entries() {
                let Some(name) = entry.name_str() else { continue };
                if !name.ends_with(".tex") {
                    continue;
                }
                let payload = pkg
                    .read(&entry)
                    .unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
                visit(&path, name, payload);
            }
        }
    }

    #[test]
    fn corpus_every_tex_parses_with_spec_distributions() {
        let Some(dir) = corpus_dir() else { return };

        let mut total = 0usize;
        let mut videos = 0usize;
        let mut containers: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut formats: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut fifs: BTreeMap<i32, usize> = BTreeMap::new();
        let mut flags: BTreeMap<u32, usize> = BTreeMap::new();
        let mut chain_lengths: BTreeMap<usize, usize> = BTreeMap::new();
        let mut animations = Vec::new();

        for_each_corpus_tex(&dir, |path, name, payload| {
            let tex = Tex::parse(payload).unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            total += 1;

            assert_eq!(tex.images.len(), 1, "{name}: imageCount != 1");

            *containers
                .entry(match tex.container {
                    ContainerVersion::Texb0001 => "TEXB0001",
                    ContainerVersion::Texb0002 => "TEXB0002",
                    ContainerVersion::Texb0003 => "TEXB0003",
                    ContainerVersion::Texb0004 => "TEXB0004",
                })
                .or_default() += 1;
            assert_eq!(tex.effective_container(), ContainerVersion::Texb0003, "{name}");
            assert!(!tex.is_video_mp4, "{name}: §4 — no corpus file sets isVideoMp4");

            *formats
                .entry(match tex.format {
                    TextureFormat::Argb8888 => "ARGB8888",
                    TextureFormat::R8 => "R8",
                    TextureFormat::Dxt5 => "DXT5",
                    TextureFormat::Rg88 => "RG88",
                    other => panic!("{name}: unexpected corpus format {other:?}"),
                })
                .or_default() += 1;
            *fifs.entry(tex.fif.0).or_default() += 1;
            *flags.entry(tex.flags.0).or_default() += 1;
            for image in &tex.images {
                *chain_lengths.entry(image.mipmaps.len()).or_default() += 1;
            }

            if tex.is_video() {
                videos += 1;
                let bytes = tex.video_payload().unwrap();
                assert_eq!(bytes.get(4..12), Some(&b"ftypisom"[..]), "{name}");
            }

            assert_eq!(tex.animation.is_some(), tex.flags.is_gif(), "{name}");
            if let Some(anim) = tex.animation.clone() {
                animations.push(anim);
            }
        });

        assert_eq!(
            total, CORPUS_TEX_COUNT,
            "corpus .tex count changed vs docs/format-tex.md §11"
        );
        assert_eq!(videos, CORPUS_VIDEO_COUNT, "video texture count vs §7.3");

        assert_eq!(containers, BTreeMap::from([("TEXB0003", 101), ("TEXB0004", 89)]));
        assert_eq!(
            formats,
            BTreeMap::from([("ARGB8888", 79), ("R8", 60), ("DXT5", 26), ("RG88", 25)])
        );
        assert_eq!(fifs, BTreeMap::from([(-1, 158), (13, 28), (2, 4)]));
        assert_eq!(
            flags,
            BTreeMap::from([(2, 171), (0, 11), (3, 4), (34, 3), (6, 1)])
        );
        assert_eq!(
            chain_lengths,
            BTreeMap::from([
                (1, 128),
                (2, 2),
                (3, 1),
                (4, 27),
                (5, 12),
                (6, 5),
                (8, 4),
                (9, 6),
                (11, 5)
            ])
        );

        assert_eq!(animations.len(), 1, "§11: exactly one TEXS block in corpus");
        let anim = animations.first().unwrap();
        assert_eq!(anim.version, AnimationVersion::Texs0003);
        assert_eq!((anim.gif_width, anim.gif_height), (201, 201));
        assert_eq!(anim.frames.len(), 39);
        let total_time: f32 = anim.frames.iter().map(|f| f.frametime).sum();
        assert!((total_time - 1.0).abs() < 1e-4, "total {total_time}");
        for frame in &anim.frames {
            assert_eq!(frame.frame_number, 0);
            assert!((frame.frametime - 1.0 / 39.0).abs() < 1e-6);
            assert_eq!((frame.width1, frame.height1), (201.0, 201.0));
            assert_eq!((frame.width2, frame.height2), (0.0, 0.0));
        }
    }

    #[test]
    fn corpus_top_mip_of_every_non_video_tex_decodes_to_rgba8() {
        let Some(dir) = corpus_dir() else { return };

        let mut decoded = 0usize;
        let mut skipped_videos = 0usize;
        for_each_corpus_tex(&dir, |path, name, payload| {
            let tex = Tex::parse(payload).unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            if tex.is_video() {
                skipped_videos += 1;
                return;
            }
            let img = tex
                .decode_rgba8(0, 0)
                .unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            assert_eq!(
                img.pixels.len(),
                4 * img.width as usize * img.height as usize,
                "{name}: pixel byte length"
            );
            assert!(img.width > 0 && img.height > 0, "{name}: empty decode");
            let mip = &tex.images[0].mipmaps[0];
            if tex.fif.is_raw() {
                assert_eq!((img.width, img.height), (mip.width, mip.height), "{name}");
                assert_eq!(
                    (mip.width, mip.height),
                    (tex.texture_width, tex.texture_height),
                    "{name}: §7.1 mip-0 dims == textureWidth/Height"
                );
            } else {
                assert_eq!((img.width, img.height), (mip.width, mip.height), "{name}");
            }
            decoded += 1;
        });

        assert_eq!(decoded + skipped_videos, CORPUS_TEX_COUNT);
        assert_eq!(skipped_videos, CORPUS_VIDEO_COUNT);
    }

    #[test]
    fn corpus_every_mip_of_every_non_video_tex_has_consistent_sizes() {
        let Some(dir) = corpus_dir() else { return };

        let mut raw_mips = 0usize;
        for_each_corpus_tex(&dir, |path, name, payload| {
            let tex = Tex::parse(payload).unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            if tex.is_video() || !tex.fif.is_raw() {
                return;
            }
            for image in &tex.images {
                for mip in &image.mipmaps {
                    let expected = expected_payload_len(tex.format, mip.width, mip.height)
                        .unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
                    assert_eq!(
                        mip.uncompressed_size, expected,
                        "{name}: {}x{} {:?}",
                        mip.width, mip.height, tex.format
                    );
                    raw_mips += 1;
                }
            }
        });
        assert_eq!(raw_mips, 331, "raw corpus mip count vs docs/format-tex.md §7.1");
    }

    #[test]
    fn corpus_any_added_format_tex_decodes_non_uniform() {
        let Some(dir) = corpus_dir() else { return };

        let added = |f: TextureFormat| {
            matches!(
                f,
                TextureFormat::Rgb888
                    | TextureFormat::Rgb565
                    | TextureFormat::Rg1616f
                    | TextureFormat::R16f
                    | TextureFormat::Bc7
                    | TextureFormat::Rgba1010102
                    | TextureFormat::Rgba16161616f
                    | TextureFormat::Rgb161616f
            )
        };

        let mut checked = 0usize;
        for_each_corpus_tex(&dir, |path, name, payload| {
            let tex = Tex::parse(payload).unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            if tex.is_video() || !tex.fif.is_raw() || !added(tex.format) {
                return;
            }
            let img = tex
                .decode_rgba8(0, 0)
                .unwrap_or_else(|e| panic!("{}: {name}: {e}", path.display()));
            assert_eq!(
                img.pixels.len(),
                4 * img.width as usize * img.height as usize,
                "{name}: pixel byte length"
            );
            let first = img.pixels.as_chunks::<4>().0.first().copied();
            assert!(
                img.pixels.as_chunks::<4>().0.iter().any(|px| Some(*px) != first),
                "{name}: {:?} decoded to a uniform color — likely a misdecode",
                tex.format
            );
            checked += 1;
        });
        eprintln!("corpus_any_added_format_tex_decodes_non_uniform: checked {checked} texture(s)");
    }
}
