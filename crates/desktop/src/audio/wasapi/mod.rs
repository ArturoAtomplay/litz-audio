use super::capture::{self, Capture, Result};
use crate::server::{CaptureOperation, CaptureRoute, Demand, Ports};
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
        let mut selected = None;
        for i in 0..devices.get_nbr_devices()?.min(32) {
            let device = devices.get_device_at_index(i)?;
            if capture::is_cable(&device) {
                if selected.is_some() {
                    return Err("Multiple VB-CABLE microphone endpoints".into());
                }
                selected = Some(device);
            }
        }
        let device = selected
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

pub fn run(tone: bool, route: Option<Route>) -> Result<()> {
    if tone {
        return Err("The PipeWire tone fixture is Linux-only".into());
    }
    wasapi::initialize_mta().ok()?;
    let _com = Com;
    // SAFETY: static handler only stores an atomic flag; no UI or allocation.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(shutdown), 1);
    }
    let saved = crate::settings::load()?;
    let mut route = route.unwrap_or(match saved.route {
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
    let mut mono = [0u8; pcm::CAPACITY * 4];
    let mut stereo = [0u8; pcm::CAPACITY * 8];
    let mut checked = Instant::now();
    while !STOP.load(Ordering::Acquire) {
        if let Ok((operation, _permit)) = operations.try_recv() {
            match operation {
                CaptureOperation::Inventory(reply) => {
                    let _ = reply.send(capture::outputs().unwrap_or_default());
                }
                CaptureOperation::Active(active, reply) => {
                    let ok = capture
                        .as_mut()
                        .is_some_and(|c| c.set_active(active).is_ok());
                    let _ = reply.send(ok);
                }
                CaptureOperation::Route(request, reply) => {
                    let next_route = request;
                    let result = Capture::open(&next_route).ok().map(|next| {
                        let (producer, samples) = rtrb::RingBuffer::new(capture::CAPACITY);
                        captured = producer;
                        fault = Arc::new(AtomicBool::new(false));
                        let resolved = Some(next.id.clone());
                        capture = Some(next);
                        route = next_route;
                        CaptureRoute {
                            samples,
                            fault: fault.clone(),
                            resolved,
                        }
                    });
                    let _ = reply.send(result);
                }
            }
        }
        if checked.elapsed() >= Duration::from_secs(1) {
            checked = Instant::now();
            if render.is_none() {
                render = Render::open().ok();
                mic_available.store(render.is_some(), Ordering::Release);
            }
            if matches!(route, Route::Mirror { name: None }) {
                let changed = capture::select(&route)
                    .ok()
                    .and_then(|device| device.get_id().ok())
                    != capture.as_ref().map(|c| c.id.clone());
                if changed {
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
