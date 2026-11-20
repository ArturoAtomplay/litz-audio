use crate::log::debug;
use litz_core::pcm::PcmConsumer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// State the virtual microphone's render callback shares with its stream.
#[derive(Default)]
pub struct Control {
    pub format_ready: AtomicBool,
    pub reset: Arc<AtomicBool>,
    pub callbacks: AtomicU64,
    pub frames: AtomicU64,
    pub silence: AtomicU64,
}

impl Control {
    pub fn report(&self) {
        debug!(
            "callbacks={} frames={} silent_frames={}",
            self.callbacks.load(Ordering::Relaxed),
            self.frames.load(Ordering::Relaxed),
            self.silence.load(Ordering::Relaxed)
        );
    }
}

/// Fills `bytes` with up to `requested` mono f32 frames and returns the bytes written.
pub fn render(
    consumer: &mut PcmConsumer,
    control: &Control,
    bytes: &mut [u8],
    requested: u64,
) -> usize {
    if control.reset.swap(false, Ordering::AcqRel) {
        consumer.clear();
    }
    let capacity = bytes.len() / size_of::<f32>();
    let frames = if requested == 0 {
        capacity
    } else {
        capacity.min(requested as usize)
    };
    let size = frames * size_of::<f32>();
    let read = if control.format_ready.load(Ordering::Acquire) {
        consumer.render(&mut bytes[..size])
    } else {
        bytes[..size].fill(0);
        0
    };
    control.callbacks.fetch_add(1, Ordering::Relaxed);
    control.frames.fetch_add(frames as u64, Ordering::Relaxed);
    control
        .silence
        .fetch_add((frames - read) as u64, Ordering::Relaxed);
    size
}

#[derive(Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn next(&mut self) -> Duration {
        let base_ms = 250_u64 << self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % (base_ms / 4 + 1);
        Duration::from_millis(base_ms - base_ms / 4 + jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::pcm;

    #[test]
    fn requested_frames_are_bounded_and_format_reset_is_silent() {
        let (mut tx, mut rx) = pcm::channel();
        let control = Control::default();
        control.format_ready.store(true, Ordering::Relaxed);
        tx.push(&[0.25; 10]);
        let mut bytes = [255; 67];
        assert_eq!(render(&mut rx, &control, &mut bytes, 3), 12);
        assert_eq!(&bytes[..4], &0.25_f32.to_ne_bytes());
        assert_eq!(render(&mut rx, &control, &mut bytes, u64::MAX), 64);
        assert_eq!(&bytes[28..64], &[0; 36]);
        tx.push(&[1.0; 10]);
        control.reset.store(true, Ordering::Relaxed);
        assert_eq!(render(&mut rx, &control, &mut bytes, 0), 64);
        assert_eq!(&bytes[..64], &[0; 64]);
    }

    #[test]
    fn reconnect_backoff_is_bounded() {
        let mut backoff = Backoff::default();
        for _ in 0..100 {
            let delay = backoff.next();
            assert!(delay >= Duration::from_millis(187));
            assert!(delay <= Duration::from_secs(8));
        }
    }
}
