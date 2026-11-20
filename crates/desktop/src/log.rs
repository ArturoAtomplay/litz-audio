//! One line per event on stderr. Under systemd each line carries its syslog
//! priority, so `journalctl -p warning` shows only problems; elsewhere lines
//! start with the program name, and with the UTC time when they go to a file.
//! Debug lines appear only with `LITZ_LOG=debug`.

use std::fmt::Arguments;
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 3,
    Warning = 4,
    Info = 6,
    Debug = 7,
}

struct Sink {
    journal: bool,
    timed: bool,
    debug: bool,
}

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| {
        let journal = journal();
        Sink {
            journal,
            timed: !journal && !std::io::stderr().is_terminal(),
            debug: std::env::var_os("LITZ_LOG").is_some_and(|level| level == "debug"),
        }
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
    } else if sink.timed {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        eprintln!("{} litzd: {line}", utc(now));
    } else {
        eprintln!("litzd: {line}");
    }
}

/// `seconds` since the Unix epoch as an ISO 8601 UTC time, using Howard
/// Hinnant's days-to-civil algorithm.
fn utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
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
    use super::utc;

    #[cfg(unix)]
    #[test]
    fn the_journal_is_the_stream_systemd_named() {
        assert!(super::names("8:42", 8, 42));
        assert!(!super::names("8:42", 8, 43));
        assert!(!super::names("8:42", 9, 42));
        assert!(!super::names("", 8, 42));
        assert!(!super::names("8", 8, 42));
    }

    #[test]
    fn utc_times_match_known_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_763_607_979), "2025-11-20T03:06:19Z");
        assert_eq!(utc(4_107_542_399), "2100-02-28T23:59:59Z");
    }
}
