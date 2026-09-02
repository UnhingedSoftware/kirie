use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling;

#[allow(unsafe_code)]
fn tell_scaler_the_colours(
    scaler: &mut scaling::Context,
    space: ffmpeg::color::Space,
    range: ffmpeg::color::Range,
    height: u32,
) {
    use ffmpeg::color::{Range, Space};
    use ffmpeg::ffi::{
        SWS_CS_ITU601, SWS_CS_ITU709, SWS_CS_SMPTE240M, sws_getCoefficients, sws_setColorspaceDetails,
    };

    let table = match space {
        Space::BT709 => SWS_CS_ITU709,
        Space::BT470BG | Space::SMPTE170M => SWS_CS_ITU601,
        Space::SMPTE240M => SWS_CS_SMPTE240M,
        _ if height >= 720 => SWS_CS_ITU709,
        _ => SWS_CS_ITU601,
    };
    let full = i32::from(range == Range::JPEG);

    // SAFETY: the scaler is live and the coefficient tables belong to ffmpeg.
    unsafe {
        let coefficients = sws_getCoefficients(table);
        let target = sws_getCoefficients(SWS_CS_ITU709);
        sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefficients,
            full,
            target,
            1,
            0,
            1 << 16,
            1 << 16,
        );
    }
}

use crate::error::VideoError;
use crate::pacing::{LoopTimeline, Timed};

pub const FRAME_QUEUE_CAP: usize = 4;

const FALLBACK_FRAME_DUR: f64 = 1.0 / 30.0;

#[derive(Debug)]
pub struct DecodedFrame {
    pub play_pts: f64,
    pub width: u32,
    pub height: u32,
    pub pixels: FramePixels,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePixels {
    Rgba,
    Nv12,
}

impl Timed for DecodedFrame {
    fn play_pts(&self) -> f64 {
        self.play_pts
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub duration: f64,
}

pub(crate) struct Decoder {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    stream_index: usize,
    time_base: f64,
    start: f64,
    info: VideoInfo,
    timeline: LoopTimeline,
    decoded: ffmpeg::frame::Video,
    last_raw: Option<f64>,
    undecodable: u64,
    pub(crate) want_nv12: bool,
    unconvertible: u64,
    frame_dur: f64,
    synth_pts: f64,
}

struct Converter {
    nv12: bool,
    scaler: Option<scaling::Context>,
    rgb: ffmpeg::frame::Video,
    #[cfg(feature = "vaapi")]
    hw: crate::hw::HwDownload,
}

impl Converter {
    fn new(nv12: bool) -> Self {
        Self {
            nv12,
            scaler: None,
            rgb: ffmpeg::frame::Video::empty(),
            #[cfg(feature = "vaapi")]
            hw: crate::hw::HwDownload::new(),
        }
    }
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        ffmpeg::init()?;
        let input = ffmpeg::format::input(path)?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| VideoError::NoVideoStream(PathBuf::from(path)))?;
        let stream_index = stream.index();
        let time_base = f64::from(stream.time_base());
        let start = if stream.start_time() == i64::MIN {
            0.0
        } else {
            stream.start_time() as f64 * time_base
        };
        let frame_rate = f64::from(stream.avg_frame_rate()).max(0.0);
        let decoder = open_video_decoder(&stream)?;
        let (width, height) = (decoder.width(), decoder.height());
        if width == 0 || height == 0 {
            return Err(VideoError::InvalidDimensions { width, height });
        }
        let duration = if input.duration() > 0 {
            input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            0.0
        };
        let info = VideoInfo {
            width,
            height,
            frame_rate,
            duration,
        };
        let frame_dur = if frame_rate > 0.0 {
            1.0 / frame_rate
        } else {
            FALLBACK_FRAME_DUR
        };
        Ok(Self {
            input,
            decoder,
            stream_index,
            time_base,
            start,
            info,
            timeline: LoopTimeline::new((duration > 0.0).then_some(duration)),
            decoded: ffmpeg::frame::Video::empty(),
            last_raw: None,
            undecodable: 0,
            want_nv12: false,
            unconvertible: 0,
            frame_dur,
            synth_pts: 0.0,
        })
    }

