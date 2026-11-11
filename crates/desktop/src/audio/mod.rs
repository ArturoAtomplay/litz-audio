#[cfg(target_os = "linux")]
#[path = "pipewire/capture.rs"]
pub mod capture;
#[cfg(windows)]
#[path = "wasapi/capture.rs"]
pub mod capture;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(all(feature = "test-tone", target_os = "linux"))]
mod tone;
pub mod voice;
#[cfg(windows)]
pub mod wasapi;
#[cfg(target_os = "linux")]
pub type CommandSender = ::pipewire::channel::Sender<crate::server::CaptureCommand>;
#[cfg(windows)]
pub type CommandSender = std::sync::mpsc::SyncSender<crate::server::CaptureCommand>;

#[cfg(target_os = "linux")]
pub use pipewire::run;
#[cfg(windows)]
pub use wasapi::run;

#[cfg(target_os = "linux")]
pub const PLATFORM: &str = "Linux";
#[cfg(windows)]
pub const PLATFORM: &str = "Windows";

/// Frames a capture route may hold for the network loop.
pub const CAPACITY: usize = 2048;

/// One stereo frame of computer audio and when its buffer arrived.
#[derive(Clone, Copy)]
pub struct Captured {
    pub samples: [f32; 2],
    pub at: std::time::Instant,
}

impl Captured {
    /// A non-finite sample would poison the encoder, so it becomes silence.
    pub fn new(samples: [f32; 2], at: std::time::Instant) -> Self {
        Self {
            samples: samples.map(|sample| if sample.is_finite() { sample } else { 0.0 }),
            at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_finite_sample_is_captured_as_silence() {
        let at = std::time::Instant::now();
        assert_eq!(Captured::new([0.5, -0.25], at).samples, [0.5, -0.25]);
        assert_eq!(Captured::new([f32::NAN, 0.5], at).samples, [0.0, 0.5]);
        assert_eq!(
            Captured::new([f32::INFINITY, f32::NEG_INFINITY], at).samples,
            [0.0, 0.0]
        );
    }
}
