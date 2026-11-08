use litz_core::protocol::Route;

pub const HELP: &str = "litzd — Litz Audio background agent
  litzd [--mirror NAME|default]   run the agent; --mirror overrides the saved output
  litzd devices                   list paired phones
  litzd forget NAME|ID            unpair a phone
  litzd --version
Phones pair by tapping this computer in the Litz app, then allowing the prompt on this computer.";

pub enum Command {
    Help,
    Version,
    Devices,
    Forget(String),
    Run { tone: bool, route: Option<Route> },
}

pub fn parse(args: Vec<String>) -> Result<Command, &'static str> {
    match args.as_slice() {
        [only] if matches!(only.as_str(), "--help" | "-h") => return Ok(Command::Help),
        [only] if matches!(only.as_str(), "--version" | "-V") => return Ok(Command::Version),
        [only] if only == "devices" => return Ok(Command::Devices),
        [command, phone] if command == "forget" && !phone.is_empty() => {
            return Ok(Command::Forget(phone.clone()));
        }
        _ => {}
    }
    let mut args = args.into_iter();
    let mut route = None;
    #[cfg(feature = "test-tone")]
    let mut tone = false;
    #[cfg(not(feature = "test-tone"))]
    let tone = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mirror" if route.is_none() => {
                let name = args
                    .next()
                    .filter(|n| !n.starts_with('-') && !n.is_empty())
                    .ok_or("--mirror requires a stable name or default")?;
                route = Some(Route::Mirror {
                    name: (name != "default").then_some(name),
                });
            }
            #[cfg(feature = "test-tone")]
            "--test-tone" if !tone => tone = true,
            _ => return Err("unknown or conflicting agent argument; see --help"),
        }
    }
    Ok(Command::Run { tone, route })
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_core::protocol::Route;

    fn parse_args(args: &str) -> Result<Command, &'static str> {
        parse(args.split_whitespace().map(str::to_owned).collect())
    }

    #[test]
    fn runs_with_a_startup_route_or_without_one() {
        assert!(matches!(
            parse(vec![]),
            Ok(Command::Run {
                tone: false,
                route: None
            })
        ));
        assert!(matches!(
            parse_args("--mirror default"),
            Ok(Command::Run {
                route: Some(Route::Mirror { name: None }),
                ..
            })
        ));
        assert!(matches!(
            parse_args("--mirror studio-headphones"),
            Ok(Command::Run {
                route: Some(Route::Mirror { name: Some(name) }),
                ..
            }) if name == "studio-headphones"
        ));
        if cfg!(feature = "test-tone") {
            assert!(matches!(
                parse_args("--test-tone"),
                Ok(Command::Run { tone: true, .. })
            ));
        } else {
            assert!(parse_args("--test-tone").is_err());
        }
    }

    #[test]
    fn rejects_incomplete_and_conflicting_route_flags() {
        for args in [
            "--mirror",
            "--mirror -invalid",
            "--mirror default --mirror speakers",
            "--help extra",
        ] {
            assert!(parse_args(args).is_err(), "{args}");
        }
    }

    #[test]
    fn help_recognizes_blank_arguments() {
        assert!(matches!(parse_args("--help"), Ok(Command::Help)));
        assert!(matches!(parse_args("-h"), Ok(Command::Help)));
        assert!(matches!(parse_args("--version"), Ok(Command::Version)));
        assert!(parse_args("--version extra").is_err());
        assert!(matches!(parse_args("devices"), Ok(Command::Devices)));
        assert!(matches!(parse_args("forget Pixel"), Ok(Command::Forget(name)) if name == "Pixel"));
        assert!(parse_args("forget").is_err());
    }
}
