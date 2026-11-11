//! The phone's own identity and the computers it has pinned. The app keeps
//! the serialized registry encrypted and never looks inside it.
use crate::failure::Failure;
use litz_core::session::{MAX_PHONES, fingerprint};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

const MAX_STORED: usize = 32000;

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

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SavedComputer {
    pub name: String,
    pub fingerprint: String,
    pub endpoint: SocketAddr,
}

#[derive(Serialize, Deserialize)]
struct Computer {
    name: String,
    certificate: Vec<u8>,
    endpoint: SocketAddr,
}

/// The stored form, unchanged since 0.1.0. `desktop` and `endpoint` are the
/// selected computer; an empty `desktop` means none is selected.
#[derive(Serialize, Deserialize)]
struct Registry {
    registry_version: u8,
    certificate: Vec<u8>,
    private_key: Vec<u8>,
    desktop: Vec<u8>,
    endpoint: SocketAddr,
    known_desktops: Vec<Computer>,
}

fn invalid(detail: &str) -> Failure {
    Failure::Internal {
        detail: detail.into(),
    }
}

impl Registry {
    fn open(stored: &[u8]) -> Result<Self, Failure> {
        let registry: Self = serde_json::from_slice(stored)
            .map_err(|_| invalid("saved computers are unreadable"))?;
        if registry.registry_version != 1 {
            return Err(invalid("unsupported saved-computer version"));
        }
        Ok(registry)
    }

    fn store(&self) -> Result<Vec<u8>, Failure> {
        let stored = serde_json::to_vec(self).map_err(|error| Failure::of(&error))?;
        if stored.len() > MAX_STORED {
            return Err(invalid("saved computer list is too large"));
        }
        Ok(stored)
    }

    fn selected(&self) -> Option<String> {
        (!self.desktop.is_empty()).then(|| fingerprint(&self.desktop).to_string())
    }
}

fn pin(computer: &Computer) -> String {
    fingerprint(&computer.certificate).to_string()
}

#[uniffi::export]
pub fn registry_create() -> Result<Vec<u8>, Failure> {
    let phone = create_identity()?;
    Registry {
        registry_version: 1,
        certificate: phone.certificate,
        private_key: phone.private_key,
        desktop: phone.desktop,
        endpoint: phone.endpoint,
        known_desktops: Vec::new(),
    }
    .store()
}

/// The identity the engine and pairing use, with the selected computer if any.
#[uniffi::export]
pub fn registry_phone(stored: Vec<u8>) -> Result<Identity, Failure> {
    let registry = Registry::open(&stored)?;
    Ok(Identity {
        desktop: registry.desktop,
        certificate: registry.certificate,
        private_key: registry.private_key,
        endpoint: registry.endpoint,
    })
}

#[uniffi::export]
pub fn registry_saved(stored: Vec<u8>) -> Result<Vec<SavedComputer>, Failure> {
    let computers = Registry::open(&stored)?.known_desktops;
    Ok(computers
        .iter()
        .map(|computer| SavedComputer {
            name: computer.name.clone(),
            fingerprint: pin(computer),
            endpoint: computer.endpoint,
        })
        .collect())
}

/// Fingerprint of the selected computer.
#[uniffi::export]
pub fn registry_selected(stored: Vec<u8>) -> Result<Option<String>, Failure> {
    Ok(Registry::open(&stored)?.selected())
}

/// Pins `certificate` and selects it.
#[uniffi::export]
pub fn registry_remember(
    stored: Vec<u8>,
    name: String,
    certificate: Vec<u8>,
    endpoint: SocketAddr,
) -> Result<Vec<u8>, Failure> {
    let mut registry = Registry::open(&stored)?;
    registry
        .known_desktops
        .retain(|known| known.certificate != certificate);
    if registry.known_desktops.len() >= MAX_PHONES {
        return Err(invalid("saved computer list is full"));
    }
    registry.known_desktops.push(Computer {
        name: name.chars().take(64).collect(),
        certificate: certificate.clone(),
        endpoint,
    });
    registry.desktop = certificate;
    registry.endpoint = endpoint;
    registry.store()
}

#[uniffi::export]
pub fn registry_select(stored: Vec<u8>, fingerprint: String) -> Result<Vec<u8>, Failure> {
    let mut registry = Registry::open(&stored)?;
    let computer = registry
        .known_desktops
        .iter()
        .find(|known| pin(known) == fingerprint)
        .ok_or_else(|| invalid("saved computer unavailable"))?;
    registry.desktop = computer.certificate.clone();
    registry.endpoint = computer.endpoint;
    registry.store()
}

