use super::mic::{Backoff, Control, render};
use crate::log::warning;
use crate::server::{CaptureCommand, CaptureOperation, CaptureRoute, Demand, Ports};
use libpulse_binding as pulse;
use litz_core::{pcm, protocol::Route, wake::Wake};
use pulse::context::{Context, FlagSet as ContextFlags, State as ContextState};
use pulse::def::BufferAttr;
use pulse::mainloop::api::Mainloop as _;
use pulse::mainloop::standard::Mainloop;
use pulse::proplist::{Proplist, properties};
use pulse::sample::{Format, Spec};
use pulse::stream::{FlagSet as StreamFlags, SeekMode, State as StreamState, Stream};
use pulse::time::MicroSeconds;
use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

mod capture;
use capture::Capture;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const MIC_SINK: &str = "litz.microphone_sink";
const MIC_SOURCE: &str = "litz.microphone";
pub(super) const SPEAKER: &str = "litz.speaker";

#[derive(Clone)]
pub struct CommandSender {
    queue: mpsc::Sender<CaptureCommand>,
    wake: Wake,
}

impl CommandSender {
    pub fn send(&self, command: CaptureCommand) -> std::result::Result<(), CaptureCommand> {
        self.queue.send(command).map_err(|error| error.0)?;
        self.wake.signal();
        Ok(())
    }
}

/// A connection to the sound server. Every request completes by running the
/// mainloop from here, outside its callbacks, which libpulse does not allow
/// to nest.
pub(super) struct Server {
    context: Context,
    mainloop: Mainloop,
    pending: Rc<Cell<usize>>,
}

// The context must be freed before the mainloop it was created on.
impl Drop for Server {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

impl Server {
    /// Never spawns a daemon; socket activation starts the session's server.
    fn connect() -> Result<Self> {
        let mut mainloop = Mainloop::new().ok_or("PulseAudio mainloop")?;
        let mut props = Proplist::new().ok_or("PulseAudio properties")?;
        let _ = props.set_str(properties::APPLICATION_NAME, "Litz Audio");
        let _ = props.set_str(properties::APPLICATION_ICON_NAME, "audio-input-microphone");
        let mut context = Context::new_with_proplist(&mainloop, "Litz Audio", &props)
            .ok_or("PulseAudio context")?;
        context.connect(None, ContextFlags::NOAUTOSPAWN, None)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match context.get_state() {
                ContextState::Ready => break,
                ContextState::Failed | ContextState::Terminated => {
                    return Err("no PulseAudio server".into());
                }
                _ if Instant::now() >= deadline => {
                    return Err("PulseAudio connection timeout".into());
                }
                _ => step(&mut mainloop, Some(MicroSeconds(20_000)))?,
            }
        }
        Ok(Self {
            context,
            mainloop,
            pending: Rc::default(),
        })
    }

    fn healthy(&self) -> bool {
        self.context.get_state() == ContextState::Ready
    }

    fn settle(&mut self, done: impl Fn() -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done() {
            if !self.healthy() {
                return Err("PulseAudio connection lost".into());
            }
            if Instant::now() >= deadline {
                return Err("PulseAudio request timeout".into());
            }
            step(&mut self.mainloop, Some(MicroSeconds(20_000)))?;
        }
        Ok(())
    }

    pub(super) fn wait<T: 'static>(
        &mut self,
        request: impl FnOnce(&mut Context, Rc<RefCell<Option<T>>>),
    ) -> Result<T> {
        let answer = Rc::new(RefCell::new(None));
        request(&mut self.context, answer.clone());
        self.settle(|| answer.borrow().is_some())?;
        Ok(answer.take().expect("settled"))
    }

    fn server_name(&mut self) -> Result<String> {
        self.wait(|context, answer| {
            context.introspect().get_server_info(move |info| {
                *answer.borrow_mut() = Some(info.server_name.as_deref().unwrap_or("").to_owned());
            });
        })
    }

    pub(super) fn default_sink(&mut self) -> Result<Option<String>> {
        self.wait(|context, answer| {
            context.introspect().get_server_info(move |info| {
                *answer.borrow_mut() = Some(info.default_sink_name.as_deref().map(str::to_owned));
            });
        })
    }

    pub(super) fn load(&mut self, module: &str, argument: &str) -> Result<u32> {
        let index = self.wait(|context, answer| {
            context
                .introspect()
                .load_module(module, argument, move |index| {
                    *answer.borrow_mut() = Some(index);
                });
        })?;
        if index == pulse::def::INVALID_INDEX {
            return Err(format!("PulseAudio refused {module}").into());
        }
        Ok(index)
    }

    /// Queues the unload; `finish` waits for every queued one.
    pub(super) fn unload(&mut self, index: u32) {
        let pending = self.pending.clone();
        pending.set(pending.get() + 1);
        self.context.introspect().unload_module(index, move |_| {
            pending.set(pending.get() - 1);
        });
    }

    fn finish(&mut self) {
        let pending = self.pending.clone();
        let _ = self.settle(|| pending.get() == 0);
    }

    /// Removes the devices a previous litzd left behind when it was killed.
    fn remove_stale(&mut self) -> Result<()> {
        let stale = self.wait(|context, answer| {
            let mut found = Vec::new();
            context
                .introspect()
                .get_module_info_list(move |item| match item {
                    pulse::callbacks::ListResult::Item(module) => {
                        if module.argument.as_deref().is_some_and(owned_by_litz) {
                            found.push(module.index);
                        }
                    }
                    _ => *answer.borrow_mut() = Some(std::mem::take(&mut found)),
                });
        })?;
        for index in stale {
            self.unload(index);
        }
        self.finish();
        Ok(())
    }
}

