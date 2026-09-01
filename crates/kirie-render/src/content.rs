use std::fs;
use std::io::BufReader;
use std::path::Path;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use kirie_formats::tex::Tex;

use crate::error::RenderError;
use crate::schedule::FrameSchedule;

#[derive(Debug, Clone)]
pub struct ImagePage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FramePlacement {
    pub page: usize,
    pub duration: f32,
    pub translation: [f32; 2],
    pub axes: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplerSpec {
    pub nearest: bool,
    pub clamp_uvs: bool,
}

#[derive(Debug, Clone)]
pub struct ImageContent {
    pub pages: Vec<ImagePage>,
    pub frames: Vec<FramePlacement>,
    pub sampler: SamplerSpec,
    pub content_width: u32,
    pub content_height: u32,
}

impl ImageContent {
    pub fn pad_pages_to_max(&mut self) {
        let (Some(width), Some(height)) = (
            self.pages.iter().map(|p| p.width).max(),
            self.pages.iter().map(|p| p.height).max(),
        ) else {
            return;
        };
        let sizes: Vec<(u32, u32)> = self.pages.iter().map(|p| (p.width, p.height)).collect();
        if sizes.iter().all(|size| *size == (width, height)) {
            return;
        }
        for frame in &mut self.frames {
            let Some((page_width, page_height)) = sizes.get(frame.page).copied() else {
                continue;
            };
            let sx = page_width as f32 / width as f32;
            let sy = page_height as f32 / height as f32;
            frame.translation[0] *= sx;
            frame.translation[1] *= sy;
            frame.axes[0] *= sx;
            frame.axes[1] *= sx;
            frame.axes[2] *= sy;
            frame.axes[3] *= sy;
        }
        let stride = width as usize * 4;
        for page in &mut self.pages {
            if (page.width, page.height) == (width, height) {
                continue;
            }
            let mut padded = vec![0_u8; stride * height as usize];
            let row_bytes = page.width as usize * 4;
            for row in 0..page.height as usize {
                let from = row * row_bytes;
                let to = row * stride;
                let Some(source) = page.pixels.get(from..from + row_bytes) else {
                    break;
                };
                padded[to..to + row_bytes].copy_from_slice(source);
            }
            page.width = width;
            page.height = height;
            page.pixels = padded;
        }
    }

    pub fn from_path(path: &Path) -> Result<Self, RenderError> {
        let is_tex = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tex"));
        if is_tex {
            let bytes = fs::read(path).map_err(|source| RenderError::Io {
                path: path.to_owned(),
                source,
            })?;
            return Self::from_tex_bytes(&bytes);
        }

        let file = fs::File::open(path).map_err(|source| RenderError::Io {
            path: path.to_owned(),
            source,
        })?;
        let reader = image::ImageReader::new(BufReader::new(file))
            .with_guessed_format()
            .map_err(|source| RenderError::Io {
                path: path.to_owned(),
                source,
            })?;

        if reader.format() == Some(image::ImageFormat::Gif) {
            Self::from_gif(reader.into_inner())
        } else {
            let rgba = reader.decode()?.into_rgba8();
            let (width, height) = rgba.dimensions();
            Self::from_single_rgba8(width, height, rgba.into_raw())
        }
    }

    pub fn from_tex_bytes(bytes: &[u8]) -> Result<Self, RenderError> {
        let tex = Tex::parse(bytes)?;
        Self::from_tex(&tex)
    }

