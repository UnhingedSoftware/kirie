//! Hardware video decode: which ffmpeg device each platform asks for, and the
//! copy back to system memory that the rest of the pipeline expects.
//!
//! Windows always tries D3D11VA and then DXVA2. Both are in the ffmpeg the
//! release build gets from vcpkg, and both reach whichever GPU drives the
//! screen through its own video engine (NVDEC, Quick Sync, VCN) without any
//! vendor SDK. Linux asks for VAAPI when built with the `vaapi` feature.
//! Anywhere else, or when nothing here works for a stream, frames are decoded
//! on the CPU as before. `KIRIE_NO_HWDEC` turns this off entirely.
#![allow(unsafe_code)]

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use ffmpeg_next::format::Pixel;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HwAttachError {
    #[error("no decoder for {0:?}")]
    DecoderNotFound(ffmpeg::codec::Id),
    #[error("codec {codec} has no {backend} support")]
    Unsupported { codec: String, backend: &'static str },
    #[error("{backend} device creation failed: {err}")]
    Device {
        backend: &'static str,
        err: ffmpeg::Error,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Backend {
    pub(crate) name: &'static str,
    device: ffi::AVHWDeviceType,
}

#[cfg(windows)]
const BACKENDS: &[Backend] = &[
    Backend {
        name: "D3D11VA",
        device: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    },
    Backend {
        name: "DXVA2",
        device: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
    },
];

#[cfg(all(not(windows), feature = "vaapi"))]
const BACKENDS: &[Backend] = &[Backend {
    name: "VAAPI",
    device: ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
}];

#[cfg(all(not(windows), not(feature = "vaapi")))]
const BACKENDS: &[Backend] = &[];

/// The hardware decoders worth trying on this platform, best first.
pub(crate) fn backends() -> &'static [Backend] {
    if std::env::var_os("KIRIE_NO_HWDEC").is_some() {
        return &[];
    }
    BACKENDS
}

/// Gives `ctx` a hardware device of `backend`'s type, and answers the pixel
/// format its decoded frames will carry. libavcodec's default `get_format`
/// then picks that format whenever the stream's profile is one the hardware
/// takes, and falls back to a software format by itself when it is not.
pub(crate) fn attach(
    ctx: &mut ffmpeg::codec::context::Context,
    backend: Backend,
) -> Result<Pixel, HwAttachError> {
    let id = ctx.id();
    let codec = ffmpeg::codec::decoder::find(id).ok_or(HwAttachError::DecoderNotFound(id))?;

    let mut format = None;
    for index in 0.. {
        // SAFETY: `codec.as_ptr()` is the valid, program-lifetime AVCodec
        // `find` returned -- ffmpeg's codec descriptors are static and are
        // never freed. `avcodec_get_hw_config` answers null once `index`
        // runs past the last configuration, which is what ends the loop,
        // and `as_ref` turns exactly that null into `None` rather than
        // dereferencing it. The borrow lasts only as long as `config`,
        // which is read and dropped inside this iteration.
        let config = unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), index).as_ref() };
        let Some(config) = config else { break };
        if config.device_type == backend.device
            && config.methods & (ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
        {
            format = Some(Pixel::from(config.pix_fmt));
            break;
        }
    }
    let Some(format) = format else {
        return Err(HwAttachError::Unsupported {
            codec: codec.name().to_owned(),
            backend: backend.name,
        });
    };

    let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `&mut device` is a valid out-pointer; NULL device path + NULL
    // options ask ffmpeg to open the default device of this type (for D3D11VA
    // and DXVA2 that is adapter 0, the GPU driving the main screen), which is
    // what this function is for. On success `device` owns one reference to a
    // fresh AVBufferRef; on failure ffmpeg leaves it null and the error is
    // returned below before anything reads it.
    let ret = unsafe {
        ffi::av_hwdevice_ctx_create(
            &mut device,
            backend.device,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret < 0 {
        return Err(HwAttachError::Device {
            backend: backend.name,
            err: ffmpeg::Error::from(ret),
        });
    }

    // SAFETY: `ctx` wraps a live, not-yet-opened AVCodecContext (owned by
    // the caller, borrowed mutably here), so writing one of its fields
    // aliases nothing. `hw_device_ctx` is documented as being set by the
    // caller and owned and freed by libavcodec afterwards, so handing the
    // reference over rather than taking another one is the contract, and
    // the field was null until now -- the caller makes a fresh context for
    // every backend it tries and attaches to it once, before it is opened --
    // so nothing is overwritten and leaked.
    unsafe {
        (*ctx.as_mut_ptr()).hw_device_ctx = device;
    }
    Ok(format)
}

/// The hardware a decoder was opened with, for `HwDownload`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Attached {
    pub(crate) name: &'static str,
    pub(crate) format: Pixel,
}

pub(crate) struct HwDownload {
    attached: Option<Attached>,
    frame: ffmpeg::frame::Video,
    announced: bool,
}

impl HwDownload {
    pub(crate) fn new(attached: Option<Attached>) -> Self {
        Self {
            attached,
            frame: ffmpeg::frame::Video::empty(),
            announced: false,
        }
    }

    /// Copies a frame that lives on the GPU into system memory (NV12 for
    /// 8-bit video, P010 for 10-bit) and answers it; answers `None` for a
    /// frame the CPU decoded, which needs no copy.
    pub(crate) fn download(
        &mut self,
        src: &ffmpeg::frame::Video,
    ) -> Result<Option<&ffmpeg::frame::Video>, ffmpeg::Error> {
        let Some(attached) = self.attached else {
            return Ok(None);
        };
        let on_gpu = src.format() == attached.format;
        if !self.announced {
            self.announced = true;
            if on_gpu {
                tracing::info!(backend = attached.name, "hardware video decode active");
            } else {
                tracing::info!(
                    backend = attached.name,
                    format = ?src.format(),
                    "the GPU's decoder does not take this video's profile; decoding on the CPU"
                );
            }
        }
        if !on_gpu {
            return Ok(None);
        }

        if self.frame.width() != src.width() || self.frame.height() != src.height() {
            // SAFETY: `self.frame` is a valid owned AVFrame; av_frame_unref
            // drops whatever buffers it holds and leaves it clean and
            // reusable, which is exactly what is wanted before a transfer
            // into a different size. It takes no ownership of the pointer.
            unsafe { ffi::av_frame_unref(self.frame.as_mut_ptr()) };
        }
        // SAFETY: dst is a valid owned AVFrame — either clean (FFmpeg
        // allocates it on first use) or holding buffers of the same size as
        // this source, which is the case av_hwframe_transfer_data can reuse.
        // `src` is borrowed for the call and only read. A negative return is
        // an ordinary error, handled below; it never leaves dst in a state
        // that is unsound to unref.
        let mut ret = unsafe { ffi::av_hwframe_transfer_data(self.frame.as_mut_ptr(), src.as_ptr(), 0) };
        if ret < 0 {
            // SAFETY: same as the av_frame_unref above.
            unsafe { ffi::av_frame_unref(self.frame.as_mut_ptr()) };
            // SAFETY: same as the transfer above, with dst now clean.
            ret = unsafe { ffi::av_hwframe_transfer_data(self.frame.as_mut_ptr(), src.as_ptr(), 0) };
        }
        if ret < 0 {
            return Err(ffmpeg::Error::from(ret));
        }
        // The transfer copies pixels only; the scaler and the NV12 path read
        // colour details off the frame they are handed.
        self.frame.set_color_space(src.color_space());
        self.frame.set_color_range(src.color_range());
        Ok(Some(&self.frame))
    }
}
