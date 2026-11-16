#![warn(clippy::undocumented_unsafe_blocks)]

mod approval;
mod audio;
mod cli;
mod devices;
mod discovery;
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
        cli::Command::Forget(phone) => devices::forget(&phone),
        cli::Command::Version => {
            println!("litzd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        cli::Command::Run { tone, route } => audio::run(tone, route),
    }
}
