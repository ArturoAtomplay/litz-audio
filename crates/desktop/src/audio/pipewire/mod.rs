use litz_core::pcm::{self, PcmConsumer};
use litz_core::protocol::Route;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Default)]
struct Control {
    format_ready: AtomicBool,
    reset: Arc<AtomicBool>,
    callbacks: AtomicU64,
    frames: AtomicU64,
    silence: AtomicU64,
}

pub fn run(tone: bool, route: Option<Route>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let stopped = Rc::new(Cell::new(false));
    let on_signal = || {
        let stopped = stopped.clone();
        let mainloop = mainloop.clone();
        move || {
            stopped.set(true);
            mainloop.quit();
        }
    };
    let _term = mainloop
        .loop_()
        .add_signal_local(pw::loop_::Signal::TERM, on_signal());
    let _int = mainloop
        .loop_()
        .add_signal_local(pw::loop_::Signal::INT, on_signal());
    let mut backoff = Backoff::default();
    while !stopped.get() {
        let started = std::time::Instant::now();
        if let Err(error) = serve(&mainloop, tone, route.as_ref()) {
            eprintln!("litzd: {error}");
        }
        if stopped.get() {
            break;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            backoff = Backoff::default();
        }
        let delay = backoff.next();
        eprintln!(
            "litzd: recreating PipeWire source in {} ms",
            delay.as_millis()
        );
        let loop_clone = mainloop.clone();
        let timer = mainloop.loop_().add_timer(move |_| loop_clone.quit());
        timer.update_timer(Some(delay), None).into_result()?;
        mainloop.run();
    }
    Ok(())
}

fn serve(mainloop: &pw::main_loop::MainLoopRc, tone: bool, route: Option<&Route>) -> Result<()> {
    let saved = crate::settings::load()?;
    let default_route = Route::Mirror { name: None };
    let route = route.or(saved.route.as_ref()).or(Some(&default_route));
    let context = pw::context::ContextRc::new(mainloop, None)?;
    let core = context.connect_rc(None)?;
    let loop_clone = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _, res, message| {
            // Route replacement can leave queued commands for a node the
            // server has already removed (ENOENT). That is not a lost core.
            if core_failure_requires_restart(id, res) {
                eprintln!("litzd: PipeWire core error {res}: {message}");
                loop_clone.quit();
            }
        })
        .register();

    let (producer, consumer) = pcm::channel();
    let control = Arc::new(Control::default());
    let wake = litz_core::wake::Wake::try_new()?;
    let mut capture =
        route.and_then(
            |route| match super::capture::open(&core, mainloop, route, wake.clone()) {
                Ok(capture) => Some(capture),
                Err(error) => {
                    eprintln!("litzd: capture route unavailable: {error}");
                    None
                }
            },
        );
    let samples = capture.as_mut().and_then(|c| c.samples.take());
    let capture_fault = capture
        .as_ref()
        .map(|c| c.fault.clone())
        .unwrap_or_default();
    let resolved = capture.as_ref().and_then(|c| c.resolved.clone());
    let effective = route.cloned();
    let route_core = core.clone();
    let route_loop = mainloop.clone();
    let capture = std::cell::RefCell::new(capture);
    let route_wake = wake.clone();
    let (commands, command_receiver) = pw::channel::channel::<crate::server::CaptureCommand>();
    let _commands = command_receiver.attach(mainloop.loop_(), move |(operation, _permit)| {
        use crate::server::CaptureOperation;
        match operation {
            CaptureOperation::Active(active, response) => {
                let applied = capture
                    .borrow()
                    .as_ref()
                    .is_some_and(|c| c.stream.set_active(active).is_ok());
                let _ = response.send(applied);
            }
            CaptureOperation::Inventory(response) => {
                let _ = response
                    .send(super::capture::inventory(&route_core, &route_loop).unwrap_or_default());
            }
            CaptureOperation::Route(route, response) => {
                let result =
                    super::capture::open(&route_core, &route_loop, &route, route_wake.clone())
                        .ok()
                        .map(|mut next| {
                            let result = crate::server::CaptureRoute {
                                samples: next.samples.take().unwrap(),
                                fault: next.fault.clone(),
                                resolved: next.resolved.clone(),
                            };
                            *capture.borrow_mut() = Some(next);
                            result
                        });
                let _ = response.send(result);
            }
        }
    });
    let (mut demand_tx, demand_rx) = rtrb::RingBuffer::new(8);
    let ports = crate::server::Ports {
        mic: producer,
        mic_available: Arc::new(AtomicBool::new(true)),
        capture: samples,
        capture_fault,
        demand: demand_rx,
        wake: wake.clone(),
        reset: control.reset.clone(),
        route: effective,
        resolved,
        revision: saved.revision,
        voice: saved.voice,
        commands,
        command_slots: Arc::new(tokio::sync::Semaphore::new(1)),
    };
    #[cfg(feature = "test-tone")]
    let (fixture, _network) = if tone {
        (Some(super::tone::Fixture::new(ports.mic, true)), None)
    } else {
        (None, crate::server::start_network(ports))
    };
    #[cfg(feature = "test-tone")]
    let fixture_control = fixture.as_ref().map(|fixture| fixture.control());
    #[cfg(not(feature = "test-tone"))]
    let _network = {
        let _ = tone;
        crate::server::start_network(ports)
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "Litz Microphone",
        properties! {
            "node.name" => "litz.microphone",
            "node.description" => "Litz Microphone",
            "media.class" => "Audio/Source",
            "media.type" => "Audio",
            "media.category" => "Capture",
            "media.role" => "Communication",
            "node.virtual" => "true",
            "audio.channels" => "1",
            "audio.position" => "[ MONO ]",
            "node.latency" => "512/48000",
            "node.always-process" => "false",
            "node.pause-on-idle" => "true",
        },
    )?;

    let loop_clone = mainloop.clone();
    let state_control = control.clone();
    let format_control = control.clone();
    let format_loop = mainloop.clone();
    let _control_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            eprintln!("litzd: stream {old:?} -> {new:?}");
            if !matches!(new, pw::stream::StreamState::Streaming) {
                state_control.reset.store(true, Ordering::Release);
            }
            #[cfg(feature = "test-tone")]
            if let Some(fixture_control) = &fixture_control {
                fixture_control.set_streaming(matches!(new, pw::stream::StreamState::Streaming));
            }
            if matches!(
                new,
                pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
            ) {
                loop_clone.quit();
            }
        })
        .param_changed(move |_, _, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let valid = param.is_some_and(|pod| {
                let mut info = AudioInfoRaw::new();
                spa::param::format_utils::parse_format(pod).is_ok_and(|(kind, subtype)| {
                    kind == spa::param::format::MediaType::Audio
                        && subtype == spa::param::format::MediaSubtype::Raw
                }) && info.parse(pod).is_ok()
                    && info.format() == AudioFormat::F32LE
                    && info.rate() == pcm::RATE
                    && info.channels() == 1
            });
            format_control.format_ready.store(valid, Ordering::Release);
            format_control.reset.store(true, Ordering::Release);
            if param.is_some() && !valid {
                eprintln!("litzd: rejected unexpected stream format");
                format_loop.quit();
            }
        })
        .register()?;

    let rt_control = control.clone();
    let _process_listener = stream
        .add_local_listener_with_user_data(consumer)
        .process(move |stream, consumer| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested();
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let reset = rt_control.reset.load(Ordering::Acquire);
            let size = if let Some(bytes) = data.data() {
                render(consumer, &rt_control, bytes, requested)
            } else {
                0
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = size_of::<f32>() as i32;
            *chunk.size_mut() = size as u32;
            let _ = demand_tx.push(crate::server::Demand {
                frames: size / 4,
                at: std::time::Instant::now(),
                reset,
            });
            wake.signal();
        })
        .register()?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(pcm::RATE);
    info.set_channels(1);
    let mut positions = [0; spa::param::audio::MAX_CHANNELS];
    positions[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(positions);
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("invalid format pod")?;
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut [pod],
    )?;
    mainloop.run();
    let _ = stream.disconnect();
    eprintln!(
        "litzd: callbacks={} frames={} silent_frames={}",
        control.callbacks.load(Ordering::Relaxed),
        control.frames.load(Ordering::Relaxed),
        control.silence.load(Ordering::Relaxed)
    );
    Ok(())
}

