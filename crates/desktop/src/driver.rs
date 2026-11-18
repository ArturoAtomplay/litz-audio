use std::{
    ffi::{OsString, c_void},
    fs, io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIGCF_ALLCLASSES, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    DiUninstallDevice, HDEVINFO, INF_STYLE_WIN4, INFCONTEXT, INSTALLFLAG_NONINTERACTIVE,
    MAX_CLASS_NAME_LEN, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupCloseInfFile, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW,
    SetupDiSetDeviceRegistryPropertyW, SetupFindFirstLineW, SetupGetStringFieldW,
    SetupOpenInfFileW, SetupUninstallOEMInfW, UpdateDriverForPlugAndPlayDevicesW,
};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::core::GUID;

const HARDWARE_ID: &str = "ROOT\\LitzAudio";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct DeviceSet(HDEVINFO);
impl DeviceSet {
    fn new(set: HDEVINFO) -> io::Result<Self> {
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(set))
        }
    }

    /// Every device, present or not, whose hardware IDs include ours.
    fn ours(&self) -> Vec<SP_DEVINFO_DATA> {
        let mut found = Vec::new();
        for index in 0.. {
            let mut device = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: the set is open and `device` has its size field set.
            if unsafe { SetupDiEnumDeviceInfo(self.0, index, &mut device) } == 0 {
                break;
            }
            if self.is_ours(&device) {
                found.push(device);
            }
        }
        found
    }

    fn is_ours(&self, device: &SP_DEVINFO_DATA) -> bool {
        let mut ids = [0u16; 512];
        // SAFETY: the buffer length is passed in bytes and outlives the call.
        let read = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                self.0,
                device,
                SPDRP_HARDWAREID,
                ptr::null_mut(),
                ids.as_mut_ptr().cast(),
                size_of_val(&ids) as u32,
                ptr::null_mut(),
            )
        };
        read != 0
            && ids
                .split(|&c| c == 0)
                .take_while(|id| !id.is_empty())
                .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(HARDWARE_ID))
    }

    /// The path of our device's first interface in `class`.
    fn interface(&self, class: &GUID) -> Option<Vec<u16>> {
        for index in 0.. {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: the set is open and `interface` has its size field set.
            if unsafe {
                SetupDiEnumDeviceInterfaces(self.0, ptr::null(), class, index, &mut interface)
            } == 0
            {
                return None;
            }
            let mut detail = [0u32; 512];
            detail[0] = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut device = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: `detail` is u32-aligned with cbSize set, and its byte
            // length is passed; `device` has its size field set.
            let ok = unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    self.0,
                    &interface,
                    detail.as_mut_ptr().cast(),
                    size_of_val(&detail) as u32,
                    ptr::null_mut(),
                    &mut device,
                )
            };
            if ok != 0 && self.is_ours(&device) {
                let path = device_path(&detail);
                let end = path.iter().position(|&c| c == 0)?;
                return Some(path[..=end].to_vec());
            }
        }
        None
    }
}

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the set was opened by SetupAPI and is destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// The DevicePath that follows the cbSize field.
fn device_path(detail: &[u32]) -> &[u16] {
    let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    // SAFETY: u16 has a smaller alignment than u32 and the slice stays
    // within `detail`.
    unsafe {
        std::slice::from_raw_parts(
            detail.as_ptr().cast::<u8>().add(offset).cast(),
            (size_of_val(detail) - offset) / 2,
        )
    }
}

fn all_devices() -> io::Result<DeviceSet> {
    // SAFETY: null class and enumerator with DIGCF_ALLCLASSES list every device.
    DeviceSet::new(unsafe {
        SetupDiGetClassDevsW(ptr::null(), ptr::null(), ptr::null_mut(), DIGCF_ALLCLASSES)
    })
}

