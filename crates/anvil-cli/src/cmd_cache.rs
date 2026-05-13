//! Handlers for `/file-cache` and `/cmd-cache` slash commands (Phase 1 / Bucket 1.5).
//!
//! Both commands are mirror images of each other: same subcommand vocabulary,
//! same output shape, and they share the formatting helpers in this module.
//!
//! Subcommands (both commands):
//!   - `stats`  — summary counts + bytes + most-recent timestamp (default)
//!   - `list`   — table of every cached entry
//!   - `forget <key>` — drop a single entry (path for file-cache, command for cmd-cache)
//!   - `prune`/`prune-stale` — remove stale entries, report count
//!   - `clear [--yes]` — drop every entry (gated by --yes confirmation)
//!   - `help`   — usage text
//!
//! These commands must return instantly — they run on the TUI input thread,
//! so they MUST NOT fire any LLM call or any blocking network/IO beyond a
//! handful of small file reads from the per-project cache directory.

use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::{
    CommandCacheEntry, CommandCacheManager, FileCacheEntry, FileCacheManager,
};

use crate::LiveCli;

// ─── Format helpers ──────────────────────────────────────────────────────────

/// Format a byte count as a short human-readable string ("0 B", "512 B",
/// "1.4 KB", "2.3 MB", "1.20 GB"). Uses powers of 1024.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Format a Unix timestamp as a human-friendly "N units ago" relative to now.
/// Returns "—" for zero/unset timestamps.
#[must_use]
pub(crate) fn format_ago(unix_secs: u64) -> String {
    if unix_secs == 0 {
        return "—".to_string();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now <= unix_secs {
        return "just now".to_string();
    }
    let diff = now - unix_secs;
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3_600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3_600)
    } else if diff < 86_400 * 2 {
        "yesterday".to_string()
    } else if diff < 86_400 * 30 {
        format!("{}d ago", diff / 86_400)
    } else if diff < 86_400 * 365 {
        format!("{}mo ago", diff / (86_400 * 30))
    } else {
        format!("{}y ago", diff / (86_400 * 365))
    }
}

/// Truncate a string to `max_chars` characters, appending an ellipsis when
/// truncation actually happens. Char-boundary safe.
#[must_use]
pub(crate) fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return s.chars().take(max_chars).collect();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// Same as `truncate_with_ellipsis` but keeps the TAIL of the string rather
/// than the head — useful for paths, where the filename is the part the user
/// recognises.
#[must_use]
pub(crate) fn truncate_with_ellipsis_tail(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return s.chars().rev().take(max_chars).collect::<Vec<_>>().into_iter().rev().collect();
    }
    let skip = count - (max_chars - 1);
    let mut out = String::from("…");
    out.extend(s.chars().skip(skip));
    out
}

/// Parse a `/file-cache` or `/cmd-cache` action string into (subcommand, args).
/// Returns ("", "") when the action is None or empty.
#[must_use]
pub(crate) fn parse_action(action: Option<&str>) -> (&str, &str) {
    let raw = action.unwrap_or("").trim();
    if raw.is_empty() {
        return ("", "");
    }
    let mut iter = raw.splitn(2, char::is_whitespace);
    let sub = iter.next().unwrap_or("");
    let rest = iter.next().unwrap_or("").trim();
    (sub, rest)
}

// ─── Project root resolution ─────────────────────────────────────────────────

/// Resolve a project root from the current working directory.
fn resolve_project_root() -> Result<PathBuf, String> {
    env::current_dir().map_err(|e| format!("Could not resolve current directory: {e}"))
}

// ─── File-cache rendering ────────────────────────────────────────────────────

fn render_file_cache_help() -> String {
    [
        "Usage:",
        "  /file-cache               same as `stats`",
        "  /file-cache stats         show entry count, total bytes, last update",
        "  /file-cache list          list every cached file with size + hit count",
        "  /file-cache forget <path> drop the entry for one file",
        "  /file-cache prune         remove entries whose files are gone or changed",
        "  /file-cache clear --yes   drop every entry (irreversible)",
        "  /file-cache help          show this usage text",
    ]
    .join("\n")
}