fn core_failure_requires_restart(id: u32, result: i32) -> bool {
    id == pw::core::PW_ID_CORE && result != -libc::ENOENT
}

fn render(
    consumer: &mut PcmConsumer,
    control: &Control,
    bytes: &mut [u8],
    requested: u64,
) -> usize {
    if control.reset.swap(false, Ordering::AcqRel) {
        consumer.clear();
    }
    let capacity = bytes.len() / size_of::<f32>();
    let frames = if requested == 0 {
        capacity
    } else {
        capacity.min(requested as usize)
    };
    let size = frames * size_of::<f32>();
    let read = if control.format_ready.load(Ordering::Acquire) {
        consumer.render(&mut bytes[..size])
    } else {
        bytes[..size].fill(0);
        0
    };
    control.callbacks.fetch_add(1, Ordering::Relaxed);
    control.frames.fetch_add(frames as u64, Ordering::Relaxed);
    control
        .silence
        .fetch_add((frames - read) as u64, Ordering::Relaxed);
    size
}

#[derive(Default)]
struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        let base_ms = 250_u64 << self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % (base_ms / 4 + 1);
        Duration::from_millis(base_ms - base_ms / 4 + jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_route_object_does_not_restart_the_core() {
        assert!(!core_failure_requires_restart(
            pw::core::PW_ID_CORE,
            -libc::ENOENT
        ));
        assert!(core_failure_requires_restart(
            pw::core::PW_ID_CORE,
            -libc::EPIPE
        ));
        assert!(core_failure_requires_restart(
            pw::core::PW_ID_CORE,
            -libc::ECONNRESET
        ));
        assert!(!core_failure_requires_restart(7, -libc::ENOENT));
    }

    #[test]
    fn requested_frames_are_bounded_and_format_reset_is_silent() {
        let (mut tx, mut rx) = pcm::channel();
        let control = Control::default();
        control.format_ready.store(true, Ordering::Relaxed);
        tx.push(&[0.25; 10]);
        let mut bytes = [255; 67];
        assert_eq!(render(&mut rx, &control, &mut bytes, 3), 12);
        assert_eq!(&bytes[..4], &0.25_f32.to_ne_bytes());
        assert_eq!(render(&mut rx, &control, &mut bytes, u64::MAX), 64);
        assert_eq!(&bytes[28..64], &[0; 36]);
        tx.push(&[1.0; 10]);
        control.reset.store(true, Ordering::Relaxed);
        assert_eq!(render(&mut rx, &control, &mut bytes, 0), 64);
        assert_eq!(&bytes[..64], &[0; 64]);
    }

    #[test]
    fn reconnect_backoff_is_bounded() {
        let mut backoff = Backoff::default();
        for _ in 0..100 {
            let delay = backoff.next();
            assert!(delay >= Duration::from_millis(187));
            assert!(delay <= Duration::from_secs(8));
        }
    }
}