/// Creates the root-enumerated Litz Audio device once, then installs its
/// driver from `inf` unless the device already has this or a newer one.
/// Requires an elevated process. Returns whether Windows needs a restart
/// because the old driver was in use.
pub fn install(inf: &Path) -> io::Result<bool> {
    let inf = wide(&std::path::absolute(inf)?.to_string_lossy());
    if all_devices()?.ours().is_empty() {
        let mut class = GUID::from_u128(0);
        let mut class_name = [0u16; MAX_CLASS_NAME_LEN as usize];
        // SAFETY: both output buffers are sized as passed.
        check(unsafe {
            SetupDiGetINFClassW(
                inf.as_ptr(),
                &mut class,
                class_name.as_mut_ptr(),
                class_name.len() as u32,
                ptr::null_mut(),
            )
        })?;
        // SAFETY: `class` was filled by SetupDiGetINFClassW.
        let set = DeviceSet::new(unsafe { SetupDiCreateDeviceInfoList(&class, ptr::null_mut()) })?;
        let mut device = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: the set is open and the class name is NUL-terminated.
        check(unsafe {
            SetupDiCreateDeviceInfoW(
                set.0,
                class_name.as_ptr(),
                &class,
                ptr::null(),
                ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut device,
            )
        })?;
        let ids: Vec<u16> = HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
        // SAFETY: a REG_MULTI_SZ with its byte length; `device` belongs to the set.
        check(unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut device,
                SPDRP_HARDWAREID,
                ids.as_ptr().cast(),
                size_of_val(ids.as_slice()) as u32,
            )
        })?;
        // SAFETY: `device` belongs to the open set.
        check(unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &device) })?;
    }
    let id = wide(HARDWARE_ID);
    let mut restart = 0;
    // SAFETY: both strings are NUL-terminated and `restart` outlives the call.
    let updated = check(unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            ptr::null_mut(),
            id.as_ptr(),
            inf.as_ptr(),
            INSTALLFLAG_NONINTERACTIVE,
            &mut restart,
        )
    });
    match updated {
        Err(e) if e.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) => Ok(false),
        result => result.map(|()| restart != 0),
    }
}

/// Removes every Litz Audio device, then every Litz Audio package in the
/// driver store, including those left by earlier versions. Requires an
/// elevated process. Returns whether Windows needs a restart to finish.
pub fn uninstall() -> io::Result<bool> {
    let set = all_devices()?;
    let mut restart = 0;
    for device in set.ours() {
        let mut device_restart = 0;
        // SAFETY: `device` was enumerated from the open set.
        check(unsafe {
            DiUninstallDevice(ptr::null_mut(), set.0, &device, 0, &mut device_restart)
        })?;
        restart |= device_restart;
    }
    for package in packages()? {
        let name: Vec<u16> = package.encode_wide().chain([0]).collect();
        // SAFETY: a NUL-terminated published INF name; no other arguments.
        check(unsafe { SetupUninstallOEMInfW(name.as_ptr(), 0, ptr::null()) })?;
    }
    Ok(restart != 0)
}

/// Published names (oemN.inf) of the packages this driver's INF created.
fn packages() -> io::Result<Vec<OsString>> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot is not set"))?;
    let directory = PathBuf::from(root).join("INF");
    let mut found = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let name = entry?.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        if lower.starts_with("oem") && lower.ends_with(".inf") && is_ours(&directory.join(&name)) {
            found.push(name);
        }
    }
    Ok(found)
}

fn is_ours(inf: &Path) -> bool {
    let path: Vec<u16> = inf.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: a NUL-terminated path; a failed open returns INVALID_HANDLE_VALUE.
    let handle =
        unsafe { SetupOpenInfFileW(path.as_ptr(), ptr::null(), INF_STYLE_WIN4, ptr::null_mut()) };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let field = |key: &str| {
        let key = wide(key);
        let section = wide("Version");
        let mut line = INFCONTEXT {
            Inf: ptr::null_mut(),
            CurrentInf: ptr::null_mut(),
            Section: 0,
            Line: 0,
        };
        let mut text = [0u16; 128];
        // SAFETY: the INF is open, both names are NUL-terminated and the
        // buffer length is passed in characters.
        let found = unsafe {
            SetupFindFirstLineW(handle, section.as_ptr(), key.as_ptr(), &mut line) != 0
                && SetupGetStringFieldW(
                    &line,
                    1,
                    text.as_mut_ptr(),
                    text.len() as u32,
                    ptr::null_mut(),
                ) != 0
        };
        found.then(|| {
            let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            String::from_utf16_lossy(&text[..end])
        })
    };
    let ours = field("Provider").as_deref() == Some("Litz Audio")
        && field("CatalogFile").is_some_and(|c| c.eq_ignore_ascii_case("litzaudio.cat"));
    // SAFETY: the handle came from SetupOpenInfFileW and is closed once.
    unsafe { SetupCloseInfFile(handle as *const c_void) };
    ours
}

