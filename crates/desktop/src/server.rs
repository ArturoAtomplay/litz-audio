use crate::identity::Identity;
use crate::pairing::Hub;
use crate::stream::Exit;
use litz_core::{
    pcm::PcmProducer,
    protocol::{
        Code, DesktopRoutes, DeviceId, Endpoint, Message, PairOutcome, Record, Route, Suppression,
        VoiceSettings,
    },
    transport::{self, Control},
    wake::Wake,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn computer_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.len() <= 63 && !name.chars().any(char::is_control))
        .unwrap_or_else(|| "Litz computer".into())
}
#[derive(Clone, Copy)]
pub struct Demand {
    pub frames: usize,
    pub at: Instant,
    pub reset: bool,
}
pub enum CaptureOperation {
    Active(bool, oneshot::Sender<bool>),
    Inventory(oneshot::Sender<Vec<Endpoint>>),
    Route(Route, oneshot::Sender<Option<CaptureRoute>>),
}
pub struct CaptureRoute {
    pub samples: rtrb::Consumer<crate::audio::capture::Captured>,
    pub fault: Arc<AtomicBool>,
    pub resolved: Option<String>,
}
pub type CaptureCommand = (CaptureOperation, tokio::sync::OwnedSemaphorePermit);
pub struct Ports {
    pub mic: PcmProducer,
    pub mic_available: Arc<AtomicBool>,
    pub capture: Option<rtrb::Consumer<crate::audio::capture::Captured>>,
    pub capture_fault: Arc<AtomicBool>,
    pub demand: rtrb::Consumer<Demand>,
    pub wake: Wake,
    pub reset: Arc<AtomicBool>,
    pub route: Option<Route>,
    pub resolved: Option<String>,
    pub revision: u64,
    pub voice: VoiceSettings,
    pub commands: crate::audio::CommandSender,
    pub command_slots: Arc<tokio::sync::Semaphore>,
}
pub struct Network {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Network {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn start(ports: Ports) -> Result<Network> {
    let directory = crate::identity::directory()?;
    fs::create_dir_all(&directory)?;
    let lock = crate::identity::agent_lock(&directory)?;
    let path = directory.join("identity.json");
    let identity = crate::identity::load_or_create(&path)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("litz-engine".into())
        .spawn(move || {
            let _lock = lock;
            if let Err(error) = runtime.block_on(listen(path, identity, ports, stopped)) {
                eprintln!("litzd: secure engine: {error}");
            }
        })?;
    Ok(Network {
        stop: Some(stop),
        thread: Some(thread),
    })
}

#[cfg(target_os = "linux")]
pub fn start_network(ports: Ports) -> Option<Network> {
    match start(ports) {
        Ok(network) => Some(network),
        Err(error) => {
            eprintln!("litzd: secure engine disabled: {error}");
            None
        }
    }
}

async fn listen(
    path: PathBuf,
    identity: Identity,
    ports: Ports,
    mut stop: oneshot::Receiver<()>,
) -> transport::Result<()> {
    let trust = Arc::new(RwLock::new(identity.trust.clone()));
    let config = transport::server_pairing(
        identity.certificate.clone(),
        identity.private_key.clone(),
        trust.clone(),
    )?;
    let endpoint = transport::server_endpoint(config, identity.bind)?;
    eprintln!("litzd: secure engine listening on {} (UDP)", identity.bind);
    let identity = Arc::new(std::sync::Mutex::new(identity));
    let hub = Hub::new(identity.clone(), trust.clone(), path.clone());
    let commands = tokio::spawn({
        let hub = hub.clone();
        async move {
            if let Err(error) = crate::devices::serve(hub).await {
                eprintln!("litzd: litzd devices/forget unavailable: {error}");
            }
        }
    });
    let service_name = computer_name();
    let device_id =
        litz_core::session::fingerprint(&identity.lock().unwrap().certificate).to_string();
    let _discovery = crate::discovery::Publisher::try_new(
        &service_name,
        &device_id,
        endpoint.local_addr()?.port(),
    )?;
    let mut discovery_health = tokio::time::interval(Duration::from_secs(5));
    let ports = Arc::new(tokio::sync::Mutex::new(ports));
    let mut workers = tokio::task::JoinSet::new();
    let mut window = Instant::now();
    let mut count = 0;
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = discovery_health.tick() => {
                if hub.failed() { break; }
            },
            Some(_) = workers.join_next(), if !workers.is_empty() => {},
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break; };
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    count = 0;
                }
                count += 1;
                if count > 8 || workers.len() >= 16 {
                    incoming.refuse();
                    continue;
                }
                if !incoming.remote_address_validated() {
                    let _ = incoming.retry();
                    continue;
                }
                let ports = ports.clone();
                let hub = hub.clone();
                workers.spawn(async move {
                    let Ok(Ok(c)) =
                        tokio::time::timeout(Duration::from_secs(5), incoming).await
                    else {
                        return;
                    };
                    let result = match hub.register(&c) {
                        Ok(()) => serve_connection(&c, &hub, &ports).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = &result { eprintln!("Litz connection ended: {error}"); }
                    hub.release(&c);
                    hub.unregister(&c);
                    c.close(if result.is_ok() { 0u8 } else { 1u8 }.into(), b"session ended");
                });
            }
        }
    }
    commands.abort();
    endpoint.close(0u8.into(), b"shutdown");
    workers.shutdown().await;

    Ok(())
}

