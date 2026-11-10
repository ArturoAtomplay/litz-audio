//! The API the Android app calls. UniFFI generates its Kotlin side.
#![cfg(any(target_os = "android", target_os = "linux"))]
#![warn(clippy::undocumented_unsafe_blocks)]
mod audio;
mod engine;
mod failure;
mod pairing;

use failure::Failure;
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

/// The phone's own certificate and key, and the computer it is pinned to.
#[derive(Clone, uniffi::Record)]
pub struct Identity {
    pub desktop: Vec<u8>,
    pub certificate: Vec<u8>,
    pub private_key: Vec<u8>,
    pub endpoint: SocketAddr,
}

/// A new phone identity, not yet paired with any computer.
#[uniffi::export]
pub fn create_identity() -> Result<Identity, Failure> {
    let phone = rcgen::generate_simple_self_signed(vec!["litz.local".into()])
        .map_err(|error| Failure::of(&error))?;
    Ok(Identity {
        desktop: Vec::new(),
        certificate: phone.cert.der().to_vec(),
        private_key: phone.signing_key.serialize_der(),
        endpoint: SocketAddr::from(([0, 0, 0, 0], litz_core::protocol::PORT)),
    })
}