    pub fn from_tex(tex: &Tex<'_>) -> Result<Self, RenderError> {
        if tex.is_video() || tex.fif.is_mp4() {
            return Err(RenderError::VideoTex);
        }
        if tex.images.is_empty() {
            return Err(RenderError::NoImages);
        }

        let mut pages = Vec::with_capacity(tex.images.len());
        for (index, tex_image) in tex.images.iter().enumerate() {
            if tex_image.mipmaps.is_empty() {
                return Err(RenderError::NoMipmaps { image: index });
            }
            let decoded = tex.decode_rgba8(index, 0)?;
            if decoded.width == 0 || decoded.height == 0 {
                return Err(RenderError::InvalidDimensions {
                    width: decoded.width,
                    height: decoded.height,
                });
            }
            pages.push(ImagePage {
                width: decoded.width,
                height: decoded.height,
                pixels: decoded.pixels,
            });
        }

        let sampler = SamplerSpec {
            nearest: tex.flags.no_interpolation(),
            clamp_uvs: tex.flags.clamp_uvs(),
        };

        match &tex.animation {
            Some(animation) => {
                if animation.frames.is_empty() {
                    return Err(RenderError::EmptyAnimation);
                }
                let mut frames = Vec::with_capacity(animation.frames.len());
                for (index, frame) in animation.frames.iter().enumerate() {
                    let page = frame.frame_number as usize;
                    let Some(atlas) = pages.get(page) else {
                        return Err(RenderError::FramePageOutOfRange {
                            frame: index,
                            page,
                            pages: pages.len(),
                        });
                    };
                    let w = atlas.width as f32;
                    let h = atlas.height as f32;
                    frames.push(FramePlacement {
                        page,
                        duration: frame.frametime,
                        translation: [frame.x / w, frame.y / h],
                        axes: [
                            frame.width1 / w,
                            frame.width2 / w,
                            frame.height2 / h,
                            frame.height1 / h,
                        ],
                    });
                }
                if animation.gif_width == 0 || animation.gif_height == 0 {
                    return Err(RenderError::InvalidDimensions {
                        width: animation.gif_width,
                        height: animation.gif_height,
                    });
                }
                Ok(Self {
                    pages,
                    frames,
                    sampler,
                    content_width: animation.gif_width,
                    content_height: animation.gif_height,
                })
            }
            None => {
                let page = &pages[0];
                if tex.width == 0 || tex.height == 0 {
                    return Err(RenderError::InvalidDimensions {
                        width: tex.width,
                        height: tex.height,
                    });
                }
                let u_crop = (tex.width as f32 / page.width as f32).min(1.0);
                let v_crop = (tex.height as f32 / page.height as f32).min(1.0);
                Ok(Self {
                    pages,
                    frames: vec![FramePlacement {
                        page: 0,
                        duration: 0.0,
                        translation: [0.0, 0.0],
                        axes: [u_crop, 0.0, 0.0, v_crop],
                    }],
                    sampler,
                    content_width: tex.width,
                    content_height: tex.height,
                })
            }
        }
    }

    pub fn from_single_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidDimensions { width, height });
        }
        Ok(Self {
            pages: vec![ImagePage {
                width,
                height,
                pixels,
            }],
            frames: vec![FramePlacement {
                page: 0,
                duration: 0.0,
                translation: [0.0, 0.0],
                axes: [1.0, 0.0, 0.0, 1.0],
            }],
            sampler: SamplerSpec {
                nearest: false,
                clamp_uvs: true,
            },
            content_width: width,
            content_height: height,
        })
    }

    fn from_gif<R: std::io::BufRead + std::io::Seek>(reader: R) -> Result<Self, RenderError> {
        let decoder = GifDecoder::new(reader)?;
        let frames = decoder.into_frames().collect_frames()?;

        let mut pages = Vec::with_capacity(frames.len());
        let mut placements = Vec::with_capacity(frames.len());
        let mut canvas: Option<(u32, u32)> = None;

        for frame in frames {
            let (numer_ms, denom_ms) = frame.delay().numer_denom_ms();
            let duration = if denom_ms == 0 {
                0.0
            } else {
                numer_ms as f32 / denom_ms as f32 / 1000.0
            };
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            match canvas {
                None => canvas = Some((width, height)),
                Some((cw, ch)) if (cw, ch) != (width, height) => {
                    return Err(RenderError::FrameSizeMismatch {
                        width: cw,
                        height: ch,
                        got_width: width,
                        got_height: height,
                    });
                }
                Some(_) => {}
            }
            placements.push(FramePlacement {
                page: pages.len(),
                duration,
                translation: [0.0, 0.0],
                axes: [1.0, 0.0, 0.0, 1.0],
            });
            pages.push(ImagePage {
                width,
                height,
                pixels: buffer.into_raw(),
            });
        }

        let Some((width, height)) = canvas else {
            return Err(RenderError::EmptyAnimation);
        };
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidDimensions { width, height });
        }
        Ok(Self {
            pages,
            frames: placements,
            sampler: SamplerSpec {
                nearest: false,
                clamp_uvs: true,
            },
            content_width: width,
            content_height: height,
        })
    }

    #[must_use]
    pub fn schedule(&self) -> FrameSchedule {
        FrameSchedule::new(self.frames.iter().map(|f| f.duration).collect())
    }

    #[must_use]
    pub fn content_size(&self) -> (u32, u32) {
        (self.content_width, self.content_height)
    }
}

#[cfg(test)]
mod tests {
    use kirie_formats::tex::{
        Animation, AnimationVersion, Compression, ContainerVersion, Frame, FreeImageFormat, Mipmap, Tex,
        TexImage, TextureFlags, TextureFormat,
    };

    use super::*;

