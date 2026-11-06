use crate::identity;
use crate::pairing::Hub;
use litz_core::session::{SavedPeer, Trust, fingerprint};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
enum Request {
    Devices,
    Forget(String),
}

#[derive(Serialize, Deserialize)]
enum Response {
    Devices(Vec<Device>),
    Forgot(String),
    Error(String),
}

#[derive(Serialize, Deserialize)]
struct Device {
    id: String,
    name: String,
    last_seen: u64,
}

impl From<&SavedPeer> for Device {
    fn from(phone: &SavedPeer) -> Self {
        Self {
            id: fingerprint(&phone.certificate).to_string()[..8].to_owned(),
            name: phone.name.clone(),
            last_seen: phone.last_seen,
        }
    }
}

/// `query` is a phone's name or the ID printed by `litzd devices`.
fn find<'a>(trust: &'a Trust, query: &str) -> std::result::Result<&'a SavedPeer, String> {
    let matches: Vec<_> = trust
        .phones
        .iter()
        .filter(|phone| phone.name == query || Device::from(*phone).id == query)
        .collect();
    match matches.as_slice() {
        [] => Err(format!("no paired phone matches \u{201c}{query}\u{201d}")),
        [phone] => Ok(phone),
        _ => Err("several phones have that name; use the ID from `litzd devices`".into()),
    }
}

fn last_used(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    match now.saturating_sub(seconds) / 86_400 {
        _ if seconds == 0 => "unknown".into(),
        0 => "today".into(),
        1 => "yesterday".into(),
        days => format!("{days} days ago"),
    }
}

pub fn list() -> Result<()> {
    let devices = match ask(&Request::Devices)? {
        Some(Response::Devices(devices)) => devices,
        Some(Response::Error(error)) => return Err(error.into()),
        Some(Response::Forgot(_)) => return Err("unexpected reply from litzd".into()),
        None => saved()?.phones.iter().map(Device::from).collect(),
    };
    if devices.is_empty() {
        println!("No phones are paired.");
    }
    for device in devices {
        println!(
            "{}  {}  (last used {})",
            device.id,
            device.name,
            last_used(device.last_seen)
        );
    }
    Ok(())
}

pub fn forget(query: &str) -> Result<()> {
    let name = match ask(&Request::Forget(query.into()))? {
        Some(Response::Forgot(name)) => name,
        Some(Response::Error(error)) => return Err(error.into()),
        Some(Response::Devices(_)) => return Err("unexpected reply from litzd".into()),
        None => forget_saved(query)?,
    };
    println!("Forgot \u{201c}{name}\u{201d}. It must pair again to connect.");
    Ok(())
}

fn saved() -> Result<Trust> {
    let path = identity::directory()?.join("identity.json");
    if !path.exists() {
        return Ok(Trust::default());
    }
    Ok(identity::load_or_create(&path)?.trust)
}

/// Only safe when no daemon answered: the daemon keeps the registry in memory.
fn forget_saved(query: &str) -> Result<String> {
    let directory = identity::directory()?;
    let path = directory.join("identity.json");
    if !path.exists() {
        return Err("no phones are paired".into());
    }
    let _lock =
        identity::agent_lock(&directory).map_err(|_| "litzd is starting; try again shortly")?;
    let mut identity = identity::load_or_create(&path)?;
    let phone = find(&identity.trust, query)?;
    let name = phone.name.clone();
    identity.trust = identity.trust.forget(&phone.certificate.clone());
    identity::save(&path, &identity)?;
    Ok(name)
}

/// `None` when no daemon is listening.
fn ask(request: &Request) -> Result<Option<Response>> {
    let Some(stream) = connect()? else {
        return Ok(None);
    };
    let mut writer = &stream;
    writer.write_all(&serde_json::to_vec(request)?)?;
    writer.write_all(b"\n")?;
    let mut line = String::new();
    io::BufReader::new(&stream).read_line(&mut line)?;
    Ok(Some(serde_json::from_str(&line)?))
}

async fn handle<S: AsyncRead + AsyncWrite>(stream: S, hub: &Arc<Hub>) -> io::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut line = String::new();
    tokio::io::BufReader::new(reader.take(4096))
        .read_line(&mut line)
        .await?;
    let response = match serde_json::from_str(&line) {
        Ok(Request::Devices) => {
            Response::Devices(hub.trust().phones.iter().map(Device::from).collect())
        }
        Ok(Request::Forget(query)) => {
            let found = find(&hub.trust(), &query).map(|p| (p.certificate.clone(), p.name.clone()));
            match found {
                Ok((certificate, name)) => match hub.forget(&certificate).await {
                    Ok(()) => Response::Forgot(name),
                    Err(error) => Response::Error(error.to_string()),
                },
                Err(error) => Response::Error(error),
            }
        }
        Err(_) => Response::Error("invalid request".into()),
    };
    let mut bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.shutdown().await
}

fn spawn_handler<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, hub: &Arc<Hub>) {
    let hub = hub.clone();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle(stream, &hub)).await;
    });
}

#[cfg(target_os = "linux")]
fn socket_path() -> Result<std::path::PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    Ok(match runtime {
        Some(directory) => directory.join("litz"),
        None => identity::directory()?,
    }
    .join("litzd.sock"))
}

#[cfg(target_os = "linux")]
fn connect() -> Result<Option<std::os::unix::net::UnixStream>> {
    match std::os::unix::net::UnixStream::connect(socket_path()?) {
        Ok(stream) => {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            Ok(Some(stream))
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "linux")]
pub async fn serve(hub: Arc<Hub>) -> Result<()> {
    let path = socket_path()?;
    crate::platform::create_directory(path.parent().ok_or("socket directory")?)?;
    // The agent lock is held, so a socket left here belongs to a dead daemon.
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    // SAFETY: getuid has no preconditions and cannot fail.
    let user = unsafe { libc::getuid() };
    loop {
        let (stream, _) = listener.accept().await?;
        if stream.peer_cred().is_ok_and(|peer| peer.uid() == user) {
            spawn_handler(stream, &hub);
        }
    }
}

#[cfg(windows)]
fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user.chars().filter(char::is_ascii_alphanumeric).collect();
    format!(r"\\.\pipe\litz-audio-{user}")
}

#[cfg(windows)]
fn connect() -> Result<Option<std::fs::File>> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
    {
        Ok(pipe) => Ok(Some(pipe)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Creating the first pipe instance stops another process from
/// impersonating the daemon.
#[cfg(windows)]
pub async fn serve(hub: Arc<Hub>) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = pipe_name();
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)?;
    loop {
        server.connect().await?;
        let connected = server;
        server = ServerOptions::new()
            .reject_remote_clients(true)
            .create(&name)?;
        spawn_handler(connected, &hub);
    }
}