fn owned_by_litz(argument: &str) -> bool {
    [MIC_SINK, MIC_SOURCE, SPEAKER].iter().any(|name| {
        argument.starts_with(&format!("sink_name={name} "))
            || argument.starts_with(&format!("source_name={name} "))
    })
}

fn step(mainloop: &mut Mainloop, timeout: Option<MicroSeconds>) -> Result<()> {
    mainloop.prepare(timeout)?;
    mainloop.poll()?;
    mainloop.dispatch()?;
    Ok(())
}

/// True when the session's sound server is PulseAudio itself, not
/// pipewire-pulse, which reports itself in its server name.
pub fn serves_audio() -> bool {
    Server::connect()
        .and_then(|mut server| server.server_name())
        .is_ok_and(|name| !name.to_ascii_lowercase().contains("pipewire"))
}

/// The virtual microphone: a null sink the phone's voice is played into and
/// a remapped source of its monitor, which applications accept as a
/// microphone where they reject monitors.
struct Microphone {
    stream: Rc<RefCell<Stream>>,
    modules: [u32; 2],
}

impl Microphone {
    fn open(
        server: &mut Server,
        mut consumer: pcm::PcmConsumer,
        control: Arc<Control>,
        mut demand: rtrb::Producer<Demand>,
        wake: Wake,
    ) -> Result<Self> {
        let sink = server.load(
            "module-null-sink",
            &format!(
                "sink_name={MIC_SINK} format=float32le rate={} channels=1 \
                 sink_properties='device.description=\"Litz Microphone input\"'",
                pcm::RATE
            ),
        )?;
        let source = match server.load(
            "module-remap-source",
            &format!(
                "source_name={MIC_SOURCE} master={MIC_SINK}.monitor channels=1 \
                 source_properties='device.description=\"Litz Microphone\" \
                 device.icon_name=audio-input-microphone'"
            ),
        ) {
            Ok(source) => source,
            Err(error) => {
                server.unload(sink);
                server.finish();
                return Err(error);
            }
        };
        let modules = [sink, source];
        let opened = (|| {
            let spec = Spec {
                format: Format::F32le,
                rate: pcm::RATE,
                channels: 1,
            };
            let mut props = Proplist::new().ok_or("PulseAudio properties")?;
            let _ = props.set_str(properties::MEDIA_ROLE, "phone");
            let stream = Rc::new(RefCell::new(
                Stream::new_with_proplist(
                    &mut server.context,
                    "Litz Microphone",
                    &spec,
                    None,
                    &mut props,
                )
                .ok_or("PulseAudio microphone stream")?,
            ));
            let frame = size_of::<f32>() as u32;
            let attr = BufferAttr {
                maxlength: u32::MAX,
                tlength: 1024 * frame,
                prebuf: 0,
                minreq: 256 * frame,
                fragsize: u32::MAX,
            };
            let writer = Rc::downgrade(&stream);
            let mut bytes = [0u8; pcm::CAPACITY * 4];
            control.format_ready.store(true, Ordering::Release);
            stream
                .borrow_mut()
                .set_write_callback(Some(Box::new(move |requested| {
                    let Some(stream) = writer.upgrade() else {
                        return;
                    };
                    let reset = control.reset.load(Ordering::Acquire);
                    let frames = (requested / size_of::<f32>()) as u64;
                    let size = render(&mut consumer, &control, &mut bytes, frames);
                    if size == 0 {
                        return;
                    }
                    let _ = stream
                        .borrow_mut()
                        .write_copy(&bytes[..size], 0, SeekMode::Relative);
                    let _ = demand.push(Demand {
                        frames: size / size_of::<f32>(),
                        at: Instant::now(),
                        reset,
                    });
                    wake.signal();
                })));
            stream.borrow_mut().connect_playback(
                Some(MIC_SINK),
                Some(&attr),
                StreamFlags::DONT_MOVE | StreamFlags::ADJUST_LATENCY,
                None,
                None,
            )?;
            let ready = stream.clone();
            server.settle(|| ready.borrow().get_state() == StreamState::Ready)?;
            Ok::<_, Box<dyn std::error::Error>>(stream)
        })();
        match opened {
            Ok(stream) => Ok(Self { stream, modules }),
            Err(error) => {
                for module in modules {
                    server.unload(module);
                }
                server.finish();
                Err(error)
            }
        }
    }

