use std::path::PathBuf;

use kirie_formats::tex::TexError;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Tex(#[from] TexError),

    #[error(transparent)]
    Image(#[from] image::ImageError),

    #[error("video .tex is not image content (docs/format-tex.md §7.3)")]
    VideoTex,

    #[error("texture contains no images")]
    NoImages,

    #[error("texture image {image} has no mip levels")]
    NoMipmaps { image: usize },

    #[error(
        "animation frame {frame} references image {page}, but only {pages} exist (docs/format-tex.md §8)"
    )]
    FramePageOutOfRange { frame: usize, page: usize, pages: usize },

    #[error("animated texture has an empty frame table (docs/format-tex.md §8)")]
    EmptyAnimation,

    #[error("zero-sized image content ({width}x{height})")]
    InvalidDimensions { width: u32, height: u32 },

    #[error("gif frame is {got_width}x{got_height}, expected canvas {width}x{height}")]
    FrameSizeMismatch {
        width: u32,
        height: u32,
        got_width: u32,
        got_height: u32,
    },

    #[error("image page {width}x{height} exceeds the device texture limit {max}")]
    TextureTooLarge { width: u32, height: u32, max: u32 },

    #[error("unknown scaling mode {0:?} (expected stretch|fit|fill|default, docs/compat-cli.md §2)")]
    BadScalingMode(String),

    #[error("unknown clamp mode {0:?} (expected clamp|border|repeat, docs/compat-cli.md §2)")]
    BadClampMode(String),
}
