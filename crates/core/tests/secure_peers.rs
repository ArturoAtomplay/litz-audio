use litz_core::{
    media::{
        codec::Encoder,
        receiver::{CHUNK, Receiver},
    },
    protocol::*,
    session::{Session, Trust},
    transport::{self, Control, Result},
};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

fn identity() -> (Vec<u8>, Vec<u8>) {
    let id = rcgen::generate_simple_self_signed(vec!["litz.local".into()]).unwrap();
    (id.cert.der().to_vec(), id.signing_key.serialize_der())
}

#[tokio::test(flavor = "current_thread")]
async fn callback_burst_yields_to_transport_without_dropping_frames() -> Result<()> {
    let (cert, key) = identity();
    let (phone, phone_key) = identity();
    let trust = Arc::new(RwLock::new(
        Trust::default().approve(&phone, "Phone", 0, None).unwrap(),
    ));
    let server = transport::server_endpoint(
        transport::server(cert.clone(), key, trust)?,
        "127.0.0.1:0".parse()?,
    )?;
    let mut client = transport::client_endpoint("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(transport::client(cert, phone, phone_key)?);
    let (incoming, outgoing) = tokio::join!(
        async { server.accept().await.unwrap().await },
        client.connect(server.local_addr()?, "litz.local")?,
    );
    let incoming = incoming?;
    let outgoing = outgoing?;
    let receive = tokio::spawn(async move {
        let mut packets = Vec::with_capacity(5);
        for _ in 0..5 {
            packets.push(incoming.read_datagram().await?);
        }
        Ok::<_, quinn::ConnectionError>(packets)
    });
    let mut sender = transport::MediaSender::default();
    // Five codec frames can be delivered in one normal 2048-frame device burst.
    for sequence in 0..5u8 {
        if sequence == 2 {
            let packet = [sequence; 256];
            // A status/control request can cancel the yield between frames.
            // Retrying the retained packet must neither lose nor duplicate it.
            tokio::select! {
                biased;
                result = sender.send(&outgoing, &packet) => {
                    panic!("third burst frame should yield before submission: {result:?}");
                }
                _ = std::future::ready(()) => {}
            }
        }
        assert!(
            sender.send(&outgoing, &[sequence; 256]).await?,
            "dropped burst frame {sequence}"
        );
    }
    let packets = tokio::time::timeout(Duration::from_secs(2), receive).await???;
    assert_eq!(packets.len(), 5);
    for (sequence, packet) in packets.iter().enumerate() {
        assert_eq!(packet.as_ref(), &[sequence as u8; 256]);
    }
    Ok(())
}
async fn connect(
    endpoint: &quinn::Endpoint,
    addr: std::net::SocketAddr,
) -> Result<(quinn::Connection, Control)> {
    let c = endpoint.connect(addr, "litz.local")?.await?;
    let control = Control::new(c.open_bi().await?);
    Ok((c, control))
}
async fn open_session(control: &mut Control) -> Result<Message> {
    control.write(&Record::new(1, Message::GetState {})).await?;
    Ok(control.read().await?.message)
}

#[tokio::test(flavor = "current_thread")]
async fn secure_duplex_pairing_pinning_and_busy() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), exercise()).await?
}
async fn exercise() -> Result<()> {
    let (cert, key) = identity();
    let (phone, phone_key) = identity();
    let trust = Arc::new(RwLock::new(
        Trust::default().approve(&phone, "Phone", 0, None).unwrap(),
    ));
    let session = Arc::new(tokio::sync::Mutex::new(Session::new(0)));
    // Tracks whether any connection currently holds the session.
    let occupied = Arc::new(tokio::sync::Mutex::new(false));
    let server = transport::server_endpoint(
        transport::server(cert.clone(), key, trust.clone())?,
        "127.0.0.1:0".parse()?,
    )?;
    let addr = server.local_addr()?;
    let listener = server.clone();
    let shared = session.clone();
    let shared_trust = trust.clone();
    let accept = tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        // The fixture bounds incoming handshakes just like a production acceptor.
        while let Some(incoming) = listener.accept().await {
            if workers.len() >= 4 {
                incoming.refuse();
                continue;
            }
            let session = shared.clone();
            let trust = shared_trust.clone();
            let occupied = occupied.clone();
            workers.spawn(async move {
                let Ok(Ok(c)) = tokio::time::timeout(Duration::from_secs(3), incoming).await else {
                    return;
                };
                let result = async {
                    let mut control = Control::new(c.accept_bi().await?);
                    let request = control.read().await?;
                    assert!(matches!(request.message, Message::GetState {}));
                    let cert = transport::peer_certificate(&c)?;
                    // Only an explicitly pinned certificate authenticates; there
                    // is no token or self-declared identity path.
                    let trusted = trust.read().unwrap().contains(&cert);
                    if !trusted {
                        control
                            .write(&Record::new(
                                1,
                                Message::Error {
                                    code: Code::Unauthorized,
                                },
                            ))
                            .await?;
                        control.send.finish()?;
                        let _ = control.send.stopped().await;
                        return Ok(());
                    }
                    // Enforce single-owner semantics: reject a second connection
                    // while one already holds the session, mirroring hub.acquire.
                    {
                        let mut in_use = occupied.lock().await;
                        if *in_use {
                            control
                                .write(&Record::new(1, Message::Error { code: Code::Busy }))
                                .await?;
                            control.send.finish()?;
                            let _ = control.send.stopped().await;
                            return Ok(());
                        }
                        *in_use = true;
                    }
                    let result = serve(&c, &mut control, &session).await;
                    *occupied.lock().await = false;
                    *session.lock().await = Session::new(0);
                    result
                }
                .await;
                if result.is_err() {
                    c.close(1u8.into(), b"fixture error");
                }
            });
            while workers.try_join_next().is_some() {}
        }
        workers.shutdown().await;
    });
    let mut client = transport::client_endpoint("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(transport::client(
        cert.clone(),
        phone.clone(),
        phone_key.clone(),
    )?);

    let (c, mut control) = connect(&client, addr).await?;
    assert!(matches!(
        open_session(&mut control).await?,
        Message::Status { .. }
    ));
    assert!(trust.read().unwrap().contains(&phone));
    // An unpinned certificate is refused: TLS 1.3 client-auth failure surfaces
    // as a post-handshake close, not a failed local connect.
    let (stranger, stranger_key) = identity();
    let mut stranger_client = transport::client_endpoint("127.0.0.1:0".parse()?)?;
    stranger_client.set_default_client_config(transport::client(
        cert.clone(),
        stranger,
        stranger_key,
    )?);
    let stranger_connection = stranger_client.connect(addr, "litz.local")?.await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stranger_connection.closed())
            .await
            .is_ok()
    );
    control
        .write(&Record::new(3, Message::Capabilities { desktop: None }))
        .await?;
    assert!(matches!(
        control.read().await?.message,
        Message::Capabilities { desktop: None }
    ));
    let (second, mut second_control) = connect(&client, addr).await?;
    assert!(matches!(
        open_session(&mut second_control).await?,
        Message::Error { code: Code::Busy }
    ));
    second.close(0u8.into(), b"done");
    let mic = Config {
        stream: Stream::Mic,
        epoch: 1,
    };
    let playback = Config {
        stream: Stream::Playback,
        epoch: 1,
    };
    for config in [mic, playback] {
        control
            .write(&Record::new(
                config.index() as u32 + 2,
                Message::Start(config),
            ))
            .await?;
        assert!(
            matches!(control.read().await?.message,Message::Status { running, .. } if running[config.index()] == Some(config))
        );
    }
    let mut encoder = Encoder::try_new(mic)?;
    let mut sender = transport::MediaSender::default();
    let mut receiver = Receiver::try_new(playback)?;
    let mut packet = [0; MAX_DATAGRAM];
    let mut pcm = [0.0; FRAME * 2];
    let mut chunk = [0.0; CHUNK * 2];
    let signal: [f32; FRAME] = std::array::from_fn(|i| (i as f32 * 0.13).sin() * 0.1);
    let start = tokio::time::Instant::now();
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = 0;
    let mut energy = 0.0f64;
    let mut ticks = 0;
    while ticks < 220 {
        tokio::select! {
            _ = clock.tick() => {
                ticks += 1;
                let size = encoder.encode(&signal,&mut packet,c.max_datagram_size().unwrap_or(0))?;
                sender.send(&c,&packet[..size]).await?;
                for out in pcm.as_chunks_mut::<{ CHUNK * 2 }>().0 {
                    receiver.render(start.elapsed().as_secs_f64(), &mut chunk)?;
                    out.copy_from_slice(&chunk);
                }
                energy += pcm.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            }
            packet = c.read_datagram() => {
                receiver.push(&packet?,start.elapsed().as_secs_f64())?; received += 1;
            }
        }
    }
    assert!(received > 180 && energy > 10.0);
    control
        .write(&Record::new(
            9,
            Message::Stop {
                stream: Stream::Mic,
            },
        ))
        .await?;
    assert!(matches!(
        control.read().await?.message,
        Message::Status { .. }
    ));
    c.close(0u8.into(), b"done");
    server.close(0u8.into(), b"done");
    accept.abort();
    Ok(())
}

