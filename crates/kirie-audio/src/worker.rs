use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use ringbuf::HeapCons;
use ringbuf::traits::Consumer;
use rustfft::{Fft, FftPlanner};

use crate::dsp::{Smoother, WAVE_BUFFER_SIZE};
use crate::spectrum::AudioSpectrum;

struct FrameAssembler {
    buf: [u8; WAVE_BUFFER_SIZE],
    len: usize,
}

impl FrameAssembler {
    const fn new() -> Self {
        Self {
            buf: [0; WAVE_BUFFER_SIZE],
            len: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Option<[u8; WAVE_BUFFER_SIZE]> {
        let mut latest: Option<[u8; WAVE_BUFFER_SIZE]> = None;
        let mut src = bytes;
        while !src.is_empty() {
            let need = WAVE_BUFFER_SIZE - self.len;
            let take = need.min(src.len());
            self.buf[self.len..self.len + take].copy_from_slice(&src[..take]);
            self.len += take;
            src = &src[take..];
            if self.len == WAVE_BUFFER_SIZE {
                latest = Some(self.buf);
                self.len = 0;
            }
        }
        latest
    }
}

pub(crate) struct WorkerParams {
    pub level: f32,
    pub gate: f32,
    pub tick: Duration,
    pub power_save: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub(crate) fn run(
    mut cons: HeapCons<u8>,
    shared: Arc<ArcSwap<AudioSpectrum>>,
    shutdown: Arc<AtomicBool>,
    params: WorkerParams,
) {
    let fft: Arc<dyn Fft<f32>> = FftPlanner::<f32>::new().plan_fft_forward(WAVE_BUFFER_SIZE);
    let mut assembler = FrameAssembler::new();
    let mut smoother = Smoother::new();
    let mut drain = [0u8; 8192];

    let mut ref_db: f32 = crate::dsp::REF_DB_MAX;
    let mut published_silence = false;
    while !shutdown.load(Ordering::Relaxed) {
        let mut latest: Option<[u8; WAVE_BUFFER_SIZE]> = None;
        loop {
            let n = cons.pop_slice(&mut drain);
            if n == 0 {
                break;
            }
            if let Some(frame) = assembler.push(&drain[..n]) {
                latest = Some(frame);
            }
        }

        if let Some(frame) = latest {
            let (mut targets, frame_peak) =
                crate::dsp::analyze_frame(fft.as_ref(), &frame, params.gate, ref_db);
            match frame_peak {
                Some(peak) if peak > ref_db => ref_db = peak.min(crate::dsp::REF_DB_MAX),
                _ => ref_db = (ref_db - crate::dsp::REF_DECAY_DB).max(crate::dsp::REF_DB_MIN),
            }
            if (params.level - 1.0).abs() > f32::EPSILON {
                for v in targets
                    .b64
                    .iter_mut()
                    .chain(targets.b32.iter_mut())
                    .chain(targets.b16.iter_mut())
                {
                    *v = (*v * params.level).min(1.0);
                }
            }
            smoother.set_targets(targets);
        }

        smoother.tick();
        let settled = latest.is_none() && smoother.is_settled_silent();
        if !(settled && published_silence) {
            shared.store(Arc::new(AudioSpectrum::from(&smoother)));
        }
        published_silence = settled;

        let tick = if params
            .power_save
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
        {
            params.tick * 2
        } else {
            params.tick
        };
        std::thread::sleep(tick);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_partial_then_complete() {
        let mut a = FrameAssembler::new();
        assert!(a.push(&[7u8; 500]).is_none());
        assert_eq!(a.len, 500);
        let frame = a.push(&[9u8; WAVE_BUFFER_SIZE - 500 + 3]).expect("frame");
        assert_eq!(frame[0], 7);
        assert_eq!(frame[499], 7);
        assert_eq!(frame[500], 9);
        assert_eq!(a.len, 3);
    }

    #[test]
    fn assembler_keeps_latest_when_multiple() {
        let mut a = FrameAssembler::new();
        let mut bytes = vec![1u8; WAVE_BUFFER_SIZE];
        bytes.extend_from_slice(&[2u8; WAVE_BUFFER_SIZE]);
        bytes.extend_from_slice(&[3u8; 10]);
        let frame = a.push(&bytes).expect("frame");
        assert!(frame.iter().all(|&b| b == 2));
        assert_eq!(a.len, 10);
    }

    #[test]
    fn assembler_empty_is_noop() {
        let mut a = FrameAssembler::new();
        assert!(a.push(&[]).is_none());
        assert_eq!(a.len, 0);
    }
}
