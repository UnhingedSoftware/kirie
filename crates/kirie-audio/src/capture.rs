//! PulseAudio capture thread: records whatever is playing (see [`pick_source`])
//! as S16 / 44100 / mono, folds it to the U8 domain the DSP works in, and pushes
//! the samples into the SPSC ring for the FFT worker.
//!
//! Everything here is `!Send` (PulseAudio `Mainloop`/`Context`/`Stream` wrap Rc
//! internally), so the whole pipeline is constructed and driven *inside* the
//! capture thread; only the ring producer (Send) crosses the thread boundary.
//! No `unsafe` — libpulse-binding provides safe closure callbacks (V2).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet as ContextFlags, State as ContextState};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::proplist::Proplist;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};
use libpulse_binding::volume::{ChannelVolumes, Volume};
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

use crate::dsp::SAMPLE_RATE;
use crate::{AudioError, CaptureStatus, PlayerHint, PlayerSlot};

/// Context name (cpp:197).
const CONTEXT_NAME: &str = "wallpaperengine-audioprocessing";
/// Record stream name (cpp:115).
const STREAM_NAME: &str = "output monitor";
/// How often the capture re-checks which monitor it should be on. Two seconds
/// is well below "did the wallpaper notice my music started" while costing two
/// cheap introspection round-trips.
const RESELECT_INTERVAL: Duration = Duration::from_secs(2);

/// The level scale applied to captured samples (`KIRIE_AUDIO_BOOST`).
///
/// Read in one place because two consumers must agree on it: the fold scales
/// the samples, and the noise gate's threshold has to scale with them —
/// otherwise a boost below 1 parks the signal *at* the gate and playback
/// flips between all-zero and full frames instead of moving smoothly.
pub(crate) fn resolved_boost() -> f32 {
    std::env::var("KIRIE_AUDIO_BOOST")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|b: &f32| b.is_finite() && *b >= 0.0)
        .unwrap_or(0.15)
        .min(64.0)
}

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

/// One playback stream, as far as source selection cares.
struct SinkInput {
    /// The sink it plays into.
    sink: u32,
    /// PID of the application that opened it, when the server reports one.
    /// PipeWire frequently does not, which is why `names` exists.
    pid: Option<u32>,
    /// Every identity the stream advertises, lowercased: `node.name`,
    /// `application.name`, `application.process.binary`. Any one matching the
    /// player's short name is enough.
    names: Vec<String>,
    /// Paused streams are ignored: a corked stream produces no sound.
    corked: bool,
}

impl SinkInput {
    /// Whether this stream belongs to the player described by `hint`.
    fn matches(&self, hint: &PlayerHint) -> bool {
        if let (Some(want), Some(got)) = (hint.pid, self.pid)
            && want == got
        {
            return true;
        }
        hint.name
            .as_ref()
            .is_some_and(|want| self.names.iter().any(|n| n == want))
    }
}

/// Choose the monitor to record.
///
/// The reference records the default sink's monitor (cpp:121-128), which is
/// correct whenever applications play to the default sink. On a machine running
/// a virtual mixer — increasingly common, and the case that exposed this — the
/// default sink is an input strip that nothing plays to directly, so its monitor
/// is permanently silent while music plays into a sibling node. The wallpaper
/// then sits still on a desktop that is audibly playing music.
///
/// So the default sink is the fallback rather than the rule:
///
/// 1. the sink the media player itself is playing into, identified by the hint
///    the engine learned from MPRIS — this is the "what am I listening to"
///    answer, and the only one that stays right when several apps make sound;
/// 2. failing that, the sink of the only active stream, when there is exactly
///    one — unambiguous, so no guessing is involved;
/// 3. failing that, the default sink's monitor, as the reference does.
fn pick_source(
    mainloop: &mut Mainloop,
    context: &Context,
    shutdown: &AtomicBool,
    player: &PlayerSlot,
) -> Result<String, AudioError> {
    let wanted = player.load_full();

    // Collect every playback stream, then decide — a callback per stream would
    // have to make the decision without knowing what else exists.
    let inputs: Rc<RefCell<Vec<SinkInput>>> = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(RefCell::new(false));
    {
        let inputs = inputs.clone();
        let done = done.clone();
        context
            .introspect()
            .get_sink_input_info_list(move |result| match result {
                ListResult::Item(info) => {
                    let names = ["node.name", "application.name", "application.process.binary"]
                        .iter()
                        .filter_map(|key| info.proplist.get_str(key))
                        .map(|v| v.to_lowercase())
                        .collect();
                    inputs.borrow_mut().push(SinkInput {
                        sink: info.sink,
                        pid: info
                            .proplist
                            .get_str("application.process.id")
                            .and_then(|v| v.parse().ok()),
                        names,
                        corked: info.corked,
                    });
                }
                ListResult::End | ListResult::Error => *done.borrow_mut() = true,
            });
    }
    iterate_until(mainloop, shutdown, || done.borrow().then_some(Ok(())))?;

    let inputs = inputs.borrow();
    let active: Vec<&SinkInput> = inputs.iter().filter(|i| !i.corked).collect();
    let chosen_sink = wanted
        .as_ref()
        .and_then(|hint| active.iter().find(|i| i.matches(hint)))
        .or_else(|| active.first().filter(|_| active.len() == 1))
        .map(|i| i.sink);

    if let Some(index) = chosen_sink
        && let Some(name) = sink_name(mainloop, context, shutdown, index)?
    {
        tracing::debug!(sink = %name, player = ?wanted, "following playback for audio capture");
        return Ok(format!("{name}.monitor"));
    }

    // Nothing is playing (or the sink vanished mid-query): the default sink is
    // where sound will appear when something starts, so it is the right thing
    // to sit on until the periodic re-check finds otherwise.
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
    let sink = iterate_until(mainloop, shutdown, || default_sink.borrow().clone().map(Ok))?;
    if sink.is_empty() {
        return Err(AudioError::NoMonitor);
    }
    Ok(format!("{sink}.monitor"))
}

