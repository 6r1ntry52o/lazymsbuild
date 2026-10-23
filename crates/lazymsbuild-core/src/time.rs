//! Timestamps. Everything lazymsbuild writes uses RFC 3339 with the local offset
//! (`2026-10-23T12:00:00+09:00`).

use std::time::SystemTime;

use chrono::{DateTime, FixedOffset, Local, SecondsFormat};

/// Now, in RFC 3339 with the local offset.
pub fn now() -> String {
    format(Local::now())
}

/// A system time in RFC 3339 with the local offset.
pub fn from_system(time: SystemTime) -> String {
    format(DateTime::<Local>::from(time))
}

fn format(time: DateTime<Local>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, false)
}

/// Parses an RFC 3339 timestamp.
pub fn parse(text: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(text).ok()
}

/// Whether `time` is later than the RFC 3339 timestamp `than`. An unreadable `than` counts as
/// "never", so anything is later.
pub fn is_after(time: SystemTime, than: &str) -> bool {
    match parse(than) {
        Some(than) => DateTime::<Local>::from(time) > than,
        None => true,
    }
}

/// Short local clock time (`12:03:11`) of an RFC 3339 timestamp, for the UI.
pub fn clock(text: &str) -> String {
    match parse(text) {
        Some(t) => t.with_timezone(&Local).format("%H:%M:%S").to_string(),
        None => text.to_string(),
    }
}

/// A file-name friendly timestamp (`20261023-120000`), for log files.
pub fn file_stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn round_trips() {
        let text = now();
        assert!(parse(&text).is_some(), "{text}");
        let earlier = SystemTime::now() - Duration::from_secs(60);
        assert!(!is_after(earlier, &text));
        assert!(is_after(SystemTime::now() + Duration::from_secs(60), &text));
        assert!(is_after(earlier, "not a time"));
    }
}
