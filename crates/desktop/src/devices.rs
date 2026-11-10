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

/// The account running this process. Its SID names the pipe, and a pipe it
/// does not own belongs to someone squatting on that name.
#[cfg(windows)]
struct User {
    token: Vec<usize>,
    sid: String,
}

#[cfg(windows)]
impl User {
    fn current() -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUser};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut handle = std::ptr::null_mut();
        // SAFETY: the pseudo handle needs no closing; `handle` is a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        // SAFETY: a null buffer of length 0 only queries the required length.
        unsafe { GetTokenInformation(handle, TokenUser, std::ptr::null_mut(), 0, &mut length) };
        // usize elements keep the TOKEN_USER pointer field aligned.
        let mut token = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: `token` holds at least `length` writable bytes.
        let filled = unsafe {
            GetTokenInformation(
                handle,
                TokenUser,
                token.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: `handle` came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(handle) };
        if filled == 0 {
            return Err(error);
        }
        let mut user = Self {
            token,
            sid: String::new(),
        };
        let mut text = std::ptr::null_mut();
        // SAFETY: `psid` points into `user.token`; the string is freed below.
        if unsafe { ConvertSidToStringSidW(user.psid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ConvertSidToStringSidW returned a NUL-terminated string,
        // allocated with LocalAlloc and freed once here.
        unsafe {
            user.sid = String::from_utf16_lossy(windows::core::PCWSTR(text).as_wide());
            LocalFree(text.cast());
        }
        Ok(user)
    }

    fn psid(&self) -> windows_sys::Win32::Security::PSID {
        // SAFETY: `token` holds the TOKEN_USER that GetTokenInformation wrote.
        unsafe {
            (*self
                .token
                .as_ptr()
                .cast::<windows_sys::Win32::Security::TOKEN_USER>())
            .User
            .Sid
        }
    }

    fn pipe(&self) -> String {
        format!(r"\\.\pipe\litz-audio-{}", self.sid)
    }

    fn owns(&self, handle: std::os::windows::io::RawHandle) -> io::Result<bool> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
        use windows_sys::Win32::Security::{EqualSid, OWNER_SECURITY_INFORMATION};
        let (mut owner, mut descriptor) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: `handle` is open; the out pointers are valid and `descriptor`
        // owns `owner`, so it is freed after the comparison.
        unsafe {
            let status = GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            let equal = EqualSid(owner, self.psid()) != 0;
            LocalFree(descriptor);
            Ok(equal)
        }
    }
}

#[cfg(windows)]
fn connect() -> Result<Option<std::fs::File>> {
    use std::os::windows::io::AsRawHandle;
    let user = User::current()?;
    let pipe = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(user.pipe())
    {
        Ok(pipe) => pipe,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !user.owns(pipe.as_raw_handle())? {
        return Err("the litzd pipe belongs to another account".into());
    }
    Ok(Some(pipe))
}

/// Owned by the user, who alone may open it. Without an explicit owner the
/// pipe takes the token's default owner, which for an elevated administrator
/// is the Administrators group that every admin account shares.
#[cfg(windows)]
struct Descriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is LocalAlloc'd memory that is only read after creation.
#[cfg(windows)]
unsafe impl Send for Descriptor {}
// SAFETY: as above; nothing mutates the descriptor once built.
#[cfg(windows)]
unsafe impl Sync for Descriptor {}

#[cfg(windows)]
impl Descriptor {
    fn private_to(user: &User) -> io::Result<Self> {
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let sddl: Vec<u16> = format!("O:{0}D:P(A;;GA;;;{0})", user.sid)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and `descriptor` is a valid out pointer.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn create(
        &self,
        name: &str,
        first: bool,
    ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` and the descriptor it points to outlive the call.
        unsafe {
            tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }
    }
}

#[cfg(windows)]
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}

/// Creating the first instance fails if another process already holds the
/// name, so nobody can stand in for the daemon.
#[cfg(windows)]
pub async fn serve(hub: Arc<Hub>) -> Result<()> {
    let user = User::current()?;
    let descriptor = Descriptor::private_to(&user)?;
    let name = user.pipe();
    let mut server = descriptor.create(&name, true)?;
    loop {
        server.connect().await?;
        let next = descriptor.create(&name, false)?;
        spawn_handler(std::mem::replace(&mut server, next), &hub);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;

    #[tokio::test]
    async fn pipe_belongs_to_the_user_who_created_it() {
        let user = User::current().unwrap();
        assert!(user.sid.starts_with("S-1-"));
        let name = format!(r"\\.\pipe\litz-audio-test-{}", std::process::id());
        let _server = Descriptor::private_to(&user)
            .unwrap()
            .create(&name, true)
            .unwrap();
        let client = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        assert!(user.owns(client.as_raw_handle()).unwrap());
    }
}
