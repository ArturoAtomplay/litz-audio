use super::capture::{self, Capture, Result};
use crate::server::{CaptureCommand, CaptureOperation, CaptureRoute, Demand, Ports};
use litz_core::wake::Wake;
use litz_core::{pcm, protocol::Route};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use wasapi::{DeviceEnumerator, DeviceEventCallbacks, Direction, Role};

use crate::driver::Microphone;

static STOP: AtomicBool = AtomicBool::new(false);
unsafe extern "system" fn shutdown(_: u32) -> i32 {
    STOP.store(true, Ordering::Release);
    1
}
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
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
    Microphone(Microphone),
    Moved(Option<Opened<Box<Capture>>>),
}

/// Enumerates and opens devices, which can take far longer than an audio
/// period, so the audio thread never does. `route` and `capture` describe what
/// the audio thread currently captures.
fn serve_devices(
    work: mpsc::Receiver<DeviceWork>,
    notify: mpsc::Sender<DeviceWork>,
    ready: mpsc::SyncSender<Ready>,
    mic_available: Arc<AtomicBool>,
    mut route: Route,
    mut capture: Option<String>,
) {
    if wasapi::initialize_mta().ok().is_err() {
        return;
    }
    let _com = Com;
    let mut callbacks = DeviceEventCallbacks::new();
    callbacks.set_default_device_callback(move |direction, role, _| {
        if direction == Direction::Render && role == Role::Console {
            let _ = notify.send(DeviceWork::Reopen);
        }
    });
    let _registration = DeviceEnumerator::new()
        .and_then(|devices| devices.register_notification_callback(callbacks))
        .ok();
    let reopen = |route: &Route, capture: &mut Option<String>| {
        let opened = Capture::open(route).ok();
        *capture = opened.as_ref().map(|c| c.id.clone());
        Ready::Moved(opened.map(Box::new).map(Opened))
    };
    let mut checked = Instant::now();
    loop {
        let follow = matches!(route, Route::Mirror { name: None });
        let next = match work.recv_timeout(Duration::from_millis(250)) {
            Ok(DeviceWork::Command((CaptureOperation::Inventory(reply), _permit))) => {
                let _ = reply.send(capture::outputs().unwrap_or_default());
                None
            }
            Ok(DeviceWork::Command((CaptureOperation::Active(active, reply), permit))) => {
                Some(Ready::Active(active, reply, permit))
            }
            Ok(DeviceWork::Command((CaptureOperation::Route(request, reply), permit))) => {
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
            Ok(DeviceWork::Reopen) if follow => Some(reopen(&route, &mut capture)),
            Ok(DeviceWork::Reopen) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Ok(DeviceWork::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if let Some(next) = next
            && ready.send(next).is_err()
        {
            return;
        }
        if checked.elapsed() < Duration::from_secs(1) {
            continue;
        }
        checked = Instant::now();
        if !mic_available.load(Ordering::Acquire)
            && let Ok(microphone) = Microphone::open()
            && ready.send(Ready::Microphone(microphone)).is_err()
        {
            return;
        }
        if matches!(route, Route::Mirror { name: None })
            && capture::select(&route)
                .ok()
                .and_then(|device| device.get_id().ok())
                != capture
            && ready.send(reopen(&route, &mut capture)).is_err()
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
    let mut microphone = Microphone::open().ok();
    if microphone.is_none() {
        eprintln!("litzd: Litz Microphone unavailable; reinstall Litz to restore its audio driver");
    }
    let mut capture = Capture::open(&route).ok();
    let wake = Wake::try_new()?;
    let reset = Arc::new(AtomicBool::new(true));
    let mic_available = Arc::new(AtomicBool::new(microphone.is_some()));
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
        route: effective,
        resolved: Some(output),
        revision: saved.revision,
        voice: saved.voice,
        commands,
        command_slots: Arc::new(tokio::sync::Semaphore::new(1)),
    })?;
    let (prepared, ready) = mpsc::sync_channel(4);
    let devices = {
        let mic_available = mic_available.clone();
        let capture = capture.as_ref().map(|c| c.id.clone());
        std::thread::Builder::new()
            .name("litz-devices".into())
            .spawn(move || serve_devices(work, notify, prepared, mic_available, route, capture))?
    };
    let timer = crate::platform::Timer::new()?;
    let mut feeding = false;
    let mut mono = [0u8; pcm::CAPACITY * 4];
    let mut samples = [0i32; pcm::CAPACITY];
    while !STOP.load(Ordering::Acquire) && !devices.is_finished() {
        if let Ok(ready) = ready.try_recv() {
            match ready {
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
                Ready::Microphone(next) => {
                    microphone = Some(next);
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
        if let Some(m) = &microphone {
            let result = (|| -> Result<()> {
                let (running, queued) = m.state()?;
                if !running {
                    feeding = false;
                    return Ok(());
                }
                let restart = reset.swap(false, Ordering::AcqRel) || !feeding;
                feeding = true;
                if restart {
                    consumer.clear();
                }
                let frames = Microphone::TARGET.saturating_sub(queued).min(pcm::CAPACITY);
                consumer.render(&mut mono[..frames * 4]);
                for (source, sample) in mono[..frames * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(&mut samples)
                {
                    *sample =
                        (f32::from_le_bytes(*source).clamp(-1.0, 1.0) * 2_147_483_648.0) as i32;
                }
                m.write(&samples[..frames])?;
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
                microphone = None;
                feeding = false;
                mic_available.store(false, Ordering::Release);
                consumer.clear();
            }
        }
        wake.signal();
        if feeding {
            timer.sleep(Duration::from_millis(5));
        } else if let Some(c) = capture.as_ref().filter(|c| c.active) {
            let _ = c.event.wait_for_event(10);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_listed_and_routes_opened_without_the_audio_thread() {
        let (commands, work) = CommandSender::channel();
        let notify = commands.notifier();
        let (prepared, ready) = mpsc::sync_channel(4);
        let devices = std::thread::spawn(move || {
            serve_devices(
                work,
                notify,
                prepared,
                Arc::new(AtomicBool::new(true)),
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
        drop(commands);
        devices.join().unwrap();
    }
}
