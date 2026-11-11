//! Where the phone has no audio backend, such as the machine the tests run
//! on. A backend for an operating system is a sibling module with these same
//! items, selected in `lib.rs`.
use crate::audio::AudioInfo;
use std::ffi::c_void;

pub enum Device {}

impl Device {
    /// # Safety
    /// None here. A backend keeps `bridge` for its callbacks, so the caller
    /// keeps it at this address until the device is dropped.
    pub unsafe fn open(
        _bridge: *mut c_void,
        _mic: bool,
        _playback: bool,
        _input: i32,
        _output: i32,
        _source: i32,
    ) -> Option<Self> {
        None
    }

    pub fn start(&self) -> bool {
        match *self {}
    }

    pub fn info(&self) -> AudioInfo {
        match *self {}
    }
}

pub fn log_panics() {}

pub fn raise_thread_priority() {}
