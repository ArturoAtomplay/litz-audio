use serde::{Deserialize, Serialize};

pub mod types;
pub use types::*;

pub const VERSION: u8 = 1;
pub const ALPN: &[u8] = b"litz/1";
pub const PORT: u16 = 47165;
pub const HEADER: usize = 20;
pub const MAX_DATAGRAM: usize = 1100;
pub const MAX_CONTROL: usize = 65536;
pub const FRAME: usize = 480;
pub const APPROVAL_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// The mDNS host name a computer answers to. `id` is its certificate
/// fingerprint in hex; `None` when it is too short to be one.
pub fn mdns_host(id: &str) -> Option<String> {
    Some(format!("litz-{}.local", id.get(..16)?))
}

/// Why a connection was closed. The phone decides from these whether to
/// reconnect, stop, or pair again, so they are part of the wire protocol.
pub mod close {
    use quinn::VarInt;

    /// Ended normally; the phone may reconnect.
    pub const ENDED: VarInt = VarInt::from_u32(0);
    /// The session failed on the computer.
    pub const FAILED: VarInt = VarInt::from_u32(1);
    /// Another phone took the audio.
    pub const MOVED: VarInt = VarInt::from_u32(0x20);
    /// The computer no longer trusts this phone.
    pub const FORGOTTEN: VarInt = VarInt::from_u32(0x21);
    /// The computer could not save its registry and must restart.
    pub const STORAGE: VarInt = VarInt::from_u32(0x22);
    /// QUIC's transport error for the TLS alert no_application_protocol: the
    /// peer speaks another protocol version.
    pub const NO_COMMON_PROTOCOL: u64 = 0x178;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Mic,
    Playback,
}
impl Stream {
    pub const ALL: [Self; 2] = [Self::Mic, Self::Playback];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn channels(self) -> usize {
        match self {
            Self::Mic => 1,
            Self::Playback => 2,
        }
    }
    /// Opus bitrate ceiling and congestion floor, in bits per second.
    pub fn bitrates(self) -> (i32, i32) {
        match self {
            Self::Mic => (80_000, 40_000),
            Self::Playback => (192_000, 96_000),
        }
    }
    fn wire(self) -> u16 {
        self as u16 + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub stream: Stream,
    pub epoch: u32,
}
impl Config {
    pub fn valid(self) -> bool {
        self.epoch != 0
    }
    pub fn index(self) -> usize {
        self.stream.index()
    }
    pub fn channels(self) -> usize {
        self.stream.channels()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub config: Config,
    pub sequence: u32,
    pub timestamp: u64,
}
impl Header {
    pub fn write(self, out: &mut [u8]) -> Result<(), &'static str> {
        if out.len() < HEADER || !self.config.valid() {
            return Err("invalid header");
        }
        out[0] = VERSION;
        out[1] = 0;
        out[2..4].copy_from_slice(&self.config.stream.wire().to_be_bytes());
        out[4..8].copy_from_slice(&self.config.epoch.to_be_bytes());
        out[8..12].copy_from_slice(&self.sequence.to_be_bytes());
        out[12..20].copy_from_slice(&self.timestamp.to_be_bytes());
        Ok(())
    }
    pub fn parse(
        bytes: &[u8],
        config: Config,
        limit: usize,
    ) -> Result<(Self, &[u8]), &'static str> {
        if bytes.len() <= HEADER
            || bytes.len() > limit.min(MAX_DATAGRAM)
            || bytes[0] != VERSION
            || bytes[1] != 0
            || !config.valid()
            || bytes[2..4] != config.stream.wire().to_be_bytes()
            || bytes[4..8] != config.epoch.to_be_bytes()
        {
            return Err("invalid media packet");
        }
        Ok((
            Self {
                config,
                sequence: u32::from_be_bytes(bytes[8..12].try_into().expect("checked header size")),
                timestamp: u64::from_be_bytes(
                    bytes[12..20].try_into().expect("checked header size"),
                ),
            },
            &bytes[HEADER..],
        ))
    }
}

