use litz_core::media::receiver::{CHUNK, Receiver};
use litz_core::protocol::MAX_DATAGRAM;
use litz_core::wake::{Wake, WakeListener};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::Serialize;
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::Instant,
};

const BLOCK: usize = 2048;
const PACKETS: usize = 32;

#[derive(Clone, Copy)]
pub struct Captured {
    pub samples: [f32; BLOCK],
    pub len: usize,
    pub at: Instant,
}
struct Packet {
    len: usize,
    arrival: f64,
    data: [u8; MAX_DATAGRAM],
}

pub struct GainLimiter {
    pub gain: f32,
    limiter: f32,
}
impl GainLimiter {
    pub fn new(gain: f32) -> Self {
        Self { gain, limiter: 1.0 }
    }
    pub fn process<const CHANNELS: usize>(&mut self, samples: &mut [f32], target: f32) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            self.gain += (target - self.gain).clamp(-1.0 / 480.0, 1.0 / 480.0);
            let mut peak = 0.0f32;
            for sample in frame.iter_mut() {
                *sample = if sample.is_finite() {
                    *sample * self.gain
                } else {
                    0.0
                };
                peak = peak.max(sample.abs());
            }
            let required = (0.95 / peak.max(0.95)).min(1.0);
            self.limiter = required.min(self.limiter + (1.0 - self.limiter) * 0.0002);
            for sample in frame {
                *sample *= self.limiter;
            }
        }
    }
}

/// Owned by the render callback. The engine hands receivers and packets over
/// through rings and gets replaced receivers back, so nothing is allocated or
/// freed on the realtime thread.
struct Render {
    packets: Consumer<Packet>,
    installs: Consumer<Option<Box<Receiver>>>,
    retired: Producer<Box<Receiver>>,
    receiver: Option<Box<Receiver>>,
    pcm: [f32; CHUNK * 2],
    offset: usize,
    level: GainLimiter,
}

struct Bridge {
    capture: UnsafeCell<Producer<Captured>>,
    render: UnsafeCell<Render>,
    wake: Wake,
    origin: Instant,
    active: AtomicBool,
    overflow: AtomicBool,
    gain: AtomicU32,
    callback_frames: AtomicU64,
    late: AtomicU64,
    concealed: AtomicU64,
    buffer_us: AtomicU64,
}

struct Ends {
    capture: Consumer<Captured>,
    packets: Producer<Packet>,
    installs: Producer<Option<Box<Receiver>>>,
    retired: Consumer<Box<Receiver>>,
}

impl Bridge {
    fn try_new() -> std::io::Result<(Box<Self>, Ends)> {
        let (capture_tx, capture) = RingBuffer::new(32);
        let (packets, packets_rx) = RingBuffer::new(PACKETS);
        let (installs, installs_rx) = RingBuffer::new(4);
        let (retired_tx, retired) = RingBuffer::new(4);
        let bridge = Box::new(Self {
            capture: UnsafeCell::new(capture_tx),
            render: UnsafeCell::new(Render {
                packets: packets_rx,
                installs: installs_rx,
                retired: retired_tx,
                receiver: None,
                pcm: [0.0; CHUNK * 2],
                offset: CHUNK,
                level: GainLimiter::new(1.0),
            }),
            wake: Wake::try_new()?,
            origin: Instant::now(),
            active: AtomicBool::new(false),
            overflow: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            callback_frames: AtomicU64::new(0),
            late: AtomicU64::new(0),
            concealed: AtomicU64::new(0),
            buffer_us: AtomicU64::new(0),
        });
        let ends = Ends {
            capture,
            packets,
            installs,
            retired,
        };
        Ok((bridge, ends))
    }
}

#[repr(C)]
#[derive(Default, Serialize)]
pub struct Info {
    pub input_rate: i32,
    pub output_rate: i32,
    pub input_device: i32,
    pub output_device: i32,
    pub input_exclusive: i32,
    pub output_exclusive: i32,
    pub input_low_latency: i32,
    pub output_low_latency: i32,
    pub input_buffer: i32,
    pub output_buffer: i32,
    pub input_xruns: i32,
    pub output_xruns: i32,
    pub error: i32,
    pub source: i32,
}
#[cfg(target_os = "android")]
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

#[derive(Default)]
pub struct Listening {
    pub buffer_ms: f64,
    pub late: u64,
    pub concealed: u64,
}

