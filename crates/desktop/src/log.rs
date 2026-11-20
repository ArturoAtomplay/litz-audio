//! One line per event on stderr. Under systemd each line carries its syslog
//! priority, so `journalctl -p warning` shows only problems; elsewhere lines
//! start with the program name. Debug lines appear only with `LITZ_LOG=debug`.

use std::fmt::Arguments;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 3,
    Warning = 4,
    Info = 6,
    Debug = 7,
}

struct Sink {
    journal: bool,
    debug: bool,
}

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| Sink {
        journal: journal(),
        debug: std::env::var_os("LITZ_LOG").is_some_and(|level| level == "debug"),
    })
}

/// Whether stderr is the journal stream systemd handed this service, which
/// JOURNAL_STREAM names by device and inode; children inherit the variable
/// without the stream.
#[cfg(unix)]
fn journal() -> bool {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;
    let Some(stream) = std::env::var_os("JOURNAL_STREAM") else {
        return false;
    };
    let Ok(stderr) = std::io::stderr().as_fd().try_clone_to_owned() else {
        return false;
    };
    std::fs::File::from(stderr)
        .metadata()
        .is_ok_and(|stderr| names(&stream.to_string_lossy(), stderr.dev(), stderr.ino()))
}
#[cfg(not(unix))]
fn journal() -> bool {
    false
}

#[cfg(unix)]
fn names(stream: &str, device: u64, inode: u64) -> bool {
    stream
        .split_once(':')
        .is_some_and(|(d, i)| d.parse() == Ok(device) && i.parse() == Ok(inode))
}

pub fn write(level: Level, line: Arguments) {
    let sink = sink();
    if level == Level::Debug && !sink.debug {
        return;
    }
    if sink.journal {
        eprintln!("<{}>{line}", level as u8);
    } else {
        eprintln!("litzd: {line}");
    }
}

macro_rules! error {
    ($($line:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($line)*)) };
}
macro_rules! warning {
    ($($line:tt)*) => { $crate::log::write($crate::log::Level::Warning, format_args!($($line)*)) };
}
macro_rules! info {
    ($($line:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($line)*)) };
}
macro_rules! debug {
    ($($line:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($line)*)) };
}
pub(crate) use {debug, error, info, warning};

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn the_journal_is_the_stream_systemd_named() {
        assert!(super::names("8:42", 8, 42));
        assert!(!super::names("8:42", 8, 43));
        assert!(!super::names("8:42", 9, 42));
        assert!(!super::names("", 8, 42));
        assert!(!super::names("8", 8, 42));
    }
}
