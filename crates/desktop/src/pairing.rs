use crate::identity::{self, Identity};
use litz_core::protocol::Message;
use litz_core::session::Trust;
use litz_core::transport;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

struct Owner {
    certificate: Vec<u8>,
    name: String,
    connection: quinn::Connection,
}
#[derive(Default)]
struct Lease {
    generation: u64,
    owner: Option<Owner>,
}
impl Lease {
    fn held_by(&self, connection: &quinn::Connection) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|o| o.connection.stable_id() == connection.stable_id())
    }
    /// Every change of owner starts a new generation, so a phone that read an
    /// older one cannot take over by accident.
    fn hand_to(&mut self, owner: Option<Owner>) -> Option<Owner> {
        self.generation += 1;
        std::mem::replace(&mut self.owner, owner)
    }
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub struct Hub {
    identity: Arc<Mutex<Identity>>,
    trust: Arc<RwLock<Trust>>,
    path: PathBuf,
    lease: Mutex<Lease>,
    attempts: Mutex<(Instant, u32)>,
    persistence: Arc<tokio::sync::Semaphore>,
    approval: Arc<tokio::sync::Semaphore>,
    failed: AtomicBool,
    connections: Mutex<Vec<(Vec<u8>, quinn::Connection)>>,
}
impl Hub {
    pub fn new(
        identity: Arc<Mutex<Identity>>,
        trust: Arc<RwLock<Trust>>,
        path: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            trust,
            path,
            lease: Mutex::new(Lease::default()),
            attempts: Mutex::new((Instant::now(), 0)),
            persistence: Arc::new(tokio::sync::Semaphore::new(1)),
            approval: Arc::new(tokio::sync::Semaphore::new(1)),
            failed: AtomicBool::new(false),
            connections: Mutex::new(Vec::new()),
        })
    }
    /// One phone may be awaiting an approval prompt; extra requests during that
    /// window are refused, and prompt spam is rate limited.
    pub fn begin_pairing(&self) -> transport::Result<tokio::sync::OwnedSemaphorePermit> {
        let mut attempts = self.attempts.lock().unwrap();
        if attempts.0.elapsed() >= Duration::from_secs(30) {
            *attempts = (Instant::now(), 0);
        }
        if attempts.1 >= 5 {
            return Err("pairing rate limited; try again shortly".into());
        }
        attempts.1 += 1;
        drop(attempts);
        self.approval
            .clone()
            .try_acquire_owned()
            .map_err(|_| "another pairing request is being reviewed".into())
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub fn register(&self, connection: &quinn::Connection) -> transport::Result<()> {
        let cert = transport::peer_certificate(connection)?;
        let mut connections = self.connections.lock().unwrap();
        if self.failed() || connections.len() >= 16 {
            return Err("connection registry unavailable".into());
        }
        connections.push((cert, connection.clone()));
        Ok(())
    }
    pub fn unregister(&self, connection: &quinn::Connection) {
        self.connections
            .lock()
            .unwrap()
            .retain(|(_, c)| c.stable_id() != connection.stable_id());
    }
    pub fn trusted(&self, cert: &[u8]) -> bool {
        !self.failed() && self.trust.read().unwrap().contains(cert)
    }
    pub fn trust(&self) -> Trust {
        self.trust.read().unwrap().clone()
    }
    pub fn certificate(&self) -> Vec<u8> {
        self.identity.lock().unwrap().certificate.clone()
    }
    /// (certificate, name, last_seen) of the phone a new pairing would replace.
    pub fn replacement(&self) -> Option<(Vec<u8>, String, u64)> {
        let trust = self.trust.read().unwrap();
        let phone = trust.least_recent()?;
        Some((
            phone.certificate.clone(),
            phone.name.clone(),
            phone.last_seen,
        ))
    }
    pub async fn approve(
        self: &Arc<Self>,
        cert: &[u8],
        name: &str,
        replace: Option<Vec<u8>>,
    ) -> transport::Result<()> {
        let (cert, name) = (cert.to_vec(), name.to_owned());
        let revoked = replace.clone();
        self.persist(
            move |trust| {
                trust
                    .approve(&cert, &name, now(), replace.as_deref())
                    .map_err(|_| "device registry full or invalid")
            },
            revoked,
            identity::save,
        )
        .await
    }
    pub async fn forget(self: &Arc<Self>, cert: &[u8]) -> transport::Result<()> {
        let cert = cert.to_vec();
        let revoked = Some(cert.clone());
        self.persist(
            move |trust| Ok(trust.forget(&cert)),
            revoked,
            identity::save,
        )
        .await
    }
    pub async fn touch(self: &Arc<Self>, cert: &[u8]) -> transport::Result<()> {
        if self.trust.read().unwrap().touched(cert, now()).is_none() {
            return Ok(());
        }
        let cert = cert.to_vec();
        self.persist(
            move |trust| Ok(trust.touched(&cert, now()).unwrap_or_else(|| trust.clone())),
            None,
            identity::save,
        )
        .await
    }
    async fn persist<F, S>(
        self: &Arc<Self>,
        update: F,
        revoke: Option<Vec<u8>>,
        save: S,
    ) -> transport::Result<()>
    where
        F: FnOnce(&Trust) -> Result<Trust, &'static str> + Send + 'static,
        S: FnOnce(&std::path::Path, &Identity) -> Result<(), Box<dyn std::error::Error>>
            + Send
            + 'static,
    {
        let permit = self
            .persistence
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "device registry closed")?;
        let hub = self.clone();
        tokio::task::spawn_blocking(move || -> transport::Result<()> {
            let _permit = permit;
            if hub.failed() {
                return Err("identity storage requires restart".into());
            }
            let mut next = hub.identity.lock().unwrap().clone();
            next.trust = update(&next.trust)?;
            if let Err(error) = save(&hub.path, &next) {
                if error.is::<identity::DurabilityUncertain>() {
                    hub.failed.store(true, Ordering::Release);
                    *hub.trust.write().unwrap() = Trust::default();
                    for (_, connection) in hub.connections.lock().unwrap().iter() {
                        connection.close(0x22u32.into(), b"identity storage requires restart");
                    }
                }
                return Err(error.to_string().into());
            }
            let mut identity = hub.identity.lock().unwrap();
            *hub.trust.write().unwrap() = next.trust.clone();
            *identity = next;
            drop(identity);
            if let Some(cert) = revoke {
                hub.revoke(&cert);
            }
            Ok(())
        })
        .await?
    }
    fn revoke(&self, cert: &[u8]) {
        for (peer, connection) in self.connections.lock().unwrap().iter() {
            if peer == cert {
                connection.close(0x21u32.into(), b"forgotten; pair again");
            }
        }
        let mut lease = self.lease.lock().unwrap();
        if lease.owner.as_ref().is_some_and(|o| o.certificate == cert)
            && let Some(owner) = lease.hand_to(None)
        {
            owner
                .connection
                .close(0x21u32.into(), b"forgotten; pair again");
        }
    }
    fn state(lease: &Lease, connection: &quinn::Connection) -> Message {
        let owned = lease.held_by(connection);
        let peer = transport::peer_certificate(connection).ok();
        let stale_self = !owned
            && lease
                .owner
                .as_ref()
                .is_some_and(|o| peer.as_deref() == Some(o.certificate.as_slice()));
        Message::Ownership {
            generation: lease.generation,
            owner: lease
                .owner
                .as_ref()
                .filter(|_| !stale_self)
                .map(|o| o.name.clone()),
            owned,
        }
    }
    pub fn status(&self, connection: &quinn::Connection) -> Message {
        Self::state(&self.lease.lock().unwrap(), connection)
    }
    pub fn acquire(
        &self,
        cert: &[u8],
        connection: &quinn::Connection,
        expected: Option<u64>,
    ) -> transport::Result<Message> {
        // Same lock order as revocation: trust cannot disappear between checking
        // authorization and publishing an owner.
        let identity = self.identity.lock().unwrap();
        if self.failed() || !identity.trust.contains(cert) {
            return Err("forgotten; pair again".into());
        }
        let mut lease = self.lease.lock().unwrap();
        let stale = expected.is_some_and(|generation| generation != lease.generation);
        let another_phone = lease.owner.as_ref().is_some_and(|o| o.certificate != cert);
        if lease.held_by(connection) || stale || (another_phone && expected.is_none()) {
            return Ok(Self::state(&lease, connection));
        }
        let name = identity
            .trust
            .phones
            .iter()
            .find(|p| p.certificate == cert)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Phone".into());
        let previous = lease.hand_to(Some(Owner {
            certificate: cert.to_vec(),
            name,
            connection: connection.clone(),
        }));
        if let Some(previous) = previous {
            previous
                .connection
                .close(0x20u32.into(), b"audio moved to another phone");
        }
        Ok(Self::state(&lease, connection))
    }
    pub fn release(&self, connection: &quinn::Connection) {
        let mut lease = self.lease.lock().unwrap();
        if lease.held_by(connection) {
            lease.hand_to(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> (Vec<u8>, Vec<u8>) {
        let identity = rcgen::generate_simple_self_signed(vec!["litz.local".into()]).unwrap();
        (
            identity.cert.der().to_vec(),
            identity.signing_key.serialize_der(),
        )
    }
    #[tokio::test]
    async fn two_saved_phones_require_explicit_current_generation_handoff() -> transport::Result<()>
    {
        let (cert, key) = id();
        let (one, one_key) = id();
        let (two, two_key) = id();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory =
            std::env::temp_dir().join(format!("litz-lease-{:016x}", u64::from_ne_bytes(nonce)));
        std::fs::create_dir(&directory)?;
        let path = directory.join("identity.json");
        let identity = Arc::new(Mutex::new(Identity {
            registry_version: 1,
            certificate: cert.clone(),
            private_key: key.clone(),
            trust: Trust::default(),
            bind: "127.0.0.1:0".parse()?,
        }));
        let trust = Arc::new(RwLock::new(Trust::default()));
        let hub = Hub::new(identity.clone(), trust.clone(), path.clone());
        hub.approve(&one, "Phone one", None).await?;
        hub.approve(&two, "Phone two", None).await?;
        hub.approve(&one, "Duplicate", None).await?;
        assert_eq!(trust.read().unwrap().phones.len(), 2);
        let server = transport::server_endpoint(
            transport::server(cert.clone(), key, trust)?,
            "127.0.0.1:0".parse()?,
        )?;
        let mut a = transport::client_endpoint("127.0.0.1:0".parse()?)?;
        a.set_default_client_config(transport::client(cert.clone(), one.clone(), one_key)?);
        let mut b = transport::client_endpoint("127.0.0.1:0".parse()?)?;
        b.set_default_client_config(transport::client(cert, two.clone(), two_key)?);
        let (client_one, server_one) =
            tokio::join!(a.connect(server.local_addr()?, "litz.local")?, async {
                server.accept().await.unwrap().await
            });
        let _client_one = client_one?;
        let server_one = server_one?;
        hub.register(&server_one)?;
        let (client_two, server_two) =
            tokio::join!(b.connect(server.local_addr()?, "litz.local")?, async {
                server.accept().await.unwrap().await
            });
        let _client_two = client_two?;
        let server_two = server_two?;
        hub.register(&server_two)?;
        assert!(matches!(
            hub.acquire(&one, &server_one, None)?,
            Message::Ownership {
                generation: 1,
                owned: true,
                ..
            }
        ));
        assert!(matches!(
            hub.acquire(&two, &server_two, None)?,
            Message::Ownership {
                generation: 1,
                owned: false,
                ..
            }
        ));
        assert!(server_one.close_reason().is_none());
        assert!(matches!(
            hub.acquire(&two, &server_two, Some(0))?,
            Message::Ownership { owned: false, .. }
        ));
        assert!(matches!(
            hub.acquire(&two, &server_two, Some(1))?,
            Message::Ownership {
                generation: 2,
                owned: true,
                ..
            }
        ));
        hub.release(&server_one); // displaced worker cleanup cannot clear new owner
        assert!(matches!(
            hub.status(&server_two),
            Message::Ownership {
                generation: 2,
                owned: true,
                ..
            }
        ));
        assert!(hub.trusted(&one) && hub.trusted(&two));
        let (replacement_client, replacement_server) =
            tokio::join!(b.connect(server.local_addr()?, "litz.local")?, async {
                server.accept().await.unwrap().await
            });
        let _replacement_client = replacement_client?;
        let replacement_server = replacement_server?;
        hub.register(&replacement_server)?;
        assert!(matches!(
            hub.status(&replacement_server),
            Message::Ownership {
                owner: None,
                owned: false,
                ..
            }
        ));
        assert!(matches!(
            hub.acquire(&two, &replacement_server, None)?,
            Message::Ownership {
                generation: 3,
                owned: true,
                ..
            }
        ));
        hub.release(&server_two);
        assert!(matches!(
            hub.status(&replacement_server),
            Message::Ownership { owned: true, .. }
        ));
        let (idle_client, idle_server) =
            tokio::join!(b.connect(server.local_addr()?, "litz.local")?, async {
                server.accept().await.unwrap().await
            });
        let _idle_client = idle_client?;
        let idle_server = idle_server?;
        hub.register(&idle_server)?;
        assert!(idle_server.close_reason().is_none());
        hub.forget(&two).await?;
        assert!(idle_server.close_reason().is_some());
        assert!(replacement_server.close_reason().is_some());
        assert!(!hub.trusted(&two));
        assert!(hub.acquire(&two, &server_two, None).is_err());
        assert!(matches!(
            hub.status(&server_one),
            Message::Ownership { owner: None, .. }
        ));
        let restored: Identity =
            serde_json::from_slice(&crate::platform::unprotect(&std::fs::read(&path)?)?)?;
        assert!(restored.trust.contains(&one) && !restored.trust.contains(&two));
        let failed = Hub::new(
            identity,
            Arc::new(RwLock::new(restored.trust)),
            directory.join("missing/identity.json"),
        );
        assert!(failed.approve(&two, "Phone two", None).await.is_err());
        assert!(!failed.trusted(&two));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let write_hub = hub.clone();
        let peer = two.clone();
        let writer = tokio::spawn(async move {
            write_hub
                .persist(
                    move |trust| trust.approve(&peer, "Two", 0, None).map_err(|_| "approve"),
                    None,
                    move |path, identity| {
                        let _ = started_tx.send(());
                        resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        identity::save(path, identity)
                    },
                )
                .await
        });
        started_rx.await?;
        assert!(hub.trusted(&one));
        assert!(!hub.trusted(&two));
        let queued_hub = hub.clone();
        let queued_peer = two.clone();
        let queued =
            tokio::spawn(async move { queued_hub.approve(&queued_peer, "Queued", None).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!queued.is_finished());
        resume_tx.send(())?;
        writer.await??;
        queued.await??;
        assert!(hub.trusted(&two));
        assert!(
            hub.persist(
                {
                    let two = two.clone();
                    move |trust| Ok(trust.forget(&two))
                },
                None,
                |path, identity| {
                    identity::save(path, identity)?;
                    Err(Box::new(identity::DurabilityUncertain(
                        std::io::Error::other("injected sync failure"),
                    )))
                },
            )
            .await
            .is_err()
        );
        assert!(hub.failed());
        assert!(!hub.trusted(&one) && !hub.trusted(&two));
        assert!(hub.approve(&two, "Retry", None).await.is_err());
        std::fs::remove_file(path)?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