fn render_file_cache_stats(entries: &[FileCacheEntry]) -> String {
    let total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();
    let total_hits: u32 = entries.iter().map(|e| e.access_count).sum();
    let last_seen = entries.iter().map(|e| e.last_seen).max().unwrap_or(0);
    let mut out = String::new();
    out.push_str("File cache (per project)\n");
    out.push_str(&format!("  Entries     : {}\n", entries.len()));
    out.push_str(&format!(
        "  Cached size : {}\n",
        format_bytes(total_bytes)
    ));
    out.push_str(&format!("  Total hits  : {total_hits}\n"));
    out.push_str(&format!("  Last update : {}\n", format_ago(last_seen)));
    out.push_str("\nRun `/file-cache list` to see entries, `/file-cache help` for more.");
    out
}

fn render_file_cache_list(entries: &[FileCacheEntry]) -> String {
    if entries.is_empty() {
        return "File cache is empty.\nRun `/file-cache help` for usage.".to_string();
    }

    // Sort newest first so the most recently touched files appear at the top.
    let mut sorted: Vec<&FileCacheEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));

    // Truncate the path column to keep the row width reasonable.
    const PATH_MAX: usize = 60;
    let header = format!(
        "{:<60}  {:>10}  {:>10}  {:>6}",
        "PATH", "SIZE", "AGE", "HITS"
    );
    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');
    for entry in sorted {
        let path_str = entry.path.to_string_lossy().to_string();
        // Tail-truncate paths so the filename (the part users recognise) stays visible.
        let path_col = truncate_with_ellipsis_tail(&path_str, PATH_MAX);
        out.push_str(&format!(
            "{:<60}  {:>10}  {:>10}  {:>6}\n",
            path_col,
            format_bytes(entry.size_bytes),
            format_ago(entry.last_seen),
            entry.access_count,
        ));
    }
    out.push_str(&format!("\n{} entries.", entries.len()));
    out
}

// ─── Cmd-cache rendering ─────────────────────────────────────────────────────

fn render_cmd_cache_help() -> String {
    [
        "Usage:",
        "  /cmd-cache                  same as `stats`",
        "  /cmd-cache stats            show entry count, stale count, total bytes",
        "  /cmd-cache list             list every cached command with TTL + hits",
        "  /cmd-cache forget <cmd>     drop the entry for one command",
        "  /cmd-cache prune-stale      remove entries whose TTL has expired",
        "  /cmd-cache prune            alias for prune-stale",
        "  /cmd-cache clear --yes      drop every entry (irreversible)",
        "  /cmd-cache help             show this usage text",
    ]
    .join("\n")
}

fn render_cmd_cache_stats(entries: &[CommandCacheEntry]) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let total_hits: u32 = entries.iter().map(|e| e.hits).sum();
    let stale = entries
        .iter()
        .filter(|e| now.saturating_sub(e.captured_at) >= e.stale_after_secs)
        .count();
    let bytes: u64 = entries
        .iter()
        .map(|e| (e.stdout.len() + e.stderr.len()) as u64)
        .sum();
    let last = entries.iter().map(|e| e.captured_at).max().unwrap_or(0);
    let mut out = String::new();
    out.push_str("Command cache (per project)\n");
    out.push_str(&format!("  Entries     : {}\n", entries.len()));
    out.push_str(&format!("  Stale       : {stale}\n"));
    out.push_str(&format!("  Cached size : {}\n", format_bytes(bytes)));
    out.push_str(&format!("  Total hits  : {total_hits}\n"));
    out.push_str(&format!("  Last update : {}\n", format_ago(last)));
    out.push_str("\nRun `/cmd-cache list` to see entries, `/cmd-cache help` for more.");
    out
}

fn render_cmd_cache_list(entries: &[CommandCacheEntry]) -> String {
    if entries.is_empty() {
        return "Command cache is empty.\nRun `/cmd-cache help` for usage.".to_string();
    }

    let mut sorted: Vec<&CommandCacheEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));

    const CMD_MAX: usize = 60;
    let header = format!(
        "{:<60}  {:>10}  {:>10}  {:>6}",
        "COMMAND", "SIZE", "AGE", "HITS"
    );
    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');
    for entry in sorted {
        let size = (entry.stdout.len() + entry.stderr.len()) as u64;
        let cmd_col = truncate_with_ellipsis(&entry.command, CMD_MAX);
        out.push_str(&format!(
            "{:<60}  {:>10}  {:>10}  {:>6}\n",
            cmd_col,
            format_bytes(size),
            format_ago(entry.captured_at),
            entry.hits,
        ));
    }
    out.push_str(&format!("\n{} entries.", entries.len()));
    out
}

