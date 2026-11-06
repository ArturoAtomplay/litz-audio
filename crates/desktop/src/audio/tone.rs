use litz_core::pcm::PcmProducer;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Default)]
struct State {
    streaming: bool,
    stop: bool,
}

#[derive(Clone)]
pub struct Control(Arc<(Mutex<State>, Condvar)>);

pub struct Fixture {
    control: Control,
    worker: Option<JoinHandle<()>>,
}

impl Fixture {
    pub fn new(mut producer: PcmProducer, enabled: bool) -> Self {
        let control = Control(Arc::new((Mutex::new(State::default()), Condvar::new())));
        let shared = control.clone();
        let worker = thread::spawn(move || {
            let (mutex, wake) = &*shared.0;
            let mut state = mutex.lock().unwrap();
            let mut phase = 0_u32;
            while !state.stop {
                if !state.streaming || !enabled {
                    state = wake.wait(state).unwrap();
                    continue;
                }
                let count = producer.free_samples();
                let mut block = [0.0; litz_core::pcm::CAPACITY];
                for sample in &mut block[..count] {
                    *sample = (std::f32::consts::TAU * phase as f32 / 48.0).sin() * 0.1;
                    phase = (phase + 1) % 48;
                }
                let _ = producer.push(&block[..count]);
                state = wake
                    .wait_timeout(state, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        });
        Self {
            control,
            worker: Some(worker),
        }
    }

    pub fn control(&self) -> Control {
        self.control.clone()
    }
}

impl Control {
    pub fn set_streaming(&self, streaming: bool) {
        let (mutex, wake) = &*self.0;
        mutex.lock().unwrap().streaming = streaming;
        wake.notify_one();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let (mutex, wake) = &*self.control.0;
        mutex.lock().unwrap().stop = true;
        wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
