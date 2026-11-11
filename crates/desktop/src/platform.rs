use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn directory() -> io::Result<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")));
    base.map(|p| p.join("litz"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "User state directory unavailable"))
}
/// Raises the calling thread above ordinary desktop work. It refills audio
/// rings every few milliseconds, and a refusal only keeps the default priority.
#[cfg(target_os = "linux")]
pub async fn raise_thread_priority() {
    const NICE: i32 = -11;
    // SAFETY: gettid has no preconditions.
    let thread = unsafe { libc::gettid() };
    // SAFETY: adjusts only the calling thread; failure keeps the default.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS as _, thread as _, NICE) } == 0 {
        return;
    }
    // An unprivileged process may not lower its own nice value; RealtimeKit
    // grants it, as it does for PipeWire.
    let granted = async {
        zbus::Connection::system()
            .await?
            .call_method(
                Some("org.freedesktop.RealtimeKit1"),
                "/org/freedesktop/RealtimeKit1",
                Some("org.freedesktop.RealtimeKit1"),
                "MakeThreadHighPriority",
                &(thread as u64, NICE),
            )
            .await
    };
    if let Err(error) = granted.await {
        eprintln!("litzd: engine thread keeps the default priority: {error}");
    }
}
#[cfg(windows)]
pub async fn raise_thread_priority() {
    pro_audio_thread();
}
/// Registers the calling thread with the Multimedia Class Scheduler.
#[cfg(windows)]
pub fn pro_audio_thread() {
    use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    let mut task = 0;
    // SAFETY: a static NUL-terminated task name and a valid out pointer. The
    // registration lasts until the thread exits.
    let handle =
        unsafe { AvSetMmThreadCharacteristicsW(windows_sys::core::w!("Pro Audio"), &mut task) };
    if handle.is_null() {
        eprintln!(
            "litzd: thread keeps the default priority: {}",
            io::Error::last_os_error()
        );
    }
}
/// Bound allocations even if a file grows between metadata validation and read.
pub fn read_limited(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    use io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Stored file exceeds its size limit",
        ));
    }
    Ok(bytes)
}
pub fn create_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn private_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
pub fn private_file(metadata: &fs::Metadata) -> bool {
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return false;
        }
    }
    true
}
pub fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    // Windows rename below requests write-through; opening a directory and
    // calling FlushFileBuffers is not the POSIX directory-fsync operation.
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use io::Write;
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let temp = path.with_extension(format!("{:016x}.new", u64::from_ne_bytes(nonce)));
    let mut file = private_options().create_new(true).open(&temp)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| replace(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: two live NUL-terminated UTF-16 paths; no pointers retained.
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
pub fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}
#[cfg(not(windows))]
pub fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}
#[cfg(windows)]
pub fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    crypt(bytes, true)
}
#[cfg(windows)]
pub fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    crypt(bytes, false)
}
#[cfg(windows)]
fn crypt(bytes: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::other("Identity too large"))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: DPAPI reads input during the call and allocates output with
    // LocalAlloc. Flags prohibit any UI. The user-scoped default is intentional.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Ok(result)
    }
}
