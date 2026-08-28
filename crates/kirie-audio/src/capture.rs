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

const CONTEXT_NAME: &str = "wallpaperengine-audioprocessing";
const STREAM_NAME: &str = "output monitor";
const RESELECT_INTERVAL: Duration = Duration::from_secs(2);

fn pump(mainloop: &mut Mainloop) -> Result<(), AudioError> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) | IterateResult::Err(_) => Err(AudioError::Mainloop),
    }
}

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

struct SinkInput {
    sink: u32,
    pid: Option<u32>,
    names: Vec<String>,
    corked: bool,
}

impl SinkInput {
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

fn pick_source(
    mainloop: &mut Mainloop,
    context: &Context,
    shutdown: &AtomicBool,
    player: &PlayerSlot,
) -> Result<String, AudioError> {
    let wanted = player.load_full();

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

    iterate_until(&mut mainloop, shutdown, || match context.get_state() {
        ContextState::Ready => Some(Ok(())),
        ContextState::Failed | ContextState::Terminated => {
            Some(Err(AudioError::Connect("context failed".into())))
        }
        _ => None,
    })?;

    let explicit = device.filter(|d| !d.is_empty());
    let source = match &explicit {
        Some(dev) => dev.clone(),
        None => pick_source(&mut mainloop, &context, shutdown, player)?,
    };

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

    let producer = Rc::new(RefCell::new(producer));
    let mut stream = open_stream(&mut mainloop, &mut context, &spec, &source, &producer, shutdown)?;

    status.store(CaptureStatus::Running.as_u8(), Ordering::Relaxed);
    tracing::info!(source = %source, "audio capture running");

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

        std::thread::sleep(Duration::from_millis(20));
    }

    close_stream(&stream);
    Ok(())
}

fn open_stream(
    mainloop: &mut Mainloop,
    context: &mut Context,
    spec: &Spec,
    source: &str,
    producer: &Rc<RefCell<HeapProd<u8>>>,
    shutdown: &AtomicBool,
) -> Result<Rc<RefCell<Stream>>, AudioError> {
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

    {
        let stream_cb = stream.clone();
        let producer_cb = producer.clone();
        let mut carry: Option<u8> = None;
        let mut scratch: Vec<i16> = Vec::new();
        let mut folded: Vec<u8> = Vec::new();
        let pregain: f32 = std::env::var("KIRIE_AUDIO_PREGAIN")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|g: &f32| g.is_finite() && *g > 0.0)
            .unwrap_or(1.0)
            .min(64.0);
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

                            folded.clear();
                            folded.reserve(scratch.len());
                            for &sample in &scratch {
                                let scaled = (f32::from(sample) * pregain).clamp(-32768.0, 32767.0) as i32;
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

    iterate_until(mainloop, shutdown, || match stream.borrow().get_state() {
        StreamState::Ready => Some(Ok(())),
        StreamState::Failed | StreamState::Terminated => Some(Err(AudioError::StreamConnect {
            source_name: source.to_owned(),
            reason: "stream failed".into(),
        })),
        _ => None,
    })?;

    if let Some(index) = stream.borrow().get_index() {
        let mut volume = ChannelVolumes::default();
        volume.set(spec.channels, Volume::NORMAL);
        context
            .introspect()
            .set_source_output_volume(index, &volume, None);
        context.introspect().set_source_output_mute(index, false, None);
    }

    Ok(stream)
}

fn close_stream(stream: &Rc<RefCell<Stream>>) {
    stream.borrow_mut().set_read_callback(None);
    let _ = stream.borrow_mut().disconnect();
}
