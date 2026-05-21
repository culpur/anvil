//! Session management: handles, IDs, directory listing, and formatting.
//!
//! Each REPL invocation creates a `SessionHandle` that ties a unique ID to a
//! JSON file on disk.  The file is written via `LiveCli::persist_session` after
//! every model turn so the conversation can be resumed with `anvil --resume`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::Session;

use crate::utils::anvil_home_dir;

/// Lightweight reference to an on-disk session file.
pub(crate) struct SessionHandle {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
}

/// Summary metadata used by the `/sessions` list command.
#[derive(Debug, Clone)]
pub(crate) struct ManagedSessionSummary {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) modified_epoch_secs: u64,
    pub(crate) message_count: usize,
}

/// Return the canonical sessions directory, creating it if needed.
///
/// Task #758: sessions live under `$ANVIL_CONFIG_HOME/sessions/` (defaulting
/// to `~/.anvil/sessions/`), NOT per-workspace under `<cwd>/.anvil/sessions/`.
/// The CWD-relative path was a bug: the exit banner promised
/// `anvil --resume <name>` would work, but the by-name resolver scanned
/// CWD-relative sidecars — running `--resume` from a different shell
/// (different CWD) found nothing.
///
/// Legacy CWD-local sessions are still readable via `legacy_sessions_dir()`
/// and `resolve_session_reference` checks both.
pub(crate) fn sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = anvil_home_dir().join("sessions");
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Pre-#758 CWD-relative path. Used only for backwards-compat reads — new
/// writes go to [`sessions_dir`]. Returns `None` if the legacy directory
/// does not exist (typical for fresh installs).
pub(crate) fn legacy_sessions_dir() -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    let path = cwd.join(".anvil").join("sessions");
    if path.exists() { Some(path) } else { None }
}

/// Create a new `SessionHandle` with a fresh timestamp-based ID.
pub(crate) fn create_managed_session_handle()
-> Result<SessionHandle, Box<dyn std::error::Error>> {
    let id = generate_session_id();
    let path = sessions_dir()?.join(format!("{id}.json"));
    Ok(SessionHandle { id, path })
}

/// Generate a session ID based on the current millisecond timestamp.
pub(crate) fn generate_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("session-{millis}")
}

/// Resolve a session reference (ID string or file path) to a `SessionHandle`.
///
/// Search order (task #758):
///   1. Literal filesystem path
///   2. Canonical sessions dir: `$ANVIL_CONFIG_HOME/sessions/<ref>.json`
///   3. Legacy CWD-local: `<cwd>/.anvil/sessions/<ref>.json` (pre-#758)
pub(crate) fn resolve_session_reference(
    reference: &str,
) -> Result<SessionHandle, Box<dyn std::error::Error>> {
    let direct = PathBuf::from(reference);
    if direct.exists() {
        let id = direct
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(reference)
            .to_string();
        return Ok(SessionHandle { id, path: direct });
    }

    let canonical = sessions_dir()?.join(format!("{reference}.json"));
    if canonical.exists() {
        return Ok(SessionHandle {
            id: reference.to_string(),
            path: canonical,
        });
    }

    if let Some(legacy_dir) = legacy_sessions_dir() {
        let legacy = legacy_dir.join(format!("{reference}.json"));
        if legacy.exists() {
            return Ok(SessionHandle {
                id: reference.to_string(),
                path: legacy,
            });
        }
    }

    Err(format!("session not found: {reference}").into())
}

/// List all managed sessions in the current workspace, sorted newest-first.
pub(crate) fn list_managed_sessions()
-> Result<Vec<ManagedSessionSummary>, Box<dyn std::error::Error>> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(sessions_dir()?)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        // Task #758: exclude `<id>.meta.json` sidecar files — their extension
        // is also `.json` so the bare suffix check above lets them through.
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with(".meta.json")
        {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified_epoch_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let message_count = Session::load_from_path(&path)
            .map(|s| s.messages.len())
            .unwrap_or_default();
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_string();
        sessions.push(ManagedSessionSummary {
            id,
            path,
            modified_epoch_secs,
            message_count,
        });
    }
    sessions.sort_by(|l, r| r.modified_epoch_secs.cmp(&l.modified_epoch_secs));
    Ok(sessions)
}

/// Format a Unix timestamp as a human-readable relative string ("5m ago").
pub(crate) fn format_relative_timestamp(epoch_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(epoch_secs);
    let elapsed = now.saturating_sub(epoch_secs);
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

/// Render a formatted session list for the `/sessions` command.
pub(crate) fn render_session_list(
    active_session_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let sessions = list_managed_sessions()?;
    let mut lines = vec![
        "Sessions".to_string(),
        format!("  Directory         {}", sessions_dir()?.display()),
    ];
    if sessions.is_empty() {
        lines.push("  No managed sessions saved yet.".to_string());
        return Ok(lines.join("\n"));
    }
    for session in sessions {
        let marker = if session.id == active_session_id {
            "● current"
        } else {
            "○ saved"
        };
        lines.push(format!(
            "  {id:<20} {marker:<10} {msgs:>3} msgs · updated {modified}",
            id = session.id,
            msgs = session.message_count,
            modified = format_relative_timestamp(session.modified_epoch_secs),
        ));
        lines.push(format!("    {}", session.path.display()));
    }
    Ok(lines.join("\n"))
}
