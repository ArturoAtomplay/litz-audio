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
use wasapi::{
    AudioClient, AudioRenderClient, DeviceEnumerator, Direction, Handle, SampleType, StreamMode,
    WaveFormat,
};

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
struct Render {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
}
impl Render {
    fn open() -> Result<Self> {
        let devices = DeviceEnumerator::new()?.get_device_collection(&Direction::Render)?;
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
            &WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None),
            &Direction::Render,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200000,
            },
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

/// What is left for the audio thread once the slow COM work is done.
enum Ready {
    Active(bool, tokio::sync::oneshot::Sender<bool>, Permit),
    Route(
        Option<Opened<Box<Capture>>>,
        tokio::sync::oneshot::Sender<Option<CaptureRoute>>,
        Permit,
    ),
    Render(Opened<Render>),
    DefaultMoved,
}

/// Enumerates and opens devices, which can take far longer than an audio
/// period, so the audio thread never does. `route` and `capture` describe what
/// the audio thread currently captures.
fn serve_devices(
    operations: mpsc::Receiver<CaptureCommand>,
    ready: mpsc::SyncSender<Ready>,
    mic_available: Arc<AtomicBool>,
    mut route: Route,
    mut capture: Option<String>,
) {
    if wasapi::initialize_mta().ok().is_err() {
        return;
    }
    let _com = Com;
    let mut checked = Instant::now();
    loop {
        let next = match operations.recv_timeout(Duration::from_millis(250)) {
            Ok((CaptureOperation::Inventory(reply), _permit)) => {
                let _ = reply.send(capture::outputs().unwrap_or_default());
                None
            }
            Ok((CaptureOperation::Active(active, reply), permit)) => {
                Some(Ready::Active(active, reply, permit))
            }
            Ok((CaptureOperation::Route(request, reply), permit)) => {
                let opened = Capture::open(&request).ok();
                if let Some(opened) = &opened {
                    capture = Some(opened.id.clone());
                    route = request;
                }
                Some(Ready::Route(
                    opened.map(Box::new).map(Opened),
                    reply,
                    permit,
                ))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
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
            && let Ok(render) = Render::open()
            && ready.send(Ready::Render(Opened(render))).is_err()
        {
            return;
        }
        if matches!(route, Route::Mirror { name: None })
            && capture::select(&route)
                .ok()
                .and_then(|device| device.get_id().ok())
                != capture
            && ready.send(Ready::DefaultMoved).is_err()
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
    let (mic, mut consumer) = pcm::channel();
    let (mut captured, samples) = rtrb::RingBuffer::new(capture::CAPACITY);
    let (mut demand, requests) = rtrb::RingBuffer::new(8);
    let (commands, operations) = mpsc::sync_channel(1);
    let effective = Some(route.clone());
    let resolved = capture.as_ref().map(|c| c.id.clone());
    let _network = crate::server::start(Ports {
        mic,
        mic_available: mic_available.clone(),
        capture: Some(samples),
        capture_fault: fault.clone(),
        demand: requests,
        wake: wake.clone(),
        reset: reset.clone(),
        route: effective,
        resolved,
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
            .spawn(move || serve_devices(operations, prepared, mic_available, route, capture))?
    };
    let mut mono = [0u8; pcm::CAPACITY * 4];
    let mut stereo = [0u8; pcm::CAPACITY * 8];
    while !STOP.load(Ordering::Acquire) && !devices.is_finished() {
        if let Ok(ready) = ready.try_recv() {
            match ready {
                Ready::Active(active, reply, _permit) => {
                    let ok = capture
                        .as_mut()
                        .is_some_and(|c| c.set_active(active).is_ok());
                    let _ = reply.send(ok);
                }
                Ready::Route(opened, reply, _permit) => {
                    let result = opened.map(|Opened(next)| {
                        let (producer, samples) = rtrb::RingBuffer::new(capture::CAPACITY);
                        captured = producer;
                        fault = Arc::new(AtomicBool::new(false));
                        let resolved = Some(next.id.clone());
                        capture = Some(*next);
                        CaptureRoute {
                            samples,
                            fault: fault.clone(),
                            resolved,
                        }
                    });
                    let _ = reply.send(result);
                }
                Ready::Render(Opened(next)) => {
                    render = Some(next);
                    mic_available.store(true, Ordering::Release);
                }
                Ready::DefaultMoved => {
                    if let Some(current) = &mut capture {
                        let _ = current.set_active(false);
                    }
                    fault.store(true, Ordering::Release);
                    wake.signal();
                }
            }
        }
        if let Some(c) = &mut capture
            && let Err(error) = c.drain(&mut captured, &fault)
        {
            eprintln!("litzd: {error}");
            fault.store(true, Ordering::Release);
            let _ = c.set_active(false);
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
                r.render
                    .write_to_device(frames, &stereo[..frames * 8], None)?;
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
        let event = render
            .as_ref()
            .map(|r| &r.event)
            .or_else(|| capture.as_ref().filter(|c| c.active).map(|c| &c.event));
        if let Some(event) = event {
            let _ = event.wait_for_event(10);
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
        let (commands, operations) = mpsc::sync_channel(1);
        let (prepared, ready) = mpsc::sync_channel(4);
        let devices = std::thread::spawn(move || {
            serve_devices(
                operations,
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
