#[cfg(target_os = "linux")]
#[path = "pipewire/capture.rs"]
pub mod capture;
#[cfg(windows)]
#[path = "wasapi/capture.rs"]
pub mod capture;
#[cfg(target_os = "linux")]
mod mic;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
mod pulse;
#[cfg(all(feature = "test-tone", target_os = "linux"))]
mod tone;
pub mod voice;
#[cfg(windows)]
pub mod wasapi;
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub enum CommandSender {
    PipeWire(::pipewire::channel::Sender<crate::server::CaptureCommand>),
    Pulse(pulse::CommandSender),
}
#[cfg(target_os = "linux")]
impl CommandSender {
    pub fn send(
        &self,
        command: crate::server::CaptureCommand,
    ) -> Result<(), crate::server::CaptureCommand> {
        match self {
            Self::PipeWire(sender) => sender.send(command),
            Self::Pulse(sender) => sender.send(command),
        }
    }
}
#[cfg(windows)]
pub type CommandSender = wasapi::CommandSender;

#[cfg(target_os = "linux")]
pub fn run(
    tone: bool,
    route: Option<litz_core::protocol::Route>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !tone && pulse::serves_audio() {
        crate::log::info!("using PulseAudio");
        return pulse::run(route);
    }
    crate::log::info!("using PipeWire");
    pipewire::run(tone, route)
}
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
