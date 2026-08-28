#![allow(unsafe_code)]

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use ffmpeg_next::format::Pixel;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HwAttachError {
    #[error("no decoder for {0:?}")]
    DecoderNotFound(ffmpeg::codec::Id),
    #[error("codec {0} has no VAAPI hw-device support")]
    Unsupported(String),
    #[error("VAAPI device creation failed: {0}")]
    Device(ffmpeg::Error),
}

pub(crate) fn attach_vaapi(ctx: &mut ffmpeg::codec::context::Context) -> Result<(), HwAttachError> {
    let id = ctx.id();
    let codec = ffmpeg::codec::decoder::find(id).ok_or(HwAttachError::DecoderNotFound(id))?;

    let mut supported = false;
    for index in 0.. {
        // SAFETY: `codec.as_ptr()` is the valid, program-lifetime AVCodec
        let config = unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), index).as_ref() };
        let Some(config) = config else { break };
        if config.device_type == ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI
            && config.methods & (ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
        {
            supported = true;
            break;
        }
    }
    if !supported {
        return Err(HwAttachError::Unsupported(codec.name().to_owned()));
    }

    let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `&mut device` is a valid out-pointer; NULL device path + NULL
    let ret = unsafe {
        ffi::av_hwdevice_ctx_create(
            &mut device,
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret < 0 {
        return Err(HwAttachError::Device(ffmpeg::Error::from(ret)));
    }

    // SAFETY: `ctx` wraps a live, not-yet-opened AVCodecContext (owned by
    unsafe {
        (*ctx.as_mut_ptr()).hw_device_ctx = device;
    }
    Ok(())
}

pub(crate) struct HwDownload {
    frame: ffmpeg::frame::Video,
    announced: bool,
}

impl HwDownload {
    pub(crate) fn new() -> Self {
        Self {
            frame: ffmpeg::frame::Video::empty(),
            announced: false,
        }
    }

    pub(crate) fn download(
        &mut self,
        src: &ffmpeg::frame::Video,
    ) -> Result<Option<&ffmpeg::frame::Video>, ffmpeg::Error> {
        if src.format() != Pixel::VAAPI {
            return Ok(None);
        }
        if !self.announced {
            tracing::info!("VAAPI hardware decode active");
            self.announced = true;
        }

        if self.frame.width() != src.width() || self.frame.height() != src.height() {
            // SAFETY: `self.frame` is a valid owned AVFrame; av_frame_unref
            unsafe { ffi::av_frame_unref(self.frame.as_mut_ptr()) };
        }
        // SAFETY: dst is a valid owned AVFrame — either clean (FFmpeg
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
        Ok(Some(&self.frame))
    }
}
