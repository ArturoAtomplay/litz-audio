use super::capture::{self, Capture, Result};
use crate::server::{CaptureCommand, CaptureOperation, CaptureRoute, Demand, Ports};
use litz_core::wake::Wake;
use litz_core::{pcm, protocol::Route};
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, Ordering},
        mpsc,
    },
    time::Instant,
};
use wasapi::{
    AudioClient, AudioRenderClient, DeviceEnumerator, DeviceEventCallbacks, Direction, Handle,
    Role, SampleType, StreamMode, WaveFormat,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

static STOP: AtomicBool = AtomicBool::new(false);
static STOP_SIGNAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
unsafe extern "system" fn shutdown(_: u32) -> i32 {
    STOP.store(true, Ordering::Release);
    let signal = STOP_SIGNAL.load(Ordering::Acquire);
    if !signal.is_null() {
        // SAFETY: run() clears STOP_SIGNAL before it closes the event.
        unsafe { SetEvent(signal) };
    }
    1
}

/// An auto-reset event the audio loop sleeps on besides the device events.
struct Signal(HANDLE);
// SAFETY: an event may be set and waited on from any thread.
unsafe impl Send for Signal {}
// SAFETY: as above; setting an event needs no exclusive access.
unsafe impl Sync for Signal {}
impl Signal {
    fn new() -> Result<Self> {
        // SAFETY: default security, auto-reset, initially unset and unnamed.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(handle))
    }
    fn set(&self) {
        // SAFETY: the event stays open until this Signal is dropped.
        unsafe { SetEvent(self.0) };
    }
}
impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: this Signal owns the handle and nothing uses it afterwards.
        unsafe { CloseHandle(self.0) };
    }
}

/// How the device thread hands work to the audio loop. Dropping it wakes the
/// loop too, so the loop notices the device thread has stopped.
struct Handoff {
    ready: mpsc::SyncSender<Ready>,
    signal: Arc<Signal>,
}
impl Handoff {
    fn send(&self, ready: Ready) -> std::result::Result<(), mpsc::SendError<Ready>> {
        self.ready.send(ready)?;
        self.signal.set();
        Ok(())
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        self.signal.set();
    }
}
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}
struct Render {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
}
impl Render {
    fn open() -> Result<Self> {
        let devices = DeviceEnumerator::new()?.get_device_collection(Direction::Render)?;
        let device = (0..devices.get_nbr_devices()?.min(32))
            .filter_map(|i| devices.get_device_at_index(i).ok())
            .filter(capture::is_cable)
            .min_by_key(|device| {
                !device
                    .get_description()
                    .is_ok_and(|pin| pin == "CABLE Input")
            })
            .ok_or("Install VB-CABLE to use the phone microphone; listening remains available")?;
        let mut client = device.get_iaudioclient()?;
        client.initialize_client(
            &WaveFormat::new(32, 32, SampleType::Float, 48000, 2, None),
            Direction::Render,
            StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200000,
            },
            None,
        )?;
        let event = client.set_get_eventhandle()?;
        let render = client.get_audiorenderclient()?;
        client.start_stream()?;
        Ok(Self {
            client,
            render,
            event,
        })
    }
}
impl Drop for Render {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}

/// A WASAPI object on its way from the device thread to the audio thread.
struct Opened<T>(T);
// SAFETY: COM objects made in the multithreaded apartment may be called from
// any thread that joined it, which both threads do, and only the thread that
// holds the value uses it.
unsafe impl<T> Send for Opened<T> {}

type Permit = tokio::sync::OwnedSemaphorePermit;

pub enum DeviceWork {
    Command(CaptureCommand),
    Reopen,
    /// An endpoint appeared, went away or was reconfigured.
    Changed,
    Closed,
}

struct Commands(mpsc::Sender<DeviceWork>);
impl Drop for Commands {
    fn drop(&mut self) {
        let _ = self.0.send(DeviceWork::Closed);
    }
}