const KSCATEGORY_CAPTURE: GUID = GUID::from_u128(0x65e8773d_8f56_11d0_a3b9_00a0c9223196);
const KSPROPSETID_LITZ_FEED: GUID = GUID::from_u128(0x89b4253b_9034_497d_9878_aed954e01fc6);
const IOCTL_KS_PROPERTY: u32 = 0x002f_0003;
const KSPROPERTY_TYPE_GET: u32 = 1;
const KSPROPERTY_TYPE_SET: u32 = 2;

#[repr(C)]
struct KsProperty {
    set: GUID,
    id: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct FeedState {
    running: u32,
    queued_frames: u32,
}

/// Litz Microphone's private input: 48 kHz mono 32-bit PCM that the driver
/// plays to recording apps on its own clock.
pub struct Microphone(OwnedHandle);
impl Microphone {
    /// Frames to keep queued: the driver reads 10 ms at a time, and the rest
    /// absorbs a late wakeup of the writing thread.
    pub const TARGET: usize = 1440;

    pub fn open() -> io::Result<Self> {
        // SAFETY: lists present capture interfaces; no other preconditions.
        let set = DeviceSet::new(unsafe {
            SetupDiGetClassDevsW(
                &KSCATEGORY_CAPTURE,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        })?;
        let path = set.interface(&KSCATEGORY_CAPTURE).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Litz Audio driver not installed")
        })?;
        // SAFETY: the path is NUL-terminated; the returned handle is checked.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateFileW returned a valid handle that nothing else owns.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    fn property(&self, flags: u32, value: *mut u8, size: usize) -> io::Result<()> {
        let property = KsProperty {
            set: KSPROPSETID_LITZ_FEED,
            id: 0,
            flags,
        };
        let mut returned = 0;
        // SAFETY: the handle is open; `property` and `value` outlive this
        // synchronous call and their sizes are passed.
        check(unsafe {
            DeviceIoControl(
                self.0.as_raw_handle(),
                IOCTL_KS_PROPERTY,
                (&raw const property).cast(),
                size_of::<KsProperty>() as u32,
                value.cast(),
                size as u32,
                &mut returned,
                ptr::null_mut(),
            )
        })
    }

    /// Whether an app records Litz Microphone, and how many frames the
    /// driver holds for it.
    pub fn state(&self) -> io::Result<(bool, usize)> {
        let mut state = FeedState::default();
        self.property(
            KSPROPERTY_TYPE_GET,
            (&raw mut state).cast(),
            size_of::<FeedState>(),
        )?;
        Ok((state.running != 0, state.queued_frames as usize))
    }

    pub fn write(&self, samples: &[i32]) -> io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.property(
            KSPROPERTY_TYPE_SET,
            samples.as_ptr().cast_mut().cast(),
            size_of_val(samples),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, IAudioCaptureClient,
    };

    /// Distinct nonzero samples with 24 significant bits, so 32-bit PCM and
    /// the audio engine's floats carry them exactly.
    fn signal(frame: usize) -> i32 {
        let x = ((frame as u32).wrapping_mul(2654435761) >> 8) as i32 - (1 << 23);
        (x | 1) << 7
    }

    fn feed_until(end: Instant) -> usize {
        crate::platform::pro_audio_thread();
        let microphone = Microphone::open().unwrap();
        let timer = crate::platform::Timer::new().unwrap();
        let mut written = 0;
        while Instant::now() < end {
            let (running, queued) = microphone.state().unwrap();
            if running {
                let frames = Microphone::TARGET.saturating_sub(queued);
                let block: Vec<i32> = (written..written + frames).map(signal).collect();
                microphone.write(&block).unwrap();
                written += frames;
            }
            timer.sleep(Duration::from_millis(5));
        }
        written
    }