    fn alive(&self) -> bool {
        self.stream.borrow().get_state().is_good()
    }

    fn close(self, server: &mut Server) {
        let mut stream = self.stream.borrow_mut();
        stream.set_write_callback(None);
        let _ = stream.disconnect();
        drop(stream);
        for module in self.modules.into_iter().rev() {
            server.unload(module);
        }
    }
}

static STOPPED: AtomicBool = AtomicBool::new(false);
static STOP_WAKE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn stop(_: libc::c_int) {
    STOPPED.store(true, Ordering::Release);
    let fd = STOP_WAKE.load(Ordering::Acquire);
    let one = 1u64;
    // SAFETY: write is async-signal-safe; fd is the eventfd run() keeps open
    // for the life of the process, and the 8-byte value is on this frame.
    unsafe {
        libc::write(fd, (&one as *const u64).cast(), 8);
    }
}

fn stopped() -> bool {
    STOPPED.load(Ordering::Acquire)
}

pub fn run(route: Option<Route>) -> Result<()> {
    let stop_wake = Wake::try_new()?;
    STOP_WAKE.store(stop_wake.as_raw_fd(), Ordering::Release);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only stores an atomic and writes to an eventfd,
        // both async-signal-safe.
        unsafe {
            libc::signal(
                signal,
                stop as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
    let mut backoff = Backoff::default();
    loop {
        let started = Instant::now();
        if let Err(error) = serve(route.as_ref(), &stop_wake) {
            warning!("{error}");
        }
        if stopped() {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(30) {
            backoff = Backoff::default();
        }
        let delay = backoff.next();
        warning!("reconnecting to PulseAudio in {} ms", delay.as_millis());
        let mut stop = [libc::pollfd {
            fd: stop_wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one pollfd for the eventfd run() keeps open for this call.
        unsafe {
            libc::poll(stop.as_mut_ptr(), 1, delay.as_millis() as libc::c_int);
        }
        if stopped() {
            return Ok(());
        }
    }
}

fn serve(route: Option<&Route>, stop_wake: &Wake) -> Result<()> {
    let saved = crate::settings::load()?;
    let default_route = Route::Mirror { name: None };
    let route = route
        .or(saved.route.as_ref())
        .unwrap_or(&default_route)
        .clone();
    let mut server = Server::connect()?;
    let drained = stop_wake.clone();
    let _stop_event = server.mainloop.new_io_event(
        stop_wake.as_raw_fd(),
        pulse::mainloop::events::io::FlagSet::INPUT,
        Box::new(move |_, _, _| drained.drain()),
    );
    serve_connected(&mut server, route, saved)
}

fn serve_connected(
    server: &mut Server,
    route: Route,
    saved: crate::settings::Settings,
) -> Result<()> {
    server.remove_stale()?;
    let (producer, consumer) = pcm::channel();
    let control = Arc::new(Control::default());
    let wake = Wake::try_new()?;
    let (demand_tx, demand_rx) = rtrb::RingBuffer::new(8);
    let microphone = Microphone::open(server, consumer, control.clone(), demand_tx, wake.clone())?;
    let changed = Rc::new(Cell::new(false));
    let on_change = changed.clone();
    let (outputs_changed, outputs) = tokio::sync::watch::channel(());
    server
        .context
        .set_subscribe_callback(Some(Box::new(move |facility, operation, _| {
            on_change.set(true);
            if facility == Some(pulse::context::subscribe::Facility::Sink)
                && matches!(
                    operation,
                    Some(
                        pulse::context::subscribe::Operation::New
                            | pulse::context::subscribe::Operation::Removed
                    )
                )
            {
                outputs_changed.send_replace(());
            }
        })));
    server.context.subscribe(
        pulse::context::subscribe::InterestMaskSet::SERVER
            | pulse::context::subscribe::InterestMaskSet::SINK,
        |_| {},
    );
    let mut capture = match Capture::open(server, &route, wake.clone()) {
        Ok(capture) => Some(capture),
        Err(error) => {
            warning!("capture route unavailable: {error}");
            None
        }
    };
    let commands_wake = Wake::try_new()?;
    let (queue, commands) = mpsc::channel();
    let drained = commands_wake.clone();
    let _commands_event = server.mainloop.new_io_event(
        commands_wake.as_raw_fd(),
        pulse::mainloop::events::io::FlagSet::INPUT,
        Box::new(move |_, _, _| drained.drain()),
    );
    let ports = Ports {
        mic: producer,
        mic_available: Arc::new(AtomicBool::new(true)),
        capture: capture.as_mut().and_then(|c| c.samples.take()),
        capture_fault: capture
            .as_ref()
            .map(|c| c.fault.clone())
            .unwrap_or_default(),
        demand: demand_rx,
        wake: wake.clone(),
        reset: control.reset.clone(),
        route: Some(route.clone()),
        preferred: Some(route),
        resolved: capture.as_ref().map(|c| c.resolved.subscribe()),
        outputs,
        revision: saved.revision,
        voice: saved.voice,
        commands: super::CommandSender::Pulse(CommandSender {
            queue,
            wake: commands_wake,
        }),
        command_slots: Arc::new(tokio::sync::Semaphore::new(1)),
    };
    let network = crate::server::start_network(ports);
    let result = (|| {
        while !stopped() {
            step(&mut server.mainloop, None)?;
            if !server.healthy() {
                return Err("PulseAudio connection lost".into());
            }
            if !microphone.alive() {
                return Err("PulseAudio closed the virtual microphone".into());
            }
            if capture.as_ref().is_some_and(|c| c.lost()) {
                return Err("PulseAudio closed the capture stream".into());
            }
            if changed.replace(false)
                && let Some(c) = &capture
            {
                let default = server.default_sink()?;
                c.follow(default);
            }
            while let Ok((operation, _permit)) = commands.try_recv() {
                handle(server, &mut capture, operation, &wake)?;
            }
        }
        Ok(())
    })();
    drop(network);
    if let Some(capture) = capture {
        capture.close(server);
    }
    microphone.close(server);
    server.finish();
    control.report();
    result
}

fn handle(
    server: &mut Server,
    capture: &mut Option<Capture>,
    operation: CaptureOperation,
    wake: &Wake,
) -> Result<()> {
    match operation {
        CaptureOperation::Active(active, reply) => {
            let applied = match capture {
                Some(c) => c.set_active(server, active).is_ok(),
                None => false,
            };
            let _ = reply.send(applied);
        }
        CaptureOperation::Inventory(reply) => {
            let _ = reply.send(capture::outputs(server).map_err(|e| e.to_string()));
        }
        CaptureOperation::Route(route, reply) => {
            if route == Route::PhoneOnly
                && let Some(old) = capture.take_if(|c| c.is_speaker())
            {
                old.close(server);
                server.finish();
            }
            let opened = match Capture::open(server, &route, wake.clone()) {
                Ok(mut next) => {
                    let result = CaptureRoute {
                        samples: next.samples.take().expect("fresh capture"),
                        fault: next.fault.clone(),
                        resolved: next.resolved.subscribe(),
                    };
                    if let Some(old) = capture.replace(next) {
                        old.close(server);
                    }
                    Some(result)
                }
                Err(error) => {
                    warning!("capture route unavailable: {error}");
                    None
                }
            };
            let _ = reply.send(opened);
        }
    }
    if server.healthy() {
        Ok(())
    } else {
        Err("PulseAudio connection lost".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_litz_devices_count_as_stale() {
        assert!(owned_by_litz(
            "sink_name=litz.microphone_sink format=float32le rate=48000 channels=1"
        ));
        assert!(owned_by_litz("source_name=litz.microphone master=x"));
        assert!(owned_by_litz("sink_name=litz.speaker format=float32le"));
        assert!(!owned_by_litz("sink_name=litz.microphone_sink2 rate=1"));
        assert!(!owned_by_litz("sink_name=headphones master=litz.speaker"));
    }
}