/// Wire schema: `{"version":1,"request_id":7,"message":{"type":"Start",
/// "payload":{"stream":"mic","epoch":1}}}`. Media is always 48 kHz in
/// 480-frame Opus packets, mono mic and stereo playback. Status acknowledges
/// the exact applied Start configuration. A trusted phone opens with GetState.
///
/// Unknown fields are skipped, so a later build may add one without breaking
/// this reader; the record as a whole stays within MAX_CONTROL. Every bounded
/// field is checked while it is parsed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub version: u8,
    pub request_id: u32,
    pub message: Message,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Capabilities {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desktop: Option<DesktopRoutes>,
    },
    GetState {},
    ApplySettings {
        expected_revision: u64,
        route: Route,
    },
    ApplyVoice {
        expected_revision: u64,
        voice: VoiceSettings,
    },
    Start(Config),
    Stop {
        stream: Stream,
    },
    Status {
        revision: u64,
        running: [Option<Config>; 2],
    },
    Error {
        code: Code,
    },
    Unpair {},
    /// Pairing is commit-then-reveal: the phone commits to its nonce before it
    /// sees the computer's, so neither side can steer the verification code.
    PairRequest {
        id: DeviceId,
        #[serde(deserialize_with = "bounds::display_name")]
        name: String,
        commitment: [u8; 32],
    },
    PairNonce {
        nonce: [u8; 32],
    },
    PairReveal {
        nonce: [u8; 32],
    },
    PairResult {
        id: DeviceId,
        outcome: PairOutcome,
    },
    GetOwnership {},
    Acquire {
        expected_generation: Option<u64>,
    },
    Release {},
    Ownership {
        generation: u64,
        #[serde(deserialize_with = "bounds::optional_text::<_, 64>")]
        owner: Option<String>,
        owned: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairOutcome {
    Accepted,
    Declined,
    Cancelled,
    Full,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    Mirror {
        #[serde(deserialize_with = "bounds::optional_text::<_, 256>")]
        name: Option<String>,
    },
    PhoneOnly,
}
/// Stable backend names only. The inventory is a bounded snapshot, not authority
/// to use a route: ApplySettings must validate it again against live objects.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(deserialize_with = "bounds::text::<_, 256>")]
    pub name: String,
    #[serde(deserialize_with = "bounds::text::<_, 256>")]
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopRoutes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_runtime: Option<VoiceRuntime>,
    #[serde(deserialize_with = "bounds::text::<_, 32>")]
    pub platform: String,
    #[serde(deserialize_with = "bounds::text::<_, 256>")]
    pub name: String,
    #[serde(deserialize_with = "bounds::at_most::<_, _, 32>")]
    pub endpoints: Vec<Endpoint>,
    pub effective: Option<Route>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "bounds::optional_text::<_, 256>"
    )]
    pub resolved: Option<String>,
    pub revision: u64,
    #[serde(default)]
    pub voice: VoiceSettings,
    #[serde(default, deserialize_with = "bounds::at_most::<_, _, 8>")]
    pub suppression_algorithms: Vec<Suppression>,
}
/// Actual effects state, independent of the saved user preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceRuntime {
    Off,
    Active,
    Bypassed,
    Recovering,
}
/// Computer-wide microphone preferences. Integer EQ gains prevent NaN/Infinity
/// and keep the bounded wire contract independent of DSP library configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub suppression: Suppression,
    pub auto_level: bool,
    #[serde(deserialize_with = "bounds::gains")]
    pub eq_db: [i8; 3],
    #[serde(
        skip_serializing_if = "InputSensitivity::is_off",
        deserialize_with = "bounds::sensitivity"
    )]
    pub input_sensitivity: InputSensitivity,
}
impl VoiceSettings {
    pub fn enabled(&self) -> bool {
        self.suppression != Suppression::None
            || self.auto_level
            || self.eq_db != [0; 3]
            || !self.input_sensitivity.is_off()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSensitivity {
    #[default]
    Off,
    Automatic,
    Manual(i8),
}
impl InputSensitivity {
    fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suppression {
    #[default]
    None,
    Rnnoise,
    DeepFilterNet,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Code {
    Busy,
    Unauthorized,
    Unsupported,
    Invalid,
    RevisionConflict,
    RouteConflict,
}
impl Record {
    pub fn new(request_id: u32, message: Message) -> Self {
        Self {
            message,
            request_id,
            version: VERSION,
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let json = serde_json::to_vec(self).map_err(|_| "control JSON")?;
        if json.len() > MAX_CONTROL {
            return Err("control too large");
        }
        let mut out = Vec::with_capacity(json.len() + 4);
        out.extend_from_slice(&(json.len() as u32).to_be_bytes());
        out.extend(json);
        Ok(out)
    }
    pub fn decode(json: &[u8]) -> Result<Self, &'static str> {
        if json.len() > MAX_CONTROL {
            return Err("control too large");
        }
        let record: Self = serde_json::from_slice(json).map_err(|_| "control JSON")?;
        if record.version != VERSION {
            return Err("protocol version");
        }
        Ok(record)
    }
}

mod bounds {
    use super::InputSensitivity;
    use serde::{Deserialize, Deserializer, de::Error};

    fn check<E: Error>(text: String, max: usize) -> Result<String, E> {
        if text.is_empty() || text.chars().count() > max {
            return Err(E::custom(format!("text must be 1 to {max} characters")));
        }
        Ok(text)
    }

    pub fn text<'de, D: Deserializer<'de>, const MAX: usize>(d: D) -> Result<String, D::Error> {
        check(String::deserialize(d)?, MAX)
    }

    pub fn optional_text<'de, D: Deserializer<'de>, const MAX: usize>(
        d: D,
    ) -> Result<Option<String>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| check(text, MAX))
            .transpose()
    }

    /// Shown verbatim in the computer's approval prompt.
    pub fn display_name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let name = check(String::deserialize(d)?, 64)?;
        if name.chars().any(char::is_control) {
            return Err(D::Error::custom("name contains control characters"));
        }
        Ok(name)
    }

    pub fn at_most<'de, D: Deserializer<'de>, T: Deserialize<'de>, const MAX: usize>(
        d: D,
    ) -> Result<Vec<T>, D::Error> {
        let items = Vec::deserialize(d)?;
        if items.len() > MAX {
            return Err(D::Error::custom(format!("at most {MAX} entries")));
        }
        Ok(items)
    }