    /// Returns what a shared-mode recording app gets, and how many packets
    /// WASAPI flagged as lost after the first.
    fn record_until(end: Instant) -> (Vec<f32>, usize) {
        crate::platform::pro_audio_thread();
        wasapi::initialize_mta().ok().unwrap();
        let devices = DeviceEnumerator::new()
            .unwrap()
            .get_device_collection(Direction::Capture)
            .unwrap();
        let device = (0..devices.get_nbr_devices().unwrap())
            .filter_map(|i| devices.get_device_at_index(i).ok())
            .find(|d| {
                d.get_interface_friendlyname()
                    .is_ok_and(|n| n == "Litz Audio")
            })
            .expect("Litz Microphone installed");
        let mut client = device.get_iaudioclient().unwrap();
        client
            .initialize_client(
                &WaveFormat::new(32, 32, SampleType::Float, 48000, 1, None),
                Direction::Capture,
                StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: 1_000_000,
                },
                None,
            )
            .unwrap();
        let event = client.set_get_eventhandle().unwrap();
        // SAFETY: the client was initialized for capture above.
        let capture: IAudioCaptureClient =
            unsafe { client.as_iaudioclient().GetService().unwrap() };
        client.start_stream().unwrap();
        let (mut received, mut lost) = (Vec::new(), 0);
        while Instant::now() < end {
            let _ = event.wait_for_event(100);
            // SAFETY: GetBuffer's bytes stay valid until ReleaseBuffer; a
            // silent packet may carry a null pointer and is read as zeros.
            unsafe {
                while capture.GetNextPacketSize().unwrap() > 0 {
                    let (mut data, mut frames, mut flags) = (ptr::null_mut(), 0u32, 0u32);
                    capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .unwrap();
                    let count = frames as usize;
                    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0
                        && !received.is_empty()
                    {
                        lost += 1;
                    }
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        received.extend(std::iter::repeat_n(0.0, count));
                    } else {
                        received.extend_from_slice(std::slice::from_raw_parts(data.cast(), count));
                    }
                    capture.ReleaseBuffer(frames).unwrap();
                }
            }
        }
        client.stop_stream().unwrap();
        (received, lost)
    }

    #[test]
    #[ignore = "needs the Litz Audio driver installed"]
    fn microphone_plays_written_pcm_unchanged() {
        let seconds: u64 = std::env::var("LITZ_DRIVER_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let end = Instant::now() + Duration::from_secs(seconds);
        let (written, (received, lost)) = std::thread::scope(|scope| {
            let recording = scope.spawn(|| record_until(end));
            (feed_until(end), recording.join().unwrap())
        });
        let expected = |frame: usize| signal(frame) as f32 / 2_147_483_648.0;
        let start = received
            .iter()
            .position(|&s| s != 0.0)
            .expect("Litz Microphone stayed silent");
        let first = (0..written)
            .find(|&f| expected(f) == received[start])
            .expect("first recorded sample was never written");
        let mut position = first;
        let mut jumps = Vec::new();
        for (i, &sample) in received[start..].iter().enumerate() {
            if sample != expected(position) {
                let found =
                    (position.saturating_sub(48000)..written).find(|&f| expected(f) == sample);
                jumps.push(format!(
                    "{:.3} s: {}",
                    i as f64 / 48000.0,
                    found.map_or(format!("unknown {sample}"), |f| format!(
                        "{:+} frames",
                        f as i64 - position as i64
                    ))
                ));
                position = found.unwrap_or(position);
            }
            position += 1;
        }
        let checked = received.len() - start;
        assert!(
            lost == 0 && jumps.is_empty(),
            "{lost} packets lost, {} discontinuities: {:?}",
            jumps.len(),
            &jumps[..jumps.len().min(20)]
        );
        assert!(
            checked as f64 > (seconds as f64 - 1.0) * 48000.0,
            "only {checked} frames recorded"
        );
        eprintln!(
            "{checked} frames ({:.1} s) matched",
            checked as f64 / 48000.0
        );
    }
}
