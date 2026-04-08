use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronEntry {
    pub id: String,
    pub name: String,
    pub cron_expression: String,
    pub prompt: String,
    pub enabled: bool,
    pub last_run: Option<u64>,
    pub next_run: u64,
    /// Optional URL of a remote Anvil instance to run the prompt on.
    pub target_url: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CronStore {
    entries: Vec<CronEntry>,
}

// ---------------------------------------------------------------------------
// CronManager
// ---------------------------------------------------------------------------

pub struct CronManager {
    store: CronStore,
    store_path: PathBuf,
}

impl CronManager {
    /// Construct by loading (or creating) the persistent store.
    #[must_use] 
    pub fn new(store_path: PathBuf) -> Self {
        let store = Self::load_store(&store_path);
        Self { store, store_path }
    }

    /// Return the process-global singleton, persisted at `~/.anvil/cron.json`.
    #[must_use]
    pub fn global() -> &'static Mutex<CronManager> {
        static INSTANCE: OnceLock<Mutex<CronManager>> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let path = default_store_path();
            Mutex::new(CronManager::new(path))
        })
    }

    // -----------------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------------

    /// Create a new cron entry.  Returns the entry ID.
    pub fn create(
        &mut self,
        cron_expression: String,
        prompt: String,
        name: Option<String>,
        target_url: Option<String>,
    ) -> Result<String, String> {
        let id = make_id();
        let now = unix_secs();
        let next_run = next_run_time(&cron_expression, now)
            .ok_or_else(|| format!("invalid cron expression: `{cron_expression}`"))?;

        let entry_name = name.unwrap_or_else(|| format!("cron-{id}"));

        let entry = CronEntry {
            id: id.clone(),
            name: entry_name,
            cron_expression,
            prompt,
            enabled: true,
            last_run: None,
            next_run,
            target_url,
            created_at: now,
        };

        self.store.entries.push(entry);
        self.save()?;
        Ok(id)
    }

    /// Return a snapshot of all cron entries sorted by `created_at`.
    #[must_use]
    pub fn list(&self) -> Vec<CronEntry> {
        let mut entries = self.store.entries.clone();
        entries.sort_by_key(|e| e.created_at);
        entries
    }

    /// Return a single entry by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CronEntry> {
        self.store.entries.iter().find(|e| e.id == id)
    }

    /// Delete an entry by ID.  Returns an error if the ID is not found.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let before = self.store.entries.len();
        self.store.entries.retain(|e| e.id != id);
        if self.store.entries.len() == before {
            return Err(format!("cron entry `{id}` not found"));
        }
        self.save()
    }

    /// Update the enabled flag or expression of an existing entry.
    pub fn update(
        &mut self,
        id: &str,
        enabled: Option<bool>,
        cron_expression: Option<String>,
        name: Option<String>,
    ) -> Result<(), String> {
        let entry = self
            .store
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("cron entry `{id}` not found"))?;

        if let Some(expr) = cron_expression {
            let now = unix_secs();
            let next =
                next_run_time(&expr, now).ok_or_else(|| format!("invalid cron expression: `{expr}`"))?;
            entry.cron_expression = expr;
            entry.next_run = next;
        }
        if let Some(en) = enabled {
            entry.enabled = en;
        }
        if let Some(n) = name {
            entry.name = n;
        }
        self.save()
    }

    /// Return all entries whose `next_run` is at or before `now` and that are
    /// enabled.  For each returned entry the `last_run` and `next_run` fields
    /// are updated in the store.
    pub fn run_pending(&mut self) -> Result<Vec<CronEntry>, String> {
        let now = unix_secs();
        let mut due: Vec<CronEntry> = Vec::new();

        for entry in &mut self.store.entries {
            if !entry.enabled {
                continue;
            }
            if entry.next_run <= now {
                due.push(entry.clone());
                entry.last_run = Some(now);
                entry.next_run =
                    next_run_time(&entry.cron_expression, now).unwrap_or(u64::MAX);
            }
        }

        if !due.is_empty() {
            self.save()?;
        }

        Ok(due)
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    fn load_store(path: &PathBuf) -> CronStore {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => CronStore::default(),
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create cron dir: {e}"))?;
        }
        let json =
            serde_json::to_string_pretty(&self.store).map_err(|e| format!("serialize error: {e}"))?;
        fs::write(&self.store_path, json).map_err(|e| format!("cannot write cron store: {e}"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Cron expression parsing
//
// Supported subset:
//   <minute> <hour> <day-of-month> <month> <day-of-week>
//
// Each field may be:
//   *          — any value
//   N          — exact value
//   */N        — every N units (step)
//   N-M        — range (interpreted as: match any value from N to M inclusive)
//
// next_run_time computes the next Unix timestamp (seconds) at which the
// expression fires, starting from `after` + 1 second.
// ---------------------------------------------------------------------------

struct CronFields {
    minute: CronField,
    hour: CronField,
    dom: CronField,   // day-of-month
    month: CronField,
    dow: CronField,   // day-of-week (0=Sun, 6=Sat)
}

#[derive(Clone)]
enum CronField {
    Any,
    Exact(u32),
    Step(u32),            // */N
    Range(u32, u32),      // N-M
}

impl CronField {
    fn parse(s: &str) -> Option<Self> {
        if s == "*" {
            return Some(Self::Any);
        }
        if let Some(rest) = s.strip_prefix("*/") {
            let n: u32 = rest.parse().ok()?;
            if n == 0 {
                return None;
            }
            return Some(Self::Step(n));
        }
        if let Some((a, b)) = s.split_once('-') {
            let lo: u32 = a.parse().ok()?;
            let hi: u32 = b.parse().ok()?;
            return Some(Self::Range(lo, hi));
        }
        let n: u32 = s.parse().ok()?;
        Some(Self::Exact(n))
    }

    fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(n) => value == *n,
            Self::Step(n) => value.is_multiple_of(*n),
            Self::Range(lo, hi) => value >= *lo && value <= *hi,
        }
    }
}

fn parse_cron(expr: &str) -> Option<CronFields> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    Some(CronFields {
        minute: CronField::parse(parts[0])?,
        hour:   CronField::parse(parts[1])?,
        dom:    CronField::parse(parts[2])?,
        month:  CronField::parse(parts[3])?,
        dow:    CronField::parse(parts[4])?,
    })
}

