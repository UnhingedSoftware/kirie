//! PulseAudio capture thread: records the default sink's monitor (or an
//! explicit `--audio-device` source) as U8 / 44100 / mono and pushes raw bytes
//! into the SPSC ring for the FFT worker.
//!
//! Everything here is `!Send` (PulseAudio `Mainloop`/`Context`/`Stream` wrap Rc
//! internally), so the whole pipeline is constructed and driven *inside* the
//! capture thread; only the ring producer (Send) crosses the thread boundary.
//! No `unsafe` — libpulse-binding provides safe closure callbacks (V2).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use libpulse_binding::context::{Context, FlagSet as ContextFlags, State as ContextState};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::proplist::Proplist;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

use crate::dsp::SAMPLE_RATE;
use crate::{AudioError, CaptureStatus};

/// Context name (cpp:197).
const CONTEXT_NAME: &str = "wallpaperengine-audioprocessing";
/// Record stream name (cpp:115).
const STREAM_NAME: &str = "output monitor";

/// Drive one `iterate` step, mapping quit/err to a typed error.
fn pump(mainloop: &mut Mainloop) -> Result<(), AudioError> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) | IterateResult::Err(_) => Err(AudioError::Mainloop),
    }
}

/// Blocking-poll wrapper: iterate until `check` returns `Some`, an error, or the
/// shutdown flag trips. A short sleep between non-blocking iterations keeps the
/// thread from busy-spinning while waiting for the server to answer.
fn iterate_until<T>(
    mainloop: &mut Mainloop,
    shutdown: &AtomicBool,
    mut check: impl FnMut() -> Option<Result<T, AudioError>>,
) -> Result<T, AudioError> {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(AudioError::Mainloop);
        }
        pump(mainloop)?;
        if let Some(res) = check() {
            return res;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Connect the context, resolve the source, open the record stream and pump the
/// mainloop until shutdown. Errors here leave the spectrum silent (caller logs).
pub(crate) fn run(
    device: Option<String>,
    producer: HeapProd<u8>,
    status: &Arc<AtomicU8>,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), AudioError> {
    let mut mainloop = Mainloop::new().ok_or_else(|| AudioError::Connect("no mainloop".into()))?;

    let proplist = Proplist::new().ok_or_else(|| AudioError::Connect("no proplist".into()))?;
    let mut context = Context::new_with_proplist(&mainloop, CONTEXT_NAME, &proplist)
        .ok_or_else(|| AudioError::Connect("no context".into()))?;
    context
        .connect(None, ContextFlags::NOFLAGS, None)
        .map_err(|e| AudioError::Connect(format!("{e:?}")))?;

    // Wait for PA_CONTEXT_READY (cpp:206-209).
    iterate_until(&mut mainloop, shutdown, || match context.get_state() {
        ContextState::Ready => Some(Ok(())),
        ContextState::Failed | ContextState::Terminated => {
            Some(Err(AudioError::Connect("context failed".into())))
        }
        _ => None,
    })?;

    // Resolve the capture source: explicit device, else "<default_sink>.monitor"
    // (cpp:121-128).
    let source = match device.filter(|d| !d.is_empty()) {
        Some(dev) => dev,
        None => {
            let default_sink: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
            {
                let slot = default_sink.clone();
                context.introspect().get_server_info(move |info| {
                    *slot.borrow_mut() = Some(
                        info.default_sink_name
                            .as_ref()
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                    );
                });
            }
            let sink = iterate_until(&mut mainloop, shutdown, || default_sink.borrow().clone().map(Ok))?;
            if sink.is_empty() {
                return Err(AudioError::NoMonitor);
            }
            format!("{sink}.monitor")
        }
    };

    // Record stream: S16LE / 44100 / mono, folded to the U8 domain the DSP
    // works in (`sample - 128`, cpp:248-273) by the read callback below.
    //
    // The reference records U8 directly (cpp:107-110), which is correct against
    // a real PulseAudio server. PipeWire's PulseAudio compatibility layer,
    // however, hands back a stream of pure silence for a U8 record stream on a
    // monitor source — measurably so: the same monitor, at the same instant,
    // yields a full-scale signal as S16 and a flat 128 as U8. Since PipeWire is
    // what nearly every current desktop runs, keeping the reference's format
    // meant every audio-reactive wallpaper — scene and web alike — sat still on
    // a machine whose audio was working perfectly.
    //
    // S16 is a universally supported capture format, so this costs nothing on a
    // real PulseAudio server and the DSP downstream is unchanged.
    let spec = Spec {
        format: Format::S16le,
        channels: 1,
        rate: SAMPLE_RATE,
    };
    if !spec.is_valid() {
        return Err(AudioError::StreamConnect {
            source_name: source,
            reason: "invalid sample spec".into(),
        });
    }

    // Opt the record stream out of the session manager's saved routing.
    //
    // PipeWire's stream-restore remembers, per application name, which source a
    // recording stream was last moved to, and re-applies it in place of the
    // source the application asked for. A wallpaper's capture is not a stream a
    // user routes by choice — it is "whatever is coming out of the speakers" —
    // so a routing saved once (or inherited from another app that used this
    // name) silently pins it to an unrelated device forever. That failure is
    // invisible: the stream connects, reports Running, and reads a valid,
    // permanently silent signal, so every audio-reactive wallpaper sits still
    // with nothing in any log to explain it.
    //
    // `--audio-device` remains the way to choose a source deliberately.
    let mut stream_props = Proplist::new().ok_or_else(|| AudioError::Connect("no stream proplist".into()))?;
    let _ = stream_props.set_str("state.restore-props", "false");
    let _ = stream_props.set_str("state.restore-target", "false");

    let stream = Rc::new(RefCell::new(
        Stream::new_with_proplist(&mut context, STREAM_NAME, &spec, None, &mut stream_props).ok_or_else(
            || AudioError::StreamConnect {
                source_name: source.clone(),
                reason: "stream alloc failed".into(),
            },
        )?,
    ));

    // Read callback: drain every peeked fragment into the ring; drop holes
    // (cpp:31-98). Bytes not fitting the ring are dropped (overflow tolerated).
    let producer = Rc::new(RefCell::new(producer));
    {
        let stream_cb = stream.clone();
        let producer_cb = producer.clone();
        // A fragment can end mid-sample, so the odd trailing byte is carried
        // into the next callback rather than dropped — dropping it would shift
        // the byte parity and turn the rest of the stream into noise.
        let carry: Rc<RefCell<Option<u8>>> = Rc::new(RefCell::new(None));
        let mut scratch: Vec<u8> = Vec::new();
        stream
            .borrow_mut()
            .set_read_callback(Some(Box::new(move |_nbytes| {
                let mut s = stream_cb.borrow_mut();
                loop {
                    match s.peek() {
                        Ok(PeekResult::Data(data)) => {
                            // S16LE → the DSP's U8 domain: take the high byte
                            // (a plain >>8 on the signed value) and bias to
                            // unsigned, which is exactly what a U8 capture of
                            // the same signal would have produced.
                            scratch.clear();
                            scratch.reserve(data.len() / 2 + 1);
                            let mut iter = data.iter().copied();
                            let mut lo = carry.borrow_mut().take();
                            while let Some(low) = lo.take().or_else(|| iter.next()) {
                                let Some(high) = iter.next() else {
                                    *carry.borrow_mut() = Some(low);
                                    break;
                                };
                                let sample = i16::from_le_bytes([low, high]);
                                scratch.push(((sample >> 8) as i32 + 128) as u8);
                            }
                            producer_cb.borrow_mut().push_slice(&scratch);
                            let _ = s.discard();
                        }
                        Ok(PeekResult::Hole(_)) => {
                            let _ = s.discard();
                        }
                        Ok(PeekResult::Empty) => break,
                        Err(_) => break,
                    }
                }
            })));
    }

    // Buffer attrs (cpp:130-137): U8/mono → bytes_per_sec == rate.
    let bytes_per_sec = SAMPLE_RATE;
    let fragsize = bytes_per_sec * 10 / 1000;
    let maxlength = fragsize + bytes_per_sec * 750 / 1000;
    let attr = BufferAttr {
        maxlength,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize,
    };

    stream
        .borrow_mut()
        .connect_record(Some(&source), Some(&attr), StreamFlags::ADJUST_LATENCY)
        .map_err(|e| AudioError::StreamConnect {
            source_name: source.clone(),
            reason: format!("{e:?}"),
        })?;

    // Wait for the stream to reach Ready before declaring success.
    iterate_until(&mut mainloop, shutdown, || match stream.borrow().get_state() {
        StreamState::Ready => Some(Ok(())),
        StreamState::Failed | StreamState::Terminated => Some(Err(AudioError::StreamConnect {
            source_name: source.clone(),
            reason: "stream failed".into(),
        })),
        _ => None,
    })?;

    // What the server actually bound us to, which is not necessarily what was
    // asked for: the opt-out above covers stream-restore, but a user rule or a
    // manual move can still land the stream elsewhere, and reading silence off
    // the wrong monitor is indistinguishable from a quiet desktop. Naming both
    // devices turns "the visualiser does nothing" into a one-line diagnosis.
    let bound = stream.borrow().get_device_name().map(|d| d.to_string());
    match &bound {
        Some(actual) if actual != &source => {
            tracing::warn!(
                requested = %source,
                actual = %actual,
                "audio capture was routed to a different source than requested; \
                 the spectrum will follow that device, not the one asked for \
                 (move it back with `pactl move-source-output <id> {source}`, \
                 or select it explicitly with --audio-device)"
            );
        }
        _ => {}
    }

    status.store(CaptureStatus::Running.as_u8(), Ordering::Relaxed);
    tracing::info!(source = %source, bound = ?bound, "audio capture running");

    // Drive the mainloop; the read callback fires as fragments arrive.
    while !shutdown.load(Ordering::Relaxed) {
        pump(&mut mainloop)?;
        std::thread::sleep(Duration::from_millis(5));
    }

    // Clear the callback before teardown so the closure (holding an Rc to the
    // stream) is dropped, breaking the reference cycle.
    stream.borrow_mut().set_read_callback(None);
    let _ = stream.borrow_mut().disconnect();
    Ok(())
}
