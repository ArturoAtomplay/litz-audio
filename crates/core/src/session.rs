use crate::protocol::{Code, Config, Fingerprint, Stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn fingerprint(bytes: &[u8]) -> Fingerprint {
    Fingerprint(Sha256::digest(bytes).into())
}

pub fn commitment(nonce: &[u8; 32]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"litz pairing commitment")
        .chain_update(nonce)
        .finalize()
        .into()
}

pub fn verification_code(
    phone: &[u8],
    desktop: &[u8],
    phone_nonce: &[u8; 32],
    desktop_nonce: &[u8; 32],
) -> String {
    let mut hash = Sha256::new().chain_update(b"litz pairing code");
    for part in [phone, desktop, phone_nonce, desktop_nonce] {
        hash.update((part.len() as u32).to_be_bytes());
        hash.update(part);
    }
    let digest = hash.finalize();
    let value = u32::from_be_bytes(digest[..4].try_into().expect("four bytes")) % 1_000_000;
    format!("{:03} {:03}", value / 1000, value % 1000)
}

pub const MAX_PHONES: usize = 16;
/// Recording every connection would rewrite the registry constantly.
const TOUCH_INTERVAL: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    pub phones: Vec<SavedPeer>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPeer {
    pub certificate: Vec<u8>,
    pub name: String,
    /// Unix seconds.
    #[serde(default)]
    pub last_seen: u64,
}
impl Trust {
    pub fn contains(&self, cert: &[u8]) -> bool {
        self.phones.iter().any(|known| known.certificate == cert)
    }

    pub fn least_recent(&self) -> Option<&SavedPeer> {
        if self.phones.len() < MAX_PHONES {
            return None;
        }
        self.phones.iter().min_by_key(|phone| phone.last_seen)
    }

    /// `replace` must name the phone the person agreed to forget when full.
    pub fn approve(
        &self,
        cert: &[u8],
        name: &str,
        now: u64,
        replace: Option<&[u8]>,
    ) -> Result<Self, Code> {
        if cert.is_empty() || cert.len() > 4096 || name.is_empty() || name.chars().count() > 64 {
            return Err(Code::Invalid);
        }
        if self.contains(cert) {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        if next.phones.len() >= MAX_PHONES {
            let replace = replace.ok_or(Code::Busy)?;
            let before = next.phones.len();
            next.phones.retain(|phone| phone.certificate != replace);
            if next.phones.len() == before {
                return Err(Code::Busy);
            }
        }
        next.phones.push(SavedPeer {
            certificate: cert.to_vec(),
            name: name.into(),
            last_seen: now,
        });
        Ok(next)
    }

    pub fn forget(&self, cert: &[u8]) -> Self {
        let mut next = self.clone();
        next.phones.retain(|phone| phone.certificate != cert);
        next
    }

    /// `None` when the stored time is recent enough to keep.
    pub fn touched(&self, cert: &[u8], now: u64) -> Option<Self> {
        let index = self.phones.iter().position(|p| p.certificate == cert)?;
        if now.saturating_sub(self.phones[index].last_seen) < TOUCH_INTERVAL {
            return None;
        }
        let mut next = self.clone();
        next.phones[index].last_seen = now;
        Some(next)
    }
}

/// Per-connection stream bookkeeping. Epochs only move forward so a stale
/// Start from before a reconnect cannot resurrect an old stream.
#[derive(Debug, Default)]
pub struct Session {
    pub running: [Option<Config>; 2],
    pub revision: u64,
    epochs: [u32; 2],
}

impl Session {
    pub fn new(revision: u64) -> Self {
        Self {
            revision,
            ..Self::default()
        }
    }

    pub fn validate_start(&self, config: Config) -> Result<(), Code> {
        if !config.valid() {
            return Err(Code::Invalid);
        }
        let previous = self.epochs[config.index()];
        if previous != 0 && config.epoch.wrapping_sub(previous) as i32 <= 0 {
            return Err(Code::Invalid);
        }
        Ok(())
    }

    pub fn start(&mut self, config: Config) -> Result<(), Code> {
        self.validate_start(config)?;
        self.epochs[config.index()] = config.epoch;
        self.running[config.index()] = Some(config);
        Ok(())
    }

    pub fn stop(&mut self, stream: Stream) {
        self.running[stream.index()] = None;
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;

    #[test]
    fn registry_keeps_distinct_keys_even_with_duplicate_names() {
        let first = Trust::default().approve(&[1], "Pixel", 0, None).unwrap();
        let second = first.approve(&[2], "Pixel", 0, None).unwrap();
        assert!(second.contains(&[1]) && second.contains(&[2]));
        assert_eq!(second.phones.len(), 2);
        assert_eq!(
            second.approve(&[1], "Pixel", 0, None).unwrap().phones.len(),
            2
        );
        let restored: Trust =
            serde_json::from_slice(&serde_json::to_vec(&second).unwrap()).unwrap();
        assert_eq!(restored.phones[0].last_seen, 0);
        assert!(restored.contains(&[2]));
        assert!(!restored.contains(&[3]));
    }

    #[test]
    fn full_registry_replaces_only_the_phone_the_person_agreed_to() {
        let mut trust = Trust::default();
        for cert in 1..=MAX_PHONES as u8 {
            trust = trust
                .approve(&[cert], "Phone", 100 + u64::from(cert), None)
                .unwrap();
        }
        trust = trust.touched(&[1], 1_000_000).unwrap();
        assert_eq!(trust.least_recent().unwrap().certificate, [2]);
        assert_eq!(
            trust.approve(&[17], "Phone", 0, None).unwrap_err(),
            Code::Busy
        );
        assert_eq!(
            trust.approve(&[17], "Phone", 0, Some(&[99])).unwrap_err(),
            Code::Busy
        );
        let next = trust.approve(&[17], "Phone", 0, Some(&[2])).unwrap();
        assert!(next.contains(&[17]) && !next.contains(&[2]));
        assert_eq!(next.phones.len(), MAX_PHONES);
        assert_eq!(
            trust.approve(&[18], "", 0, None).unwrap_err(),
            Code::Invalid
        );
        assert!(trust.touched(&[1], 1_000_001).is_none());
        assert!(!trust.forget(&[1]).contains(&[1]));
    }

    #[test]
    fn verification_code_depends_on_every_input() {
        let code = verification_code(b"phone", b"desktop", &[1; 32], &[2; 32]);
        assert_eq!(code.len(), 7);
        assert_eq!(
            code,
            verification_code(b"phone", b"desktop", &[1; 32], &[2; 32])
        );
        assert_ne!(
            code,
            verification_code(b"phone", b"desktop", &[1; 32], &[3; 32])
        );
        assert_ne!(
            code,
            verification_code(b"phonedesktop", b"", &[1; 32], &[2; 32])
        );
        assert_ne!(commitment(&[1; 32]), commitment(&[2; 32]));
    }

    #[test]
    fn session_epochs_only_move_forward() {
        let mic = |epoch| Config {
            stream: Stream::Mic,
            epoch,
        };
        let mut session = Session::new(42);
        session.start(mic(10)).unwrap();
        session
            .start(Config {
                stream: Stream::Playback,
                epoch: 1,
            })
            .unwrap();
        assert_eq!(session.running[0], Some(mic(10)));
        assert_eq!(session.start(mic(10)), Err(Code::Invalid));
        assert_eq!(session.start(mic(0)), Err(Code::Invalid));
        session.stop(Stream::Mic);
        session.stop(Stream::Mic);
        assert_eq!(session.start(mic(9)), Err(Code::Invalid));
        session.start(mic(11)).unwrap();
    }
}
