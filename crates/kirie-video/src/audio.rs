use std::path::PathBuf;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::ChannelLayout;
use ffmpeg_next::format::Sample;
use ffmpeg_next::format::sample::Type as SampleType;
use ffmpeg_next::software::resampling;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::clock::{ConsumerSnap, ProducerSnap};
use crate::error::VideoError;
use crate::pacing::LoopTimeline;

const RING_SECONDS: f64 = 0.5;

const RING_FULL_BACKOFF: Duration = Duration::from_millis(5);

const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub(crate) enum CallbackCmd {
    Volume(f64),
    Mute(bool),
    Pause(bool),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DecodeCmd {
    Speed(f64),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioInit {
    pub volume: f64,
    pub mute: bool,
    pub silent: bool,
    pub paused: bool,
}

pub(crate) struct AudioLink {
    pub producer: triple_buffer::Output<ProducerSnap>,
    pub consumer: triple_buffer::Output<ConsumerSnap>,
    pub sample_rate: u32,
}

enum Setup {
    Ready(AudioLink),
    NoStream,
    Failed(VideoError),
}

pub(crate) fn spawn(
    path: PathBuf,
    init: AudioInit,
    callback_rx: Receiver<CallbackCmd>,
    decode_rx: Receiver<DecodeCmd>,
    shutdown_rx: Receiver<()>,
) -> Result<Option<AudioLink>, VideoError> {
    let (setup_tx, setup_rx) = bounded(1);
    std::thread::Builder::new()
        .name("kirie-audio-decode".into())
        .spawn(move || run_thread(&path, init, &callback_rx, &decode_rx, &shutdown_rx, &setup_tx))?;
    match setup_rx.recv_timeout(SETUP_TIMEOUT) {
        Ok(Setup::Ready(link)) => Ok(Some(link)),
        Ok(Setup::NoStream) => Ok(None),
        Ok(Setup::Failed(err)) => Err(err),
        Err(_) => Err(VideoError::AudioOutput(
            "audio setup did not report within timeout".into(),
        )),
    }
}

fn run_thread(
    path: &std::path::Path,
    init: AudioInit,
    callback_rx: &Receiver<CallbackCmd>,
    decode_rx: &Receiver<DecodeCmd>,
    shutdown_rx: &Receiver<()>,
    setup_tx: &Sender<Setup>,
) {
    let setup = || -> Result<Option<(DecodeState, cpal::Stream, AudioLink)>, VideoError> {
        ffmpeg::init()?;
        let input = ffmpeg::format::input(path)?;
        let Some(stream) = input.streams().best(ffmpeg::media::Type::Audio) else {
            return Ok(None);
        };
        let stream_index = stream.index();
        let time_base = f64::from(stream.time_base());
        let start = if stream.start_time() == i64::MIN {
            0.0
        } else {
            stream.start_time() as f64 * time_base
        };
        let duration = if input.duration() > 0 {
            input.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            0.0
        };
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()?;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| VideoError::AudioOutput("no default output device".into()))?;
        let supported = device
            .default_output_config()
            .map_err(|e| VideoError::AudioOutput(e.to_string()))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let sample_rate = config.sample_rate.0;
        let channels = usize::from(config.channels);
        if sample_rate == 0 || channels == 0 {
            return Err(VideoError::AudioOutput(
                "device reports zero rate/channels".into(),
            ));
        }

        let ring_cap = ((f64::from(sample_rate) * RING_SECONDS) as usize).max(1024) * channels;
        let (ring_prod, ring_cons) = HeapRb::<f32>::new(ring_cap).split();

        let now = Instant::now();
        let (prod_in, prod_out) = triple_buffer::triple_buffer(&ProducerSnap::default());
        let (cons_in, cons_out) = triple_buffer::triple_buffer(&ConsumerSnap::initial(now));

        let callback = CallbackState {
            ring: ring_cons,
            commands: callback_rx.clone(),
            snap: cons_in,
            scratch: vec![0.0; 8192 * channels],
            channels,
            volume: init.volume.clamp(0.0, 100.0),
            mute: init.mute,
            silent: init.silent,
            paused: init.paused,
            consumed: 0,
        };
        let stream = build_stream(&device, &config, sample_format, callback)?;
        stream
            .play()
            .map_err(|e| VideoError::AudioOutput(e.to_string()))?;

        let state = DecodeState {
            input,
            decoder,
            stream_index,
            time_base,
            start,
            timeline: LoopTimeline::new((duration > 0.0).then_some(duration)),
            resampler: None,
            device_rate: sample_rate,
            out_layout: ChannelLayout::default(channels as i32),
            channels,
            speed: 1.0,
            ring: ring_prod,
            snap: prod_in,
            pushed: 0,
            head: 0.0,
            synth_pts: 0.0,
            decoded: ffmpeg::frame::Audio::empty(),
            undecodable: 0,
        };
        let link = AudioLink {
            producer: prod_out,
            consumer: cons_out,
            sample_rate,
        };
        Ok(Some((state, stream, link)))
    };

    match setup() {
        Ok(Some((mut state, stream, link))) => {
            let _ = setup_tx.send(Setup::Ready(link));
            state.run(decode_rx, shutdown_rx);
            drop(stream);
        }
        Ok(None) => {
            let _ = setup_tx.send(Setup::NoStream);
        }
        Err(err) => {
            let _ = setup_tx.send(Setup::Failed(err));
        }
    }
}

struct CallbackState {
    ring: ringbuf::HeapCons<f32>,
    commands: Receiver<CallbackCmd>,
    snap: triple_buffer::Input<ConsumerSnap>,
    scratch: Vec<f32>,
    channels: usize,
    volume: f64,
    mute: bool,
    silent: bool,
    paused: bool,
    consumed: u64,
}

impl CallbackState {
    fn fill<T: cpal::SizedSample + cpal::FromSample<f32>>(&mut self, data: &mut [T]) {
        while let Ok(cmd) = self.commands.try_recv() {
            match cmd {
                CallbackCmd::Volume(v) => self.volume = v.clamp(0.0, 100.0),
                CallbackCmd::Mute(m) => self.mute = m,
                CallbackCmd::Pause(p) => self.paused = p,
            }
        }

        let silence = T::from_sample(0.0f32);
        if self.paused {
            data.fill(silence);
            self.snap.write(ConsumerSnap {
                consumed: self.consumed,
                at: Instant::now(),
                paused: true,
            });
            return;
        }

        if self.scratch.len() < data.len() {
            self.scratch.resize(data.len(), 0.0);
        }
        let got = self.ring.pop_slice(&mut self.scratch[..data.len()]);
        let gain = if self.mute || self.silent {
            0.0
        } else {
            (self.volume / 100.0) as f32
        };
        for (dst, src) in data.iter_mut().zip(&self.scratch[..got]) {
            *dst = T::from_sample(*src * gain);
        }
        for dst in &mut data[got..] {
            *dst = silence;
        }
        self.consumed += (got / self.channels.max(1)) as u64;
        self.snap.write(ConsumerSnap {
            consumed: self.consumed,
            at: Instant::now(),
            paused: false,
        });
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    state: CallbackState,
) -> Result<cpal::Stream, VideoError> {
    fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut state: CallbackState,
    ) -> Result<cpal::Stream, VideoError> {
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _| state.fill(data),
                |err| tracing::warn!(%err, "cpal stream error"),
                None,
            )
            .map_err(|e| VideoError::AudioOutput(e.to_string()))
    }
    match sample_format {
        cpal::SampleFormat::F32 => build::<f32>(device, config, state),
        cpal::SampleFormat::I16 => build::<i16>(device, config, state),
        cpal::SampleFormat::U16 => build::<u16>(device, config, state),
        cpal::SampleFormat::I32 => build::<i32>(device, config, state),
        other => Err(VideoError::UnsupportedSampleFormat(format!("{other:?}"))),
    }
}

