//! Cross-session live agent snapshots.
//!
//! Each Anvil process periodically writes a JSON snapshot of its currently
//! tracked agents to `~/.anvil/agents/<pid>.json`.  Snapshots are read by the
//! `anvil agents live` / `anvil agents monitor` subcommand so a user can see
//! every live Anvil subagent running on the machine, across all live sessions.
//!
//! Design notes:
//! - **Best-effort, fire-and-forget writes.**  A snapshot write failure must
//!   never affect the running session.  All I/O errors are swallowed.
//! - **Atomic writes** via `<pid>.json.tmp` + `rename`, so a reader can never
//!   observe a half-written file.
//! - **Liveness check** on read: dead-PID snapshots are filtered (and pruned).
//!   On Unix we use `kill(pid, 0)` via libc; on Windows the first cut trusts
//!   the snapshots and a proper check is left as a TODO (#462).
//! - **Cleanup** is the writer's responsibility: each process should call
//!   [`clear_snapshot`] on a graceful exit AND from a panic hook.  Readers
//!   also prune any dead-pid file they encounter, so a crashed process'
//!   leftover snapshot is reaped on the next `anvil agents live` run.
//!
//! v2.2.14 CC-139-F1 — issue #462.

// `kill(pid, 0)` requires an `unsafe extern "C"` block on Unix.  The workspace
// lint denies `unsafe_code` by default, so this file opts in explicitly.  Only
// the `pid_is_alive` helper and its test scaffolding use it.
#![allow(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─── Public types ─────────────────────────────────────────────────────────────

/// One entry per live agent in a process snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Per-session agent id (stringified; `AgentManager` uses `usize`).
    pub id: String,
    /// User-friendly agent name (e.g. "auth-refactor", "test-runner").
    pub name: String,
    /// Agent kind / persona label (`general`, `backend`, `frontend`, …).
    pub kind: String,
    /// Coarse lifecycle status: `running`, `waiting`, `completed`, `failed`.
    pub status: String,
    /// Unix seconds at which the agent was spawned.
    pub started_at: u64,
}

/// Full snapshot for one Anvil process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    /// OS pid of the writing Anvil process.
    pub pid: u32,
    /// The session id that owns these agents.
    pub session_id: String,
    /// Unix seconds at which this snapshot was last written.
    pub written_at: u64,
    /// All tracked agents (running/waiting/recently completed).
    pub agents: Vec<AgentEntry>,
}

// ─── Paths ────────────────────────────────────────────────────────────────────