async fn serve(
    c: &quinn::Connection,
    control: &mut Control,
    session: &tokio::sync::Mutex<Session>,
) -> Result<()> {
    control
        .write(&Record::new(
            1,
            Message::Status {
                revision: 0,
                running: [None; 2],
            },
        ))
        .await?;
    let mic = Config {
        stream: Stream::Mic,
        epoch: 1,
    };
    let playback = Config {
        stream: Stream::Playback,
        epoch: 1,
    };
    let mut receiver = Receiver::try_new(mic)?;
    let mut encoder = Encoder::try_new(playback)?;
    let mut sender = transport::MediaSender::default();
    let signal: [f32; FRAME * 2] = std::array::from_fn(|i| (i as f32 * 0.07).sin() * 0.1);
    let mut packet = [0; MAX_DATAGRAM];
    let mut out = [0.0; CHUNK];
    let mut energy = 0.0;
    let start = tokio::time::Instant::now();
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            request = control.read() => {
                let request = request?;
                let mut owner = session.lock().await;
                let message = match request.message {
                    Message::Capabilities { .. } => Message::Capabilities { desktop: None },
                    Message::Start(config) => { owner.start(config).map_err(|_| "start")?; Message::Status { revision:0,running:owner.running } }
                    Message::Stop { stream } => {
                        assert!(energy > 10.0); owner.stop(stream);
                        receiver.clear(); Message::Status { revision:0,running:owner.running }
                    }
                    _ => Message::Error { code:Code::Unsupported },
                };
                drop(owner); control.write(&Record::new(request.request_id,message)).await?;
            }
            packet = c.read_datagram() => {
                let packet = packet?;
                if session.lock().await.running[0].is_some() { receiver.push(&packet,start.elapsed().as_secs_f64())?; }
            }
            _ = clock.tick() => {
                let owner = session.lock().await;
                if owner.running[0].is_some() {
                    for _ in 0..FRAME / CHUNK {
                        receiver.render(start.elapsed().as_secs_f64(), &mut out)?;
                        energy += out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
                    }
                }
                if owner.running[1].is_some() {
                    let size = encoder.encode(&signal,&mut packet,c.max_datagram_size().unwrap_or(0))?;
                    sender.send(c,&packet[..size]).await?;
                }
            }
        }
    }
}
