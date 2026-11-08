use litz_core::{protocol::PORT, session::Trust};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub registry_version: u8,
    pub certificate: Vec<u8>,
    pub private_key: Vec<u8>,
    pub trust: Trust,
    pub bind: std::net::SocketAddr,
}
pub fn directory() -> Result<PathBuf> {
    Ok(crate::platform::directory()?)
}
#[derive(Debug)]
pub struct DurabilityUncertain(pub(crate) std::io::Error);
impl std::fmt::Display for DurabilityUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "identity replaced but directory sync failed; restart required: {}",
            self.0
        )
    }
}
impl std::error::Error for DurabilityUncertain {}

/// Same-directory rename plus fsync: token consumption and the phone pin form one
/// transaction, never two separately updated files. No token/private key logging.
pub fn save(path: &Path, identity: &Identity) -> Result<()> {
    save_with_sync(path, identity, crate::platform::sync_parent)
}

fn save_with_sync(
    path: &Path,
    identity: &Identity,
    sync: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let encoded = serde_json::to_vec(identity)?;
    if encoded.len() > 32768 {
        return Err("identity registry is full".into());
    }
    let parent = path.parent().ok_or("identity parent")?;
    crate::platform::write_atomic(path, &crate::platform::protect(&encoded)?)?;
    sync(parent).map_err(DurabilityUncertain)?;
    Ok(())
}
/// Commit credentials before delivery, holding the agent lock through both.
pub fn agent_lock(dir: &Path) -> Result<fs::File> {
    let lock = crate::platform::private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("agent.lock"))?;
    if lock.try_lock().is_err() {
        return Err(
            "Stop the running Litz service before changing installation credentials".into(),
        );
    }
    Ok(lock)
}

pub fn load_or_create(path: &Path) -> Result<Identity> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !crate::platform::private_file(&metadata) || metadata.len() > 65536 {
                return Err("identity must be a private regular file".into());
            }
            let plain = crate::platform::unprotect(&crate::platform::read_limited(path, 65536)?)?;
            if plain.len() > 32768 {
                return Err("identity too large".into());
            }
            let identity: Identity = serde_json::from_slice(&plain)?;
            if identity.registry_version != 1 {
                return Err("unsupported device registry version".into());
            }
            Ok(identity)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let dir = path.parent().ok_or("identity parent")?;
            crate::platform::create_directory(dir)?;
            let generated = rcgen::generate_simple_self_signed(vec!["litz.local".into()])?;
            let identity = Identity {
                registry_version: 1,
                certificate: generated.cert.der().to_vec(),
                private_key: generated.signing_key.serialize_der(),
                trust: Trust::default(),
                bind: std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, PORT)),
            };
            save(path, &identity)?;
            Ok(identity)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn pairing_commit_is_private_and_durable() {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let dir =
            std::env::temp_dir().join(format!("litz-trust-{:016x}", u64::from_ne_bytes(nonce)));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("identity.json");
        let mut identity = Identity {
            registry_version: 1,
            certificate: vec![1],
            private_key: vec![2],
            bind: "127.0.0.1:47165".parse().unwrap(),
            trust: Trust::default(),
        };
        save(&path, &identity).unwrap();
        identity.trust = identity.trust.approve(&[3], "Phone", 0, None).unwrap();
        save(&path, &identity).unwrap();
        let reloaded: Identity =
            serde_json::from_slice(&crate::platform::unprotect(&fs::read(&path).unwrap()).unwrap())
                .unwrap();
        assert!(reloaded.trust.contains(&[3]));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(!reloaded.trust.contains(&[4]));
        identity.trust = identity.trust.approve(&[5], "Phone", 0, None).unwrap();
        let error = save_with_sync(&path, &identity, |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        })
        .unwrap_err();
        assert!(error.is::<DurabilityUncertain>());
        let replaced: Identity =
            serde_json::from_slice(&crate::platform::unprotect(&fs::read(&path).unwrap()).unwrap())
                .unwrap();
        assert!(replaced.trust.contains(&[5]));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
