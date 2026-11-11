use litz_core::protocol::{Endpoint, Route};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, Handle, SampleType, StreamMode, WaveFormat,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, IAudioCaptureClient,
};

use crate::audio::{CAPACITY, Captured};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Matches the adapter VB-CABLE's driver installs, not an endpoint name people
/// can rename in Sound settings. Its "CABLE Input" and "CABLE In 16 Ch" pins
/// both feed CABLE Output, so neither is a listening source.
pub fn is_cable(device: &Device) -> bool {
    device
        .get_interface_friendlyname()
        .is_ok_and(|name| name == "VB-Audio Virtual Cable")
}
pub fn outputs() -> Result<Vec<Endpoint>> {
    let devices = DeviceEnumerator::new()?.get_device_collection(&Direction::Render)?;
    let mut result = Vec::new();
    for i in 0..devices.get_nbr_devices()?.min(32) {
        let device = devices.get_device_at_index(i)?;
        if is_cable(&device) {
            continue;
        }
        let name = device.get_id()?;
        let label = device.get_friendlyname()?;
        if name.len() <= 256 && label.len() <= 256 {
            result.push(Endpoint { name, label });
        }
    }
    Ok(result)
}
pub fn select(route: &Route) -> Result<Device> {
    let enumerator = DeviceEnumerator::new()?;
    let device = match route {
        Route::Mirror { name: Some(id) } => enumerator.get_device(id)?,
        Route::Mirror { name: None } => enumerator.get_default_device(&Direction::Render)?,
        Route::PhoneOnly => return Err("Phone-only output is unavailable".into()),
    };
    if device.get_direction() != Direction::Render || is_cable(&device) {
        return Err("Microphone loopback is not a listening source".into());
    }
    Ok(device)
}

pub struct Capture {
    pub client: AudioClient,
    capture: IAudioCaptureClient,
    pub event: Handle,
    pub id: String,
    pub active: bool,
    bytes: [u8; CAPACITY * 8],
    first: bool,
}
impl Capture {
    pub fn open(route: &Route) -> Result<Self> {
        let device = select(route)?;
        let id = device.get_id()?;
        let mut client = device.get_iaudioclient()?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
        client.initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200000,
            },
        )?;
        let event = client.set_get_eventhandle()?;
        // SAFETY: WASAPI initialized for capture; COM keeps this service alive.
        let capture = unsafe {
            client
                .as_iaudioclient()
                .GetService::<IAudioCaptureClient>()?
        };
        Ok(Self {
            client,
            capture,
            event,
            id,
            active: false,
            bytes: [0; CAPACITY * 8],
            first: true,
        })
    }
    pub fn set_active(&mut self, active: bool) -> Result<()> {
        if active != self.active {
            if active {
                self.client.start_stream()?;
                self.first = true;
            } else {
                self.client.stop_stream()?;
                self.client.reset_stream()?;
            }
            self.active = active;
        }
        Ok(())
    }
    pub fn drain(
        &mut self,
        output: &mut rtrb::Producer<Captured>,
        fault: &Arc<AtomicBool>,
    ) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        for _ in 0..8 {
            // SAFETY: capture COM object belongs to this audio thread.
            let available = unsafe { self.capture.GetNextPacketSize()? } as usize;
            if available == 0 {
                break;
            }
            if available > CAPACITY {
                return Err("WASAPI capture backlog".into());
            }
            let mut pointer = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            // Do not use wasapi 0.26's slice helper here: a SILENT buffer is
            // allowed to have a null data pointer. Copy only non-silent PCM.
            // SAFETY: GetBuffer owns the returned bytes until ReleaseBuffer below;
            // capacity and the silent/null cases are checked before copying.
            unsafe {
                self.capture
                    .GetBuffer(&mut pointer, &mut frames, &mut flags, None, None)?;
                let valid = frames as usize <= CAPACITY;
                if valid {
                    let bytes = &mut self.bytes[..frames as usize * 8];
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        bytes.fill(0);
                    } else if !pointer.is_null() {
                        bytes.copy_from_slice(std::slice::from_raw_parts(pointer, bytes.len()));
                    } else {
                        self.capture.ReleaseBuffer(frames)?;
                        return Err("Missing WASAPI PCM".into());
                    }
                }
                self.capture.ReleaseBuffer(frames)?;
                if !valid {
                    return Err("Oversized WASAPI packet".into());
                }
            }
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && !self.first {
                return Err("WASAPI capture discontinuity".into());
            }
            self.first = false;
            let at = Instant::now();
            for bytes in self.bytes[..frames as usize * 8].as_chunks::<8>().0 {
                let samples = [
                    f32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    f32::from_le_bytes(bytes[4..].try_into().unwrap()),
                ];
                if output.push(Captured::new(samples, at)).is_err() {
                    fault.store(true, Ordering::Release);
                    return Err("WASAPI capture overflow".into());
                }
            }
        }
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}