    pub fn gains<'de, D: Deserializer<'de>>(d: D) -> Result<[i8; 3], D::Error> {
        let gains = <[i8; 3]>::deserialize(d)?;
        if gains.iter().any(|gain| !(-12..=12).contains(gain)) {
            return Err(D::Error::custom("EQ gains must be within ±12 dB"));
        }
        Ok(gains)
    }

    pub fn sensitivity<'de, D: Deserializer<'de>>(d: D) -> Result<InputSensitivity, D::Error> {
        let value = InputSensitivity::deserialize(d)?;
        if matches!(value, InputSensitivity::Manual(db) if !(-60..=-20).contains(&db)) {
            return Err(D::Error::custom("sensitivity must be -60 to -20 dB"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mdns_host_is_the_start_of_the_fingerprint() {
        assert_eq!(
            mdns_host(&"0123456789abcdef".repeat(4)).as_deref(),
            Some("litz-0123456789abcdef.local")
        );
        assert_eq!(mdns_host("abc"), None);
    }
    #[test]
    fn close_codes_keep_their_wire_values() {
        let codes = [
            close::ENDED,
            close::FAILED,
            close::MOVED,
            close::FORGOTTEN,
            close::STORAGE,
        ];
        assert_eq!(codes.map(u64::from), [0, 1, 0x20, 0x21, 0x22]);
        assert_eq!(close::NO_COMMON_PROTOCOL, 0x100 + 120);
    }
    #[test]
    fn pairing_messages_roundtrip() {
        for message in [
            Message::PairRequest {
                id: DeviceId([1; 16]),
                name: "Phone".into(),
                commitment: [7; 32],
            },
            Message::PairNonce { nonce: [8; 32] },
            Message::PairReveal { nonce: [9; 32] },
            Message::PairResult {
                id: DeviceId([1; 16]),
                outcome: PairOutcome::Declined,
            },
        ] {
            let wire = Record::new(1, message).encode().unwrap();
            assert!(Record::decode(&wire[4..]).is_ok());
        }
    }
    #[test]
    fn unknown_fields_are_skipped_at_every_level() {
        for json in [
            r#"{"version":1,"request_id":1,"message":{"type":"GetState","payload":{}},"extra":0}"#,
            r#"{"version":1,"request_id":1,"message":{"type":"GetState","payload":{},"extra":0}}"#,
            r#"{"version":1,"request_id":1,"message":{"type":"Stop","payload":{"stream":"mic","extra":0}}}"#,
            r#"{"version":1,"request_id":1,"message":{"type":"Start","payload":{"stream":"mic","epoch":1,"extra":0}}}"#,
            r#"{"version":1,"request_id":1,"message":{"type":"ApplyVoice","payload":{"expected_revision":1,"voice":{"auto_level":true,"extra":{"nested":[1,2]}}}}}"#,
        ] {
            assert!(Record::decode(json.as_bytes()).is_ok(), "{json}");
        }
        let oversized = format!(
            r#"{{"version":1,"request_id":1,"message":{{"type":"GetState","payload":{{}}}},"extra":"{}"}}"#,
            "x".repeat(MAX_CONTROL)
        );
        assert!(Record::decode(oversized.as_bytes()).is_err());
    }
    #[test]
    fn pairing_names_are_short_and_printable() {
        let request = |name: &str| {
            format!(
                r#"{{"version":1,"request_id":1,"message":{{"type":"PairRequest","payload":{{"id":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"name":{},"commitment":[{}]}}}}}}"#,
                serde_json::to_string(name).unwrap(),
                vec!["0"; 32].join(",")
            )
        };
        assert!(Record::decode(request("Pixel 8").as_bytes()).is_ok());
        for name in ["", "line\nbreak", &"x".repeat(65)] {
            assert!(
                Record::decode(request(name).as_bytes()).is_err(),
                "{name:?}"
            );
        }
    }
    #[test]
    fn desktop_routes_golden_wire_format() {
        let record = Record::new(
            9,
            Message::Capabilities {
                desktop: Some(DesktopRoutes {
                    voice_runtime: None,
                    platform: "Linux".into(),
                    name: "Studio".into(),
                    endpoints: vec![Endpoint {
                        name: "alsa_output.usb".into(),
                        label: "USB headset".into(),
                    }],
                    effective: Some(Route::Mirror { name: None }),
                    resolved: None,
                    revision: 3,
                    voice: VoiceSettings::default(),
                    suppression_algorithms: vec![],
                }),
            },
        );
        let wire = record.encode().unwrap();
        assert_eq!(&wire[4..], br#"{"version":1,"request_id":9,"message":{"type":"Capabilities","payload":{"desktop":{"platform":"Linux","name":"Studio","endpoints":[{"name":"alsa_output.usb","label":"USB headset"}],"effective":{"Mirror":{"name":null}},"revision":3,"voice":{"suppression":"none","auto_level":false,"eq_db":[0,0,0]},"suppression_algorithms":[]}}}}"#);
        assert!(Record::decode(&wire[4..]).is_ok());
        let mut oversized: serde_json::Value = serde_json::from_slice(&wire[4..]).unwrap();
        oversized["message"]["payload"]["desktop"]["endpoints"] = serde_json::json!(vec![
            serde_json::json!({"name":"stable", "label":"Output"});
            33
        ]);
        assert!(Record::decode(&serde_json::to_vec(&oversized).unwrap()).is_err());
        oversized["message"]["payload"]["desktop"]["endpoints"] =
            serde_json::json!([{"name":"x".repeat(257),"label":"Output"}]);
        assert!(Record::decode(&serde_json::to_vec(&oversized).unwrap()).is_err());
    }
    #[test]
    fn input_sensitivity_is_optional_and_bounded() {
        let plain = r#"{"suppression":"none","auto_level":false,"eq_db":[0,0,0]}"#;
        let settings: VoiceSettings = serde_json::from_str(plain).unwrap();
        assert_eq!(settings.input_sensitivity, InputSensitivity::Off);
        assert_eq!(serde_json::to_string(&settings).unwrap(), plain);
        let manual = VoiceSettings {
            input_sensitivity: InputSensitivity::Manual(-40),
            ..settings
        };
        assert!(manual.enabled());
        assert_eq!(
            serde_json::from_str::<VoiceSettings>(&serde_json::to_string(&manual).unwrap())
                .unwrap(),
            manual
        );
        let out_of_range = r#"{"input_sensitivity":{"manual":-10}}"#;
        assert!(serde_json::from_str::<VoiceSettings>(out_of_range).is_err());
        assert!(serde_json::from_str::<VoiceSettings>(r#"{"eq_db":[13,0,0]}"#).is_err());
    }
    #[test]
    fn apply_route_golden_wire_format() {
        let record = Record::new(
            4,
            Message::ApplySettings {
                expected_revision: 3,
                route: Route::Mirror {
                    name: Some("alsa_output.usb".into()),
                },
            },
        );
        let wire = record.encode().unwrap();
        assert_eq!(&wire[4..], br#"{"version":1,"request_id":4,"message":{"type":"ApplySettings","payload":{"expected_revision":3,"route":{"Mirror":{"name":"alsa_output.usb"}}}}}"#);
        assert!(Record::decode(&wire[4..]).is_ok());
    }
    #[test]
    fn start_golden_names_the_stream() {
        let config = Config {
            stream: Stream::Playback,
            epoch: 3,
        };
        let wire = Record::new(2, Message::Start(config)).encode().unwrap();
        assert_eq!(
            &wire[4..],
            br#"{"version":1,"request_id":2,"message":{"type":"Start","payload":{"stream":"playback","epoch":3}}}"#
        );
        assert!(
            Record::decode(
                br#"{"version":1,"request_id":2,"message":{"type":"Start","payload":{"stream":2,"epoch":3}}}"#
            )
            .is_err()
        );
    }
    #[test]
    fn framing_and_validation() {
        let record = Record::new(7, Message::GetState {});
        assert_eq!(
            &record.encode().unwrap()[4..],
            br#"{"version":1,"request_id":7,"message":{"type":"GetState","payload":{}}}"#
        );
        assert!(Record::decode(&vec![b' '; MAX_CONTROL + 1]).is_err());
        assert!(
            Record::decode(
                br#"{"version":2,"request_id":7,"message":{"type":"GetState","payload":{}}}"#
            )
            .is_err()
        );
        let config = Config {
            stream: Stream::Mic,
            epoch: 1,
        };
        let mut packet = [0; HEADER + 1];
        Header {
            config,
            sequence: u32::MAX,
            timestamp: 480,
        }
        .write(&mut packet)
        .unwrap();
        assert_eq!(
            Header::parse(&packet, config, 1100).unwrap().0.sequence,
            u32::MAX
        );
        packet[1] = 1;
        assert!(Header::parse(&packet, config, 1100).is_err());
        packet[1] = 0;
        packet[7] = 2;
        assert!(Header::parse(&packet, config, 1100).is_err());
        assert!(Header::parse(&packet[..HEADER], config, 1100).is_err());
        let playback = Config {
            stream: Stream::Playback,
            ..config
        };
        assert!(Header::parse(&packet, playback, 1100).is_err());
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            let _ = Record::decode(&bytes);
            let config = Config { stream: Stream::Mic, epoch: 1 };
            let _ = Header::parse(&bytes, config, MAX_DATAGRAM);
        }

        #[test]
        fn json_shaped_input_never_panics(text in r#"[\[\]{}",:\\a-z0-9 ]{0,512}"#) {
            let _ = Record::decode(text.as_bytes());
        }

        #[test]
        fn protocol_messages_proptest(
            request_id in any::<u32>(),
            playback in any::<bool>(),
            epoch in 1u32..=1000u32,
            eq0 in -12i8..=12i8,
            eq1 in -12i8..=12i8,
            eq2 in -12i8..=12i8,
            msg_kind in 0u8..=4u8
        ) {
            let stream = if playback { Stream::Playback } else { Stream::Mic };
            let msg = match msg_kind {
                0 => Message::GetState {},
                1 => Message::Start(Config { stream, epoch }),
                2 => Message::Stop { stream },
                3 => Message::Unpair {},
                _ => Message::ApplyVoice {
                    expected_revision: 100,
                    voice: VoiceSettings {
                        eq_db: [eq0, eq1, eq2],
                        auto_level: false,
                        ..VoiceSettings::default()
                    },
                },
            };
            let record = Record::new(request_id, msg);
            let wire = record.encode().unwrap();
            prop_assert!(wire.len() >= 4);
            let decoded = Record::decode(&wire[4..]).unwrap();
            prop_assert_eq!(decoded.request_id, request_id);
            prop_assert_eq!(decoded.version, VERSION);
        }
    }
}