/// The network's handle on the device thread. Notifications also queue
/// work there, so the thread stops when the last of these handles is dropped,
/// not when its channel disconnects.
#[derive(Clone)]
pub struct CommandSender(Arc<Commands>);
impl CommandSender {
    pub fn channel() -> (Self, mpsc::Receiver<DeviceWork>) {
        let (sender, work) = mpsc::channel();
        (Self(Arc::new(Commands(sender))), work)
    }
    pub fn send(
        &self,
        command: CaptureCommand,
    ) -> std::result::Result<(), mpsc::SendError<DeviceWork>> {
        self.0.0.send(DeviceWork::Command(command))
    }
    fn notifier(&self) -> mpsc::Sender<DeviceWork> {
        self.0.0.clone()
    }
}

/// What is left for the audio thread once the slow COM work is done.
enum Ready {
    Active(bool, tokio::sync::oneshot::Sender<bool>, Permit),
    Route(
        Option<Opened<Box<Capture>>>,
        bool,
        tokio::sync::oneshot::Sender<Option<CaptureRoute>>,
        Permit,
    ),
    Render(Opened<Render>),
    Moved(Option<Opened<Box<Capture>>>),
}

/// Enumerates and opens devices, which can take far longer than an audio
/// period, so the audio thread never does. `route` and `capture` describe what
/// the audio thread currently captures.
fn serve_devices(
    work: mpsc::Receiver<DeviceWork>,
    notify: mpsc::Sender<DeviceWork>,
    ready: Handoff,
    mic_available: Arc<AtomicBool>,
    outputs: tokio::sync::watch::Sender<()>,
    mut route: Route,
    mut capture: Option<String>,
) {
    if wasapi::initialize_mta().ok().is_err() {
        return;
    }
    let _com = Com;
    let mut callbacks = DeviceEventCallbacks::new();
    let default = notify.clone();
    callbacks.set_default_device_callback(move |direction, role, _| {
        if direction == Direction::Render && role == Role::Console {
            let _ = default.send(DeviceWork::Reopen);
        }
    });
    let state = notify.clone();
    callbacks.set_device_state_callback(move |_, _| drop(state.send(DeviceWork::Changed)));
    let added = notify.clone();
    callbacks.set_device_added_callback(move |_| drop(added.send(DeviceWork::Changed)));
    let removed = notify.clone();
    callbacks.set_device_removed_callback(move |_| drop(removed.send(DeviceWork::Changed)));
    callbacks.set_property_value_callback(move |_, _| drop(notify.send(DeviceWork::Changed)));
    let _registration = match DeviceEnumerator::new()
        .and_then(|devices| devices.register_notification_callback(callbacks))
    {
        Ok(registration) => registration,
        Err(error) => {
            eprintln!("litzd: device notifications unavailable: {error}");
            return;
        }
    };
    let reopen = |route: &Route, capture: &mut Option<String>| {
        let opened = Capture::open(route).ok();
        *capture = opened.as_ref().map(|c| c.id.clone());
        Ready::Moved(opened.map(Box::new).map(Opened))
    };
    loop {
        let follow = matches!(route, Route::Mirror { name: None });
        let Ok(received) = work.recv() else {
            return;
        };
        let next = match received {
            DeviceWork::Command((CaptureOperation::Inventory(reply), _permit)) => {
                let _ = reply.send(capture::outputs().map_err(|e| e.to_string()));
                None
            }
            DeviceWork::Command((CaptureOperation::Active(active, reply), permit)) => {
                Some(Ready::Active(active, reply, permit))
            }
            DeviceWork::Command((CaptureOperation::Route(request, reply), permit)) => {
                let opened = Capture::open(&request).ok();
                if let Some(opened) = &opened {
                    capture = Some(opened.id.clone());
                    route = request;
                }
                Some(Ready::Route(
                    opened.map(Box::new).map(Opened),
                    matches!(route, Route::Mirror { name: None }),
                    reply,
                    permit,
                ))
            }
            DeviceWork::Reopen if follow => Some(reopen(&route, &mut capture)),
            DeviceWork::Reopen => None,
            DeviceWork::Changed => {
                outputs.send_replace(());
                if !mic_available.load(Ordering::Acquire)
                    && let Ok(render) = Render::open()
                    && ready.send(Ready::Render(Opened(render))).is_err()
                {
                    return;
                }
                (follow
                    && capture::select(&route)
                        .ok()
                        .and_then(|device| device.get_id().ok())
                        != capture)
                    .then(|| reopen(&route, &mut capture))
            }
            DeviceWork::Closed => return,
        };
        if let Some(next) = next
            && ready.send(next).is_err()
        {
            return;
        }
    }
}