/// The directory containing per-process agent snapshots.
///
/// Resolves to `$ANVIL_CONFIG_HOME/agents/` when that env var is set
/// (tests use it), otherwise `$HOME/.anvil/agents/`, otherwise `./.anvil/agents/`.
#[must_use]
pub fn snapshot_dir() -> PathBuf {
    let home = std::env::var_os("ANVIL_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".anvil")))
        .unwrap_or_else(|| PathBuf::from(".anvil"));
    home.join("agents")
}

fn snapshot_path(pid: u32) -> PathBuf {
    snapshot_dir().join(format!("{pid}.json"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Writer API ───────────────────────────────────────────────────────────────

/// Write (or overwrite) this process's snapshot.
///
/// Best-effort: silently swallows I/O errors so a transient disk problem can
/// never interfere with the live session.  Uses an atomic `.tmp` + rename so
/// a concurrent reader never observes a half-written file.
pub fn write_snapshot(pid: u32, session_id: &str, agents: &[AgentEntry]) {
    let dir = snapshot_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let snapshot = AgentSnapshot {
        pid,
        session_id: session_id.to_string(),
        written_at: unix_now(),
        agents: agents.to_vec(),
    };

    let final_path = snapshot_path(pid);
    let tmp_path = dir.join(format!("{pid}.json.tmp"));

    let Ok(json) = serde_json::to_vec(&snapshot) else {
        return;
    };

    if fs::write(&tmp_path, json).is_err() {
        let _ = fs::remove_file(&tmp_path);
        return;
    }

    if fs::rename(&tmp_path, &final_path).is_err() {
        // rename failed — try to clean up the stray tmp file so it doesn't
        // accumulate forever.  Don't propagate the error.
        let _ = fs::remove_file(&tmp_path);
    }
}

/// Delete this process's snapshot file.
///
/// Call from a graceful-exit guard AND from a panic hook, so a crashed
/// process's stale snapshot is reaped promptly.  Errors are swallowed.
pub fn clear_snapshot(pid: u32) {
    let _ = fs::remove_file(snapshot_path(pid));
}

// ─── Reader API ───────────────────────────────────────────────────────────────

/// Read every snapshot in `snapshot_dir()`, filter out those whose pid is no
/// longer alive, and return what's left.
///
/// Dead-pid files are pruned from disk as a side effect, so the next call
/// won't see them again.  Malformed JSON files are ignored (and pruned).
#[must_use]
pub fn read_all_snapshots() -> Vec<AgentSnapshot> {
    let dir = snapshot_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.parse::<u32>().is_err() {
            // Not a `<pid>.json` file — leave it alone.
            continue;
        }

        match load_snapshot_file(&path) {
            Some(snap) if pid_is_alive(snap.pid) => out.push(snap),
            _ => {
                // Either malformed or dead-pid — prune.
                let _ = fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|s| s.pid);
    out
}

fn load_snapshot_file(path: &Path) -> Option<AgentSnapshot> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<AgentSnapshot>(&bytes).ok()
}

// ─── Liveness check ───────────────────────────────────────────────────────────

/// Best-effort liveness probe.
///
/// - **Unix:** uses `kill(pid, 0)` via libc, which returns 0 iff the process
///   exists (signal is *not* delivered when sig=0).
/// - **Windows:** TODO(#462) — currently trusts the snapshot.  A future patch
///   should use `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, …)`.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) with sig=0 is a permission/existence probe — it
        // delivers no signal.  pid_t is i32 on all supported Unix platforms;
        // valid process IDs fit safely in u32 → i32.
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: see above.
        unsafe { kill(pid.cast_signed(), 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // TODO(#462): Windows liveness check via OpenProcess + GetExitCodeProcess.
        let _ = pid;
        true
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // Snapshot tests mutate ANVIL_CONFIG_HOME — serialise them with the
    // crate-internal ENV_LOCK for in-binary mutual exclusion AND the
    // workspace-wide `#[serial(anvil_config_home)]` token so we don't race
    // any other crate's ANVIL_CONFIG_HOME-mutating test that happens to be
    // scheduled into the same binary's thread pool.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[cfg_attr(test, allow(unsafe_code))]
    struct EnvGuard {
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(home: &Path) -> Self {
            let prev = std::env::var_os("ANVIL_CONFIG_HOME");
            // SAFETY: tests are serialised by ENV_LOCK; we are the sole writer.
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", home); }
            Self { prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests are serialised by ENV_LOCK; we are the sole writer.
            unsafe {
                match self.prev.take() {
                    Some(value) => std::env::set_var("ANVIL_CONFIG_HOME", value),
                    None => std::env::remove_var("ANVIL_CONFIG_HOME"),
                }
            }
        }
    }

    fn sample_entry(id: &str) -> AgentEntry {
        AgentEntry {
            id: id.to_string(),
            name: format!("agent-{id}"),
            kind: "general".to_string(),
            status: "running".to_string(),
            started_at: 1_700_000_000,
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn write_then_read_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let pid = std::process::id();
        let agents = vec![sample_entry("1"), sample_entry("2")];
        write_snapshot(pid, "session-abc", &agents);

        let all = read_all_snapshots();
        assert_eq!(all.len(), 1, "expected exactly one live snapshot");
        let snap = &all[0];
        assert_eq!(snap.pid, pid);
        assert_eq!(snap.session_id, "session-abc");
        assert_eq!(snap.agents, agents);

        clear_snapshot(pid);
        assert!(read_all_snapshots().is_empty(), "clear_snapshot should remove the file");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn dead_pid_snapshot_is_pruned() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        // Pick a PID we are very confident is NOT running.  PID 0 and 1 are
        // special on most Unixes (1 = init/launchd, 0 = kernel); a high
        // sentinel like 2_000_000_000 is essentially guaranteed to be free.
        let dead_pid = 2_000_000_000_u32;
        // Sanity: pid_is_alive must agree.
        assert!(!pid_is_alive(dead_pid), "sentinel pid must not be alive");

        write_snapshot(dead_pid, "ghost-session", &[sample_entry("99")]);

        // File should now exist on disk.
        let file_path = snapshot_dir().join(format!("{dead_pid}.json"));
        assert!(file_path.exists(), "snapshot file should have been written");

        // read_all_snapshots must filter it out AND prune the file.
        let all = read_all_snapshots();
        assert!(
            all.iter().all(|s| s.pid != dead_pid),
            "dead-pid snapshot must be filtered out"
        );
        assert!(
            !file_path.exists(),
            "dead-pid snapshot file should be pruned from disk"
        );
    }

    #[test]
    fn live_pid_is_alive() {
        // The current process is, by definition, alive.
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn malformed_snapshot_is_pruned() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let dir = snapshot_dir();
        fs::create_dir_all(&dir).unwrap();
        // Write garbage that lexically looks like `<pid>.json`.
        let path = dir.join("12345.json");
        fs::write(&path, b"this is not valid json {{").unwrap();

        let _ = read_all_snapshots();
        assert!(
            !path.exists(),
            "malformed snapshot file must be pruned on read"
        );
    }

    #[test]
    #[serial(anvil_config_home)]
    fn empty_dir_returns_empty() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());
        // Don't even create the agents/ dir — reader must handle that.
        assert!(read_all_snapshots().is_empty());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn atomic_write_no_tmp_left_behind() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(tmp.path());

        let pid = std::process::id();
        write_snapshot(pid, "s", &[sample_entry("1")]);

        let dir = snapshot_dir();
        let tmp_path = dir.join(format!("{pid}.json.tmp"));
        assert!(!tmp_path.exists(), "tmp file must be renamed away");

        clear_snapshot(pid);
    }
}
