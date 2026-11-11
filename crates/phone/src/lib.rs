//! The phone's side of Litz and the API its app calls. UniFFI generates the
//! app's side of that API.
#![warn(clippy::undocumented_unsafe_blocks)]
#[cfg(target_os = "android")]
mod android;
mod audio;
mod engine;
mod failure;
#[cfg(not(target_os = "android"))]
mod host;
mod identity;
mod pairing;

#[cfg(target_os = "android")]
use android as platform;
#[cfg(not(target_os = "android"))]
use host as platform;

pub use identity::{Identity, create_identity};
use litz_core::protocol::{
    Code, DesktopRoutes, Endpoint, InputSensitivity, Route, Suppression, VoiceRuntime,
    VoiceSettings,
};
use std::net::SocketAddr;

uniffi::setup_scaffolding!();

uniffi::custom_type!(SocketAddr, String, {
    remote,
    lower: |address| address.to_string(),
    try_lift: |text| Ok(text.parse()?),
});

/// What the computer advertises as `v` over mDNS.
#[uniffi::export]
pub fn protocol_version() -> u8 {
    litz_core::protocol::VERSION
}

/// The port a computer listens on unless it advertises another.
#[uniffi::export]
pub fn default_port() -> u16 {
    litz_core::protocol::PORT
}

/// The mDNS host name of the computer with this certificate fingerprint.
#[uniffi::export]
pub fn mdns_host(fingerprint: String) -> Option<String> {
    litz_core::protocol::mdns_host(&fingerprint)
}

type EqualizerGains = [i8; 3];

uniffi::custom_type!(EqualizerGains, Vec<i8>, {
    remote,
    lower: |gains| gains.to_vec(),
    try_lift: |gains| {
        gains
            .try_into()
            .map_err(|_| uniffi::deps::anyhow::anyhow!("an equalizer has three bands"))
    },
});

#[uniffi::remote(Enum)]
enum Code {
    Busy,
    Unauthorized,
    Unsupported,
    Invalid,
    RevisionConflict,
    RouteConflict,
}

#[uniffi::remote(Enum)]
enum Route {
    Mirror { name: Option<String> },
    PhoneOnly,
}

#[uniffi::remote(Enum)]
enum Suppression {
    None,
    Rnnoise,
    DeepFilterNet,
}

#[uniffi::remote(Enum)]
enum VoiceRuntime {
    Off,
    Active,
    Bypassed,
    Recovering,
}

#[uniffi::remote(Enum)]
enum InputSensitivity {
    Off,
    Automatic,
    Manual(i8),
}

#[uniffi::remote(Record)]
struct VoiceSettings {
    suppression: Suppression,
    auto_level: bool,
    eq_db: EqualizerGains,
    input_sensitivity: InputSensitivity,
}

#[uniffi::remote(Record)]
struct Endpoint {
    name: String,
    label: String,
}

#[uniffi::remote(Record)]
struct DesktopRoutes {
    voice_runtime: Option<VoiceRuntime>,
    platform: String,
    name: String,
    endpoints: Vec<Endpoint>,
    effective: Option<Route>,
    resolved: Option<String>,
    revision: u64,
    voice: VoiceSettings,
    suppression_algorithms: Vec<Suppression>,
}