    fn synthetic_tex<'a>(
        payload: &'a [u8],
        width: u32,
        height: u32,
        real_w: u32,
        real_h: u32,
        flags: u32,
        animation: Option<Animation>,
    ) -> Tex<'a> {
        Tex {
            format: TextureFormat::Argb8888,
            flags: TextureFlags(flags),
            texture_width: width,
            texture_height: height,
            width: real_w,
            height: real_h,
            unknown: 0,
            container: ContainerVersion::Texb0003,
            fif: FreeImageFormat::UNKNOWN,
            is_video_mp4: false,
            images: vec![TexImage {
                mipmaps: vec![Mipmap {
                    width,
                    height,
                    compression: Compression::Stored,
                    uncompressed_size: payload.len(),
                    payload,
                }],
            }],
            animation,
        }
    }

    fn rgba_page(width: u32, height: u32) -> Vec<u8> {
        (0..width * height * 4).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn static_tex_crops_npot_padding() {
        let payload = rgba_page(8, 8);
        let tex = synthetic_tex(&payload, 8, 8, 6, 5, 0, None);
        let content = ImageContent::from_tex(&tex).unwrap();
        assert_eq!(content.pages.len(), 1);
        assert_eq!(content.frames.len(), 1);
        let frame = content.frames[0];
        assert_eq!(frame.page, 0);
        assert_eq!(frame.duration, 0.0);
        assert_eq!(frame.translation, [0.0, 0.0]);
        assert_eq!(frame.axes, [0.75, 0.0, 0.0, 0.625]);
        assert_eq!(content.content_size(), (6, 5));
        assert!(!content.schedule().is_animated());
        assert_eq!(
            content.sampler,
            SamplerSpec {
                nearest: false,
                clamp_uvs: false
            }
        );
    }

    #[test]
    fn tex_flags_drive_the_sampler_spec() {
        let payload = rgba_page(4, 4);
        let tex = synthetic_tex(&payload, 4, 4, 4, 4, 0x3, None);
        let content = ImageContent::from_tex(&tex).unwrap();
        assert_eq!(
            content.sampler,
            SamplerSpec {
                nearest: true,
                clamp_uvs: true
            }
        );
    }

    #[test]
    fn animated_tex_builds_atlas_placements() {
        let payload = rgba_page(8, 4);
        let animation = Animation {
            version: AnimationVersion::Texs0003,
            gif_width: 4,
            gif_height: 4,
            frames: vec![
                Frame {
                    frame_number: 0,
                    frametime: 0.25,
                    x: 0.0,
                    y: 0.0,
                    width1: 4.0,
                    width2: 0.0,
                    height2: 0.0,
                    height1: 4.0,
                },
                Frame {
                    frame_number: 0,
                    frametime: 0.5,
                    x: 4.0,
                    y: 0.0,
                    width1: 4.0,
                    width2: 0.0,
                    height2: 0.0,
                    height1: 4.0,
                },
            ],
        };
        let tex = synthetic_tex(&payload, 8, 4, 8, 4, TextureFlags::IS_GIF, Some(animation));
        let content = ImageContent::from_tex(&tex).unwrap();
        assert_eq!(content.content_size(), (4, 4));
        assert_eq!(content.frames.len(), 2);
        assert_eq!(content.frames[0].translation, [0.0, 0.0]);
        assert_eq!(content.frames[0].axes, [0.5, 0.0, 0.0, 1.0]);
        assert_eq!(content.frames[1].translation, [0.5, 0.0]);
        assert_eq!(content.frames[1].axes, [0.5, 0.0, 0.0, 1.0]);

        let schedule = content.schedule();
        assert!(schedule.is_animated());
        assert_eq!(schedule.durations(), &[0.25, 0.5]);
        assert_eq!(schedule.frame_at(0.1), 0);
        assert_eq!(schedule.frame_at(0.3), 1);
        assert_eq!(schedule.frame_at(0.8), 0);
    }

    #[test]
    fn rotated_frames_keep_cross_axes() {
        let payload = rgba_page(8, 8);
        let animation = Animation {
            version: AnimationVersion::Texs0002,
            gif_width: 4,
            gif_height: 4,
            frames: vec![Frame {
                frame_number: 0,
                frametime: 0.1,
                x: 2.0,
                y: 4.0,
                width1: 0.0,
                width2: 4.0,
                height2: 4.0,
                height1: 0.0,
            }],
        };
        let tex = synthetic_tex(&payload, 8, 8, 8, 8, TextureFlags::IS_GIF, Some(animation));
        let content = ImageContent::from_tex(&tex).unwrap();
        assert_eq!(content.frames[0].translation, [0.25, 0.5]);
        assert_eq!(content.frames[0].axes, [0.0, 0.5, 0.5, 0.0]);
    }

    #[test]
    fn malformed_tex_content_yields_typed_errors() {
        let payload = rgba_page(4, 4);
        let animation = Animation {
            version: AnimationVersion::Texs0003,
            gif_width: 4,
            gif_height: 4,
            frames: vec![Frame {
                frame_number: 3,
                frametime: 0.1,
                x: 0.0,
                y: 0.0,
                width1: 4.0,
                width2: 0.0,
                height2: 0.0,
                height1: 4.0,
            }],
        };
        let tex = synthetic_tex(&payload, 4, 4, 4, 4, TextureFlags::IS_GIF, Some(animation));
        assert!(matches!(
            ImageContent::from_tex(&tex),
            Err(RenderError::FramePageOutOfRange {
                frame: 0,
                page: 3,
                pages: 1
            })
        ));

        let empty = Animation {
            version: AnimationVersion::Texs0003,
            gif_width: 4,
            gif_height: 4,
            frames: vec![],
        };
        let tex = synthetic_tex(&payload, 4, 4, 4, 4, TextureFlags::IS_GIF, Some(empty));
        assert!(matches!(
            ImageContent::from_tex(&tex),
            Err(RenderError::EmptyAnimation)
        ));

        let mut no_images = synthetic_tex(&payload, 4, 4, 4, 4, 0, None);
        no_images.images.clear();
        assert!(matches!(
            ImageContent::from_tex(&no_images),
            Err(RenderError::NoImages)
        ));

        let video = synthetic_tex(&payload, 4, 4, 4, 4, TextureFlags::VIDEO, None);
        assert!(matches!(
            ImageContent::from_tex(&video),
            Err(RenderError::VideoTex)
        ));
    }

    #[test]
    fn plain_animated_gif_keeps_frames_and_delays() {
        let dir = std::env::temp_dir().join("kirie-render-gif-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two-frame.gif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            let red = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
            let blue = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255]));
            let delay = image::Delay::from_numer_denom_ms(100, 1);
            encoder
                .encode_frame(image::Frame::from_parts(red, 0, 0, delay))
                .unwrap();
            encoder
                .encode_frame(image::Frame::from_parts(blue, 0, 0, delay))
                .unwrap();
        }

        let content = ImageContent::from_path(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(content.pages.len(), 2);
        assert_eq!(content.frames.len(), 2);
        assert_eq!(content.content_size(), (4, 4));
        for (index, frame) in content.frames.iter().enumerate() {
            assert_eq!(frame.page, index);
            assert_eq!(frame.translation, [0.0, 0.0]);
            assert_eq!(frame.axes, [1.0, 0.0, 0.0, 1.0]);
            assert!((frame.duration - 0.1).abs() < 1e-6, "delay {}", frame.duration);
        }
        assert_eq!(&content.pages[0].pixels[0..4], &[255, 0, 0, 255]);
        assert!(content.schedule().is_animated());
    }

    #[test]
    fn zero_sized_rgba_is_rejected() {
        assert!(matches!(
            ImageContent::from_single_rgba8(0, 4, vec![]),
            Err(RenderError::InvalidDimensions { .. })
        ));
    }
}

