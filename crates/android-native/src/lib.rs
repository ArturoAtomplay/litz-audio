//! The API the Android app calls. UniFFI generates its Kotlin side.
#![cfg(any(target_os = "android", target_os = "linux"))]
#![warn(clippy::undocumented_unsafe_blocks)]
mod audio;
mod engine;
mod failure;
mod identity;
#[cfg(target_os = "android")]
mod oboe;
mod pairing;

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
