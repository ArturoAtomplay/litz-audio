use super::codec::Decoder;
use crate::protocol::{Config, FRAME, HEADER, Header, MAX_DATAGRAM};
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction};

/// Frames produced per `render` call.
pub const CHUNK: usize = FRAME / 4;
const SLOTS: usize = 16;
const RATE: f64 = crate::pcm::RATE as f64;
const FRAME_SECONDS: f64 = FRAME as f64 / RATE;
const INITIAL_DELAY: f64 = 0.010;
const MARGIN: f64 = 0.003;
const WINDOW: f64 = 2.0;
const STRETCH: f64 = 0.005;
const SKIP_SURPLUS: f64 = 0.040;
const STALL: f64 = 0.080;

struct Slot {
    sequence: u32,
    len: usize,
    /// Arrival time, or the time it was needed if it was concealed as lost.
    arrival: f64,
    missed: bool,
    data: [u8; MAX_DATAGRAM - HEADER],
}

/// Adaptive jitter buffer with clock-drift correction.
///
/// Slack is the time from a packet's arrival until its audio is needed; late
/// packets report negative slack. Every window, the minimum slack over the
/// last two windows, less the consumer quantum and MARGIN, is delay that can
/// be removed (or must be added) by resampling 0.5% faster or slower. A packet
/// missing while nothing newer has arrived is held for one frame, not dropped.
pub struct Receiver {
    config: Config,
    slots: [Slot; SLOTS],
    decoder: Decoder,
    asrc: Async<f32>,
    pcm: [f32; FRAME * 6],
    pcm_frames: usize,
    next: Option<(u32, u64)>,
    first_deadline: f64,
    render_quantum: usize,
    started: bool,
    holding: bool,
    hold_spent: bool,
    clock_anchor: Option<(u64, f64)>,
    rate: f64,
    ratio: f64,
    latest_timestamp: u64,
    last_arrival: f64,
    window_start: f64,
    window_min: f64,
    previous_min: f64,
    /// Seconds of delay still to remove (positive) or add (negative).
    adjust: f64,
    delay: f64,
    late: u64,
    concealed: u64,
    resets: u64,
    generation: u64,
}