/// Resolve a sink index to its name, or `None` if it is gone.
fn sink_name(
    mainloop: &mut Mainloop,
    context: &Context,
    shutdown: &AtomicBool,
    index: u32,
) -> Result<Option<String>, AudioError> {
    let name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let done = Rc::new(RefCell::new(false));
    {
        let name = name.clone();
        let done = done.clone();
        context
            .introspect()
            .get_sink_info_by_index(index, move |result| match result {
                ListResult::Item(info) => {
                    *name.borrow_mut() = info.name.as_ref().map(|n| n.to_string());
                }
                ListResult::End | ListResult::Error => *done.borrow_mut() = true,
            });
    }
    iterate_until(mainloop, shutdown, || done.borrow().then_some(Ok(())))?;
    let out = name.borrow().clone();
    Ok(out)
}

/// Connect the context, resolve the source, open the record stream and pump the
/// mainloop until shutdown. Errors here leave the spectrum silent (caller logs).
pub(crate) fn run(
    device: Option<String>,
    producer: HeapProd<u8>,
    status: &Arc<AtomicU8>,
    shutdown: &Arc<AtomicBool>,
    player: &PlayerSlot,
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

    // Resolve the capture source: an explicit `--audio-device` wins outright,
    // otherwise follow whatever is actually producing sound (see `pick_source`).
    let explicit = device.filter(|d| !d.is_empty());
    let source = match &explicit {
        Some(dev) => dev.clone(),
        None => pick_source(&mut mainloop, &context, shutdown, player)?,
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
    let producer = Rc::new(RefCell::new(producer));
    let mut stream = open_stream(&mut mainloop, &mut context, &spec, &source, &producer, shutdown)?;

    status.store(CaptureStatus::Running.as_u8(), Ordering::Relaxed);
    tracing::info!(source = %source, "audio capture running");

    // Drive the mainloop; the read callback fires as fragments arrive.
    //
    // The chosen monitor is re-checked periodically, because what is playing
    // changes long after launch: a player opened later, a track moved to
    // another device, or simply the engine having started before any music did.
    // A change means opening a **new** stream: a PulseAudio stream object is
    // single-use, so disconnect + reconnect on the same one fails with
    // `BadState` — worse, it leaves a live-looking capture thread reading
    // nothing forever. The old stream is torn down only after its replacement
    // is up, so a failed switch keeps the working one (V9: never fatal).
    let mut current = source;
    let mut next_check = Instant::now() + RESELECT_INTERVAL;
    while !shutdown.load(Ordering::Relaxed) {
        pump(&mut mainloop)?;

        if explicit.is_none() && Instant::now() >= next_check {
            next_check = Instant::now() + RESELECT_INTERVAL;
            if let Ok(best) = pick_source(&mut mainloop, &context, shutdown, player)
                && best != current
            {
                match open_stream(&mut mainloop, &mut context, &spec, &best, &producer, shutdown) {
                    Ok(fresh) => {
                        tracing::info!(from = %current, to = %best, "audio capture following playback");
                        close_stream(&stream);
                        stream = fresh;
                        current = best;
                    }
                    Err(e) => {
                        tracing::warn!(source = %best, error = %e, "audio re-bind failed; keeping current source");
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    close_stream(&stream);
    Ok(())
}

/// Open, connect and fully prepare one record stream on `source`.
///
/// Everything a working capture needs happens in here, in order: the
/// stream-restore opt-out, the S16→U8 read callback, connect, waiting for
/// Ready, and forcing the stream to full volume/unmuted. Bundling it is the
/// point — the follow loop replaces streams at runtime, and a replacement that
/// skipped any one of these steps would reintroduce the exact silent failure
/// that step exists to prevent.
fn open_stream(
    mainloop: &mut Mainloop,
    context: &mut Context,
    spec: &Spec,
    source: &str,
    producer: &Rc<RefCell<HeapProd<u8>>>,
    shutdown: &AtomicBool,
) -> Result<Rc<RefCell<Stream>>, AudioError> {
    // Opt the record stream out of the session manager's saved routing and
    // volume. Stream-restore remembers both per application name and re-applies
    // them over what the application asked for; a wallpaper's capture is
    // "whatever is coming out of the speakers", so a routing or a 0% volume
    // saved once (by the user, or by anything else that ever used this name)
    // would silently pin the spectrum to an unrelated or muted stream forever.
    let mut stream_props = Proplist::new().ok_or_else(|| AudioError::Connect("no stream proplist".into()))?;
    let _ = stream_props.set_str("state.restore-props", "false");
    let _ = stream_props.set_str("state.restore-target", "false");

    let stream = Rc::new(RefCell::new(
        Stream::new_with_proplist(context, STREAM_NAME, spec, None, &mut stream_props).ok_or_else(|| {
            AudioError::StreamConnect {
                source_name: source.to_owned(),
                reason: "stream alloc failed".into(),
            }
        })?,
    ));

    // Read callback: drain every peeked fragment into the ring; drop holes
    // (cpp:31-98). Bytes not fitting the ring are dropped (overflow tolerated).
    {
        let stream_cb = stream.clone();
        let producer_cb = producer.clone();
        // A fragment can end mid-sample, so the odd trailing byte is carried
        // into the next callback rather than dropped — dropping it would shift
        // the byte parity and turn the rest of the stream into noise.
        let mut carry: Option<u8> = None;
        let mut scratch: Vec<i16> = Vec::new();
        let mut folded: Vec<u8> = Vec::new();
        let boost = resolved_boost();
        stream
            .borrow_mut()
            .set_read_callback(Some(Box::new(move |_nbytes| {
                let mut s = stream_cb.borrow_mut();
                loop {
                    match s.peek() {
                        Ok(PeekResult::Data(data)) => {
                            scratch.clear();
                            scratch.reserve(data.len() / 2 + 1);
                            let mut iter = data.iter().copied();
                            let mut lo = carry.take();
                            while let Some(low) = lo.take().or_else(|| iter.next()) {
                                let Some(high) = iter.next() else {
                                    carry = Some(low);
                                    break;
                                };
                                scratch.push(i16::from_le_bytes([low, high]));
                            }

                            // S16 → the DSP's U8 domain, at a fixed gain.
                            //
                            // Fixed, deliberately: the bars on screen must
                            // follow the listening volume — quiet music, small
                            // bars — which is what a Windows loopback gives the
                            // reference. An adaptive gain was tried here and
                            // erased exactly that relationship: it levelled
                            // every track to the same on-screen amplitude, so
                            // background-volume music produced full-height
                            // spikes 24/7.
                            //
                            // The tap is forced to 100%, so what arrives is
                            // the stream's full digital level — most desktops
                            // attenuate in hardware after that point, so raw
                            // full scale reads far louder than what the user
                            // hears. Scale down to a calm default; override
                            // with KIRIE_AUDIO_BOOST (1.0 = faithful raw,
                            // higher amplifies a genuinely quiet stream).
                            let gain = boost;
                            folded.clear();
                            folded.reserve(scratch.len());
                            for &sample in &scratch {
                                let scaled = (f32::from(sample) * gain).clamp(-32768.0, 32767.0) as i32;
                                folded.push(((scaled >> 8) + 128) as u8);
                            }
                            producer_cb.borrow_mut().push_slice(&folded);
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

    // Buffer attrs (cpp:130-137): mono → bytes_per_sec == rate × 2 for S16.
    let bytes_per_sec = SAMPLE_RATE * 2;
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
        .connect_record(Some(source), Some(&attr), StreamFlags::ADJUST_LATENCY)
        .map_err(|e| AudioError::StreamConnect {
            source_name: source.to_owned(),
            reason: format!("{e:?}"),
        })?;

    // Wait for the stream to reach Ready before declaring success.
    iterate_until(mainloop, shutdown, || match stream.borrow().get_state() {
        StreamState::Ready => Some(Ok(())),
        StreamState::Failed | StreamState::Terminated => Some(Err(AudioError::StreamConnect {
            source_name: source.to_owned(),
            reason: "stream failed".into(),
        })),
        _ => None,
    })?;

    // Force full volume and unmuted, belt to the restore-props braces above:
    // the opt-out only works on servers that honour it, and a stream that
    // arrives at 0% delivers a steady flow of all-zero samples — a failure
    // indistinguishable from a quiet desktop, which is how it went unnoticed.
    // An analysis tap has no meaningful user-chosen volume to respect.
    if let Some(index) = stream.borrow().get_index() {
        let mut volume = ChannelVolumes::default();
        volume.set(spec.channels, Volume::NORMAL);
        // Fire-and-forget: best-effort, and a server that refuses them leaves
        // exactly the behaviour that existed before (V9, never fatal).
        context
            .introspect()
            .set_source_output_volume(index, &volume, None);
        context.introspect().set_source_output_mute(index, false, None);
    }

    Ok(stream)
}

/// Tear a stream down: clear the callback first so the closure (holding an Rc
/// to the stream) is dropped, breaking the reference cycle.
fn close_stream(stream: &Rc<RefCell<Stream>>) {
    stream.borrow_mut().set_read_callback(None);
    let _ = stream.borrow_mut().disconnect();
}