pub fn run(tone: bool, route: Option<Route>) -> Result<()> {
    if tone {
        return Err("The PipeWire tone fixture is Linux-only".into());
    }
    wasapi::initialize_mta().ok()?;
    let _com = Com;
    crate::platform::pro_audio_thread();
    // SAFETY: static handler only stores an atomic flag; no UI or allocation.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(shutdown), 1);
    }
    let saved = crate::settings::load()?;
    let route = route.unwrap_or(match saved.route {
        Some(route @ Route::Mirror { .. }) => route,
        _ => Route::Mirror { name: None },
    });
    let mut render = Render::open().ok();
    if render.is_none() {
        eprintln!(
            "litzd: virtual microphone unavailable; install VB-CABLE, then select CABLE Output in calling apps"
        );
    }
    let mut capture = Capture::open(&route).ok();
    let wake = Wake::try_new()?;
    let reset = Arc::new(AtomicBool::new(true));
    let mic_available = Arc::new(AtomicBool::new(render.is_some()));
    let mut fault = Arc::new(AtomicBool::new(false));
    let mut follow = matches!(route, Route::Mirror { name: None });
    let mut listening = false;
    let (mic, mut consumer) = pcm::channel();
    let (mut captured, samples) = rtrb::RingBuffer::new(crate::audio::CAPACITY);
    let (mut demand, requests) = rtrb::RingBuffer::new(8);
    let (commands, work) = CommandSender::channel();
    let notify = commands.notifier();
    let reopen = commands.notifier();
    let effective = Some(route.clone());
    let (outputs_changed, outputs) = tokio::sync::watch::channel(());
    let (mut resolved, output) =
        tokio::sync::watch::channel(capture.as_ref().map(|c| c.id.clone()));
    let _network = crate::server::start(Ports {
        mic,
        mic_available: mic_available.clone(),
        capture: Some(samples),
        capture_fault: fault.clone(),
        demand: requests,
        wake: wake.clone(),
        reset: reset.clone(),
        route: effective.clone(),
        preferred: effective,
        resolved: Some(output),
        outputs,
        revision: saved.revision,
        voice: saved.voice,
        commands,
        command_slots: Arc::new(tokio::sync::Semaphore::new(1)),
    })?;
    let signal = Arc::new(Signal::new()?);
    let (prepared, ready) = mpsc::sync_channel(4);
    let prepared = Handoff {
        ready: prepared,
        signal: signal.clone(),
    };
    let devices = {
        let mic_available = mic_available.clone();
        let capture = capture.as_ref().map(|c| c.id.clone());
        std::thread::Builder::new()
            .name("litz-devices".into())
            .spawn(move || {
                serve_devices(
                    work,
                    notify,
                    prepared,
                    mic_available,
                    outputs_changed,
                    route,
                    capture,
                )
            })?
    };
    let mut mono = [0u8; pcm::CAPACITY * 4];
    let mut stereo = [0u8; pcm::CAPACITY * 8];
    STOP_SIGNAL.store(signal.0, Ordering::Release);
    while !STOP.load(Ordering::Acquire) && !devices.is_finished() {
        while let Ok(next) = ready.try_recv() {
            match next {
                Ready::Active(active, reply, _permit) => {
                    let ok = match &mut capture {
                        Some(c) => c.set_active(active).is_ok(),
                        None => follow,
                    };
                    listening = active && ok;
                    let _ = reply.send(ok);
                }
                Ready::Route(opened, follows, reply, _permit) => {
                    let result = opened.map(|Opened(next)| {
                        let (producer, samples) = rtrb::RingBuffer::new(crate::audio::CAPACITY);
                        captured = producer;
                        fault = Arc::new(AtomicBool::new(false));
                        follow = follows;
                        listening = false;
                        let output;
                        (resolved, output) = tokio::sync::watch::channel(Some(next.id.clone()));
                        capture = Some(*next);
                        CaptureRoute {
                            samples,
                            fault: fault.clone(),
                            resolved: output,
                        }
                    });
                    let _ = reply.send(result);
                }
                Ready::Render(Opened(next)) => {
                    render = Some(next);
                    mic_available.store(true, Ordering::Release);
                }
                Ready::Moved(next) => {
                    capture = next.map(|Opened(next)| *next);
                    if listening
                        && let Some(c) = &mut capture
                        && c.set_active(true).is_err()
                    {
                        capture = None;
                    }
                    resolved.send_replace(capture.as_ref().map(|c| c.id.clone()));
                }
            }
        }
        if let Some(c) = &mut capture
            && let Err(error) = c.drain(&mut captured)
        {
            eprintln!("litzd: {error}");
            if follow {
                capture = None;
                resolved.send_replace(None);
                let _ = reopen.send(DeviceWork::Reopen);
            } else {
                let _ = c.set_active(false);
                fault.store(true, Ordering::Release);
                wake.signal();
            }
        }
        if let Some(r) = &render {
            let result = (|| -> Result<()> {
                let frames = r.client.get_available_space_in_frames()? as usize;
                if frames > pcm::CAPACITY {
                    return Err("WASAPI render backlog".into());
                }
                let restart = reset.swap(false, Ordering::AcqRel);
                if restart {
                    consumer.clear();
                }
                consumer.render(&mut mono[..frames * 4]);
                for (source, dest) in mono[..frames * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(stereo.as_chunks_mut::<8>().0.iter_mut())
                {
                    dest[..4].copy_from_slice(source);
                    dest[4..].copy_from_slice(source);
                }
                r.render.write_to_device(&stereo[..frames * 8], None)?;
                if frames > 0
                    && demand
                        .push(Demand {
                            frames,
                            at: Instant::now(),
                            reset: restart,
                        })
                        .is_err()
                {
                    reset.store(true, Ordering::Release);
                }
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("litzd: {error}");
                render = None;
                mic_available.store(false, Ordering::Release);
                consumer.clear();
            }
        }
        wake.signal();
        let mut events = [signal.0; 3];
        let mut count = 1;
        if let Some(r) = &render {
            events[count] = r.event.as_handle().0;
            count += 1;
        }
        if let Some(c) = capture.as_ref().filter(|c| c.active) {
            events[count] = c.event.as_handle().0;
            count += 1;
        }
        // SAFETY: each handle stays open across the wait: the signal for the
        // whole loop, the device events while `render` and `capture` own them.
        unsafe { WaitForMultipleObjects(count as u32, events.as_ptr(), 0, INFINITE) };
    }
    STOP_SIGNAL.store(std::ptr::null_mut(), Ordering::Release);
    if !STOP.load(Ordering::Acquire) {
        return Err("the device thread stopped".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn outputs_are_listed_and_routes_opened_without_the_audio_thread() {
        let (commands, work) = CommandSender::channel();
        let notify = commands.notifier();
        let signal = Arc::new(Signal::new().unwrap());
        let woken = || {
            // SAFETY: the test holds the signal open across the check.
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(signal.0, 0) == 0 }
        };
        let (prepared, ready) = mpsc::sync_channel(4);
        let prepared = Handoff {
            ready: prepared,
            signal: signal.clone(),
        };
        let devices = std::thread::spawn(move || {
            serve_devices(
                work,
                notify,
                prepared,
                Arc::new(AtomicBool::new(true)),
                tokio::sync::watch::channel(()).0,
                Route::PhoneOnly,
                None,
            )
        });
        let slots = Arc::new(tokio::sync::Semaphore::new(2));
        let (listed, inventory) = tokio::sync::oneshot::channel();
        commands
            .send((
                CaptureOperation::Inventory(listed),
                slots.clone().try_acquire_owned().unwrap(),
            ))
            .unwrap();
        // Nothing drains `ready` here: the answer cannot depend on the audio loop.
        assert!(inventory.blocking_recv().is_ok());
        let (opened, _route) = tokio::sync::oneshot::channel();
        commands
            .send((
                CaptureOperation::Route(Route::PhoneOnly, opened),
                slots.try_acquire_owned().unwrap(),
            ))
            .unwrap();
        let handed = ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(handed, Ready::Route(None, ..)));
        assert!(woken(), "handed work must wake the audio loop");
        drop(commands);
        devices.join().unwrap();
        assert!(woken(), "a stopped device thread must wake the audio loop");
    }
}
