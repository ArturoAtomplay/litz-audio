use super::suppression;
use litz_core::protocol::{FRAME, InputSensitivity, VoiceSettings};
use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub(super) type Result<T> = std::result::Result<T, String>;

const HOLD_FRAMES: u8 = (0.200 * 48000.0 / FRAME as f64) as u8;
const LEVEL_RELEASE_FRAMES: u16 = (0.200 * 48000.0 / FRAME as f64) as u16;
const AUTOMATIC_THRESHOLD_FACTOR: f32 = 4.0;

// A denoiser or linear predictor can mistake a sustained voiced vowel for
// stationary noise or a room reflection. Compare each frame with recent input
// at plausible pitch periods before allowing strong attenuation.
//
// 100–600 Hz @ 48 kHz (lags 80–480).
pub(super) fn periodicity(previous: &[f32; FRAME], current: &[f32; FRAME]) -> f32 {
    let count = FRAME as f64;
    let mut signal = [0.0; 2 * FRAME];
    signal[..FRAME].copy_from_slice(previous);
    signal[FRAME..].copy_from_slice(current);
    let mut sums = [(0.0f64, 0.0f64); 2 * FRAME + 1];
    for (i, x) in signal.iter().enumerate() {
        let x = f64::from(*x);
        sums[i + 1] = (sums[i].0 + x, sums[i].1 + x * x);
    }
    let window = |start: usize| {
        let (head, tail) = (sums[start], sums[start + FRAME]);
        (tail.0 - head.0, tail.1 - head.1)
    };
    let (sum, energy) = window(FRAME);
    let energy = energy - sum * sum / count;
    if energy < 1e-6 {
        return 0.0;
    }
    let mut strongest = 0.0f64;
    for lag in (80..=480).step_by(4) {
        let (delayed_sum, delayed_energy) = window(FRAME - lag);
        let variance = delayed_energy - delayed_sum * delayed_sum / count;
        if variance > 1e-6 {
            let dot = f64::from(dot(current, &signal[FRAME - lag..2 * FRAME - lag]));
            strongest =
                strongest.max((dot - sum * delayed_sum / count) / (energy * variance).sqrt());
        }
    }
    strongest.clamp(0.0, 1.0) as f32
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    for (a, b) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0) {
        for lane in 0..8 {
            lanes[lane] += a[lane] * b[lane];
        }
    }
    lanes.iter().sum()
}

