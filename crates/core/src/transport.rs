use crate::protocol::{ALPN, MAX_CONTROL, MAX_DATAGRAM, Record};
use rustls::{
    RootCertStore,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{sync::Arc, time::Duration};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub fn server_endpoint(
    config: quinn::ServerConfig,
    address: std::net::SocketAddr,
) -> Result<quinn::Endpoint> {
    endpoint(Some(config), address)
}

pub fn client_endpoint(address: std::net::SocketAddr) -> Result<quinn::Endpoint> {
    endpoint(None, address)
}
fn endpoint(
    server: Option<quinn::ServerConfig>,
    address: std::net::SocketAddr,
) -> Result<quinn::Endpoint> {
    let mut config = quinn::EndpointConfig::default();
    config.supported_versions(vec![1]);
    Ok(quinn::Endpoint::new(
        config,
        server,
        std::net::UdpSocket::bind(address)?,
        Arc::new(quinn::TokioRuntime),
    )?)
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn algorithms() -> rustls::crypto::WebPkiSupportedAlgorithms {
    rustls::crypto::ring::default_provider().signature_verification_algorithms
}

fn limits() -> Arc<quinn::TransportConfig> {
    let mut c = quinn::TransportConfig::default();
    c.max_concurrent_bidi_streams(1u8.into())
        .max_concurrent_uni_streams(0u8.into());
    c.datagram_receive_buffer_size(Some(32 * MAX_DATAGRAM));
    c.datagram_send_buffer_size(2 * MAX_DATAGRAM);
    c.stream_receive_window((MAX_CONTROL as u32 + 4).into());
    c.receive_window((2 * MAX_CONTROL as u32).into());
    c.send_window(2 * MAX_CONTROL as u64);
    c.max_idle_timeout(Some(
        Duration::from_secs(10)
            .try_into()
            .expect("ten seconds fits a QUIC idle timeout"),
    ));
    c.keep_alive_interval(Some(Duration::from_secs(3)));
    Arc::new(c)
}

/// Accepts any single self-signed phone certificate and requires TLS proof of
/// its private key, because a phone may ask to pair at any time. This grants
/// nothing: the daemon authorizes the pinned certificate after the handshake,
/// in `serve_connection`, and again on every request.
#[derive(Debug)]
struct PhoneVerifier;
impl ClientCertVerifier for PhoneVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        if !chain.is_empty() {
            return Err(rustls::Error::General("unauthorized phone".into()));
        }
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone().into_owned())?;
        rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?
            .verify_client_cert(cert, chain, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms())
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        algorithms().supported_schemes()
    }
}

pub fn server(cert: Vec<u8>, key: Vec<u8>) -> Result<quinn::ServerConfig> {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(PhoneVerifier))
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivatePkcs8KeyDer::from(key).into(),
        )?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.max_early_data_size = 0;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
    ));
    config.transport_config(limits());
    Ok(config)
}

/// Discovery supplies an address only. This verifies a self-signed certificate
/// and TLS private-key possession; the caller must use only pairing operations
/// until local approval has durably pinned the exact returned certificate.
#[derive(Debug)]
struct ProvisionalDesktopVerifier;
impl ServerCertVerifier for ProvisionalDesktopVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if !chain.is_empty() || cert.len() > 4096 {
            return Err(rustls::Error::General(
                "invalid provisional certificate".into(),
            ));
        }
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone().into_owned())?;
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?
            .verify_server_cert(cert, chain, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms())
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        algorithms().supported_schemes()
    }
}

pub fn provisional_client(cert: Vec<u8>, key: Vec<u8>) -> Result<quinn::ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ProvisionalDesktopVerifier))
        .with_client_auth_cert(
            vec![CertificateDer::from(cert)],
            PrivatePkcs8KeyDer::from(key).into(),
        )?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    ));
    config.transport_config(limits());
    Ok(config)
}