struct DecodeState {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Audio,
    stream_index: usize,
    time_base: f64,
    start: f64,
    timeline: LoopTimeline,
    resampler: Option<resampling::Context>,
    device_rate: u32,
    out_layout: ChannelLayout,
    channels: usize,
    speed: f64,
    ring: ringbuf::HeapProd<f32>,
    snap: triple_buffer::Input<ProducerSnap>,
    pushed: u64,
    head: f64,
    synth_pts: f64,
    decoded: ffmpeg::frame::Audio,
    undecodable: u64,
}

impl DecodeState {
    fn out_rate(&self) -> u32 {
        ((f64::from(self.device_rate) / self.speed).round() as u32).max(1)
    }

    fn make_resampler(&self) -> Result<resampling::Context, VideoError> {
        let in_layout = if self.decoder.channel_layout().is_empty() {
            ChannelLayout::default(i32::from(self.decoder.channels()))
        } else {
            self.decoder.channel_layout()
        };
        Ok(resampling::Context::get(
            self.decoder.format(),
            in_layout,
            self.decoder.rate(),
            Sample::F32(SampleType::Packed),
            self.out_layout,
            self.out_rate(),
        )?)
    }

    fn run(&mut self, decode_rx: &Receiver<DecodeCmd>, shutdown_rx: &Receiver<()>) {
        let mut consecutive_read_errors = 0u32;
        loop {
            loop {
                if !self.poll_commands(decode_rx, shutdown_rx) {
                    return;
                }
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.input) {
                    Ok(()) => consecutive_read_errors = 0,
                    Err(ffmpeg::Error::Eof) => break,
                    Err(err) => {
                        consecutive_read_errors += 1;
                        if consecutive_read_errors > 1000 {
                            tracing::error!(%err, "audio demux failing persistently; stopping");
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
                        tracing::warn!(%err, count = self.undecodable, "skipping undecodable audio packet(s)");
                    }
                    continue;
                }
                self.undecodable = 0;
                if !self.drain(shutdown_rx) {
                    return;
                }
            }
            let _ = self.decoder.send_eof();
            if !self.drain(shutdown_rx) {
                return;
            }
            if let Err(err) = self.input.seek(0, ..) {
                tracing::error!(%err, "audio loop seek failed; stopping");
                return;
            }
            self.decoder.flush();
            self.timeline.wrap();
            self.synth_pts = 0.0;
        }
    }

