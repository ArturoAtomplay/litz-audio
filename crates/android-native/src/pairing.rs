use crate::Identity;
use crate::failure::Failure;
use litz_core::{
    protocol::{DeviceId, Message, PairOutcome, Record},
    session::{commitment, fingerprint, verification_code},
    transport::{self, Control, Result},
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum PairingState {
    Connecting,
    /// Both screens show `code` while the computer asks the person.
    Reviewing {
        code: String,
    },
    /// The computer accepted. Nothing is pinned until the person confirms that
    /// their computer showed `code`.
    Confirming {
        code: String,
    },
    Accepted {
        certificate: Vec<u8>,
        endpoint: SocketAddr,
        fingerprint: String,
    },
    Cancelled,
    Failed {
        failure: Failure,
    },
}

#[derive(uniffi::Object)]
pub struct Pairing {
    state: Arc<Mutex<PairingState>>,
    cancel: Arc<Notify>,
    confirm: Arc<Notify>,
    worker: Option<JoinHandle<()>>,
}

#[uniffi::export]
impl Pairing {
    #[uniffi::constructor]
    pub fn new(
        identity: Identity,
        address: SocketAddr,
        expected: String,
        name: String,
    ) -> std::result::Result<Arc<Self>, Failure> {
        let invalid = |detail: &str| Failure::Internal {
            detail: detail.into(),
        };
        if address.ip().is_unspecified()
            || address.ip().is_multicast()
            || address.port() < 1024
            || (!expected.is_empty()
                && (expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit())))
            || name.is_empty()
            || name.chars().count() > 64
            || identity.certificate.is_empty()
            || identity.private_key.is_empty()
        {
            return Err(invalid("invalid pairing candidate or identity"));
        }
        let state = Arc::new(Mutex::new(PairingState::Connecting));
        let cancel = Arc::new(Notify::new());
        let confirm = Arc::new(Notify::new());
        let worker_state = state.clone();
        let worker_cancel = cancel.clone();
        let worker_confirm = confirm.clone();
        let worker = std::thread::Builder::new()
            .name("litz-pairing".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(transport::Error::from)
                    .and_then(|runtime| {
                        runtime.block_on(pair(
                            identity,
                            address,
                            expected,
                            name,
                            worker_state.clone(),
                            worker_cancel,
                            worker_confirm,
                        ))
                    });
                *worker_state.lock().unwrap() = result.unwrap_or_else(|e| PairingState::Failed {
                    failure: Failure::of(e.as_ref()),
                });
            })
            .map_err(|error| Failure::of(&error))?;
        Ok(Arc::new(Self {
            state,
            cancel,
            confirm,
            worker: Some(worker),
        }))
    }

    pub fn state(&self) -> PairingState {
        self.state.lock().unwrap().clone()
    }

    /// The person saw the same code on their computer.
    pub fn confirm(&self) {
        if matches!(*self.state.lock().unwrap(), PairingState::Confirming { .. }) {
            self.confirm.notify_one();
        }
    }

    pub fn cancel(&self) {
        self.cancel.notify_one();
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn pair(
    identity: Identity,
    address: SocketAddr,
    expected: String,
    name: String,
    state: Arc<Mutex<PairingState>>,
    cancel: Arc<Notify>,
    confirm: Arc<Notify>,
) -> Result<PairingState> {
    let bind = if address.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let mut endpoint = transport::client_endpoint(bind.parse()?)?;
    endpoint.set_default_client_config(transport::provisional_client(
        identity.certificate.clone(),
        identity.private_key,
    )?);
    let result = tokio::select! {
        _ = cancel.notified() => Ok(PairingState::Cancelled),
        result = exchange(&endpoint, &identity.certificate, address, &expected, name, &state, &confirm) => result,
    };
    // The runtime ends with this call; without waiting, CLOSE is never sent and
    // the computer keeps prompting for a phone that already gave up.
    endpoint.close(0u8.into(), b"pairing ended");
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    result
}

async fn exchange(
    endpoint: &quinn::Endpoint,
    phone: &[u8],
    address: SocketAddr,
    expected: &str,
    name: String,
    state: &Mutex<PairingState>,
    confirm: &Notify,
) -> Result<PairingState> {
    let connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(address, "litz.local")?,
    )
    .await
    .map_err(|_| Failure::ConnectTimeout)??;
    let desktop = transport::peer_certificate(&connection)?;
    let actual = fingerprint(&desktop).to_string();
    if !expected.is_empty() && actual != expected.to_ascii_lowercase() {
        return Err(Failure::IdentityChanged.into());
    }
    let mut id = [0u8; 16];
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut id).map_err(|e| e.to_string())?;
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let id = DeviceId(id);
    let mut control = Control::new(connection.open_bi().await?);
    let request = Message::PairRequest {
        id,
        name,
        commitment: commitment(&nonce),
    };
    control.write(&Record::new(1, request)).await?;
    let reply = tokio::time::timeout(Duration::from_secs(10), control.read()).await??;
    let Message::PairNonce {
        nonce: desktop_nonce,
    } = reply.message
    else {
        return Err(Failure::PairingUnavailable.into());
    };
    control
        .write(&Record::new(1, Message::PairReveal { nonce }))
        .await?;
    let code = verification_code(phone, &desktop, &nonce, &desktop_nonce);
    *state.lock().unwrap() = PairingState::Reviewing { code: code.clone() };
    let approval = litz_core::protocol::APPROVAL_WINDOW + Duration::from_secs(10);
    let reply = tokio::time::timeout(approval, control.read()).await??;
    let outcome = match reply {
        Record {
            request_id: 1,
            message:
                Message::PairResult {
                    id: received,
                    outcome,
                },
            ..
        } if received == id => outcome,
        _ => return Err(Failure::PairingUnavailable.into()),
    };
    match outcome {
        PairOutcome::Accepted => {
            *state.lock().unwrap() = PairingState::Confirming { code };
            confirm.notified().await;
            Ok(PairingState::Accepted {
                certificate: desktop,
                endpoint: address,
                fingerprint: actual,
            })
        }
        PairOutcome::Declined => Err(Failure::PairingDeclined.into()),
        PairOutcome::Full => Err(Failure::PairingFull.into()),
        PairOutcome::Unavailable => Err(Failure::PromptUnavailable.into()),
        PairOutcome::Cancelled => Err(Failure::PairingUnavailable.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A computer that answers `outcome` without asking anyone.
    fn computer(outcome: PairOutcome) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
        let generated = rcgen::generate_simple_self_signed(vec!["litz.local".into()])?;
        let desktop = generated.cert.der().to_vec();
        let server = transport::server_endpoint(
            transport::server(desktop, generated.signing_key.serialize_der())?,
            "127.0.0.1:0".parse()?,
        )?;
        let address = server.local_addr()?;
        let listener = tokio::spawn(async move {
            let connection = server
                .accept()
                .await
                .ok_or("no incoming connection")?
                .await?;
            let mut control = Control::new(connection.accept_bi().await?);
            let request = control.read().await?;
            let Message::PairRequest {
                id,
                name,
                commitment: committed,
            } = request.message
            else {
                return Err::<(), transport::Error>("pairing request missing".into());
            };
            if name.is_empty() {
                return Err("pairing name missing".into());
            }
            control
                .write(&Record::new(1, Message::PairNonce { nonce: [3; 32] }))
                .await?;
            let Message::PairReveal { nonce } = control.read().await?.message else {
                return Err("pairing reveal missing".into());
            };
            if commitment(&nonce) != committed {
                return Err("commitment mismatch".into());
            }
            control
                .write(&Record::new(1, Message::PairResult { id, outcome }))
                .await?;
            control.send.finish()?;
            let _ = tokio::time::timeout(Duration::from_secs(2), control.send.stopped()).await;
            Ok(())
        });
        Ok((address, listener))
    }

    /// The person confirms as soon as the computer accepts.
    async fn manual_attempt(expected: String, outcome: PairOutcome) -> Result<PairingState> {
        let (address, listener) = computer(outcome)?;
        let confirm = Arc::new(Notify::new());
        confirm.notify_one();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            pair(
                crate::create_identity()?,
                address,
                expected,
                "Test phone".into(),
                Arc::new(Mutex::new(PairingState::Connecting)),
                Arc::new(Notify::new()),
                confirm,
            ),
        )
        .await?;
        listener.abort();
        result
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_computer_that_accepts_by_itself_is_not_pinned() -> Result<()> {
        let (address, listener) = computer(PairOutcome::Accepted)?;
        let pairing = Pairing::new(
            crate::create_identity()?,
            address,
            String::new(),
            "Test phone".into(),
        )?;
        pairing.confirm();
        let reached = async {
            while !matches!(pairing.state(), PairingState::Confirming { .. }) {
                assert!(!matches!(pairing.state(), PairingState::Accepted { .. }));
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), reached).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(matches!(pairing.state(), PairingState::Confirming { .. }));
        pairing.confirm();
        let pinned = async {
            while !matches!(pairing.state(), PairingState::Accepted { .. }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), pinned).await?;
        listener.abort();
        drop(pairing);
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn approval_pins_the_discovered_computer_only() -> Result<()> {
        let accepted = manual_attempt(String::new(), PairOutcome::Accepted).await?;
        let PairingState::Accepted {
            certificate,
            fingerprint,
            ..
        } = accepted
        else {
            panic!("pairing was not accepted: {accepted:?}");
        };
        assert_eq!(fingerprint.len(), 64);
        assert!(!certificate.is_empty());
        let declined = manual_attempt(String::new(), PairOutcome::Declined)
            .await
            .unwrap_err();
        assert_eq!(Failure::of(declined.as_ref()), Failure::PairingDeclined);
        let unavailable = manual_attempt(String::new(), PairOutcome::Unavailable)
            .await
            .unwrap_err();
        assert_eq!(
            Failure::of(unavailable.as_ref()),
            Failure::PromptUnavailable
        );
        assert!(
            manual_attempt("0".repeat(64), PairOutcome::Accepted)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_during_review_tells_the_computer() -> Result<()> {
        let generated = rcgen::generate_simple_self_signed(vec!["litz.local".into()])?;
        let server = transport::server_endpoint(
            transport::server(
                generated.cert.der().to_vec(),
                generated.signing_key.serialize_der(),
            )?,
            "127.0.0.1:0".parse()?,
        )?;
        let pairing = Pairing::new(
            crate::create_identity()?,
            server.local_addr()?,
            String::new(),
            "Test phone".into(),
        )?;
        let connection = server.accept().await.ok_or("no phone")?.await?;
        let mut control = Control::new(connection.accept_bi().await?);
        control.read().await?;
        control
            .write(&Record::new(1, Message::PairNonce { nonce: [3; 32] }))
            .await?;
        control.read().await?;
        pairing.cancel();
        let closed = tokio::time::timeout(Duration::from_secs(2), connection.closed()).await?;
        assert!(matches!(
            closed,
            quinn::ConnectionError::ApplicationClosed(_)
        ));
        drop(pairing);
        Ok(())
    }
}
