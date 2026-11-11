//! The Android audio device: the Oboe streams in `oboe_adapter.cpp`, and what
//! an Android process does for the thread that feeds them.
use crate::audio::AudioInfo;
use std::ffi::c_void;

/// Mirrors `LitzAudioInfo` in `oboe_adapter.h`.
#[repr(C)]
#[derive(Default)]
struct Info {
    input_rate: i32,
    output_rate: i32,
    input_device: i32,
    output_device: i32,
    input_exclusive: i32,
    output_exclusive: i32,
    input_low_latency: i32,
    output_low_latency: i32,
    input_buffer: i32,
    output_buffer: i32,
    input_xruns: i32,
    output_xruns: i32,
    error: i32,
    source: i32,
}

unsafe extern "C" {
    fn litz_audio_open(
        bridge: *mut c_void,
        mic: bool,
        playback: bool,
        input: i32,
        output: i32,
        source: i32,
    ) -> *mut c_void;
    fn litz_audio_start(audio: *mut c_void) -> i32;
    fn litz_audio_info(audio: *mut c_void, info: *mut Info);
    fn litz_audio_close(audio: *mut c_void);
}

/// Open Oboe streams that call `litz_capture` and `litz_render`.
pub struct Device(*mut c_void);

impl Device {
    /// # Safety
    /// `bridge` is what the callbacks receive: it must stay at this address
    /// and live until the device is dropped.
    pub unsafe fn open(
        bridge: *mut c_void,
        mic: bool,
        playback: bool,
        input: i32,
        output: i32,
        source: i32,
    ) -> Option<Self> {
        // SAFETY: the caller keeps `bridge` live for the device's lifetime.
        let native = unsafe { litz_audio_open(bridge, mic, playback, input, output, source) };
        (!native.is_null()).then_some(Self(native))
    }

    pub fn start(&self) -> bool {
        // SAFETY: the handle came from litz_audio_open and is closed only on drop.
        unsafe { litz_audio_start(self.0) == 0 }
    }

    pub fn info(&self) -> AudioInfo {
        let mut info = Info::default();
        // SAFETY: the handle is live; C++ writes only the provided Info.
        unsafe { litz_audio_info(self.0, &mut info) };
        AudioInfo {
            input_rate: info.input_rate,
            output_rate: info.output_rate,
            input_device: info.input_device,
            output_device: info.output_device,
            input_exclusive: info.input_exclusive,
            output_exclusive: info.output_exclusive,
            input_low_latency: info.input_low_latency,
            output_low_latency: info.output_low_latency,
            input_buffer: info.input_buffer,
            output_buffer: info.output_buffer,
            input_xruns: info.input_xruns,
            output_xruns: info.output_xruns,
            error: info.error,
            source: info.source,
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the handle is closed once; close joins the callbacks.
        unsafe { litz_audio_close(self.0) };
    }
}

/// Android shows a native panic only if it reaches logcat.
pub fn log_panics() {
    static PANIC_LOG: std::sync::Once = std::sync::Once::new();
    PANIC_LOG.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            unsafe extern "C" {
                fn __android_log_write(
                    priority: libc::c_int,
                    tag: *const libc::c_char,
                    text: *const libc::c_char,
                ) -> libc::c_int;
            }
            if let Ok(message) = std::ffi::CString::new(info.to_string()) {
                // SAFETY: both C strings stay live for the synchronous log call.
                unsafe { __android_log_write(6, c"LitzNative".as_ptr(), message.as_ptr()) };
            }
            previous(info);
        }));
    });
}

pub fn raise_thread_priority() {
    // SAFETY: adjusts only the calling thread; failure keeps the default.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS as _, libc::gettid() as _, -16);
    }
}