pub struct Audio {
    native: *mut c_void,
    bridge: Box<Bridge>,
    ends: Ends,
    pub wake: WakeListener,
}
impl Audio {
    pub fn open(
        mic: bool,
        playback: bool,
        input: i32,
        output: i32,
        source: i32,
    ) -> litz_core::transport::Result<Self> {
        let (mut bridge, ends) = Bridge::try_new()?;
        let wake = WakeListener::try_new(bridge.wake.clone())?;
        #[cfg(target_os = "android")]
        // SAFETY: bridge has a stable heap address and remains live until C++ closes its callbacks.
        let native = unsafe {
            litz_audio_open(
                (&mut *bridge as *mut Bridge).cast(),
                mic,
                playback,
                input,
                output,
                source,
            )
        };
        #[cfg(not(target_os = "android"))]
        let native = {
            let _ = (&mut bridge, mic, playback, input, output, source);
            std::ptr::null_mut::<c_void>()
        };
        if native.is_null() {
            return Err("Oboe open failed: unsupported format/rate or unavailable device; no route fallback".into());
        }
        Ok(Self {
            native,
            bridge,
            ends,
            wake,
        })
    }
    pub fn start(&mut self) -> litz_core::transport::Result<()> {
        self.bridge.active.store(true, Ordering::Release);
        #[cfg(target_os = "android")]
        // SAFETY: native is a live handle returned by litz_audio_open and owned by Self.
        if unsafe { litz_audio_start(self.native) } != 0 {
            return Err("Oboe start failed".into());
        }
        Ok(())
    }
    pub fn silence(&self) {
        self.bridge.active.store(false, Ordering::Release);
    }
    pub fn fault(&self) -> bool {
        self.bridge.overflow.load(Ordering::Acquire) || self.info().error != 0
    }
    pub fn set_playback_gain(&self, gain: f32) {
        self.bridge.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
    pub fn capture(&mut self) -> Option<Captured> {
        self.ends.capture.pop().ok()
    }
    pub fn listen(&mut self, receiver: Option<Box<Receiver>>) -> litz_core::transport::Result<()> {
        self.collect();
        self.ends
            .installs
            .push(receiver)
            .map_err(|_| "playback handoff full")?;
        Ok(())
    }
    pub fn push_packet(&mut self, bytes: &[u8]) {
        if bytes.len() > MAX_DATAGRAM {
            return;
        }
        let mut packet = Packet {
            len: bytes.len(),
            arrival: self.bridge.origin.elapsed().as_secs_f64(),
            data: [0; MAX_DATAGRAM],
        };
        packet.data[..bytes.len()].copy_from_slice(bytes);
        let _ = self.ends.packets.push(packet);
    }
    pub fn collect(&mut self) {
        while self.ends.retired.pop().is_ok() {}
    }
    pub fn listening(&self) -> Listening {
        Listening {
            buffer_ms: self.bridge.buffer_us.load(Ordering::Relaxed) as f64 / 1000.0,
            late: self.bridge.late.load(Ordering::Relaxed),
            concealed: self.bridge.concealed.load(Ordering::Relaxed),
        }
    }
    pub fn callback_frames(&self) -> u64 {
        self.bridge.callback_frames.load(Ordering::Relaxed)
    }
    pub fn info(&self) -> Info {
        let mut info = Info::default();
        #[cfg(target_os = "android")]
        // SAFETY: native remains live for Self; C++ writes only the provided Info.
        unsafe {
            litz_audio_info(self.native, &mut info);
        }
        #[cfg(not(target_os = "android"))]
        let _ = (&mut info, self.native);
        info
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        self.silence();
        #[cfg(target_os = "android")]
        // SAFETY: Self owns native; close joins callbacks before bridge and rings drop.
        unsafe {
            litz_audio_close(self.native);
        }
    }
}

// Native C ABI callbacks on the realtime thread: no allocation, lock or log.
#[unsafe(no_mangle)]
unsafe extern "C" fn litz_capture(pointer: *mut c_void, samples: *const f32, frames: i32) {
    // SAFETY: C++ owns a live Bridge until close has joined all callbacks.
    let b = unsafe { &*pointer.cast::<Bridge>() };
    if !b.active.load(Ordering::Acquire) || b.overflow.load(Ordering::Acquire) || frames <= 0 {
        return;
    }
    // SAFETY: C++ supplies frames readable mono samples for the duration of this callback.
    let input = unsafe { std::slice::from_raw_parts(samples, frames as usize) };
    // SAFETY: capture callbacks are serialized and are the only users of this field.
    let capture = unsafe { &mut *b.capture.get() };
    for chunk in input.chunks(BLOCK) {
        let mut block = Captured {
            samples: [0.0; BLOCK],
            len: chunk.len(),
            at: Instant::now(),
        };
        block.samples[..chunk.len()].copy_from_slice(chunk);
        if capture.push(block).is_err() {
            b.overflow.store(true, Ordering::Release);
            break;
        }
    }
    b.wake.signal();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn litz_render(pointer: *mut c_void, samples: *mut f32, frames: i32) {
    // SAFETY: C++ owns a live Bridge until close has joined all callbacks.
    let b = unsafe { &*pointer.cast::<Bridge>() };
    if frames <= 0 {
        return;
    }
    // SAFETY: C++ supplies space for frames writable stereo samples.
    let out = unsafe { std::slice::from_raw_parts_mut(samples, frames as usize * 2) };
    if !b.active.load(Ordering::Acquire) || b.overflow.load(Ordering::Acquire) {
        out.fill(0.0);
        return;
    }
    // SAFETY: render callbacks are serialized and are the only users of this field.
    let r = unsafe { &mut *b.render.get() };
    let mut released = false;
    while let Ok(next) = r.installs.pop() {
        if let Some(old) = std::mem::replace(&mut r.receiver, next) {
            let _ = r.retired.push(old);
            released = true;
        }
        r.offset = CHUNK;
    }
    let now = b.origin.elapsed().as_secs_f64();
    let gain = f32::from_bits(b.gain.load(Ordering::Relaxed));
    match &mut r.receiver {
        Some(receiver) => {
            receiver.set_render_quantum(frames as usize);
            while let Ok(packet) = r.packets.pop() {
                let _ = receiver.push(&packet.data[..packet.len], packet.arrival);
            }
        }
        None => while r.packets.pop().is_ok() {},
    }
    for frame in out.as_chunks_mut::<2>().0 {
        if r.offset == CHUNK {
            let rendered = r
                .receiver
                .as_deref_mut()
                .is_some_and(|receiver| receiver.render(now, &mut r.pcm).is_ok());
            if rendered {
                r.level.process::<2>(&mut r.pcm, gain);
            } else {
                r.pcm.fill(0.0);
            }
            r.offset = 0;
        }
        frame.copy_from_slice(&r.pcm[r.offset * 2..r.offset * 2 + 2]);
        r.offset += 1;
    }
    if let Some(receiver) = &r.receiver {
        b.late.store(receiver.late_packets(), Ordering::Relaxed);
        b.concealed
            .store(receiver.concealed_frames(), Ordering::Relaxed);
        b.buffer_us
            .store((receiver.buffer_ms() * 1000.0) as u64, Ordering::Relaxed);
    }
    b.callback_frames.store(frames as u64, Ordering::Relaxed);
    if released {
        b.wake.signal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::media::codec::Encoder;
    use litz_core::protocol::{Config, FRAME, Header, Stream};

    fn render(bridge: &mut Bridge, out: &mut [f32]) {
        // SAFETY: the live bridge has exclusive callback access; out holds stereo frames.
        unsafe {
            litz_render(
                (bridge as *mut Bridge).cast(),
                out.as_mut_ptr(),
                out.len() as i32 / 2,
            )
        }
    }

    #[test]
    fn callback_decodes_handed_over_packets_and_returns_old_receivers() {
        let (mut bridge, mut ends) = Bridge::try_new().unwrap();
        bridge.active.store(true, Ordering::Release);
        let config = Config {
            stream: Stream::Playback,
            epoch: 1,
        };
        let mut encoder = Encoder::try_new(config).unwrap();
        ends.installs
            .push(Some(Box::new(Receiver::try_new(config).unwrap())))
            .unwrap();
        let mut out = [0.0; 288 * 2];
        let mut packet = [0; MAX_DATAGRAM];
        let mut heard = false;
        for sequence in 0..100u32 {
            let size = encoder
                .encode(&[0.3; FRAME * 2], &mut packet, MAX_DATAGRAM)
                .unwrap();
            Header {
                config,
                sequence,
                timestamp: sequence as u64 * FRAME as u64,
            }
            .write(&mut packet)
            .unwrap();
            let mut item = Packet {
                len: size,
                arrival: bridge.origin.elapsed().as_secs_f64(),
                data: [0; MAX_DATAGRAM],
            };
            item.data[..size].copy_from_slice(&packet[..size]);
            ends.packets.push(item).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
            render(&mut bridge, &mut out[..192 * 2]);
            render(&mut bridge, &mut out[..288 * 2]);
            heard |= out.iter().any(|s| s.abs() > 0.05);
        }
        assert!(heard);
        ends.installs.push(None).unwrap();
        render(&mut bridge, &mut out);
        assert!(ends.retired.pop().is_ok());
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn boost_limits_peaks_without_flat_clipping() {
        let mut level = GainLimiter::new(1.0);
        let mut frames = [0.0; FRAME * 2];
        for _ in 0..4 {
            for frame in frames.as_chunks_mut::<2>().0 {
                *frame = [0.8, -0.4];
            }
            level.process::<2>(&mut frames, 2.0);
        }
        assert!(frames.iter().all(|sample| sample.abs() <= 0.951));
        assert!(frames[0] > 0.9 && frames[1] < -0.4);
        let mut invalid = [f32::NAN; 4];
        level.process::<2>(&mut invalid, 2.0);
        assert_eq!(invalid, [0.0; 4]);
    }
}
