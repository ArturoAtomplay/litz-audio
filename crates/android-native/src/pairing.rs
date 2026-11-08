use crate::Identity;
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

pub struct Pairing {
    status: Arc<Mutex<String>>,
    cancel: Arc<Notify>,
    worker: Option<JoinHandle<()>>,
}

impl Pairing {
    pub fn start(
        identity: Identity,
        address: SocketAddr,
        expected: String,
        name: String,
    ) -> Result<Self> {
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
            return Err("invalid pairing candidate or identity".into());
        }
        let status = Arc::new(Mutex::new(
            serde_json::json!({"state":"connecting"}).to_string(),
        ));
        let cancel = Arc::new(Notify::new());
        let worker_status = status.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("litz-pairing".into())
            .spawn(move || {
                let result = (|| -> Result<String> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(pair(
                        identity,
                        address,
                        expected,
                        name,
                        worker_status.clone(),
                        worker_cancel,
                    ))
                })();
                if let Ok(mut state) = worker_status.lock() {
                    *state = result.unwrap_or_else(|e| {
                        serde_json::json!({"state":"error","message":e.to_string()}).to_string()
                    });
                }
            })?;
        Ok(Self {
            status,
            cancel,
            worker: Some(worker),
        })
    }
    pub fn status(&self) -> Result<String> {
        Ok(self
            .status
            .lock()
            .map_err(|_| "pairing status unavailable")?
            .clone())
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
    status: Arc<Mutex<String>>,
    cancel: Arc<Notify>,
) -> Result<String> {
    let bind = if address.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let mut endpoint = transport::client_endpoint(bind.parse()?)?;
    let phone = identity.certificate.clone();
    endpoint.set_default_client_config(transport::provisional_client(
        identity.certificate,
        identity.private_key,
    )?);
    let connection = tokio::select! {
        _ = cancel.notified() => return Ok(serde_json::json!({"state":"cancelled"}).to_string()),
        result = tokio::time::timeout(Duration::from_secs(5), endpoint.connect(address, "litz.local")?) => result??,
    };
    let desktop = transport::peer_certificate(&connection)?;
    let actual = fingerprint(&desktop).to_string();
    if !expected.is_empty() && actual != expected.to_ascii_lowercase() {
        return Err("discovered computer identity changed".into());
    }
    let mut id = [0u8; 16];
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut id).map_err(|e| e.to_string())?;
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let mut control = Control::new(connection.open_bi().await?);
    control
        .write(&Record::new(
            1,
            Message::PairRequest {
                id: DeviceId(id),
                name,
                commitment: commitment(&nonce),
            },
        ))
        .await?;
    let reply = tokio::select! {
        _ = cancel.notified() => return Ok(serde_json::json!({"state":"cancelled"}).to_string()),
        reply = tokio::time::timeout(Duration::from_secs(10), control.read()) => reply??,
    };
    let Message::PairNonce {
        nonce: desktop_nonce,
    } = reply.message
    else {
        return Err("Pairing unavailable; try again shortly".into());
    };
    control
        .write(&Record::new(1, Message::PairReveal { nonce }))
        .await?;
    let code = verification_code(&phone, &desktop, &nonce, &desktop_nonce);
    let approval_wait = litz_core::protocol::APPROVAL_WINDOW + Duration::from_secs(10);
    let outcome = tokio::select! {
        _ = cancel.notified() => PairOutcome::Cancelled,
        result = tokio::time::timeout(approval_wait, async {
            if let Ok(mut state) = status.lock() { *state = serde_json::json!({"state":"reviewing","code":code}).to_string(); }
            let reply = control.read().await?;
            match reply {
                Record { request_id: 1, message: Message::PairResult { id: received, outcome }, .. } if received == DeviceId(id) => {
                    match outcome {
                        PairOutcome::Declined => return Err("The computer declined the pairing request".into()),
                        PairOutcome::Full => return Err("The computer declined: all its pairing slots are in use".into()),
                        _ => {}
                    }
                    Ok::<PairOutcome, transport::Error>(outcome)
                }
                _ => Err("Pairing unavailable; try again shortly".into()),
            }
        }) => result??,
    };
    connection.close(0u8.into(), b"pairing complete");
    Ok(match outcome {
        PairOutcome::Accepted => serde_json::json!({"state":"accepted","certificate":desktop,"endpoint":address.to_string(),"fingerprint":actual}).to_string(),
        PairOutcome::Cancelled => serde_json::json!({"state":"cancelled"}).to_string(),
        PairOutcome::Declined | PairOutcome::Full => {
            serde_json::json!({"state":"error","message":"The computer declined the pairing request"}).to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::session::Trust;
    use std::sync::RwLock;

    async fn manual_attempt(expected: String, outcome: PairOutcome) -> Result<String> {
        let generated = rcgen::generate_simple_self_signed(vec!["litz.local".into()])?;
        let desktop = generated.cert.der().to_vec();
        let server = transport::server_endpoint(
            transport::server_pairing(
                desktop,
                generated.signing_key.serialize_der(),
                Arc::new(RwLock::new(Trust::default())),
            )?,
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
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            pair(
                crate::fresh_identity()?,
                address,
                expected,
                "Test phone".into(),
                Arc::new(Mutex::new(String::new())),
                Arc::new(Notify::new()),
            ),
        )
        .await?;
        listener.abort();
        result
    }

    #[tokio::test(flavor = "current_thread")]
    async fn approval_pins_the_discovered_computer_only() -> Result<()> {
        let accepted = manual_attempt(String::new(), PairOutcome::Accepted).await?;
        let status: serde_json::Value = serde_json::from_str(&accepted)?;
        assert_eq!(status["state"], "accepted");
        assert_eq!(status["fingerprint"].as_str().map(str::len), Some(64));
        assert!(
            status["certificate"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
        let declined = manual_attempt(String::new(), PairOutcome::Declined)
            .await
            .unwrap_err();
        assert!(declined.to_string().contains("declined"));
        assert!(
            manual_attempt("0".repeat(64), PairOutcome::Accepted)
                .await
                .is_err()
        );
        Ok(())
    }
}