/// Compute the next fire time (Unix seconds) after `after`.
/// Scans forward by one-minute increments, up to one year ahead.
#[must_use] 
pub fn next_run_time(expr: &str, after: u64) -> Option<u64> {
    let fields = parse_cron(expr)?;

    // Start checking one minute after `after`, rounded to the start of that minute.
    let start = (after / 60 + 1) * 60;

    // Scan up to 366 days * 24h * 60min = 527040 minutes.
    for offset in 0u64..527_040 {
        let candidate = start + offset * 60;
        let dt = unix_to_parts(candidate);
        if fields.month.matches(dt.month)
            && fields.dom.matches(dt.day)
            && fields.dow.matches(dt.weekday)
            && fields.hour.matches(dt.hour)
            && fields.minute.matches(dt.minute)
        {
            return Some(candidate);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Minimal datetime decomposition (no external crate)
// ---------------------------------------------------------------------------

struct DateTimeParts {
    minute: u32,
    hour: u32,
    day: u32,
    month: u32,
    weekday: u32, // 0 = Sunday
}

/// Very lightweight Unix-timestamp → (year, month, day, hour, minute, weekday)
/// decomposition.  Good for ~year 2000–2100.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn unix_to_parts(ts: u64) -> DateTimeParts {
    let secs_per_day: u64 = 86_400;
    let days_since_epoch = ts / secs_per_day;
    let time_of_day = ts % secs_per_day;

    let hour   = (time_of_day / 3600) as u32;
    let minute = ((time_of_day % 3600) / 60) as u32;

    // Weekday: epoch (1970-01-01) was a Thursday (4).
    let weekday = ((days_since_epoch + 4) % 7) as u32;

    // Calendar date from days since epoch using the Gregorian proleptic algorithm.
    // Using the algorithm from https://howardhinnant.github.io/date_algorithms.html
    // (civil_from_days).
    let z = days_since_epoch as i64 + 719_468;
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;          // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153;                  // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let _year = y + i64::from(month <= 2);

    DateTimeParts { minute, hour, day, month, weekday }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".anvil").join("cron.json")
}

#[allow(clippy::cast_possible_truncation)]
fn make_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let secs = unix_secs();
    let raw = secs.wrapping_mul(1_000_000_007).wrapping_add(u64::from(nanos));
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    let base = chars.len() as u64;
    let mut n = raw;
    let mut result = String::with_capacity(8);
    for _ in 0..8 {
        result.push(chars[(n % base) as usize]);
        n /= base;
    }
    result
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// CronDaemon — background thread that polls and fires due cron entries
// ---------------------------------------------------------------------------

/// How often the daemon wakes up to check for due entries.
const CRON_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// A background daemon that fires `CronEntry` items on schedule.
///
/// Call [`CronDaemon::start`] once at process startup.  The returned handle
/// keeps the thread alive; dropping it does **not** stop the thread — call
/// [`CronDaemon::stop`] explicitly if you need a clean shutdown.
pub struct CronDaemon {
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CronDaemon {
    /// Spawn the daemon thread and return a `CronDaemon` controller.
    ///
    /// The thread polls [`CronManager::global`] every 30 seconds, fires any
    /// due entries, and then goes back to sleep.  Use [`CronDaemon::stop`] to
    /// request a clean shutdown.
    #[must_use] 
    pub fn start() -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = thread::Builder::new()
            .name("anvil-cron-daemon".to_string())
            .spawn(move || {
                cron_daemon_loop(&stop_flag_clone);
            })
            .expect("failed to spawn cron daemon thread");

        Self {
            stop_flag,
            handle: Some(handle),
        }
    }

    /// Signal the daemon to stop.  Returns immediately; the background thread
    /// will exit on its next wakeup (within `CRON_POLL_INTERVAL`).
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    /// Returns `true` if the stop signal has been sent.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.stop_flag.load(Ordering::Relaxed)
    }

    /// Consume the daemon and wait for the background thread to finish.
    pub fn join(mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn cron_daemon_loop(stop_flag: &AtomicBool) {
    // Break the 30-second sleep into short slices so we can react quickly to
    // stop requests without a full 30-second delay on shutdown.
    const TICK: Duration = Duration::from_millis(500);
    #[allow(clippy::cast_possible_truncation)]
    let ticks_per_poll = (CRON_POLL_INTERVAL.as_millis() / TICK.as_millis()) as u32;
    let mut ticks_elapsed: u32 = ticks_per_poll; // fire immediately on first wakeup

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        thread::sleep(TICK);
        ticks_elapsed += 1;

        if ticks_elapsed < ticks_per_poll {
            continue;
        }
        ticks_elapsed = 0;

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        fire_due_entries();
    }
}

fn fire_due_entries() {
    // Collect due entries while holding the lock, then release before
    // potentially doing slow HTTP or process-spawn work.
    let due = match CronManager::global().lock() {
        Ok(mut mgr) => match mgr.run_pending() {
            Ok(entries) => entries,
            Err(err) => {
                eprintln!("[cron] run_pending failed: {err}");
                return;
            }
        },
        Err(poisoned) => {
            eprintln!("[cron] CronManager lock poisoned: {poisoned}");
            return;
        }
    };

    for entry in due {
        if let Some(ref url) = entry.target_url {
            fire_remote(&entry, url);
        } else {
            fire_local(&entry);
        }
    }
}

/// Execute a due entry on a remote Anvil instance via HTTP POST.
fn fire_remote(entry: &CronEntry, url: &str) {
    // Build a minimal JSON body that mirrors the RemoteTrigger format.
    let body = format!(
        r#"{{"cron_id":"{id}","prompt":{prompt_json}}}"#,
        id = entry.id,
        prompt_json = json_string_escape(&entry.prompt),
    );

    match ureq_post_blocking(url, &body) {
        Ok(status) => {
            eprintln!("[cron] fired `{}` → {url} (HTTP {status})", entry.name);
        }
        Err(err) => {
            eprintln!("[cron] remote trigger for `{}` failed: {err}", entry.name);
        }
    }
}

/// Execute a due entry locally by spawning it as a `TaskManager` task.
fn fire_local(entry: &CronEntry) {
    use crate::task::TaskManager;

    let description = format!("cron:{} — {}", entry.id, entry.name);
    match TaskManager::global().lock() {
        Ok(mut mgr) => match mgr.create(description, entry.prompt.clone()) {
            Ok(task_id) => {
                eprintln!("[cron] fired `{}` as task {task_id}", entry.name);
            }
            Err(err) => {
                eprintln!("[cron] failed to create task for `{}`: {err}", entry.name);
            }
        },
        Err(poisoned) => {
            eprintln!("[cron] TaskManager lock poisoned: {poisoned}");
        }
    }
}

/// Minimal blocking HTTP POST using only `std` + raw TCP — no external crate
/// required in the runtime crate.  Returns the HTTP status code string on
/// success.
fn ureq_post_blocking(url: &str, body: &str) -> Result<u16, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    // Parse the URL into host, port, and path.
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("unsupported scheme in URL: {url}"))?;

    let (host_port, path) = if let Some(slash) = without_scheme.find('/') {
        (&without_scheme[..slash], &without_scheme[slash..])
    } else {
        (without_scheme, "/")
    };

    let (host, port) = if let Some(colon) = host_port.rfind(':') {
        let port: u16 = host_port[colon + 1..]
            .parse()
            .map_err(|_| format!("invalid port in URL: {url}"))?;
        (&host_port[..colon], port)
    } else if url.starts_with("https://") {
        (host_port, 443_u16)
    } else {
        (host_port, 80_u16)
    };

    let addr = format!("{host}:{port}");
    let mut stream =
        TcpStream::connect(&addr).map_err(|e| format!("connect to {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();

    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let reader = BufReader::new(stream);
    let status_line = reader
        .lines()
        .next()
        .ok_or_else(|| "empty response".to_string())?
        .map_err(|e| format!("read status line: {e}"))?;

    // e.g. "HTTP/1.0 200 OK"
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed status line: {status_line}"))?
        .parse()
        .map_err(|_| format!("non-numeric status in: {status_line}"))?;

    Ok(status)
}

/// Escape a string for embedding in a JSON string literal.
fn json_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_any_field() {
        let f = CronField::parse("*").unwrap();
        assert!(f.matches(0));
        assert!(f.matches(59));
    }

    #[test]
    fn parse_exact_field() {
        let f = CronField::parse("15").unwrap();
        assert!(f.matches(15));
        assert!(!f.matches(16));
    }

    #[test]
    fn parse_step_field() {
        let f = CronField::parse("*/5").unwrap();
        assert!(f.matches(0));
        assert!(f.matches(5));
        assert!(f.matches(30));
        assert!(!f.matches(7));
    }

    #[test]
    fn parse_range_field() {
        let f = CronField::parse("9-17").unwrap();
        assert!(f.matches(9));
        assert!(f.matches(17));
        assert!(!f.matches(8));
        assert!(!f.matches(18));
    }

    #[test]
    fn next_run_every_minute() {
        // "* * * * *" — fires every minute
        let after = 1_700_000_000u64; // arbitrary fixed timestamp
        let next = next_run_time("* * * * *", after).unwrap();
        // next_run should be the start of the next minute
        assert_eq!(next, (after / 60 + 1) * 60);
    }

    #[test]
    fn next_run_every_5_minutes() {
        let after = 1_700_000_000u64;
        let next = next_run_time("*/5 * * * *", after).unwrap();
        assert!(next > after);
        let parts = unix_to_parts(next);
        assert_eq!(parts.minute % 5, 0);
    }

    #[test]
    fn invalid_expression_returns_none() {
        assert!(next_run_time("not a cron", 0).is_none());
        assert!(next_run_time("*/0 * * * *", 0).is_none());
    }
}