// ─── Pure handler entry points (used by both tests and LiveCli) ──────────────

/// Render the `/file-cache` response for an explicit project root.
/// All I/O goes through `FileCacheManager`; no LLM calls, no network.
pub(crate) fn run_file_cache_with_root(project_root: &Path, action: Option<&str>) -> String {
    let manager = match FileCacheManager::new(project_root.to_path_buf()) {
        Ok(m) => m,
        Err(e) => return format!("file-cache: failed to open cache: {e}"),
    };
    let (sub, rest) = parse_action(action);
    match sub {
        "" | "stats" => match manager.list() {
            Ok(entries) => render_file_cache_stats(&entries),
            Err(e) => format!("file-cache: failed to read cache: {e}"),
        },
        "list" => match manager.list() {
            Ok(entries) => render_file_cache_list(&entries),
            Err(e) => format!("file-cache: failed to read cache: {e}"),
        },
        "forget" => {
            if rest.is_empty() {
                return "file-cache: usage: /file-cache forget <path>".to_string();
            }
            let target = resolve_relative_path(project_root, rest);
            match manager.forget(&target) {
                Ok(()) => format!("file-cache: dropped entry for {}", target.display()),
                Err(e) => format!("file-cache: failed to forget {}: {e}", target.display()),
            }
        }
        "prune" | "prune-stale" => match manager.prune() {
            Ok(n) => format!("file-cache: pruned {n} stale entr{}.", if n == 1 { "y" } else { "ies" }),
            Err(e) => format!("file-cache: prune failed: {e}"),
        },
        "clear" => {
            if !rest.split_whitespace().any(|t| t == "--yes") {
                return "file-cache: clear will drop EVERY entry. Re-run as `/file-cache clear --yes` to confirm.".to_string();
            }
            match clear_file_cache(&manager) {
                Ok(n) => format!("file-cache: cleared {n} entr{}.", if n == 1 { "y" } else { "ies" }),
                Err(e) => format!("file-cache: clear failed: {e}"),
            }
        }
        "help" => render_file_cache_help(),
        other => format!(
            "file-cache: unknown subcommand `{other}`\n{}",
            render_file_cache_help()
        ),
    }
}

/// Render the `/cmd-cache` response for an explicit project root.
/// All I/O goes through `CommandCacheManager`; no LLM calls, no network.
pub(crate) fn run_cmd_cache_with_root(project_root: &Path, action: Option<&str>) -> String {
    let manager = match CommandCacheManager::new(project_root.to_path_buf()) {
        Ok(m) => m,
        Err(e) => return format!("cmd-cache: failed to open cache: {e}"),
    };
    let (sub, rest) = parse_action(action);
    match sub {
        "" | "stats" => match manager.list() {
            Ok(entries) => render_cmd_cache_stats(&entries),
            Err(e) => format!("cmd-cache: failed to read cache: {e}"),
        },
        "list" => match manager.list() {
            Ok(entries) => render_cmd_cache_list(&entries),
            Err(e) => format!("cmd-cache: failed to read cache: {e}"),
        },
        "forget" => {
            if rest.is_empty() {
                return "cmd-cache: usage: /cmd-cache forget <command>".to_string();
            }
            // The cache key normalises whitespace; use project_root as cwd so users
            // don't need to remember the captured cwd.
            match manager.forget(rest, project_root) {
                Ok(()) => format!("cmd-cache: dropped entry for `{rest}`"),
                Err(e) => format!("cmd-cache: failed to forget `{rest}`: {e}"),
            }
        }
        "prune" | "prune-stale" => match manager.prune_stale() {
            Ok(n) => format!("cmd-cache: pruned {n} stale entr{}.", if n == 1 { "y" } else { "ies" }),
            Err(e) => format!("cmd-cache: prune failed: {e}"),
        },
        "clear" => {
            if !rest.split_whitespace().any(|t| t == "--yes") {
                return "cmd-cache: clear will drop EVERY entry. Re-run as `/cmd-cache clear --yes` to confirm.".to_string();
            }
            match clear_cmd_cache(&manager) {
                Ok(n) => format!("cmd-cache: cleared {n} entr{}.", if n == 1 { "y" } else { "ies" }),
                Err(e) => format!("cmd-cache: clear failed: {e}"),
            }
        }
        "help" => render_cmd_cache_help(),
        other => format!(
            "cmd-cache: unknown subcommand `{other}`\n{}",
            render_cmd_cache_help()
        ),
    }
}

