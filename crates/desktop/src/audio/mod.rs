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
