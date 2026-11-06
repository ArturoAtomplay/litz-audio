use rtrb::{Consumer, Producer, RingBuffer};

pub const RATE: u32 = 48_000;
pub const CAPACITY: usize = 4096;

pub struct PcmProducer(Producer<f32>);
pub struct PcmConsumer(Consumer<f32>);

pub fn channel() -> (PcmProducer, PcmConsumer) {
    let (tx, rx) = RingBuffer::new(CAPACITY);
    (PcmProducer(tx), PcmConsumer(rx))
}

impl PcmProducer {
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let mut written = 0;
        for &sample in samples {
            if self.0.push(sample).is_err() {
                break;
            }
            written += 1;
        }
        written
    }

    pub fn free_samples(&self) -> usize {
        self.0.slots()
    }
}

impl PcmConsumer {
    /// Write native-endian f32 samples into a byte buffer of any length.
    /// Missing samples and trailing bytes become silence. Returns how many
    /// ring samples were written.
    pub fn render(&mut self, bytes: &mut [u8]) -> usize {
        if self.0.is_abandoned() {
            self.clear();
            bytes.fill(0);
            return 0;
        }
        let mut read = 0;
        let (chunks, remainder) = bytes.as_chunks_mut::<4>();
        for chunk in chunks {
            let sample = match self.0.pop() {
                Ok(sample) => {
                    read += 1;
                    sample
                }
                Err(_) => 0.0,
            };
            chunk.copy_from_slice(&sample.to_ne_bytes());
        }
        remainder.fill(0);
        read
    }

    /// Discard currently queued samples. Call from the audio callback after a reset.
    pub fn clear(&mut self) {
        for _ in 0..self.0.slots() {
            let _ = self.0.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_ne_bytes(*b))
            .collect()
    }

    #[test]
    fn empty_and_disconnected_are_silent() {
        let (mut tx, mut rx) = channel();
        tx.push(&[1.0; CAPACITY]);
        drop(tx);
        let mut bytes = [255; 1031];
        assert_eq!(rx.render(&mut bytes), 0);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn bounded_overflow_wraparound_and_arbitrary_callback_sizes() {
        let (mut tx, mut rx) = channel();
        assert_eq!(tx.push(&[0.25; CAPACITY + 1]), CAPACITY);
        assert_eq!(tx.free_samples(), 0);
        let mut first = [0; 13 * 4];
        assert_eq!(rx.render(&mut first), 13);
        assert_eq!(tx.push(&[0.5; 13]), 13);
        let mut rest = vec![255; (CAPACITY + 17) * 4];
        assert_eq!(rx.render(&mut rest), CAPACITY);
        let values = decode(&rest);
        assert!(values[..CAPACITY - 13].iter().all(|&s| s == 0.25));
        assert!(values[CAPACITY - 13..CAPACITY].iter().all(|&s| s == 0.5));
        assert!(values[CAPACITY..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn reset_drops_stale_audio() {
        let (mut tx, mut rx) = channel();
        tx.push(&[1.0; 100]);
        rx.clear();
        tx.push(&[0.5]);
        let mut bytes = [255; 12];
        assert_eq!(rx.render(&mut bytes), 1);
        assert_eq!(decode(&bytes), [0.5, 0.0, 0.0]);
    }
}