#[cfg(test)]
mod pad_tests {
    use super::*;

    fn page(width: u32, height: u32, fill: u8) -> ImagePage {
        ImagePage {
            width,
            height,
            pixels: vec![fill; (width as usize) * (height as usize) * 4],
        }
    }

    #[test]
    fn a_short_page_is_padded_and_its_frame_rescaled() {
        let mut content = ImageContent {
            pages: vec![page(4, 4, 1), page(4, 2, 2)],
            frames: vec![
                FramePlacement { page: 0, duration: 0.1, translation: [0.5, 0.5], axes: [1.0, 0.0, 0.0, 1.0] },
                FramePlacement { page: 1, duration: 0.1, translation: [0.5, 0.5], axes: [1.0, 0.0, 0.0, 1.0] },
            ],
            sampler: SamplerSpec { nearest: false, clamp_uvs: false },
            content_width: 4,
            content_height: 4,
        };
        content.pad_pages_to_max();
        assert_eq!((content.pages[1].width, content.pages[1].height), (4, 4));
        assert_eq!(content.pages[1].pixels.len(), 4 * 4 * 4);
        assert!((content.frames[0].translation[1] - 0.5).abs() < 1e-6);
        assert!((content.frames[1].translation[1] - 0.25).abs() < 1e-6);
        assert!((content.frames[1].axes[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn uniform_pages_are_left_alone() {
        let mut content = ImageContent {
            pages: vec![page(4, 4, 1), page(4, 4, 2)],
            frames: vec![FramePlacement { page: 1, duration: 0.1, translation: [0.5, 0.5], axes: [1.0, 0.0, 0.0, 1.0] }],
            sampler: SamplerSpec { nearest: false, clamp_uvs: false },
            content_width: 4,
            content_height: 4,
        };
        let before = content.frames[0];
        content.pad_pages_to_max();
        assert_eq!(content.frames[0], before);
    }
}
