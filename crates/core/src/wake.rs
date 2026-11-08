//! Real-time-safe wakeup from an audio callback; signals coalesce.

use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd, RawFd},
};

#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Clone)]
pub struct Wake(Arc<File>);

#[cfg(any(target_os = "linux", target_os = "android"))]
impl AsRawFd for Wake {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl Wake {
    pub fn try_new() -> std::io::Result<Self> {
        // SAFETY: eventfd has no pointer arguments and returns a fresh descriptor or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: eventfd returned a fresh descriptor; File takes sole ownership.
        Ok(Self(Arc::new(unsafe { File::from_raw_fd(fd) })))
    }

    pub fn signal(&self) {
        let value = 1u64;
        // SAFETY: valid eventfd and 8-byte input; a nonblocking write never
        // waits, and a saturated counter just coalesces wakeups.
        unsafe {
            libc::write(self.as_raw_fd(), (&value as *const u64).cast(), 8);
        }
    }

    fn drain(&self) {
        let mut value = 0u64;
        // SAFETY: valid nonblocking eventfd and 8-byte output buffer.
        unsafe {
            libc::read(self.as_raw_fd(), (&mut value as *mut u64).cast(), 8);
        }
    }
}

#[cfg(windows)]
#[derive(Clone)]
pub struct Wake(Arc<tokio::sync::Notify>);

#[cfg(windows)]
impl Wake {
    pub fn try_new() -> std::io::Result<Self> {
        Ok(Self(Arc::new(tokio::sync::Notify::new())))
    }

    pub fn signal(&self) {
        self.0.notify_one();
    }
}

/// Must be created inside a Tokio runtime.
pub struct WakeListener {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fd: tokio::io::unix::AsyncFd<Wake>,
    #[cfg(windows)]
    wake: Wake,
}

impl WakeListener {
    pub fn try_new(wake: Wake) -> std::io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        return Ok(Self {
            // SAFETY: Wake owns the descriptor for as long as it is registered.
            fd: unsafe { tokio::io::unix::AsyncFd::register(wake)? },
        });
        #[cfg(windows)]
        Ok(Self { wake })
    }

    pub async fn wait(&self) -> std::io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let mut guard = self.fd.readable().await?;
            self.fd.get_ref().drain();
            guard.clear_ready();
        }
        #[cfg(windows)]
        self.wake.0.notified().await;
        Ok(())
    }
}