// ─── Clear helpers (no public API on the managers, so iterate + forget) ──────

fn clear_file_cache(manager: &FileCacheManager) -> Result<usize, String> {
    let entries = manager
        .list()
        .map_err(|e| format!("list failed: {e}"))?;
    let mut count = 0usize;
    for entry in entries {
        if manager.forget(&entry.path).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn clear_cmd_cache(manager: &CommandCacheManager) -> Result<usize, String> {
    let entries = manager
        .list()
        .map_err(|e| format!("list failed: {e}"))?;
    let mut count = 0usize;
    for entry in entries {
        if manager.forget(&entry.command, &entry.cwd).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

// ─── Path resolution for `/file-cache forget` ────────────────────────────────

/// Resolve a (possibly relative) user-supplied path against `project_root`.
/// Returns the path even when it doesn't exist on disk — `forget` accepts
/// non-existent paths and silently no-ops (which is what we want).
fn resolve_relative_path(project_root: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        return p;
    }
    project_root.join(p)
}

// ─── LiveCli wiring ──────────────────────────────────────────────────────────

impl LiveCli {
    /// `/file-cache` handler (REPL and batch).
    pub(crate) fn run_file_cache_command(&self, action: Option<&str>) -> String {
        let project_root = match resolve_project_root() {
            Ok(p) => p,
            Err(e) => return format!("file-cache: {e}"),
        };
        run_file_cache_with_root(&project_root, action)
    }

    /// `/cmd-cache` handler (REPL and batch).
    pub(crate) fn run_cmd_cache_command(&self, action: Option<&str>) -> String {
        let project_root = match resolve_project_root() {
            Ok(p) => p,
            Err(e) => return format!("cmd-cache: {e}"),
        };
        run_cmd_cache_with_root(&project_root, action)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::{CommandCacheManager, FileCacheManager};
    use serial_test::serial;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    // The cache managers compute their per-project cache directory from
    // `ANVIL_CONFIG_HOME` (cmd cache) and `HOME` (file cache). Other tests in
    // the workspace mutate `ANVIL_CONFIG_HOME` (cc_139_f1_tests) — serialize
    // every cache-touching test against that env var to avoid races where two
    // managers end up pointing at different on-disk directories.

    // ── helpers ──────────────────────────────────────────────────────────────

    fn tmp_project() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    fn make_file_manager(proj: &TempDir) -> FileCacheManager {
        FileCacheManager::new(proj.path().to_path_buf()).expect("file mgr")
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, content).expect("write");
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    // ── format helpers ───────────────────────────────────────────────────────

    #[test]
    fn format_bytes_covers_magnitudes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(2048).ends_with("KB"));
        assert!(format_bytes(5 * 1024 * 1024).ends_with("MB"));
        assert!(format_bytes(3 * 1024 * 1024 * 1024).ends_with("GB"));
    }

    #[test]
    fn format_ago_handles_zero_and_recent() {
        assert_eq!(format_ago(0), "—");
        let now = now_secs();
        assert_eq!(format_ago(now.saturating_sub(5)), "5s ago");
        assert!(format_ago(now.saturating_sub(120)).ends_with("m ago"));
    }

    #[test]
    fn truncate_with_ellipsis_keeps_short_strings() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        let t = truncate_with_ellipsis("hello world how are you", 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_with_ellipsis_tail_keeps_tail() {
        assert_eq!(truncate_with_ellipsis_tail("hello", 10), "hello");
        let t = truncate_with_ellipsis_tail("/very/long/path/to/some/file.txt", 16);
        assert_eq!(t.chars().count(), 16);
        assert!(t.starts_with('…'));
        assert!(t.ends_with("file.txt"));
    }

    #[test]
    fn parse_action_splits_subcommand_and_rest() {
        assert_eq!(parse_action(None), ("", ""));
        assert_eq!(parse_action(Some("")), ("", ""));
        assert_eq!(parse_action(Some("stats")), ("stats", ""));
        assert_eq!(parse_action(Some("forget /tmp/foo.rs")), ("forget", "/tmp/foo.rs"));
        assert_eq!(parse_action(Some("clear --yes")), ("clear", "--yes"));
    }

    // ── /file-cache tests ────────────────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_stats_reports_empty_when_no_cache() {
        let proj = tmp_project();
        let out = run_file_cache_with_root(proj.path(), Some("stats"));
        assert!(out.contains("File cache"));
        assert!(out.contains("Entries     : 0"));
        assert!(out.contains("Cached size : 0 B"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_stats_reflects_stored_entries() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        for i in 0..3u8 {
            let f = proj.path().join(format!("f{i}.txt"));
            write_file(&f, &format!("hello {i}"));
            mgr.store(&f, None, vec![]).expect("store");
        }
        let out = run_file_cache_with_root(proj.path(), Some("stats"));
        assert!(out.contains("Entries     : 3"), "got: {out}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_list_renders_all_entries() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        let a = proj.path().join("alpha.txt");
        let b = proj.path().join("beta.txt");
        write_file(&a, "a");
        write_file(&b, "bb");
        mgr.store(&a, None, vec![]).expect("store a");
        mgr.store(&b, None, vec![]).expect("store b");
        let out = run_file_cache_with_root(proj.path(), Some("list"));
        assert!(out.contains("alpha.txt"), "missing alpha.txt: {out}");
        assert!(out.contains("beta.txt"), "missing beta.txt: {out}");
        assert!(out.contains("2 entries"), "missing count: {out}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_forget_removes_named_entry() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        let f = proj.path().join("removeme.txt");
        write_file(&f, "bye");
        mgr.store(&f, None, vec![]).expect("store");
        assert_eq!(mgr.list().expect("list").len(), 1);

        let out = run_file_cache_with_root(
            proj.path(),
            Some(&format!("forget {}", f.display())),
        );
        assert!(out.contains("dropped entry"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_prune_removes_stale() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        let f = proj.path().join("ephemeral.txt");
        write_file(&f, "bye");
        mgr.store(&f, None, vec![]).expect("store");
        fs::remove_file(&f).expect("rm");
        let out = run_file_cache_with_root(proj.path(), Some("prune"));
        assert!(out.contains("pruned 1"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_clear_empties_all() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        for i in 0..3u8 {
            let f = proj.path().join(format!("c{i}.txt"));
            write_file(&f, "x");
            mgr.store(&f, None, vec![]).expect("store");
        }
        assert_eq!(mgr.list().expect("list").len(), 3);
        let out = run_file_cache_with_root(proj.path(), Some("clear --yes"));
        assert!(out.contains("cleared 3"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_clear_requires_confirmation() {
        let proj = tmp_project();
        let mgr = make_file_manager(&proj);
        let f = proj.path().join("keep.txt");
        write_file(&f, "x");
        mgr.store(&f, None, vec![]).expect("store");
        let out = run_file_cache_with_root(proj.path(), Some("clear"));
        assert!(out.contains("--yes"), "got: {out}");
        // Confirm: nothing was actually deleted.
        assert_eq!(mgr.list().expect("list").len(), 1);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_help_prints_usage() {
        let proj = tmp_project();
        let out = run_file_cache_with_root(proj.path(), Some("help"));
        assert!(out.contains("/file-cache"));
        assert!(out.contains("stats"));
        assert!(out.contains("forget"));
    }

    // ── /cmd-cache tests ─────────────────────────────────────────────────────

    fn make_cmd_manager(proj: &TempDir) -> CommandCacheManager {
        // Use the same project_path_hash-keyed default location so that
        // run_cmd_cache_with_root and our direct manager observe the same cache_dir.
        CommandCacheManager::new(proj.path().to_path_buf()).expect("cmd mgr")
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_stats_reports_empty_when_no_cache() {
        let proj = tmp_project();
        let out = run_cmd_cache_with_root(proj.path(), Some("stats"));
        assert!(out.contains("Command cache"));
        assert!(out.contains("Entries     : 0"), "got: {out}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_stats_reflects_stored_entries() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        mgr.store("ls -la", cwd, "a\nb".to_string(), String::new(), 0, 5)
            .expect("store");
        mgr.store("git status", cwd, "clean".to_string(), String::new(), 0, 5)
            .expect("store");
        let out = run_cmd_cache_with_root(proj.path(), Some("stats"));
        assert!(out.contains("Entries     : 2"), "got: {out}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_list_renders_all_entries() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        mgr.store("ls -la", cwd, "a".to_string(), String::new(), 0, 5)
            .expect("store");
        mgr.store("git status", cwd, "b".to_string(), String::new(), 0, 5)
            .expect("store");
        let out = run_cmd_cache_with_root(proj.path(), Some("list"));
        assert!(out.contains("ls -la"), "missing ls: {out}");
        assert!(out.contains("git status"), "missing git: {out}");
        assert!(out.contains("2 entries"), "missing count: {out}");
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_forget_removes_named_entry() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        mgr.store("ls -la", cwd, "out".to_string(), String::new(), 0, 5)
            .expect("store");
        assert_eq!(mgr.list().expect("list").len(), 1);
        let out = run_cmd_cache_with_root(proj.path(), Some("forget ls -la"));
        assert!(out.contains("dropped entry"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_prune_removes_stale() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        // Manually plant a stale entry by direct write — captured_at far in the past
        // with a TTL that is already exceeded.
        fs::create_dir_all(&mgr.cache_dir).expect("mkdir");
        let stale = runtime::CommandCacheEntry {
            command: "ls -la".to_string(),
            cwd: cwd.to_path_buf(),
            stdout: "x".to_string(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 5,
            captured_at: now_secs().saturating_sub(10_000),
            touched_files: vec![],
            stale_after_secs: 60,
            hits: 0,
        };
        let json = serde_json::to_string_pretty(&stale).unwrap();
        // Use a hash-based filename that mirrors the cache key — easiest is to
        // write through the manager's API: store, then mutate captured_at via re-write.
        // Simpler: store, then overwrite the entry json on disk.
        mgr.store("ls -la", cwd, "x".to_string(), String::new(), 0, 5)
            .expect("store");
        // Find the just-written entry file and overwrite with the stale json.
        let entries: Vec<_> = fs::read_dir(&mgr.cache_dir)
            .expect("readdir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        assert_eq!(entries.len(), 1);
        fs::write(entries[0].path(), &json).expect("rewrite");

        let out = run_cmd_cache_with_root(proj.path(), Some("prune"));
        assert!(out.contains("pruned 1"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_clear_empties_all() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        for i in 0..3u8 {
            mgr.store(
                &format!("ls /tmp/dir{i}"),
                cwd,
                "x".to_string(),
                String::new(),
                0,
                5,
            )
            .expect("store");
        }
        assert_eq!(mgr.list().expect("list").len(), 3);
        let out = run_cmd_cache_with_root(proj.path(), Some("clear --yes"));
        assert!(out.contains("cleared 3"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_clear_requires_confirmation() {
        let proj = tmp_project();
        let mgr = make_cmd_manager(&proj);
        let cwd = proj.path();
        mgr.store("ls", cwd, "x".to_string(), String::new(), 0, 5)
            .expect("store");
        let out = run_cmd_cache_with_root(proj.path(), Some("clear"));
        assert!(out.contains("--yes"), "got: {out}");
        assert_eq!(mgr.list().expect("list").len(), 1);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_help_prints_usage() {
        let proj = tmp_project();
        let out = run_cmd_cache_with_root(proj.path(), Some("help"));
        assert!(out.contains("/cmd-cache"));
        assert!(out.contains("stats"));
        assert!(out.contains("prune-stale"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn cmd_cache_unknown_subcommand_falls_back_to_help() {
        let proj = tmp_project();
        let out = run_cmd_cache_with_root(proj.path(), Some("bogus"));
        assert!(out.contains("unknown subcommand"));
        assert!(out.contains("/cmd-cache"));
    }

    #[test]
    #[serial(anvil_config_home)]
    fn file_cache_unknown_subcommand_falls_back_to_help() {
        let proj = tmp_project();
        let out = run_file_cache_with_root(proj.path(), Some("bogus"));
        assert!(out.contains("unknown subcommand"));
        assert!(out.contains("/file-cache"));
    }
}