async fn serve_connection(
    c: &quinn::Connection,
    hub: &Arc<Hub>,
    ports: &tokio::sync::Mutex<Ports>,
) -> transport::Result<()> {
    let mut control =
        Control::new(tokio::time::timeout(Duration::from_secs(5), c.accept_bi()).await??);
    let first = tokio::time::timeout(Duration::from_secs(5), control.read()).await??;
    let cert = transport::peer_certificate(c)?;
    if matches!(first.message, Message::PairRequest { .. }) {
        return pair(c, &mut control, hub, first, cert).await;
    }
    if !matches!(first.message, Message::GetState {}) || !hub.trusted(&cert) {
        control
            .write(&Record::new(
                first.request_id,
                Message::Error {
                    code: Code::Unauthorized,
                },
            ))
            .await?;
        control.send.finish()?;
        let _ = tokio::time::timeout(Duration::from_secs(2), control.send.stopped()).await;
        return Err("pair again".into());
    }
    if let Err(error) = hub.touch(&cert).await {
        eprintln!("litzd: could not record phone activity: {error}");
    }
    control
        .write(&Record::new(
            first.request_id,
            Message::Status {
                revision: 0,
                running: [None; 2],
            },
        ))
        .await?;
    loop {
        let request = control.read().await?;
        if !hub.trusted(&cert) {
            return Err("forgotten; pair again".into());
        }
        let message = match request.message {
            Message::GetOwnership {} => hub.status(c),
            Message::GetState {} => Message::Status {
                revision: 0,
                running: [None; 2],
            },
            Message::Release {} => hub.status(c),
            Message::Capabilities { .. } => {
                let desktop = if let Ok(mut available) = ports.try_lock() {
                    Some(desktop_routes(&mut available).await?)
                } else {
                    None
                };
                Message::Capabilities { desktop }
            }
            Message::Unpair {} => {
                hub.forget(&cert).await?;
                return Ok(());
            }
            Message::Acquire {
                expected_generation,
            } => {
                let lease = hub.acquire(&cert, c, expected_generation)?;
                if matches!(lease, Message::Ownership { owned: true, .. }) {
                    let mut audio = tokio::select! { p = ports.lock() => p, _ = c.closed() => return Err("cancelled acquisition".into()) };
                    if c.close_reason().is_some() {
                        return Err("cancelled acquisition".into());
                    }
                    control
                        .write(&Record::new(request.request_id, lease))
                        .await?;
                    let result = crate::stream::run(c, &mut control, &mut audio, hub).await;
                    audio.reset.store(true, Ordering::Release);
                    if audio.capture.is_some() {
                        let _ = capture_active(&mut audio, false).await;
                    }
                    hub.release(c);
                    drop(audio);
                    match result? {
                        Exit::Released(id) => {
                            control.write(&Record::new(id, hub.status(c))).await?;
                            continue;
                        }
                        Exit::Unpaired => return Ok(()),
                    }
                }
                lease
            }
            _ => Message::Error {
                code: Code::Unsupported,
            },
        };
        control
            .write(&Record::new(request.request_id, message))
            .await?;
    }
}

pub fn available_suppression() -> Vec<Suppression> {
    let mut algorithms = vec![Suppression::None, Suppression::Rnnoise];
    if crate::audio::voice::deep_filter_model().is_some() {
        algorithms.push(Suppression::DeepFilterNet);
    }
    algorithms
}

