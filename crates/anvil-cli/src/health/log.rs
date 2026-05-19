//! Heal-session telemetry log.
//!
//! Append-only log at `~/.anvil/logs/heal-<ISO-week>.log`.  Rotates weekly
//! (one file per ISO week — no scheduled rotator, the filename changes
//! when the week rolls over and old files persist until garbage-collected).
//! Local-only, never shipped off-device.
//!
//! Format (one line per event):
//!
//! ```
//! 2026-05-19T07:42:13Z heal start ─ 3 issues detected
//! 2026-05-19T07:42:14Z heal probe ollama:daemon=down qmd:refresh=stale
//! 2026-05-19T07:42:14Z heal action ollama:start → success (took 1.2s)
//! 2026-05-19T07:42:21Z heal end ─ all repaired
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Heal-log directory: `~/.anvil/logs/`.
#[must_use]
pub fn log_dir() -> PathBuf {
    runtime::default_config_home().join("logs")
}

/// Compute today's heal-log path: `~/.anvil/logs/heal-YYYY-Www.log`.
///
/// We use ISO week (`YYYY-Www`) for weekly rotation.  Approximated as
/// `(epoch_days / 7)` since 1970-01-01 (Thu).  The exact alignment to
/// calendar weeks isn't important — what matters is the same week's heal
/// activity goes in the same file, and a new week opens a new file.
#[must_use]
pub fn current_log_path() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let week = now / (86400 * 7);
    log_dir().join(format!("heal-W{week}.log"))
}

/// Append a single line to the heal log.  Best-effort — failure to write
/// the log is logged but never propagated (we don't want to abort a
/// healing session because a log file can't be opened).
pub fn append(event: &str) {
    let path = current_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let timestamp = iso_timestamp(SystemTime::now());
    let line = format!("{timestamp} {event}\n");
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Render a `YYYY-MM-DDTHH:MM:SSZ` timestamp from a `SystemTime`.
///
/// Avoids the chrono dependency — health log timestamps don't need
/// subsecond precision or locale-aware formatting.
#[must_use]
pub fn iso_timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (year, month, day, hour, min, sec) = unix_to_ymdhms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Naive UTC decomposition of a Unix timestamp.
fn unix_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let total_days = secs / 86400;
    let rem = secs % 86400;
    let hour = (rem / 3600) as u32;
    let min = ((rem % 3600) / 60) as u32;
    let sec = (rem % 60) as u32;

    // Calendar walk forward from 1970-01-01.
    let mut year: u32 = 1970;
    let mut days_left = total_days as i64;
    loop {
        let yd = if is_leap(year) { 366 } else { 365 };
        if days_left < yd {
            break;
        }
        days_left -= yd;
        year += 1;
    }
    let dim = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for m in 1u32..=12 {
        let mut d = dim[(m - 1) as usize];
        if m == 2 && is_leap(year) {
            d = 29;
        }
        if days_left < d {
            month = m;
            break;
        }
        days_left -= d;
    }
    let day = (days_left + 1) as u32;
    (year, month, day, hour, min, sec)
}

const fn is_leap(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// High-level helpers --------------------------------------------------------

/// Log "heal start ─ N issues detected".
pub fn log_start(issue_count: usize) {
    append(&format!("heal start ─ {issue_count} issue(s) detected"));
}

/// Log the probe-result summary as a single line.
pub fn log_probe(summary: &str) {
    append(&format!("heal probe {summary}"));
}

/// Log a single repair attempt.  `outcome` is one of `success`, `failure`,
/// `skipped`.
pub fn log_action(component: &str, action: &str, outcome: &str, detail: &str) {
    let detail_str = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    };
    append(&format!("heal action {component}:{action} → {outcome}{detail_str}"));
}

/// Log "heal end ─ <outcome>".
pub fn log_end(outcome: &str) {
    append(&format!("heal end ─ {outcome}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::Duration;

    #[test]
    fn iso_timestamp_round_trip_known_value() {
        // 2026-01-01T00:00:00Z = 1767225600
        let t = UNIX_EPOCH + Duration::from_secs(1767225600);
        assert_eq!(iso_timestamp(t), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn iso_timestamp_handles_leap_year() {
        // 2024 is a leap year — 2024-02-29 should exist.
        // 2024-02-29T12:00:00Z = 1709208000
        let t = UNIX_EPOCH + Duration::from_secs(1709208000);
        assert_eq!(iso_timestamp(t), "2024-02-29T12:00:00Z");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn append_round_trips_event_to_disk() {
        // Direct file-write smoke test — uses a sandboxed temp dir via
        // env override to avoid touching the real `~/.anvil/logs`.
        let tmp = tempfile::tempdir().unwrap();
        // Safety: matches existing parse_args pattern.
        unsafe {
            std::env::set_var("ANVIL_CONFIG_HOME", tmp.path());
        }

        append("heal start ─ 2 issues detected");
        append("heal action vault:chmod → success (silent)");
        append("heal end ─ all repaired");

        let path = current_log_path();
        let content = std::fs::read_to_string(&path).expect("log file written");
        assert!(content.contains("heal start"));
        assert!(content.contains("vault:chmod"));
        assert!(content.contains("heal end"));
        // Three lines.
        assert_eq!(content.lines().count(), 3);
        // Every line begins with a `YYYY-MM-DDT…Z` timestamp.
        for line in content.lines() {
            assert!(line.starts_with("20"), "line should begin with year: {line}");
            assert!(line.contains('T'));
            assert!(line.contains('Z'));
        }

        unsafe {
            std::env::remove_var("ANVIL_CONFIG_HOME");
        }
    }

    #[test]
    fn log_path_contains_week_marker() {
        let p = current_log_path();
        let s = p.to_string_lossy().to_string();
        assert!(s.contains("heal-W"), "log path should embed ISO week: {s}");
        assert!(s.ends_with(".log"));
    }
}
