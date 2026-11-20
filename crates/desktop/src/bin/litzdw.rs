#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let stderr = log().map_or_else(|_| std::process::Stdio::null(), Into::into);
        std::process::Command::new(std::env::current_exe()?.with_file_name("litzd.exe"))
            .args(std::env::args_os().skip(1))
            .creation_flags(CREATE_NO_WINDOW)
            .stderr(stderr)
            .spawn()?;
    }
    Ok(())
}

/// Without a console litzd's stderr would go nowhere. The previous log is
/// kept beside the current one once it passes 1 MiB. A log that cannot be
/// written never keeps litzd from starting.
#[cfg(windows)]
fn log() -> std::io::Result<std::fs::File> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| std::io::Error::other("LOCALAPPDATA is not set"))?;
    let directory = std::path::PathBuf::from(base).join("litz");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("litzd.log");
    if std::fs::metadata(&path).is_ok_and(|log| log.len() > 1 << 20) {
        let _ = std::fs::rename(&path, directory.join("litzd.old.log"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}