async fn pair(
    c: &quinn::Connection,
    control: &mut Control,
    hub: &Arc<Hub>,
    request: Record,
    cert: Vec<u8>,
) -> transport::Result<()> {
    let request_id = request.request_id;
    let Message::PairRequest {
        id,
        name,
        commitment,
    } = request.message
    else {
        return Err("not a pairing request".into());
    };
    if id == DeviceId::default() {
        return Err("invalid pairing request".into());
    }
    let _permit = hub.begin_pairing()?;
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    control
        .write(&Record::new(request_id, Message::PairNonce { nonce }))
        .await?;
    let reveal = tokio::time::timeout(Duration::from_secs(10), control.read()).await??;
    let Message::PairReveal { nonce: phone_nonce } = reveal.message else {
        return Err("pairing reveal missing".into());
    };
    if litz_core::session::commitment(&phone_nonce) != commitment {
        return Err("pairing commitment mismatch".into());
    }
    let code =
        litz_core::session::verification_code(&cert, &hub.certificate(), &phone_nonce, &nonce);
    let replaces = if hub.trusted(&cert) {
        None
    } else {
        hub.replacement()
    };
    let allowed = crate::approval::approve(&crate::approval::Request {
        name: &name,
        address: c.remote_address().ip(),
        code: &code,
        replaces: replaces
            .as_ref()
            .map(|(_, name, seen)| (name.as_str(), *seen)),
    })
    .await;
    if c.close_reason().is_some() {
        return Err("pairing cancelled".into());
    }
    let outcome = if !allowed {
        if replaces.is_some() {
            PairOutcome::Full
        } else {
            PairOutcome::Declined
        }
    } else {
        match hub
            .approve(&cert, &name, replaces.map(|(cert, ..)| cert))
            .await
        {
            Ok(()) => PairOutcome::Accepted,
            Err(error) => {
                eprintln!("litzd: could not save approved phone “{name}”: {error}");
                PairOutcome::Declined
            }
        }
    };
    control
        .write(&Record::new(
            request_id,
            Message::PairResult { id, outcome },
        ))
        .await?;
    control.send.finish()?;
    let _ = tokio::time::timeout(Duration::from_secs(2), control.send.stopped()).await;
    if outcome == PairOutcome::Accepted {
        Ok(())
    } else {
        Err("pairing declined".into())
    }
}

/// Run one operation on the audio thread. A timed-out request keeps its
/// permit until the audio loop consumes it, which bounds PipeWire's otherwise
/// unbounded channel to one queued command.
async fn command<T>(
    ports: &mut Ports,
    operation: impl FnOnce(oneshot::Sender<T>) -> CaptureOperation,
    timeout: Duration,
) -> transport::Result<T> {
    let (tx, rx) = oneshot::channel();
    let permit = tokio::time::timeout(
        Duration::from_secs(2),
        ports.command_slots.clone().acquire_owned(),
    )
    .await??;
    ports
        .commands
        .send((operation(tx), permit))
        .map_err(|_| "audio loop closed")?;
    Ok(tokio::time::timeout(timeout, rx).await??)
}

pub async fn capture_active(ports: &mut Ports, active: bool) -> transport::Result<()> {
    if ports.capture.is_none() {
        return Err("capture route unavailable".into());
    }
    let applied = command(
        ports,
        |tx| CaptureOperation::Active(active, tx),
        Duration::from_secs(2),
    )
    .await?;
    if !applied {
        return Err("capture activation failed".into());
    }
    Ok(())
}

pub async fn desktop_routes(ports: &mut Ports) -> transport::Result<DesktopRoutes> {
    let endpoints = command(ports, CaptureOperation::Inventory, Duration::from_secs(3)).await?;
    Ok(DesktopRoutes {
        voice_runtime: None,
        platform: crate::audio::PLATFORM.into(),
        name: computer_name(),
        endpoints,
        effective: ports.route.clone(),
        resolved: ports.resolved.clone(),
        revision: ports.revision,
        voice: ports.voice,
        suppression_algorithms: available_suppression(),
    })
}

pub async fn reopen_capture(ports: &mut Ports, route: Route) -> transport::Result<bool> {
    let next = command(
        ports,
        |tx| CaptureOperation::Route(route.clone(), tx),
        Duration::from_secs(3),
    )
    .await?;
    let Some(next) = next else {
        return Ok(false);
    };
    ports.capture = Some(next.samples);
    ports.capture_fault = next.fault;
    ports.resolved = next.resolved;
    ports.route = Some(route);
    Ok(true)
}

pub async fn apply_route(ports: &mut Ports, route: Route) -> transport::Result<bool> {
    if !reopen_capture(ports, route).await? {
        return Ok(false);
    }
    ports.revision = ports
        .revision
        .checked_add(1)
        .ok_or("settings revision exhausted")?;
    let settings = crate::settings::Settings {
        route: ports.route.clone(),
        revision: ports.revision,
        voice: ports.voice,
    };
    if let Err(error) = tokio::task::spawn_blocking(move || {
        crate::settings::save(&settings).map_err(|error| error.to_string())
    })
    .await?
    {
        eprintln!("litzd: route applied but settings persistence failed: {error}");
        return Ok(false);
    }
    Ok(true)
}
