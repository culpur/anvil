/// Schedule expression parsing and next-fire computation for routines.
///
/// Supported expression forms:
/// - `"every Nm"` / `"every Nh"` / `"every Nd"` — recurring interval
/// - `"Nm"` / `"Nh"` / `"Nd"` — one-shot delay from creation time
/// - Five-field cron expression (`"M H Dom Mon Dow"`) — classic cron
/// - RFC 3339 UTC timestamp (`"YYYY-MM-DDTHH:MM:SSZ"`) — one-shot wall-clock
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cron::next_run_time;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A parsed schedule expression.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value")]
pub enum Schedule {
    /// Classic 5-field cron expression, e.g. `"0 9 * * 1-5"`.
    Cron(String),
    /// Recurring interval stored as seconds, e.g. `"every 30m"` → 1800.
    Interval(u64),
    /// One-shot delay from creation: stored as seconds.  The routine's
    /// `next_run` is initialised to `now + secs`; after the first fire
    /// [`next_fire`] returns `None` to disable the entry.
    OnceAfter(u64),
    /// One-shot wall-clock target stored as Unix seconds.  After the target
    /// time passes [`next_fire`] returns `None`.
    OnceAt(u64),
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a schedule string into a [`Schedule`] value.
///
/// Accepted forms (checked in this order):
/// 1. `"every Nm"` / `"every Nh"` / `"every Nd"` → [`Schedule::Interval`]
/// 2. `"Nm"` / `"Nh"` / `"Nd"` → [`Schedule::OnceAfter`]
/// 3. Five-field cron `"M H Dom Mon Dow"` → [`Schedule::Cron`]
/// 4. `"YYYY-MM-DDTHH:MM:SSZ"` (UTC only) → [`Schedule::OnceAt`]
pub fn parse_schedule(s: &str) -> Result<Schedule, String> {
    let trimmed = s.trim();

    if trimmed.is_empty() {
        return Err("empty schedule expression".to_string());
    }

    // ── 1. "every Nm/Nh/Nd" ─────────────────────────────────────────────
    if let Some(rest) = trimmed.strip_prefix("every ") {
        let secs = parse_duration_str(rest.trim())?;
        return Ok(Schedule::Interval(secs));
    }

    // ── 2. Plain duration "Nm/Nh/Nd" ────────────────────────────────────
    // Must be a single token (no spaces) to avoid shadowing cron.
    if !trimmed.contains(' ') {
        if let Ok(secs) = parse_duration_str(trimmed) {
            return Ok(Schedule::OnceAfter(secs));
        }
    }

    // ── 3. Five-field cron expression ────────────────────────────────────
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 5 {
        // Validate by actually running next_run_time; if it returns Some we
        // accept the expression.
        let probe = unix_secs();
        if next_run_time(trimmed, probe).is_some() {
            return Ok(Schedule::Cron(trimmed.to_string()));
        }
        // Had five fields but failed to parse — fall through to ISO check,
        // and if that fails too we'll return a meaningful error below.
    }

    // ── 4. ISO 8601 UTC timestamp "YYYY-MM-DDTHH:MM:SSZ" ────────────────
    if let Ok(epoch) = parse_iso8601_utc(trimmed) {
        return Ok(Schedule::OnceAt(epoch));
    }

    Err(format!("unrecognised schedule expression: `{trimmed}`"))
}

/// Compute the next fire time (Unix seconds) after `after`.
///
/// Returns `None` when the schedule is one-shot and has already been consumed:
/// - [`Schedule::OnceAfter`] — always returns `None` (the `next_run` field
///   was set at creation; after firing once the entry should be disabled).
/// - [`Schedule::OnceAt`] — returns `None` when `epoch <= after`.
#[must_use]
pub fn next_fire(schedule: &Schedule, after: u64) -> Option<u64> {
    match schedule {
        Schedule::Cron(expr) => next_run_time(expr, after),
        Schedule::Interval(secs) => Some(after + secs),
        Schedule::OnceAfter(_) => None,
        Schedule::OnceAt(epoch) => {
            if *epoch > after {
                Some(*epoch)
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Duration parsing
// ---------------------------------------------------------------------------

/// Parse `"30m"`, `"2h"`, `"1d"` into seconds.
fn parse_duration_str(s: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err("empty duration string".to_string());
    }

    let (num_part, suffix) = match s.rfind(|c: char| c.is_ascii_digit()) {
        Some(idx) => (&s[..=idx], &s[idx + 1..]),
        None => return Err(format!("no numeric value in duration: `{s}`")),
    };

    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid number in duration: `{s}`"))?;

    let multiplier = match suffix {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        other => return Err(format!("unknown duration suffix `{other}` in `{s}`")),
    };

    Ok(n * multiplier)
}

// ---------------------------------------------------------------------------
// ISO 8601 parsing
// ---------------------------------------------------------------------------

/// Parse `"YYYY-MM-DDTHH:MM:SSZ"` (UTC, Z-terminator only) into Unix seconds.
///
/// Rejects any form that deviates from the exact template, including offsets
/// other than `Z`.
pub fn parse_iso8601_utc(s: &str) -> Result<u64, String> {
    // Expected: "YYYY-MM-DDTHH:MM:SSZ"  — exactly 20 chars.
    if s.len() != 20 {
        return Err(format!(
            "ISO timestamp must be 20 chars, got {}: `{s}`",
            s.len()
        ));
    }
    if !s.ends_with('Z') {
        return Err(format!("ISO timestamp must end with 'Z' (UTC): `{s}`"));
    }

    let bytes = s.as_bytes();
    // Check structural characters: YYYY-MM-DDTHH:MM:SS Z
    //                              0123456789012345678 9
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(format!("malformed ISO timestamp: `{s}`"));
    }

    let year: u64 = parse_digits(&s[0..4], "year")?;
    let month: u64 = parse_digits(&s[5..7], "month")?;
    let day: u64 = parse_digits(&s[8..10], "day")?;
    let hour: u64 = parse_digits(&s[11..13], "hour")?;
    let min: u64 = parse_digits(&s[14..16], "minute")?;
    let sec: u64 = parse_digits(&s[17..19], "second")?;

    // Basic range validation.
    if !(1970..=2200).contains(&year) {
        return Err(format!("year out of range: {year}"));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("month out of range: {month}"));
    }
    if !(1..=31).contains(&day) {
        return Err(format!("day out of range: {day}"));
    }
    if hour > 23 {
        return Err(format!("hour out of range: {hour}"));
    }
    if min > 59 {
        return Err(format!("minute out of range: {min}"));
    }
    if sec > 59 {
        return Err(format!("second out of range: {sec}"));
    }

    Ok(parts_to_unix(year, month, day, hour, min, sec))
}

fn parse_digits(s: &str, field: &str) -> Result<u64, String> {
    s.parse::<u64>()
        .map_err(|_| format!("non-numeric {field} in ISO timestamp: `{s}`"))
}

// ---------------------------------------------------------------------------
// Calendar math — inverse of cron::unix_to_parts
// ---------------------------------------------------------------------------

/// Convert calendar components (UTC) to Unix seconds.
///
/// Uses the inverse of the Howard Hinnant civil-from-days algorithm.
/// Valid for years 1970–2200; accuracy outside that range is not guaranteed.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn parts_to_unix(year: u64, month: u64, day: u64, hour: u64, min: u64, sec: u64) -> u64 {
    // Shift January and February to months 13/14 of the previous year so that
    // leap-day always falls at the end of the "year" in this system.
    let (y, m) = if month <= 2 {
        (year as i64 - 1, month + 9)
    } else {
        (year as i64, month - 3)
    };

    let era: i64 = y.div_euclid(400);
    let yoe: i64 = y - era * 400; // [0, 399]
    let doy: i64 = (153 * m as i64 + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe: i64 = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days_since_epoch: i64 = era * 146_097 + doe - 719_468;

    let time_of_day = hour * 3600 + min * 60 + sec;
    (days_since_epoch as u64) * 86_400 + time_of_day
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_schedule positive cases ─────────────────────────────────────

    #[test]
    fn parse_once_after_minutes() {
        assert_eq!(parse_schedule("30m"), Ok(Schedule::OnceAfter(1800)));
    }

    #[test]
    fn parse_interval_hours() {
        assert_eq!(parse_schedule("every 2h"), Ok(Schedule::Interval(7200)));
    }

    #[test]
    fn parse_cron_weekday_9am() {
        assert_eq!(
            parse_schedule("0 9 * * 1-5"),
            Ok(Schedule::Cron("0 9 * * 1-5".to_string()))
        );
    }

    #[test]
    fn parse_iso_timestamp() {
        let s = parse_schedule("2026-06-01T12:00:00Z").unwrap();
        let expected_epoch = parts_to_unix(2026, 6, 1, 12, 0, 0);
        assert_eq!(s, Schedule::OnceAt(expected_epoch));
    }

    #[test]
    fn parse_interval_days() {
        assert_eq!(parse_schedule("every 1d"), Ok(Schedule::Interval(86_400)));
    }

    #[test]
    fn parse_once_after_hours() {
        assert_eq!(parse_schedule("3h"), Ok(Schedule::OnceAfter(10_800)));
    }

    // ── parse_schedule negative cases ────────────────────────────────────

    #[test]
    fn parse_empty_fails() {
        assert!(parse_schedule("").is_err());
    }

    #[test]
    fn parse_natural_language_fails() {
        assert!(parse_schedule("30 minutes").is_err());
    }

    #[test]
    fn parse_four_field_cron_fails() {
        assert!(parse_schedule("0 9 * *").is_err());
    }

    #[test]
    fn parse_invalid_iso_fails() {
        assert!(parse_schedule("2026-13-01T12:00:00Z").is_err());
    }

    #[test]
    fn parse_iso_without_z_fails() {
        assert!(parse_schedule("2026-06-01T12:00:00+00:00").is_err());
    }

    // ── next_fire ─────────────────────────────────────────────────────────

    #[test]
    fn next_fire_cron() {
        // "* * * * *" fires every minute
        let after = 1_700_000_000u64;
        let result = next_fire(&Schedule::Cron("* * * * *".to_string()), after);
        assert!(result.is_some());
        assert!(result.unwrap() > after);
    }

    #[test]
    fn next_fire_interval() {
        let after = 1_000u64;
        assert_eq!(next_fire(&Schedule::Interval(300), after), Some(1_300));
    }

    #[test]
    fn next_fire_once_after_returns_none() {
        // OnceAfter always returns None — the next_run was set at creation.
        assert_eq!(next_fire(&Schedule::OnceAfter(3600), 0), None);
    }

    #[test]
    fn next_fire_once_at_future() {
        let epoch = 2_000_000_000u64;
        assert_eq!(next_fire(&Schedule::OnceAt(epoch), epoch - 1), Some(epoch));
    }

    #[test]
    fn next_fire_once_at_past_returns_none() {
        let epoch = 1_000u64;
        assert_eq!(next_fire(&Schedule::OnceAt(epoch), epoch + 1), None);
    }

    #[test]
    fn next_fire_once_at_equal_returns_none() {
        let epoch = 1_000u64;
        assert_eq!(next_fire(&Schedule::OnceAt(epoch), epoch), None);
    }

    // ── serde round-trip ──────────────────────────────────────────────────

    #[test]
    fn schedule_serde_cron_roundtrip() {
        let s = Schedule::Cron("0 9 * * 1-5".to_string());
        let json = serde_json::to_string(&s).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_serde_interval_roundtrip() {
        let s = Schedule::Interval(3600);
        let json = serde_json::to_string(&s).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_serde_once_after_roundtrip() {
        let s = Schedule::OnceAfter(1800);
        let json = serde_json::to_string(&s).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn schedule_serde_once_at_roundtrip() {
        let epoch = parts_to_unix(2026, 6, 1, 12, 0, 0);
        let s = Schedule::OnceAt(epoch);
        let json = serde_json::to_string(&s).unwrap();
        let back: Schedule = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // ── parts_to_unix / unix_to_parts round-trips ────────────────────────

    #[test]
    fn parts_to_unix_unix_epoch() {
        // 1970-01-01T00:00:00Z == 0
        assert_eq!(parts_to_unix(1970, 1, 1, 0, 0, 0), 0);
    }

    #[test]
    fn parts_to_unix_y2k() {
        // 2000-01-01T00:00:00Z == 946684800
        assert_eq!(parts_to_unix(2000, 1, 1, 0, 0, 0), 946_684_800);
    }

    #[test]
    fn parts_to_unix_iso_roundtrip() {
        let epoch = parts_to_unix(2026, 6, 1, 12, 0, 0);
        // Convert back via parse_iso8601_utc after formatting.
        let formatted = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", 2026, 6, 1, 12, 0, 0);
        let parsed = parse_iso8601_utc(&formatted).unwrap();
        assert_eq!(epoch, parsed);
    }

    #[test]
    fn parse_iso8601_known_timestamp() {
        // 2000-01-01T00:00:00Z
        assert_eq!(parse_iso8601_utc("2000-01-01T00:00:00Z"), Ok(946_684_800));
    }
}
