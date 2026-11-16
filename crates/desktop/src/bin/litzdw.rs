#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(std::env::current_exe()?.with_file_name("litzd.exe"))
            .args(std::env::args_os().skip(1))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
    }
    Ok(())
}