    fn poll_commands(&mut self, decode_rx: &Receiver<DecodeCmd>, shutdown_rx: &Receiver<()>) -> bool {
        if matches!(shutdown_rx.try_recv(), Err(TryRecvError::Disconnected)) {
            return false;
        }
        while let Ok(cmd) = decode_rx.try_recv() {
            match cmd {
                DecodeCmd::Speed(speed) => {
                    if (speed - self.speed).abs() > f64::EPSILON {
                        self.speed = speed;
                        self.resampler = None;
                    }
                }
            }
        }
        true
    }

    fn drain(&mut self, shutdown_rx: &Receiver<()>) -> bool {
        loop {
            if self.decoder.receive_frame(&mut self.decoded).is_err() {
                return true;
            }
            if let Err(err) = self.process_frame(shutdown_rx) {
                match err {
                    ProcessStop::Shutdown => return false,
                    ProcessStop::Error(err) => {
                        tracing::warn!(%err, "dropping unprocessable audio frame");
                    }
                }
            }
        }
    }

    fn process_frame(&mut self, shutdown_rx: &Receiver<()>) -> Result<(), ProcessStop> {
        let samples = self.decoded.samples();
        if samples == 0 {
            return Ok(());
        }
        let in_rate = self.decoder.rate().max(1);

        if self.resampler.is_none() {
            self.resampler = Some(self.make_resampler().map_err(ProcessStop::Error)?);
        }
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(());
        };

        let out_rate = resampler.output().rate;
        let backlog = resampler.delay().map_or(0, |d| d.output.max(0)) as usize;
        let cap = samples * out_rate as usize / in_rate as usize + backlog + 256;
        let mut out = ffmpeg::frame::Audio::new(Sample::F32(SampleType::Packed), cap, self.out_layout);
        resampler
            .run(&self.decoded, &mut out)
            .map_err(|e| ProcessStop::Error(e.into()))?;

        let raw = match self.decoded.timestamp().or_else(|| self.decoded.pts()) {
            Some(ts) => ts as f64 * self.time_base - self.start,
            None => self.synth_pts,
        };
        let dur = samples as f64 / f64::from(in_rate);
        self.synth_pts = raw + dur;
        let play = self.timeline.map(raw, dur);

        let produced = out.samples() * self.channels;
        if produced > 0 {
            let bytes = &out.data(0)[..produced * size_of::<f32>()];
            let Ok(floats) = bytemuck::try_cast_slice::<u8, f32>(bytes) else {
                return Err(ProcessStop::Error(VideoError::AudioOutput(
                    "resampler output misaligned".into(),
                )));
            };
            let mut offset = 0;
            while offset < floats.len() {
                offset += self.ring.push_slice(&floats[offset..]);
                if offset < floats.len() {
                    if matches!(shutdown_rx.try_recv(), Err(TryRecvError::Disconnected)) {
                        return Err(ProcessStop::Shutdown);
                    }
                    std::thread::sleep(RING_FULL_BACKOFF);
                }
            }
            self.pushed += (out.samples()) as u64;
        }

        self.head = play + dur;
        self.snap.write(ProducerSnap {
            pushed: self.pushed,
            head: self.head,
            speed: self.speed,
        });
        Ok(())
    }
}

enum ProcessStop {
    Shutdown,
    Error(VideoError),
}