#[uniffi::export]
pub fn registry_forget(stored: Vec<u8>, fingerprint: String) -> Result<Vec<u8>, Failure> {
    let mut registry = Registry::open(&stored)?;
    if registry.selected().as_deref() == Some(fingerprint.as_str()) {
        registry.desktop.clear();
    }
    registry
        .known_desktops
        .retain(|known| pin(known) != fingerprint);
    registry.store()
}

/// `None` when the computer is unknown or already at `endpoint`.
#[uniffi::export]
pub fn registry_update_address(
    stored: Vec<u8>,
    fingerprint: String,
    endpoint: SocketAddr,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut registry = Registry::open(&stored)?;
    let mut changed = false;
    for known in &mut registry.known_desktops {
        if pin(known) == fingerprint && known.endpoint != endpoint {
            known.endpoint = endpoint;
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    if registry.selected().as_deref() == Some(fingerprint.as_str()) {
        registry.endpoint = endpoint;
    }
    registry.store().map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_registry_written_by_0_1_0_still_opens() {
        let text = r#"{"desktop":[1,2,3],"certificate":[1,200],"private_key":[2],"endpoint":"192.0.2.1:47165","registry_version":1,"known_desktops":[{"name":"Studio","certificate":[1,2,3],"endpoint":"192.0.2.1:47165"}]}"#;
        let stored = text.as_bytes().to_vec();
        let phone = registry_phone(stored.clone()).unwrap();
        assert_eq!(phone.certificate, [1, 200]);
        assert_eq!(phone.desktop, [1, 2, 3]);
        assert_eq!(phone.endpoint, address("192.0.2.1:47165"));
        let saved = registry_saved(stored.clone()).unwrap();
        assert_eq!(saved[0].name, "Studio");
        assert_eq!(saved[0].fingerprint, fingerprint(&[1, 2, 3]).to_string());
        assert_eq!(
            registry_selected(stored.clone()).unwrap().as_deref(),
            Some(saved[0].fingerprint.as_str())
        );
        let rewritten = Registry::open(&stored).unwrap().store().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&rewritten).unwrap(),
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
        let later = text.replace(r#""registry_version":1"#, r#""registry_version":2"#);
        assert!(registry_phone(later.into_bytes()).is_err());
    }

    #[test]
    fn pins_follow_selection_addresses_and_forgetting() {
        let fresh = registry_create().unwrap();
        assert!(registry_saved(fresh.clone()).unwrap().is_empty());
        assert_eq!(registry_selected(fresh.clone()).unwrap(), None);
        let one =
            registry_remember(fresh, "One".into(), vec![1], address("192.0.2.1:47165")).unwrap();
        let two =
            registry_remember(one, "Two".into(), vec![2], address("192.0.2.2:47165")).unwrap();
        let again =
            registry_remember(two, "x".repeat(70), vec![1], address("192.0.2.3:47165")).unwrap();
        let saved = registry_saved(again.clone()).unwrap();
        assert_eq!(saved.len(), 2);
        let first = saved
            .iter()
            .find(|c| c.endpoint == address("192.0.2.3:47165"))
            .unwrap();
        assert_eq!(first.name.chars().count(), 64);
        let pin = fingerprint(&[2]).to_string();
        let selected = registry_select(again, pin.clone()).unwrap();
        assert_eq!(registry_phone(selected.clone()).unwrap().desktop, [2]);
        assert!(registry_select(selected.clone(), "unknown".into()).is_err());
        assert!(
            registry_update_address(selected.clone(), pin.clone(), address("192.0.2.2:47165"))
                .unwrap()
                .is_none()
        );
        let moved = registry_update_address(selected, pin.clone(), address("192.0.2.9:47165"))
            .unwrap()
            .unwrap();
        assert_eq!(
            registry_phone(moved.clone()).unwrap().endpoint,
            address("192.0.2.9:47165")
        );
        let forgotten = registry_forget(moved, pin).unwrap();
        assert_eq!(registry_selected(forgotten.clone()).unwrap(), None);
        assert_eq!(registry_saved(forgotten.clone()).unwrap().len(), 1);
        let mut full = forgotten;
        for certificate in 10..10 + MAX_PHONES as u8 - 1 {
            full = registry_remember(full, "N".into(), vec![certificate], address("192.0.2.1:1"))
                .unwrap();
        }
        assert!(registry_remember(full, "N".into(), vec![99], address("192.0.2.1:1")).is_err());
    }
}