    pub fn info(&self) -> VideoInfo {
        self.info
    }

    pub fn run(mut self, frames: &Sender<DecodedFrame>, recycle: &Receiver<Vec<u8>>) {
        let mut converter = Converter::new(self.want_nv12);
        let mut consecutive_read_errors = 0u32;
        loop {
            loop {
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.input) {
                    Ok(()) => consecutive_read_errors = 0,
                    Err(ffmpeg::Error::Eof) => break,
                    Err(err) => {
                        consecutive_read_errors += 1;
                        if consecutive_read_errors > 1000 {
                            tracing::error!(%err, "video demux failing persistently; stopping");
                            return;
                        }
                        continue;
                    }
                }
                if packet.stream() != self.stream_index {
                    continue;
                }
                if let Err(err) = self.decoder.send_packet(&packet) {
                    self.undecodable += 1;
                    if self.undecodable.is_power_of_two() {
                        tracing::warn!(%err, count = self.undecodable, "skipping undecodable video packet(s)");
                    }
                    continue;
                }
                self.undecodable = 0;
                if !self.drain(&mut converter, frames, recycle) {
                    return;
                }
            }

            let _ = self.decoder.send_eof();
            if !self.drain(&mut converter, frames, recycle) {
                return;
            }
            if let Err(err) = self.input.seek(0, ..) {
                tracing::error!(%err, "loop seek to 0 failed; stopping video decode");
                return;
            }
            self.decoder.flush();
            self.timeline.wrap();
            self.last_raw = None;
            self.synth_pts = 0.0;
        }
    }

    fn drain(
        &mut self,
        converter: &mut Converter,
        frames: &Sender<DecodedFrame>,
        recycle: &Receiver<Vec<u8>>,
    ) -> bool {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => match self.convert(converter, recycle) {
                    Ok(frame) => {
                        self.unconvertible = 0;
                        if frames.send(frame).is_err() {
                            return false;
                        }
                    }
                    Err(err) => {
                        self.unconvertible += 1;
                        if self.unconvertible.is_power_of_two() {
                            tracing::warn!(%err, count = self.unconvertible, "dropping unconvertible video frame(s)");
                        }
                    }
                },
                Err(_) => return true,
            }
        }
    }

    fn convert(
        &mut self,
        converter: &mut Converter,
        recycle: &Receiver<Vec<u8>>,
    ) -> Result<DecodedFrame, VideoError> {
        #[cfg(feature = "vaapi")]
        let decoded = match converter.hw.download(&self.decoded)? {
            Some(sw) => sw,
            None => &self.decoded,
        };
        #[cfg(not(feature = "vaapi"))]
        let decoded = &self.decoded;

        let (width, height) = (decoded.width(), decoded.height());
        if width == 0 || height == 0 {
            return Err(VideoError::InvalidDimensions { width, height });
        }

        if converter.nv12 && decoded.format() == Pixel::NV12 && width % 2 == 0 && height % 2 == 0 {
            let raw = match self.decoded.timestamp().or_else(|| self.decoded.pts()) {
                Some(ts) => ts as f64 * self.time_base - self.start,
                None => self.synth_pts,
            };
            if let Some(last) = self.last_raw {
                let delta = raw - last;
                if delta > 0.0 && delta < 1.0 {
                    self.frame_dur = delta;
                }
            }
            self.last_raw = Some(raw);
            self.synth_pts = raw + self.frame_dur;
            let play_pts = self.timeline.map(raw, self.frame_dur);

            let mut data = recycle.try_recv().unwrap_or_default();
            copy_nv12(decoded, &mut data);
            return Ok(DecodedFrame {
                play_pts,
                width,
                height,
                pixels: FramePixels::Nv12,
                data,
            });
        }

        let needs_scaler = match &converter.scaler {
            None => true,
            Some(s) => {
                s.input().format != decoded.format() || s.input().width != width || s.input().height != height
            }
        };
        if needs_scaler {
            let mut fresh = scaling::Context::get(
                decoded.format(),
                width,
                height,
                Pixel::RGBA,
                width,
                height,
                scaling::Flags::FAST_BILINEAR,
            )?;
            tell_scaler_the_colours(&mut fresh, decoded.color_space(), decoded.color_range(), height);
            converter.scaler = Some(fresh);
            converter.rgb = ffmpeg::frame::Video::empty();
            if width != self.info.width || height != self.info.height {
                tracing::info!(
                    from = format!("{}x{}", self.info.width, self.info.height),
                    to = format!("{width}x{height}"),
                    "video stream geometry changed"
                );
                self.info.width = width;
                self.info.height = height;
            }
        }
        let Some(scaler) = converter.scaler.as_mut() else {
            return Err(VideoError::InvalidDimensions { width, height });
        };
        scaler.run(decoded, &mut converter.rgb)?;

        let raw = match self.decoded.timestamp().or_else(|| self.decoded.pts()) {
            Some(ts) => ts as f64 * self.time_base - self.start,
            None => self.synth_pts,
        };
        if let Some(last) = self.last_raw {
            let delta = raw - last;
            if delta > 0.0 && delta < 1.0 {
                self.frame_dur = delta;
            }
        }
        self.last_raw = Some(raw);
        self.synth_pts = raw + self.frame_dur;
        let play_pts = self.timeline.map(raw, self.frame_dur);

        let mut data = recycle.try_recv().unwrap_or_default();
        copy_rgba(&converter.rgb, &mut data);

        Ok(DecodedFrame {
            play_pts,
            width,
            height,
            pixels: FramePixels::Rgba,
            data,
        })
    }
}