pub fn client(desktop: Vec<u8>, cert: Vec<u8>, key: Vec<u8>) -> Result<quinn::ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(desktop))?;
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![CertificateDer::from(cert)],
            PrivatePkcs8KeyDer::from(key).into(),
        )?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    ));
    config.transport_config(limits());
    Ok(config)
}

pub fn peer_certificate(c: &quinn::Connection) -> Result<Vec<u8>> {
    let identity = c.peer_identity().ok_or("no phone identity")?;
    let chain = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| "identity type")?;
    if chain.len() != 1 {
        return Err("identity chain".into());
    }
    Ok(chain[0].to_vec())
}

/// One stream, one outstanding request; 32 incoming records/second, no pipelining.
pub struct Control {
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
    window: tokio::time::Instant,
    count: u32,
    input: Box<[u8]>,
    filled: usize,
    started: Option<tokio::time::Instant>,
}
impl Control {
    pub fn new((send, recv): (quinn::SendStream, quinn::RecvStream)) -> Self {
        Self {
            send,
            recv,
            window: tokio::time::Instant::now(),
            count: 0,
            input: vec![0; MAX_CONTROL + 4].into_boxed_slice(),
            filled: 0,
            started: None,
        }
    }
    pub async fn write(&mut self, record: &Record) -> Result<()> {
        let bytes = record.encode()?;
        tokio::time::timeout(Duration::from_secs(5), self.send.write_all(&bytes)).await??;
        Ok(())
    }
    pub async fn read(&mut self) -> Result<Record> {
        // Preserve partial input across select! cancellation (read_exact would
        // otherwise lose partial framing when media/destination demand wins).
        self.fill_to(4).await?;
        let size =
            u32::from_be_bytes(self.input[..4].try_into().expect("four-byte prefix")) as usize;
        if size == 0 || size > MAX_CONTROL {
            return Err("control length".into());
        }
        self.fill_to(size + 4).await?;
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = tokio::time::Instant::now();
            self.count = 0;
        }
        self.count += 1;
        if self.count > 32 {
            return Err("control rate".into());
        }
        let record = Record::decode(&self.input[4..size + 4])?;
        self.filled = 0;
        self.started = None;
        Ok(record)
    }
    async fn fill_to(&mut self, end: usize) -> Result<()> {
        while self.filled < end {
            let read = self.recv.read(&mut self.input[self.filled..end]);
            let n = if let Some(started) = self.started {
                tokio::time::timeout_at(started + Duration::from_secs(5), read).await??
            } else {
                read.await?
            }
            .ok_or("control closed")?;
            self.started.get_or_insert_with(tokio::time::Instant::now);
            self.filled += n;
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MediaSender {
    pending: [Option<bytes::Bytes>; 2],
}
impl MediaSender {
    /// At most two submitted frames, regardless of their encoded byte sizes. A slot
    /// becomes reusable when Quinn releases its shared Bytes, not on a guessed timer.
    pub async fn send(&mut self, connection: &quinn::Connection, packet: &[u8]) -> Result<bool> {
        if packet.len()
            > connection
                .max_datagram_size()
                .unwrap_or(0)
                .min(MAX_DATAGRAM)
        {
            return Err("path datagram limit".into());
        }
        // Capture callbacks can contain several codec frames. On a single-thread
        // runtime the connection driver cannot drain the first two until we
        // yield; treating that scheduling delay as congestion drops clean audio.
        if self
            .pending
            .iter()
            .all(|slot| slot.as_ref().is_some_and(|bytes| !bytes.is_unique()))
        {
            tokio::task::yield_now().await;
        }
        let Some(slot) = self
            .pending
            .iter_mut()
            .find(|slot| slot.as_ref().is_none_or(bytes::Bytes::is_unique))
        else {
            return Ok(false);
        };
        let bytes = bytes::Bytes::copy_from_slice(packet);
        connection.send_datagram(bytes.clone())?;
        *slot = Some(bytes);
        Ok(true)
    }
}
