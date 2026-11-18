#![warn(clippy::undocumented_unsafe_blocks)]

mod approval;
mod audio;
mod cli;
mod devices;
mod discovery;
#[cfg(windows)]
mod driver;
mod identity;
mod pairing;
mod platform;
mod server;
mod settings;
mod stream;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "Arguments must be valid Unicode")
        })
        .collect::<Result<Vec<_>, _>>()?;
    // COM starts litzd this way when a pairing notification outlives the
    // daemon; that request is already gone.
    #[cfg(windows)]
    if args.first().is_some_and(|arg| arg == "-ToastActivated") {
        return Ok(());
    }
    match cli::parse(args)? {
        cli::Command::Help => {
            println!("{}", cli::HELP);
            Ok(())
        }
        cli::Command::Devices => devices::list(),
        #[cfg(windows)]
        cli::Command::InstallDriver(inf) => restart_if(driver::install(&inf)?),
        #[cfg(windows)]
        cli::Command::UninstallDriver => restart_if(driver::uninstall()?),
        cli::Command::Forget(phone) => devices::forget(&phone),
        cli::Command::Version => {
            println!("litzd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        cli::Command::Run { tone, route } => audio::run(tone, route),
    }
}

/// Exits with ERROR_SUCCESS_REBOOT_REQUIRED, which installers understand.
#[cfg(windows)]
fn restart_if(needed: bool) -> Result<(), Box<dyn std::error::Error>> {
    if needed {
        eprintln!("litzd: restart Windows to finish updating Litz Microphone");
        std::process::exit(3010);
    }
    Ok(())
}