fn copy_nv12(frame: &ffmpeg::frame::Video, buf: &mut Vec<u8>) {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    buf.clear();
    buf.reserve(w * h + w * (h / 2));
    let y = frame.data(0);
    let ys = frame.stride(0);
    for row in 0..h {
        buf.extend_from_slice(&y[row * ys..row * ys + w]);
    }
    let uv = frame.data(1);
    let uvs = frame.stride(1);
    for row in 0..h / 2 {
        buf.extend_from_slice(&uv[row * uvs..row * uvs + w]);
    }
}

fn open_video_decoder(
    stream: &ffmpeg::format::stream::Stream<'_>,
) -> Result<ffmpeg::decoder::Video, VideoError> {
    #[cfg(feature = "vaapi")]
    {
        let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        match crate::hw::attach_vaapi(&mut context) {
            Ok(()) => match context.decoder().video() {
                Ok(decoder) => {
                    tracing::info!("VAAPI device attached; hardware decode enabled for supported profiles");
                    return Ok(decoder);
                }
                Err(err) => {
                    tracing::info!(%err, "VAAPI decoder open failed; falling back to CPU decode");
                }
            },
            Err(err) => tracing::info!(%err, "VAAPI unavailable; using CPU decode"),
        }
    }
    Ok(
        ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .video()?,
    )
}

fn copy_rgba(rgb: &ffmpeg::frame::Video, buf: &mut Vec<u8>) {
    let width = rgb.width() as usize;
    let height = rgb.height() as usize;
    let row = width * 4;
    let stride = rgb.stride(0);
    let data = rgb.data(0);
    buf.clear();
    buf.reserve_exact(row * height);
    if stride == row {
        buf.extend_from_slice(&data[..row * height]);
    } else {
        for y in 0..height {
            let start = y * stride;
            buf.extend_from_slice(&data[start..start + row]);
        }
    }
}