impl Receiver {
    pub fn try_new(config: Config) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if !config.valid() {
            return Err("invalid config".into());
        }
        let params = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2);
        Ok(Self {
            config,
            slots: std::array::from_fn(|_| Slot {
                sequence: 0,
                len: 0,
                arrival: 0.0,
                missed: false,
                data: [0; MAX_DATAGRAM - HEADER],
            }),
            decoder: Decoder::try_new(config)?,
            asrc: Async::new_sinc(
                1.0,
                1.01,
                &params,
                CHUNK,
                config.channels(),
                FixedAsync::Output,
            )?,
            pcm: [0.0; FRAME * 6],
            pcm_frames: 0,
            next: None,
            first_deadline: 0.0,
            render_quantum: CHUNK,
            started: false,
            holding: false,
            hold_spent: false,
            clock_anchor: None,
            rate: 1.0,
            ratio: 1.0,
            latest_timestamp: 0,
            last_arrival: 0.0,
            window_start: f64::NAN,
            window_min: f64::INFINITY,
            previous_min: f64::INFINITY,
            adjust: 0.0,
            delay: INITIAL_DELAY,
            late: 0,
            concealed: 0,
            resets: 0,
            generation: 0,
        })
    }
    pub fn buffer_ms(&self) -> f64 {
        self.delay * 1000.0
    }
    pub fn late_packets(&self) -> u64 {
        self.late
    }
    pub fn concealed_frames(&self) -> u64 {
        self.concealed
    }
    pub fn drift_ratio(&self) -> f64 {
        1.0 / self.rate
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn set_render_quantum(&mut self, frames: usize) {
        let frames = frames.clamp(CHUNK, crate::pcm::CAPACITY);
        if frames > self.render_quantum {
            if !self.started {
                self.first_deadline += (frames - self.render_quantum) as f64 / RATE;
            }
            self.render_quantum = frames;
        }
    }

    pub fn push(&mut self, packet: &[u8], arrival: f64) -> Result<(), &'static str> {
        if !arrival.is_finite() {
            return Err("arrival time");
        }
        let (header, payload) = Header::parse(packet, self.config, MAX_DATAGRAM)?;
        let (sequence, timestamp) = match self.next {
            Some(next) => next,
            None => {
                self.latest_timestamp = header.timestamp;
                self.first_deadline = arrival + INITIAL_DELAY + self.render_quantum as f64 / RATE;
                *self.next.insert((header.sequence, header.timestamp))
            }
        };
        let distance = header.sequence.wrapping_sub(sequence) as i32;
        let slot = &mut self.slots[header.sequence as usize % SLOTS];
        if distance < 0 {
            if distance >= -(SLOTS as i32) && slot.missed && slot.sequence == header.sequence {
                slot.missed = false;
                self.last_arrival = arrival;
                self.late += 1;
                self.window_min = self.window_min.min(slot.arrival - arrival);
            }
            return Ok(());
        }
        if header.timestamp != timestamp.wrapping_add(distance as u64 * FRAME as u64) {
            return Err("timestamp discontinuity");
        }
        if distance >= SLOTS as i32 {
            self.clear();
            self.resets += 1;
            return self.push(packet, arrival);
        }
        self.last_arrival = arrival;
        if slot.len == 0 {
            slot.sequence = header.sequence;
            slot.len = payload.len();
            slot.arrival = arrival;
            slot.missed = false;
            slot.data[..payload.len()].copy_from_slice(payload);
        }
        if header.timestamp.wrapping_sub(self.latest_timestamp) as i64 > 0 {
            self.latest_timestamp = header.timestamp;
        }
        match self.clock_anchor {
            Some((ts, time)) if arrival - time >= 10.0 && header.timestamp > ts => {
                let measured = (header.timestamp - ts) as f64 / (RATE * (arrival - time));
                if (0.998..=1.002).contains(&measured) {
                    self.rate += 0.25 * (measured - self.rate);
                }
                self.clock_anchor = Some((header.timestamp, arrival));
            }
            Some(_) => {}
            None => self.clock_anchor = Some((header.timestamp, arrival)),
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.len = 0;
            slot.missed = false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.next = None;
        self.started = false;
        self.holding = false;
        self.hold_spent = false;
        self.latest_timestamp = 0;
        self.clock_anchor = None;
        self.pcm_frames = 0;
        self.rate = 1.0;
        self.ratio = 1.0;
        self.window_start = f64::NAN;
        self.window_min = f64::INFINITY;
        self.previous_min = f64::INFINITY;
        self.adjust = 0.0;
        self.asrc.reset();
        self.decoder.reset();
    }

    fn restart_window(&mut self, now: f64) {
        self.window_start = now;
        self.window_min = f64::INFINITY;
        self.previous_min = f64::INFINITY;
    }

    fn evaluate(&mut self, now: f64) {
        if self.window_start.is_nan() || self.adjust != 0.0 {
            self.restart_window(now);
            return;
        }
        if now - self.window_start < WINDOW {
            return;
        }
        let complete = self.previous_min.is_finite();
        let slack = self.window_min.min(self.previous_min);
        self.previous_min = self.window_min;
        self.window_min = f64::INFINITY;
        self.window_start = now;
        if !complete {
            return;
        }
        // The consumer pulls a whole quantum at once, so decoding can happen
        // up to one quantum before the audio is needed.
        let floor = self.render_quantum as f64 / RATE + MARGIN;
        let mut surplus = slack - floor;
        if surplus >= SKIP_SURPLUS {
            while surplus > FRAME_SECONDS + floor {
                let Some((seq, ts)) = self.next else { break };
                let slot = &mut self.slots[seq as usize % SLOTS];
                if slot.len == 0 || slot.sequence != seq {
                    break;
                }
                slot.len = 0;
                self.next = Some((seq.wrapping_add(1), ts.wrapping_add(FRAME as u64)));
                surplus -= FRAME_SECONDS;
            }
        }
        if surplus.abs() >= 0.001 {
            self.adjust = surplus.max(-FRAME_SECONDS);
            self.restart_window(now);
        }
    }

    fn decode_next(&mut self, now: f64, needed: usize) -> Result<(), &'static str> {
        let channels = self.config.channels();
        let (seq, ts) = self.next.ok_or("receiver state")?;
        let advance = Some((seq.wrapping_add(1), ts.wrapping_add(FRAME as u64)));
        // When the decoded audio starts being read, excluding ASRC lookahead.
        let needed_at = now + (self.pcm_frames + CHUNK).saturating_sub(needed) as f64 / RATE;
        let newer = self.latest_timestamp.wrapping_sub(ts) as i64 > 0;
        let slot = &mut self.slots[seq as usize % SLOTS];
        let dest = &mut self.pcm[self.pcm_frames * channels..(self.pcm_frames + FRAME) * channels];
        if slot.len != 0 && slot.sequence == seq {
            if self.decoder.decode(&slot.data[..slot.len], dest).is_err() {
                self.concealed += 1;
                self.decoder.decode(&[], dest)?;
            }
            let slack = needed_at - slot.arrival;
            slot.len = 0;
            self.window_min = self.window_min.min(slack);
            self.delay += 0.05 * (slack - self.delay);
            self.holding = false;
            self.hold_spent = false;
            self.next = advance;
        } else if self.holding || self.hold_spent || newer {
            slot.sequence = seq;
            slot.arrival = needed_at;
            slot.missed = true;
            self.next = advance;
            if std::mem::take(&mut self.holding) && newer {
                // Lost, not late: the held frame already concealed it.
                return Ok(());
            }
            self.concealed += 1;
            self.decoder.decode(&[], dest)?;
        } else {
            self.concealed += 1;
            self.decoder.decode(&[], dest)?;
            self.holding = true;
            self.hold_spent = true;
            self.adjust = self.adjust.min(0.0);
            self.restart_window(now);
        }
        self.pcm_frames += FRAME;
        Ok(())
    }

    pub fn render(&mut self, now: f64, out: &mut [f32]) -> Result<(), &'static str> {
        let channels = self.config.channels();
        if out.len() != CHUNK * channels {
            return Err("destination chunk");
        }
        if self.next.is_none() || now < self.first_deadline {
            out.fill(0.0);
            return Ok(());
        }
        if now - self.last_arrival > STALL {
            self.clear();
            self.last_arrival = now;
            out.fill(0.0);
            return Ok(());
        }
        self.started = true;
        self.evaluate(now);
        let speed = if self.adjust > 0.0 {
            1.0 - STRETCH
        } else if self.adjust < 0.0 {
            1.0 + STRETCH
        } else {
            1.0
        };
        // Glide to the new speed over ~100 ms instead of stepping the pitch.
        self.ratio += (speed / self.rate - self.ratio).clamp(-0.000125, 0.000125);
        self.asrc
            .as_adjustable()
            .ok_or("ASRC not adjustable")?
            .set_resample_ratio(self.ratio, true)
            .map_err(|_| "ASRC ratio")?;
        let needed = self.asrc.input_frames_next();
        while self.pcm_frames < needed {
            if (self.pcm_frames + FRAME) * channels > self.pcm.len() {
                return Err("ASRC capacity");
            }
            self.decode_next(now, needed)?;
        }
        let input = InterleavedSlice::new(&self.pcm[..needed * channels], channels, needed)
            .map_err(|_| "ASRC input")?;
        let mut output =
            InterleavedSlice::new_mut(out, channels, CHUNK).map_err(|_| "ASRC output")?;
        let (used, _) = self
            .asrc
            .process_into_buffer(&input, &mut output, None)
            .map_err(|_| "ASRC process")?;
        self.pcm
            .copy_within(used * channels..self.pcm_frames * channels, 0);
        self.pcm_frames -= used;
        let removed = (used as f64 - CHUNK as f64 * self.rate) / RATE;
        if self.adjust > 0.0 {
            self.adjust = (self.adjust - removed).max(0.0);
        } else if self.adjust < 0.0 {
            self.adjust = (self.adjust - removed).min(0.0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::Encoder;
    use crate::protocol::Stream;

    /// Streams stereo packets into device callbacks of `quantum` frames.
    /// `delay(seq)` is extra network delay; `None` drops the packet.
    fn run(seconds: f64, quantum: usize, ppm: f64, delay: impl Fn(u32) -> Option<f64>) -> Receiver {
        let config = Config {
            stream: Stream::Playback,
            epoch: 1,
        };
        let mut encoder = Encoder::try_new(config).unwrap();
        let mut receiver = Receiver::try_new(config).unwrap();
        receiver.set_render_quantum(quantum);
        let period = FRAME_SECONDS / (1.0 + ppm / 1e6);
        let mut arrivals: Vec<(f64, u32)> = (0..(seconds / period) as u32)
            .filter_map(|seq| Some((seq as f64 * period + 0.002 + delay(seq)?, seq)))
            .collect();
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut packet = [0; MAX_DATAGRAM];
        let mut out = [0.0; CHUNK * 2];
        let (mut next, mut queued) = (0, 0);
        let callback = quantum as f64 / RATE;
        for step in 0..(seconds / callback) as usize {
            let now = step as f64 * callback;
            while let Some(&(arrival, seq)) = arrivals.get(next).filter(|a| a.0 <= now) {
                let pcm: [f32; FRAME * 2] = std::array::from_fn(|i| {
                    ((seq as usize * FRAME + i / 2) as f32 * 0.03).sin() * 0.2
                });
                let size = encoder.encode(&pcm, &mut packet, MAX_DATAGRAM).unwrap();
                Header {
                    config,
                    sequence: seq,
                    timestamp: seq as u64 * FRAME as u64,
                }
                .write(&mut packet)
                .unwrap();
                receiver.push(&packet[..size], arrival).unwrap();
                next += 1;
            }
            while queued < quantum {
                receiver.render(now, &mut out).unwrap();
                assert!(out.iter().all(|s| s.is_finite()));
                queued += CHUNK;
            }
            queued -= quantum;
        }
        receiver
    }

    #[test]
    fn clean_network_settles_near_the_device_quantum() {
        for quantum in [96, 192, 1024, 2048] {
            let r = run(15.0, quantum, 0.0, |seq| {
                Some((seq as f64 * 0.37).sin().abs() * 0.001)
            });
            let floor = quantum as f64 / 48.0;
            assert_eq!(r.concealed_frames(), 0, "quantum={quantum}");
            assert!(
                r.buffer_ms() < floor * 1.5 + 8.0,
                "quantum={quantum}: {}",
                r.buffer_ms()
            );
        }
    }

    #[test]
    fn wifi_stalls_play_late_audio_and_drain_afterwards() {
        let r = run(25.0, 192, 0.0, |seq| {
            let sent = seq as f64 * FRAME_SECONDS + 0.002;
            let stall = |at: f64, length: f64| (at + length - sent).clamp(0.0, length);
            Some(stall(1.0, 0.015).max(stall(5.0, 0.060)))
        });
        assert_eq!(r.resets, 0);
        assert!(r.concealed_frames() <= 8, "PLC={}", r.concealed_frames());
        assert!(r.buffer_ms() < 15.0, "buffer {}", r.buffer_ms());
    }

    #[test]
    fn drift_loss_and_reordering_stay_bounded() {
        for ppm in [-500.0, 500.0] {
            let r = run(120.0, 480, ppm, |seq| match seq {
                _ if seq % 503 < 3 => None,
                _ if seq.is_multiple_of(97) => Some(0.014),
                _ => Some((seq as f64 * 0.17).sin().abs() * 0.001),
            });
            assert_eq!(r.resets, 0, "ppm={ppm}");
            assert!(
                r.concealed_frames() < 1200,
                "ppm={ppm}: {}",
                r.concealed_frames()
            );
            assert!(r.buffer_ms() < 40.0, "ppm={ppm}: {}", r.buffer_ms());
            assert!((r.drift_ratio() - 1.0 / (1.0 + ppm / 1e6)).abs() < 0.0002);
        }
    }

    #[test]
    fn starvation_is_silence_not_endless_plc() {
        let config = Config {
            stream: Stream::Playback,
            epoch: 1,
        };
        let mut packet = [0; MAX_DATAGRAM];
        let size = Encoder::try_new(config)
            .unwrap()
            .encode(&[0.2; FRAME * 2], &mut packet, MAX_DATAGRAM)
            .unwrap();
        let mut receiver = Receiver::try_new(config).unwrap();
        let mut out = [1.0; CHUNK * 2];
        receiver.push(&packet[..size], 0.0).unwrap();
        receiver.render(0.03, &mut out).unwrap();
        receiver.render(0.12, &mut out).unwrap();
        assert!(out.iter().all(|v| *v == 0.0));
    }
}