/// Bounded RMS level adaptation, followed by a sample-peak envelope limiter.
/// Gain rises slowly on periodic voiced input; non-voiced noise cannot pump it.
#[derive(Clone, Copy)]
struct Level {
    automatic: bool,
    gain_db: f32,
    limiter: f32,
    inactive_frames: u16,
}
impl Level {
    fn process(&mut self, samples: &mut [f32; FRAME], periodicity: f32) {
        let rms = (samples.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt();
        let speech = self.automatic && rms > 0.001 && periodicity > 0.55;
        if self.automatic && speech {
            self.inactive_frames = 0;
            let target = (-16.0 - 20.0 * rms.log10()).clamp(0.0, 18.0);
            // 3 dB/s increase, 12 dB/s decrease, at 100 blocks per second.
            self.gain_db += (target - self.gain_db).clamp(-0.12, 0.03);
        } else if self.automatic {
            self.inactive_frames = self.inactive_frames.saturating_add(1);
            if rms > 0.01 {
                let target = (-16.0 - 20.0 * rms.log10()).clamp(0.0, 18.0);
                self.gain_db += (target - self.gain_db).clamp(-0.12, 0.0);
            }
            if self.inactive_frames >= LEVEL_RELEASE_FRAMES {
                self.gain_db += (-self.gain_db).clamp(-0.06, 0.06);
            }
        }
        if !self.automatic {
            self.inactive_frames = 0;
            self.gain_db += (-self.gain_db).clamp(-0.3, 0.3);
        }
        let gain = 10f32.powf(self.gain_db / 20.0);
        for sample in samples {
            let x = *sample * gain;
            let required = (0.95 / x.abs().max(0.95)).min(1.0);
            self.limiter = required.min(self.limiter + (1.0 - self.limiter) * 0.0002);
            *sample = x * self.limiter;
        }
    }
}

struct Gate {
    previous: [f32; FRAME],
    ready: bool,
    open: bool,
    hold: u8,
    noise_floor: f32,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            previous: [0.0; FRAME],
            ready: false,
            open: false,
            hold: 0,
            noise_floor: 0.001,
        }
    }
}
impl Gate {
    fn process(
        &mut self,
        samples: &mut [f32; FRAME],
        input: &[f32; FRAME],
        sensitivity: InputSensitivity,
        periodicity: f32,
    ) {
        if sensitivity == InputSensitivity::Off {
            *self = Self::default();
            return;
        }
        let rms = (input.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt();
        let threshold = match sensitivity {
            InputSensitivity::Off => unreachable!(),
            InputSensitivity::Automatic => {
                (self.noise_floor * AUTOMATIC_THRESHOLD_FACTOR).clamp(0.004, 0.06)
            }
            InputSensitivity::Manual(db) => 10f32.powf(db as f32 / 20.0),
        };
        let was_open = self.open;
        if rms >= threshold {
            self.open = true;
            self.hold = HOLD_FRAMES;
        } else if self.open {
            if rms >= threshold * 0.5 {
                self.hold = HOLD_FRAMES;
            } else {
                self.hold = self.hold.saturating_sub(1);
                self.open = self.hold != 0;
            }
        }
        if sensitivity == InputSensitivity::Automatic
            && !was_open
            && !self.open
            && periodicity < 0.45
        {
            let rate = if rms > self.noise_floor { 0.005 } else { 0.01 };
            self.noise_floor = (self.noise_floor * (1.0 - rate) + rms * rate).clamp(0.0005, 0.03);
        }
        let current = *samples;
        if self.open && self.ready {
            *samples = self.previous;
            if !was_open {
                for (i, sample) in samples[..240].iter_mut().enumerate() {
                    *sample *= (i + 1) as f32 / 240.0;
                }
            }
        } else if was_open && self.ready {
            *samples = self.previous;
            for (i, sample) in samples[240..].iter_mut().enumerate() {
                *sample *= (239 - i) as f32 / 240.0;
            }
        } else {
            samples.fill(0.0);
        }
        self.previous = current;
        self.ready = true;
    }
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}
impl Biquad {
    fn peak(hz: f64, db: i8) -> Self {
        // RBJ peaking EQ, Q=0.707, normalized a0 (W3C Audio EQ Cookbook).
        let a = 10f64.powf(f64::from(db) / 40.0);
        let omega = std::f64::consts::TAU * hz / 48000.0;
        let alpha = omega.sin() / std::f64::consts::SQRT_2;
        let a0 = 1.0 + alpha / a;
        Self {
            b: [
                (1.0 + alpha * a) / a0,
                -2.0 * omega.cos() / a0,
                (1.0 - alpha * a) / a0,
            ],
            a: [-2.0 * omega.cos() / a0, (1.0 - alpha / a) / a0],
            z: [0.0; 2],
        }
    }
    fn process(&mut self, input: f32) -> f32 {
        let x = f64::from(input);
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y as f32
    }
}

pub(super) struct Pipeline {
    noise: suppression::Processor,
    eq: [Biquad; 3],
    level: Level,
    gate: Gate,
    // Raw input of the previous frame; periodicity is measured pre-denoiser
    // because DeepFilterNet can suppress voiced vowels it misclassifies as noise.
    previous_input: [f32; FRAME],
    settings: VoiceSettings,
    eq_target: [Biquad; 3],
    settling: usize,
}
impl Pipeline {
    fn configure(&mut self, settings: VoiceSettings) {
        if settings.eq_db != self.settings.eq_db {
            self.eq_target = std::array::from_fn(|i| {
                Biquad::peak([200.0, 1200.0, 5000.0][i], settings.eq_db[i])
            });
            self.settling = 2400; // 50 ms coefficient ramp; retain delay state.
        }
        self.level.automatic = settings.auto_level;
        if settings.input_sensitivity != self.settings.input_sensitivity {
            self.gate = Gate::default();
        }
        self.settings = settings;
    }
    pub(super) fn reset(&mut self, settings: VoiceSettings) -> Result<()> {
        self.settings = settings;
        self.settling = 0;
        self.eq_target =
            std::array::from_fn(|i| Biquad::peak([200.0, 1200.0, 5000.0][i], settings.eq_db[i]));
        self.noise.reset(settings.suppression)?;
        self.eq =
            std::array::from_fn(|i| Biquad::peak([200.0, 1200.0, 5000.0][i], settings.eq_db[i]));
        self.level = Level {
            automatic: settings.auto_level,
            gain_db: 0.0,
            limiter: 1.0,
            inactive_frames: 0,
        };
        self.gate = Gate::default();
        self.previous_input = [0.0; FRAME];
        Ok(())
    }
    pub(super) fn try_new(settings: VoiceSettings) -> Result<Self> {
        Ok(Self {
            settings,
            settling: 0,
            eq_target: std::array::from_fn(|i| {
                Biquad::peak([200.0, 1200.0, 5000.0][i], settings.eq_db[i])
            }),
            noise: suppression::Processor::try_new(settings.suppression)?,
            eq: std::array::from_fn(|i| {
                Biquad::peak([200.0, 1200.0, 5000.0][i], settings.eq_db[i])
            }),
            level: Level {
                automatic: settings.auto_level,
                gain_db: 0.0,
                limiter: 1.0,
                inactive_frames: 0,
            },
            gate: Gate::default(),
            previous_input: [0.0; FRAME],
        })
    }
    pub(super) fn process(&mut self, samples: &mut [f32; FRAME]) -> Result<()> {
        if samples.iter().any(|s| !s.is_finite() || s.abs() > 16.0) {
            return Err("Invalid microphone PCM".into());
        }
        if !self.settings.enabled() && self.settling == 0 && self.level.gain_db == 0.0 {
            return Ok(());
        }
        let input = *samples;
        let p = if self.settings.auto_level
            || self.settings.input_sensitivity != InputSensitivity::Off
        {
            periodicity(&self.previous_input, &input)
        } else {
            0.0
        };
        self.noise.process(samples)?;
        if self.settling == 0 && self.settings.eq_db == [0; 3] {
            self.eq.iter_mut().for_each(|filter| filter.z = [0.0; 2]);
        } else {
            for sample in samples.iter_mut() {
                if self.settling > 0 {
                    for (filter, target) in self.eq.iter_mut().zip(&self.eq_target) {
                        for (value, goal) in filter.b.iter_mut().zip(target.b) {
                            *value += (goal - *value) / self.settling as f64;
                        }
                        for (value, goal) in filter.a.iter_mut().zip(target.a) {
                            *value += (goal - *value) / self.settling as f64;
                        }
                    }
                    self.settling -= 1;
                }
                for filter in &mut self.eq {
                    *sample = filter.process(*sample);
                }
            }
        }
        self.level.process(samples, p);
        self.gate
            .process(samples, &input, self.settings.input_sensitivity, p);
        self.previous_input = input;
        if samples.iter().any(|s| !s.is_finite()) {
            return Err("Voice processor produced invalid PCM".into());
        }
        for sample in samples {
            *sample = sample.clamp(-1.0, 1.0);
        }
        Ok(())
    }
}

struct Job {
    settings: VoiceSettings,
    generation: u64,
    at: Instant,
    samples: [f32; FRAME],
    reply: tokio::sync::oneshot::Sender<Result<([f32; FRAME], Level)>>,
}
pub struct Worker {
    settings: VoiceSettings,
    history: VecDeque<[f32; FRAME]>,
    tx: Option<mpsc::SyncSender<Job>>,
    thread: Option<thread::JoinHandle<()>>,
    generation: u64,
    disabled: bool,
    retry_at: Instant,
    retry_attempt: u32,
    restarting: Option<tokio::task::JoinHandle<Result<Worker>>>,
    level: Option<Level>,
    last_output: Option<f32>,
    transition: bool,
}
impl Worker {
    /// Call from spawn_blocking before acknowledging a new microphone epoch.
    pub fn try_new(settings: VoiceSettings) -> Result<Self> {
        Self::warm(settings, VecDeque::new(), None)
    }
    pub fn runtime(&self) -> litz_core::protocol::VoiceRuntime {
        use litz_core::protocol::VoiceRuntime;
        if !self.settings.enabled() {
            VoiceRuntime::Off
        } else if self.disabled && self.restarting.is_some() {
            VoiceRuntime::Recovering
        } else if self.disabled {
            VoiceRuntime::Bypassed
        } else {
            VoiceRuntime::Active
        }
    }
    pub fn transition_from(&mut self, previous: &Self) {
        self.last_output = previous.last_output;
        self.transition = true;
    }
    fn finish(&mut self, samples: &mut [f32; FRAME]) {
        if self.transition
            && let Some(previous) = self.last_output
        {
            for (i, sample) in samples[..240].iter_mut().enumerate() {
                let amount = (i + 1) as f32 / 240.0;
                *sample = previous * (1.0 - amount) + *sample * amount;
            }
        }
        self.last_output = Some(samples[FRAME - 1]);
        self.transition = false;
    }
    pub fn can_configure(&self, settings: VoiceSettings) -> bool {
        !self.disabled && settings.suppression == self.settings.suppression
    }
    pub fn configure(&mut self, settings: VoiceSettings) {
        if self.settings.enabled() != settings.enabled() {
            self.generation = self.generation.wrapping_add(1);
            self.level = None;
            self.transition = true;
        }
        self.settings = settings;
    }
    pub fn history(&self) -> VecDeque<[f32; FRAME]> {
        self.history.clone()
    }
    pub fn level(&self) -> Option<(f32, f32)> {
        self.level.map(|level| (level.gain_db, level.limiter))
    }
    pub fn warm(
        settings: VoiceSettings,
        history: VecDeque<[f32; FRAME]>,
        previous_level: Option<(f32, f32)>,
    ) -> Result<Self> {
        let retained_history = history.clone();
        let history_at = (!history.is_empty()).then(Instant::now);
        let (tx, input) = mpsc::sync_channel::<Job>(1);
        let (ready, initialized) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("litz-voice".into())
            .spawn(move || {
                // Tract owns thread-local inference state. Build and use it on
                // this worker rather than asserting Send for upstream objects.
                let mut pipeline = match Pipeline::try_new(settings) {
                    Ok(pipeline) => pipeline,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                for mut frame in history {
                    if let Err(error) = pipeline.process(&mut frame) {
                        let _ = ready.send(Err(error));
                        return;
                    }
                }
                if let Some((gain_db, limiter)) = previous_level {
                    pipeline.level.gain_db = gain_db;
                    pipeline.level.limiter = limiter;
                }
                let _ = ready.send(Ok(()));
                let mut generation = 0;
                let mut warmed_at = history_at;
                while let Ok(mut job) = input.recv() {
                    let result = (|| {
                        let obsolete_history = warmed_at.take().is_some_and(|at| {
                            job.at.saturating_duration_since(at) > Duration::from_millis(20)
                        });
                        if job.generation != generation {
                            pipeline.reset(job.settings)?;
                            generation = job.generation;
                        } else if obsolete_history {
                            let gain_db = pipeline.level.gain_db;
                            let limiter = pipeline.level.limiter;
                            pipeline.reset(job.settings)?;
                            pipeline.level.gain_db = gain_db;
                            pipeline.level.limiter = limiter;
                        }
                        if job.at.elapsed() > Duration::from_millis(20) {
                            return Err("Voice processing deadline missed".into());
                        }
                        pipeline.configure(job.settings);
                        pipeline.process(&mut job.samples)?;
                        Ok((job.samples, pipeline.level))
                    })();
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        if let Err(error) = initialized.recv().map_err(|e| e.to_string())? {
            let _ = thread.join();
            return Err(error);
        }
        Ok(Self {
            settings,
            history: retained_history,
            tx: Some(tx),
            thread: Some(thread),
            generation: 0,
            disabled: false,
            retry_at: Instant::now(),
            retry_attempt: 0,
            restarting: None,
            last_output: None,
            transition: false,
            level: previous_level.map(|(gain_db, limiter)| Level {
                automatic: settings.auto_level,
                gain_db,
                limiter,
                inactive_frames: 0,
            }),
        })
    }
    pub fn reset(&mut self) -> Result<()> {
        self.history.clear();
        self.level = None;
        self.last_output = None;
        self.transition = false;
        if let Some(restarting) = self.restarting.take() {
            restarting.abort();
            self.back_off();
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Voice generation exhausted")?;
        Ok(())
    }
    pub async fn process(&mut self, samples: &mut [f32; FRAME]) -> Result<()> {
        if samples.iter().any(|x| !x.is_finite() || x.abs() > 16.0) {
            samples.fill(0.0);
            self.reset()?;
            return Ok(());
        }
        // With every effect off, keep the PCM on this task. A worker round trip
        // on each 10 ms block can stall the virtual microphone even though the
        // worker would return the samples unchanged.
        if !self.settings.enabled() {
            self.remember(samples);
            self.finish(samples);
            return Ok(());
        }
        if self.disabled {
            if self
                .restarting
                .as_ref()
                .is_some_and(|task| task.is_finished())
            {
                let result = self.restarting.take().unwrap().await;
                if let Ok(Ok(mut next)) = result {
                    next.transition_from(self);
                    next.history = self.history.clone();
                    *self = next;
                } else {
                    self.back_off();
                }
            }
            if self.restarting.is_none() && Instant::now() >= self.retry_at && self.alive() {
                self.disabled = false;
                self.generation = self.generation.wrapping_add(1);
            }
            if self.disabled {
                self.remember(samples);
                if self.restarting.is_none() && Instant::now() >= self.retry_at {
                    let settings = self.settings;
                    let history = self.history.clone();
                    let level = self.level();
                    self.restarting = Some(tokio::task::spawn_blocking(move || {
                        Worker::warm(settings, history, level)
                    }));
                }
                self.finish(samples);
                return Ok(());
            }
        }
        let dry = *samples;
        if let Err(error) = self.process_current(samples).await {
            eprintln!("litzd: voice processing bypassed ({error}); retrying automatically");
            *samples = dry;
            self.disabled = true;
            self.transition = true;
            self.back_off();
            if !self.alive() {
                self.tx.take();
                if let Some(thread) = self.thread.take() {
                    tokio::task::spawn_blocking(move || {
                        let _ = thread.join();
                    });
                }
            }
        } else if self.retry_attempt > 0 && self.retry_at.elapsed() > Duration::from_secs(30) {
            self.retry_attempt = 0;
        }
        self.finish(samples);
        Ok(())
    }
    fn alive(&self) -> bool {
        self.tx.is_some() && self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
    fn back_off(&mut self) {
        self.retry_at = Instant::now() + Duration::from_secs(2u64 << self.retry_attempt);
        self.retry_attempt = (self.retry_attempt + 1).min(4);
    }
    fn remember(&mut self, samples: &[f32; FRAME]) {
        if self.history.len() == 20 {
            self.history.pop_front();
        }
        self.history.push_back(*samples);
    }
    async fn process_current(&mut self, samples: &mut [f32; FRAME]) -> Result<()> {
        self.remember(samples);
        let (reply, response) = tokio::sync::oneshot::channel();
        self.tx
            .as_ref()
            .ok_or("Voice worker stopped")?
            .try_send(Job {
                settings: self.settings,
                generation: self.generation,
                at: Instant::now(),
                samples: *samples,
                reply,
            })
            .map_err(|_| "Voice worker overloaded or stopped")?;
        samples.fill(0.0);
        let (processed, level) = tokio::time::timeout(Duration::from_millis(20), response)
            .await
            .map_err(|_| "Voice processing deadline missed")?
            .map_err(|_| "Voice worker stopped")??;
        *samples = processed;
        self.level = Some(level);
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn process_gate(
        gate: &mut Gate,
        previous_input: &mut [f32; FRAME],
        samples: &mut [f32; FRAME],
        mode: InputSensitivity,
    ) {
        let input = *samples;
        let p = periodicity(previous_input, &input);
        *previous_input = input;
        gate.process(samples, &input, mode, p);
    }
    #[test]
    fn gate_keeps_a_leading_frame_and_holds_short_pauses() {
        let mut gate = Gate::default();
        let mode = InputSensitivity::Manual(-40);
        let mut previous_input = [0.0; FRAME];
        let mut quiet = [0.001; FRAME];
        process_gate(&mut gate, &mut previous_input, &mut quiet, mode);
        assert_eq!(quiet, [0.0; FRAME]);
        let mut speech = [0.1; FRAME];
        process_gate(&mut gate, &mut previous_input, &mut speech, mode);
        assert!(speech[FRAME - 1] > 0.0);
        let mut speech = [0.1; FRAME];
        process_gate(&mut gate, &mut previous_input, &mut speech, mode);
        assert!(speech.iter().all(|sample| *sample > 0.0));
        for _ in 0..19 {
            let mut pause = [0.0; FRAME];
            process_gate(&mut gate, &mut previous_input, &mut pause, mode);
            assert!(gate.open);
        }
        let mut pause = [0.0; FRAME];
        process_gate(&mut gate, &mut previous_input, &mut pause, mode);
        assert!(!gate.open);
        let mut dry = [0.2; FRAME];
        process_gate(
            &mut gate,
            &mut previous_input,
            &mut dry,
            InputSensitivity::Off,
        );
        assert_eq!(dry, [0.2; FRAME]);
    }
    #[test]
    fn automatic_gate_does_not_learn_active_audio_as_noise() {
        let mut gate = Gate::default();
        let mut previous_input = [0.0; FRAME];
        let mut seed = 1u32;
        for _ in 0..400 {
            let mut noise = [0.0; FRAME];
            for sample in &mut noise {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *sample = (seed as f32 / u32::MAX as f32 - 0.5) * 0.08;
            }
            process_gate(
                &mut gate,
                &mut previous_input,
                &mut noise,
                InputSensitivity::Automatic,
            );
        }
        assert!(gate.open);
        assert_eq!(gate.noise_floor, 0.001);
        let mut speech = [0.1; FRAME];
        process_gate(
            &mut gate,
            &mut previous_input,
            &mut speech,
            InputSensitivity::Automatic,
        );
        assert!(gate.open);
    }
    #[test]
    fn manual_gate_uses_input_before_voice_effects() {
        let mut gate = Gate::default();
        let input = [0.05; FRAME];
        let mut processed = [0.005; FRAME];
        gate.process(&mut processed, &input, InputSensitivity::Manual(-40), 0.0);
        assert!(gate.open);
        let mut processed = [0.005; FRAME];
        gate.process(&mut processed, &input, InputSensitivity::Manual(-40), 0.0);
        assert!(processed.iter().all(|sample| *sample > 0.0));
    }
    #[test]
    fn sensitivity_alone_enables_the_voice_pipeline() {
        let settings = VoiceSettings {
            input_sensitivity: InputSensitivity::Manual(-40),
            ..Default::default()
        };
        assert!(settings.enabled());
        let mut pipeline = Pipeline::try_new(settings).unwrap();
        let mut quiet = [0.001; FRAME];
        pipeline.process(&mut quiet).unwrap();
        assert_eq!(quiet, [0.0; FRAME]);
        let mut speech = [0.1; FRAME];
        pipeline.process(&mut speech).unwrap();
        let mut speech = [0.1; FRAME];
        pipeline.process(&mut speech).unwrap();
        assert!(speech.iter().all(|sample| *sample > 0.0));
    }
    #[tokio::test]
    async fn bypass_sanitizes_invalid_pcm_and_reset_forgets_gain() {
        let mut worker = Worker::try_new(VoiceSettings::default()).unwrap();
        let mut invalid = [f32::NAN; FRAME];
        worker.process(&mut invalid).await.unwrap();
        assert_eq!(invalid, [0.0; FRAME]);
        worker.level = Some(Level {
            automatic: true,
            gain_db: 18.0,
            limiter: 0.5,
            inactive_frames: 0,
        });
        worker.reset().unwrap();
        assert!(worker.level().is_none());
        assert!(worker.history().is_empty());
    }
    #[tokio::test]
    async fn obsolete_warmup_cannot_replay_previous_speech() {
        let settings = VoiceSettings {
            suppression: litz_core::protocol::Suppression::DeepFilterNet,
            ..Default::default()
        };
        let history = std::iter::repeat_n([0.1; FRAME], 20).collect();
        let Some(mut worker) = Worker::warm(settings, history, None).ok() else {
            return;
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut silent = [0.0; FRAME];
        worker.process(&mut silent).await.unwrap();
        assert!(silent.iter().all(|x| x.abs() < 1e-6));
    }
    #[test]
    fn effect_transition_is_bounded_and_finishes_within_one_frame() {
        let mut worker = Worker::try_new(VoiceSettings::default()).unwrap();
        worker.last_output = Some(0.5);
        worker.transition = true;
        let mut frame = [-0.5; FRAME];
        worker.finish(&mut frame);
        assert!((frame[0] - 0.5).abs() < 0.005);
        assert_eq!(frame[239], -0.5);
        assert_eq!(frame[FRAME - 1], -0.5);
        assert!(!worker.transition);
    }
    #[test]
    fn sustained_tone_stays_audible_with_general_effects() {
        use litz_core::protocol::Suppression;
        let profiles = [
            (
                "rnnoise",
                VoiceSettings {
                    suppression: Suppression::Rnnoise,
                    ..Default::default()
                },
            ),
            (
                "level",
                VoiceSettings {
                    auto_level: true,
                    ..Default::default()
                },
            ),
            (
                "eq",
                VoiceSettings {
                    eq_db: [12, -12, 12],
                    ..Default::default()
                },
            ),
            (
                "rnnoise_all",
                VoiceSettings {
                    suppression: Suppression::Rnnoise,
                    auto_level: true,
                    eq_db: [12, -12, 12],
                    ..Default::default()
                },
            ),
        ];
        for (name, settings) in profiles {
            let mut pipeline = Pipeline::try_new(settings).unwrap();
            let mut powers = [0.0f64; 4];
            for block in 0..160 {
                let mut frame = std::array::from_fn(|i| {
                    let phase =
                        (block * FRAME + i) as f32 * std::f32::consts::TAU * 200.0 / 48000.0;
                    0.08 * phase.sin() + 0.04 * (2.0 * phase).sin()
                });
                pipeline.process(&mut frame).unwrap();
                assert!(
                    frame.iter().all(|x| x.is_finite() && x.abs() <= 1.0),
                    "{name}"
                );
                powers[block / 40] += frame.iter().map(|x| f64::from(x * x)).sum::<f64>();
            }
            let rms = powers.map(|p| (p / (40 * FRAME) as f64).sqrt());
            assert!(rms[3] > 0.04, "{name} erased sustained voice: {rms:?}");
            assert!(rms[3] > rms[2] * 0.85, "{name} kept fading voice: {rms:?}");
        }
    }
    #[test]
    fn room_reduction_does_not_gate_tones_across_pitch_range() {
        for pitch in [100.0f32, 120.0, 300.0, 380.0, 500.0] {
            let mut pipeline = Pipeline::try_new(VoiceSettings {
                auto_level: true,
                ..Default::default()
            })
            .unwrap();
            let (mut before, mut after) = (0.0f64, 0.0f64);
            for block in 0..120 {
                let mut frame = std::array::from_fn(|i| {
                    let phase =
                        (block * FRAME + i) as f32 * std::f32::consts::TAU * pitch / 48000.0;
                    0.08 * phase.sin() + 0.04 * (2.0 * phase).sin()
                });
                if block >= 80 {
                    before += frame.iter().map(|x| f64::from(x * x)).sum::<f64>();
                }
                pipeline.process(&mut frame).unwrap();
                if block >= 80 {
                    after += frame.iter().map(|x| f64::from(x * x)).sum::<f64>();
                }
            }
            assert!(
                after > before * 0.4,
                "room gated {pitch} Hz voice: {before} -> {after}"
            );
        }
    }
    #[test]
    fn live_eq_and_level_edits_preserve_history_and_ramp() {
        let settings = VoiceSettings {
            auto_level: true,
            ..Default::default()
        };
        let mut pipeline = Pipeline::try_new(settings).unwrap();
        for _ in 0..100 {
            pipeline.process(&mut [0.02; FRAME]).unwrap();
        }
        let previous_gain = pipeline.level.gain_db;
        let previous_state = pipeline.eq[0].z;
        pipeline.configure(VoiceSettings {
            eq_db: [12, -12, 6],
            ..settings
        });
        assert_eq!(pipeline.level.gain_db, previous_gain);
        assert_eq!(pipeline.eq[0].z, previous_state);
        assert_eq!(pipeline.settling, 2400);
        for _ in 0..5 {
            let mut frame = [0.02; FRAME];
            pipeline.process(&mut frame).unwrap();
            assert!(frame.iter().all(|v| v.is_finite() && v.abs() < 0.5));
        }
        assert_eq!(pipeline.settling, 0);
        assert_eq!(pipeline.eq[0].b, pipeline.eq_target[0].b);
        let previous_gain = pipeline.level.gain_db;
        pipeline.configure(VoiceSettings {
            auto_level: false,
            ..pipeline.settings
        });
        assert_eq!(pipeline.level.gain_db, previous_gain);
        pipeline.process(&mut [0.02; FRAME]).unwrap();
        assert!((pipeline.level.gain_db - previous_gain).abs() <= 0.301);
    }
    #[test]
    fn rapid_extreme_eq_edits_do_not_create_spikes_or_invalid_pcm() {
        let mut pipeline = Pipeline::try_new(VoiceSettings {
            eq_db: [12, -12, 12],
            ..Default::default()
        })
        .unwrap();
        let mut previous = 0.0f32;
        for block in 0..300 {
            if block % 3 == 0 {
                pipeline.configure(VoiceSettings {
                    eq_db: if block % 2 == 0 {
                        [-12, 12, -12]
                    } else {
                        [12, -12, 12]
                    },
                    ..Default::default()
                });
            }
            let mut frame = std::array::from_fn(|i| {
                let phase = (block * FRAME + i) as f32 * std::f32::consts::TAU / 48000.0;
                0.05 * (200.0 * phase).sin() + 0.05 * (1200.0 * phase).sin()
            });
            pipeline.process(&mut frame).unwrap();
            assert!(frame.iter().all(|x| x.is_finite() && x.abs() < 0.8));
            assert!(
                (frame[0] - previous).abs() < 0.2,
                "EQ edit clicked at frame {block}"
            );
            previous = frame[FRAME - 1];
        }
    }

    #[tokio::test]
    async fn live_eq_does_not_recreate_denoiser_or_reset_epoch() {
        let settings = VoiceSettings {
            suppression: litz_core::protocol::Suppression::Rnnoise,
            ..Default::default()
        };
        let mut worker = Worker::try_new(settings).unwrap();
        worker.process(&mut [0.1; FRAME]).await.unwrap();
        let thread = worker.thread.as_ref().unwrap().thread().id();
        let next = VoiceSettings {
            eq_db: [1, 0, 0],
            ..settings
        };
        assert!(worker.can_configure(next));
        worker.configure(next);
        worker.process(&mut [0.1; FRAME]).await.unwrap();
        assert_eq!(worker.thread.as_ref().unwrap().thread().id(), thread);
        assert_eq!(worker.generation, 0);
        assert_eq!(worker.history().len(), 2);
        worker.reset().unwrap();
        assert!(worker.history().is_empty());
    }
    #[tokio::test]
    async fn replacement_warms_from_history_without_gating_the_stream() {
        let mut old = Worker::try_new(VoiceSettings {
            auto_level: true,
            ..Default::default()
        })
        .unwrap();
        for _ in 0..25 {
            let mut voice = std::array::from_fn(|i| {
                let phase = i as f32 * std::f32::consts::TAU * 200.0 / 48000.0;
                0.028 * phase.sin()
            });
            old.process(&mut voice).await.unwrap();
        }
        assert_eq!(old.history().len(), 20);
        let previous_level = old.level().unwrap();
        assert!(previous_level.0 > 0.0);
        let settings = VoiceSettings {
            auto_level: true,
            ..Default::default()
        };
        let mut next = Worker::warm(settings, old.history(), old.level()).unwrap();
        assert_eq!(next.level().unwrap(), previous_level);
        tokio::task::spawn_blocking(move || drop(old));
        let mut frame = [0.1; FRAME];
        next.process(&mut frame).await.unwrap();
        assert!(frame.iter().all(|sample| sample.is_finite()));
        next.reset().unwrap();
        let mut silence = [0.0; FRAME];
        next.process(&mut silence).await.unwrap();
        assert_eq!(silence, [0.0; FRAME]);
    }
    #[tokio::test]
    async fn missed_deadline_resumes_the_loaded_effect_on_its_thread() {
        let mut worker = Worker::try_new(VoiceSettings {
            auto_level: true,
            ..Default::default()
        })
        .unwrap();
        let thread = worker.thread.as_ref().unwrap().thread().id();
        let (reply, response) = tokio::sync::oneshot::channel();
        drop(response);
        worker
            .tx
            .as_ref()
            .unwrap()
            .send(Job {
                settings: worker.settings,
                generation: worker.generation,
                at: Instant::now(),
                samples: [0.1; FRAME],
                reply,
            })
            .unwrap();
        worker.disabled = true;
        for _ in 0..100 {
            worker.retry_at = Instant::now();
            worker.process(&mut [0.1; FRAME]).await.unwrap();
            assert!(worker.restarting.is_none());
            if !worker.disabled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!worker.disabled);
        assert_eq!(worker.thread.as_ref().unwrap().thread().id(), thread);
    }
    #[tokio::test]
    async fn stream_resets_do_not_cut_the_retry_backoff_short() {
        let mut worker = Worker::try_new(VoiceSettings {
            auto_level: true,
            ..Default::default()
        })
        .unwrap();
        worker.disabled = true;
        worker.back_off();
        for _ in 0..10 {
            worker.reset().unwrap();
            let mut frame = [0.1; FRAME];
            worker.process(&mut frame).await.unwrap();
            assert!(worker.disabled);
            assert!(worker.restarting.is_none());
            assert_eq!(frame, [0.1; FRAME]);
        }
    }
    #[tokio::test]
    async fn overloaded_effect_recovers_without_settings_change() {
        let mut worker = Worker::try_new(VoiceSettings {
            auto_level: true,
            ..Default::default()
        })
        .unwrap();
        worker.tx.take();
        worker.process(&mut [0.1; FRAME]).await.unwrap();
        assert!(worker.disabled);
        worker.retry_at = Instant::now();
        for _ in 0..100 {
            let mut frame = [0.1; FRAME];
            worker.process(&mut frame).await.unwrap();
            if !worker.disabled {
                assert!(worker.tx.is_some());
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("effect did not recover");
    }
    #[tokio::test]
    async fn disabled_effects_preserve_pcm_without_waiting_for_worker() {
        let mut worker = Worker::try_new(VoiceSettings::default()).unwrap();
        worker.tx.take();
        for n in 0..25 {
            let mut frame = [n as f32 / 100.0; FRAME];
            worker.process(&mut frame).await.unwrap();
            assert_eq!(frame, [n as f32 / 100.0; FRAME]);
        }
        assert!(!worker.disabled);
        assert_eq!(worker.history().len(), 20);
    }
    #[tokio::test]
    async fn enabling_effects_after_bypass_resets_worker_state() {
        let settings = VoiceSettings {
            auto_level: true,
            ..Default::default()
        };
        let mut worker = Worker::try_new(settings).unwrap();
        worker.process(&mut [0.02; FRAME]).await.unwrap();
        assert!(worker.level().is_some());
        worker.configure(VoiceSettings::default());
        assert!(worker.level().is_none());
        let generation = worker.generation;
        worker.process(&mut [0.02; FRAME]).await.unwrap();
        worker.configure(settings);
        assert_eq!(worker.generation, generation + 1);
        worker.process(&mut [0.02; FRAME]).await.unwrap();
        assert!(worker.level().is_some());
    }
    #[test]
    fn automatic_level_is_bounded_slow_and_does_not_raise_silence() {
        let mut level = Level {
            automatic: true,
            gain_db: 0.0,
            limiter: 1.0,
            inactive_frames: 0,
        };
        for _ in 0..1000 {
            level.process(&mut [0.0001; FRAME], 0.0);
        }
        assert_eq!(level.gain_db, 0.0);
        let voice = std::array::from_fn(|i| {
            let phase = i as f32 * std::f32::consts::TAU * 200.0 / 48000.0;
            0.02 * phase.sin()
        });
        let mut frame = voice;
        level.process(&mut frame, 0.8);
        assert!((level.gain_db - 0.03).abs() < 1e-6);
        for _ in 0..1000 {
            let mut frame = voice;
            level.process(&mut frame, 0.8);
        }
        assert!(level.gain_db <= 18.0);
        let mut loud = [16.0; FRAME];
        level.process(&mut loud, 0.0);
        assert!(loud.iter().all(|v| v.abs() <= 0.951));
    }
    #[test]
    fn automatic_level_preserves_an_already_loud_voice() {
        let mut level = Level {
            automatic: true,
            gain_db: 0.0,
            limiter: 1.0,
            inactive_frames: 0,
        };
        let mut output = [0.0; FRAME];
        for block in 0..100 {
            output = std::array::from_fn(|i| {
                let phase = (block * FRAME + i) as f32 * std::f32::consts::TAU * 220.0 / 48000.0;
                0.3 * phase.sin()
            });
            level.process(&mut output, 0.8);
        }
        let rms = (output.iter().map(|sample| sample * sample).sum::<f32>() / FRAME as f32).sqrt();
        assert!(level.gain_db >= 0.0);
        assert!(
            rms > 0.2,
            "automatic level attenuated an already clear voice"
        );
    }
    #[test]
    fn automatic_level_lifts_quiet_voiced_sound_without_pumping_room_noise() {
        let mut level = Level {
            automatic: true,
            gain_db: 0.0,
            limiter: 1.0,
            inactive_frames: 0,
        };
        let mut seed = 19u32;
        for _ in 0..100 {
            let mut noise = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as i32 as f32 / i32::MAX as f32 * 0.04
            });
            level.process(&mut noise, 0.0);
        }
        assert_eq!(level.gain_db, 0.0);
        for block in 0..100 {
            let mut voice = std::array::from_fn(|i| {
                let phase = (block * FRAME + i) as f32 * std::f32::consts::TAU * 200.0 / 48000.0;
                0.003 * phase.sin()
            });
            level.process(&mut voice, 0.8);
        }
        assert!(level.gain_db > 2.0, "quiet voice was not raised");
        for _ in 0..100 {
            let mut noise = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed as i32 as f32 / i32::MAX as f32 * 0.04
            });
            level.process(&mut noise, 0.0);
        }
        assert!(level.gain_db < 0.1, "noise retained the voice gain");
        assert_eq!(periodicity(&[0.02; FRAME], &[0.02; FRAME]), 0.0);
    }
    #[tokio::test]
    async fn worker_reset_discards_filter_history() {
        let settings = VoiceSettings {
            eq_db: [12, 12, 12],
            ..Default::default()
        };
        let mut worker = Worker::try_new(settings).unwrap();
        worker.process(&mut [0.5; FRAME]).await.unwrap();
        worker.reset().unwrap();
        let mut silent = [0.0; FRAME];
        worker.process(&mut silent).await.unwrap();
        assert!(silent.iter().all(|v| *v == 0.0));
    }
    #[test]
    fn eq_has_requested_center_gain_and_flat_bypass() {
        for db in -12..=12 {
            let mut filter = Biquad::peak(1200.0, db);
            let mut input = 0.0f64;
            let mut output = 0.0f64;
            for i in 0..48000 {
                let x = (std::f32::consts::TAU * 1200.0 * i as f32 / 48000.0).sin() * 0.05;
                let y = filter.process(x);
                if i > 24000 {
                    input += f64::from(x * x);
                    output += f64::from(y * y);
                }
            }
            assert!((10.0 * (output / input).log10() - f64::from(db)).abs() < 0.02);
        }
    }
    #[test]
    fn silence_and_invalid_pcm_do_not_escape() {
        let mut p = Pipeline::try_new(VoiceSettings {
            auto_level: true,
            eq_db: [12; 3],
            ..Default::default()
        })
        .unwrap();
        for _ in 0..30 {
            let mut frame = [0.0; FRAME];
            p.process(&mut frame).unwrap();
            assert!(frame.iter().all(|x| x.is_finite() && x.abs() < 1e-5));
        }
        assert!(p.process(&mut [f32::NAN; FRAME]).is_err());
    }
    #[tokio::test]
    async fn unavailable_worker_bypasses_effect_without_stopping_audio() {
        let (tx, input) = mpsc::sync_channel(1);
        drop(input);
        let settings = VoiceSettings {
            auto_level: true,
            ..Default::default()
        };
        let mut worker = Worker {
            settings,
            history: VecDeque::new(),
            tx: Some(tx),
            thread: None,
            generation: 0,
            disabled: false,
            retry_at: Instant::now(),
            retry_attempt: 0,
            restarting: None,
            level: None,
            last_output: None,
            transition: false,
        };
        let mut frame = [0.5; FRAME];
        worker.process(&mut frame).await.unwrap();
        assert_eq!(frame, [0.5; FRAME]);
        assert!(worker.disabled);
        assert!(!worker.can_configure(settings));
    }
    #[test]
    fn automatic_gate_noise_floor_recovers_after_the_room_quiets() {
        let mut gate = Gate {
            noise_floor: 0.025,
            ..Gate::default()
        };

        let mut seed = 3u32;
        let mut previous_input = [0.0; FRAME];
        let mut recovered_at = None;
        for frame in 0..2000 {
            let mut quiet = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed as i32 as f32 / i32::MAX as f32 * 0.001
            });
            let input = quiet;
            let p = periodicity(&previous_input, &input);
            previous_input = input;
            gate.process(&mut quiet, &input, InputSensitivity::Automatic, p);
            if gate.noise_floor < 0.0025 {
                recovered_at = Some(frame);
                break;
            }
        }
        assert!(
            recovered_at.is_some_and(|frame| frame < 500),
            "noise floor took {recovered_at:?} frames to release after the room quieted"
        );
    }
    #[test]
    fn periodicity_detects_high_female_voice_fundamentals() {
        let mut previous = [0.0f32; FRAME];
        let current: [f32; FRAME] = std::array::from_fn(|i| {
            (i as f32 * std::f32::consts::TAU * 500.0 / 48000.0).sin() * 0.1
        });
        let p = periodicity(&previous, &current);
        assert!(p > 0.8, "500 Hz pure tone not detected as periodic: {p:.3}");

        previous = std::array::from_fn(|i| {
            ((i + FRAME) as f32 * std::f32::consts::TAU * 500.0 / 48000.0).sin() * 0.1
        });
        let p2 = periodicity(&previous, &current);
        assert!(
            p2 > 0.8,
            "500 Hz with primed previous not periodic: {p2:.3}"
        );
    }
}
