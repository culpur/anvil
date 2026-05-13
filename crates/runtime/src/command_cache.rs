// Phase 4.2: env::set_var in tests requires unsafe under Rust 2024.
#![cfg_attr(test, allow(unsafe_code))]

/// Command-output cache for Anvil (v2.3 W12 — token economy).
///
/// Caches the stdout/stderr of read-only shell commands so that repeated
/// identical invocations within a session (or across sessions while files have
/// not changed) do not re-execute the command.
///
/// # Storage layout
/// ```text
/// ~/.anvil/projects/<project-hash>/cmd-cache/<command-hash>.json
/// ```
/// where `command-hash = sha256(command + cwd)` (first 16 hex chars).
///
/// # Invalidation
/// An entry is considered stale when:
/// - `now() - captured_at >= stale_after_secs`  (TTL expiry), OR
/// - any file in `touched_files` has `mtime > captured_at` (file-watch).
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::default_config_home;

// ─── Public types ─────────────────────────────────────────────────────────────

/// A single cached command result.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommandCacheEntry {
    /// Canonical command string (trimmed, normalised whitespace).
    pub command: String,
    /// Working directory the command was run in.
    pub cwd: PathBuf,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: i32,
    /// Wall-clock duration of the original command in milliseconds.
    pub duration_ms: u64,
    /// Unix timestamp (seconds) when the entry was first stored.
    pub captured_at: u64,
    /// Files whose mtime is checked at lookup to detect invalidation.
    pub touched_files: Vec<PathBuf>,
    /// TTL hint in seconds.  Default varies per command class (see below).
    pub stale_after_secs: u64,
    /// How many times this entry has been served from cache.
    pub hits: u32,
}

/// Aggregated statistics about the cache for a project.
#[derive(Debug, Default, Clone)]
pub struct CommandCacheStats {
    pub total_entries: usize,
    pub stale_entries: usize,
    pub total_hits: u32,
    pub total_size_bytes: u64,
}

/// All error variants that can come from the cache manager.
#[derive(Debug)]
pub enum CommandCacheError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The cache directory could not be created.
    DirCreate(std::io::Error),
}

impl std::fmt::Display for CommandCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "command-cache I/O error: {e}"),
            Self::Json(e) => write!(f, "command-cache JSON error: {e}"),
            Self::DirCreate(e) => write!(f, "command-cache dir create error: {e}"),
        }
    }
}

impl From<std::io::Error> for CommandCacheError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for CommandCacheError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

// ─── Phase 4.2 (L7 §9) size-cap config ───────────────────────────────────────

/// Phase 4.2: env var that overrides the command-cache size cap (in MB).
/// Default 50. Set to 0 to disable size-cap eviction.
pub const CMD_CACHE_MAX_MB_ENV: &str = "ANVIL_CMD_CACHE_MAX_MB";

/// Phase 4.2: default size cap, in MB.
pub const DEFAULT_CMD_CACHE_MAX_MB: u64 = 50;

// ─── Per-session hot-path tracker ─────────────────────────────────────────────

/// Tracks how many times each `(canonical_command, cwd)` pair has been looked
/// up as cacheable during the current process lifetime.  When the count reaches
/// `HOT_PATH_THRESHOLD` the pair is logged once with a `[command-cache] hot path`
/// prefix (seed for W14 pattern-promote).
static SESSION_INVOCATION_COUNTS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();

const HOT_PATH_THRESHOLD: u32 = 3;

fn session_counter() -> &'static Mutex<HashMap<String, u32>> {
    SESSION_INVOCATION_COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_cacheable_invocation(command: &str, cwd: &Path) {
    let key = format!("{}|{}", command, cwd.display());
    let mut map = session_counter().lock().unwrap_or_else(|e| e.into_inner());
    let count = map.entry(key.clone()).or_insert(0);
    *count += 1;
    if *count == HOT_PATH_THRESHOLD {
        eprintln!("[command-cache] hot path: {command}");
    }
}

// ─── Cacheability rules ────────────────────────────────────────────────────────

/// Returns `true` if the command result is safe to cache.
///
/// "Safe" means: read-only, deterministic, no mutation of filesystem/processes.
/// When in doubt this function returns `false`.
#[must_use]
pub fn is_cacheable(command: &str) -> bool {
    let cmd = command.trim();
    if cmd.is_empty() {
        return false;
    }

    // Hard-reject anything containing shell metacharacters that imply writes:
    // redirects (> >>), pipes-to-tee, in-place flags.
    if contains_write_metachar(cmd) {
        return false;
    }

    // Reject compound commands: split on &&, ||, ;, |, & and check each part.
    // If any part is non-cacheable, the whole compound is non-cacheable.
    if is_compound(cmd) {
        for part in split_compound(cmd) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if !is_cacheable_simple(part) {
                return false;
            }
        }
        return true;
    }

    is_cacheable_simple(cmd)
}

/// Check a simple (non-compound) command string for cacheability.
fn is_cacheable_simple(cmd: &str) -> bool {
    let effective = strip_wrappers(cmd);
    let token = leading_token(effective);

    // Non-cacheable leading tokens (mutators).
    const MUTATORS: &[&str] = &[
        "rm", "mv", "cp", "mkdir", "rmdir", "touch", "chmod", "chown",
        "kill", "killall", "dd", "tee", "truncate", "install", "unlink",
        "ln", "mkfifo", "mknod", "shred", "wipe",
    ];
    if MUTATORS.contains(&token) {
        return false;
    }

    // Non-cacheable: npm install / run, pnpm i, cargo build/run/test/install/publish/clean, make, etc.
    const BUILD_TOOLS: &[&str] = &[
        "make", "gmake", "cmake", "ninja", "bazel", "gradle", "mvn", "ant",
    ];
    if BUILD_TOOLS.contains(&token) {
        return false;
    }

    // cargo: only specific read subcommands are cacheable.
    if token == "cargo" {
        let rest = after_token(effective, "cargo");
        let sub = leading_token(rest);
        const CARGO_CACHE_OK: &[&str] = &["tree", "metadata", "--version", "version"];
        return CARGO_CACHE_OK.contains(&sub);
    }

    // npm: only ls / view.
    if token == "npm" {
        let rest = after_token(effective, "npm");
        let sub = leading_token(rest);
        const NPM_CACHE_OK: &[&str] = &["ls", "list", "view", "info", "--version", "version"];
        return NPM_CACHE_OK.contains(&sub);
    }

    // pnpm: only list / view.
    if token == "pnpm" {
        let rest = after_token(effective, "pnpm");
        let sub = leading_token(rest);
        const PNPM_CACHE_OK: &[&str] = &["list", "ls", "view", "info", "--version", "version"];
        return PNPM_CACHE_OK.contains(&sub);
    }

    // yarn: only info.
    if token == "yarn" {
        let rest = after_token(effective, "yarn");
        let sub = leading_token(rest);
        const YARN_CACHE_OK: &[&str] = &["info", "list", "--version", "version"];
        return YARN_CACHE_OK.contains(&sub);
    }

    // git: only specific read subcommands.
    if token == "git" {
        let rest = after_token(effective, "git");
        let sub = leading_token(rest);
        const GIT_CACHE_OK: &[&str] = &[
            "status", "log", "diff", "show", "ls-files", "config",
            "remote", "rev-parse", "branch", "tag",
        ];
        // Additional check: reject --write / --global-write style flags.
        if rest.contains("--write") || rest.contains("--replace-all") {
            return false;
        }
        return GIT_CACHE_OK.contains(&sub);
    }

    // Reject git write operations mentioned inline (extra safety for compound paths).
    const GIT_WRITE_SUBS: &[&str] = &[
        "push", "pull", "fetch", "commit", "merge", "rebase", "reset",
        "checkout", "stash", "apply", "cherry-pick", "am", "revert",
        "bisect", "clean",
    ];
    if token == "git" {
        let rest = after_token(effective, "git");
        let sub = leading_token(rest);
        if GIT_WRITE_SUBS.contains(&sub) {
            return false;
        }
    }

    // find: only cacheable if it does NOT contain -delete or -exec.
    if token == "find" {
        let rest = after_token(effective, "find");
        if rest.contains("-delete") || rest.contains("-exec") || rest.contains("-execdir") {
            return false;
        }
        return true;
    }

    // Reject sources of non-determinism.
    if has_nondeterminism(effective) {
        return false;
    }

    // Version commands: <program> --version or -V.
    if is_version_flag_only(effective) {
        return true;
    }

    // Allow-listed read-only leading tokens.
    const READ_ONLY: &[&str] = &[
        "ls", "cat", "head", "tail", "wc", "file", "stat", "du", "df",
        "grep", "rg", "ag", "ack", "egrep", "fgrep",
        "which", "whereis", "type",
        "env", "printenv", "echo", "pwd",
        "python", "python3", "node", "rustc", "ruby", "go", "java",
        "command",
    ];
    if READ_ONLY.contains(&token) {
        return true;
    }

    false
}

/// Strip transparent wrappers (sudo, time, nice, timeout, nohup, stdbuf, xargs)
/// to reveal the actual command token.
fn strip_wrappers(cmd: &str) -> &str {
    const WRAPPERS: &[&str] = &[
        "sudo", "time", "nice", "timeout", "nohup", "stdbuf", "xargs",
        "ionice", "taskset", "env",
    ];
    // Wrappers that take a mandatory non-flag positional argument before the real command.
    const WRAPPERS_WITH_POS_ARG: &[&str] = &["timeout", "nice", "ionice", "taskset"];
    let mut remaining = cmd.trim();
    loop {
        let token = leading_token(remaining);
        if !WRAPPERS.contains(&token) {
            break;
        }
        // Advance past the wrapper token.
        let mut iter = remaining.splitn(2, char::is_whitespace);
        iter.next(); // wrapper token
        remaining = iter.next().unwrap_or("").trim();
        // Skip wrapper flags (tokens starting with '-').
        loop {
            let next = leading_token(remaining);
            if next.starts_with('-') {
                let mut fi = remaining.splitn(2, char::is_whitespace);
                fi.next();
                let after_flag = fi.next().unwrap_or("").trim();
                let next2 = leading_token(after_flag);
                // Flag with value: e.g. "--user foo" where "foo" doesn't start with '-'
                if !next2.starts_with('-') && !next2.is_empty() {
                    remaining = after_flag.splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim();
                } else {
                    remaining = after_flag;
                }
            } else {
                break;
            }
        }
        // For wrappers that take a mandatory positional arg (like `timeout 5`),
        // skip one non-flag token if it looks like a number or duration.
        if WRAPPERS_WITH_POS_ARG.contains(&token) {
            let next = leading_token(remaining);
            if !next.is_empty() && !next.starts_with('-') && next.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                let mut iter2 = remaining.splitn(2, char::is_whitespace);
                iter2.next(); // skip the numeric arg
                remaining = iter2.next().unwrap_or("").trim();
            }
        }
    }
    remaining
}

fn leading_token(s: &str) -> &str {
    s.trim().splitn(2, char::is_whitespace).next().unwrap_or("")
}

fn after_token<'a>(s: &'a str, _token: &str) -> &'a str {
    s.trim().splitn(2, char::is_whitespace).nth(1).unwrap_or("").trim()
}

/// Returns `true` if the command string contains shell metacharacters that
/// imply a write operation.
fn contains_write_metachar(cmd: &str) -> bool {
    // > or >> redirects (not inside single-quoted strings, best effort).
    // |tee , --write, --in-place, -i (sed/perl), -w.
    cmd.contains(">>")
        || cmd.contains(">|")
        // `>` not preceded by `<` (avoid <<EOF) and not `>=`.
        || contains_redirect_gt(cmd)
        || cmd.contains("|tee")
        || cmd.contains("| tee")
        || cmd.contains("--write")
        || cmd.contains("--in-place")
        // sed/perl -i (in-place): look for " -i " or " -i$" pattern.
        || cmd.contains(" -i ")
        || cmd.ends_with(" -i")
}

fn contains_redirect_gt(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'>' {
            let prev = if i > 0 { bytes[i - 1] } else { 0 };
            let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
            // Ignore >= (comparison), >> (already caught above), <>
            if next == b'>' || prev == b'<' || next == b'=' {
                continue;
            }
            return true;
        }
    }
    false
}

/// Returns `true` if `cmd` contains compound-command separators (&&, ||, ;, |, &).
fn is_compound(cmd: &str) -> bool {
    cmd.contains("&&")
        || cmd.contains("||")
        || cmd.contains(';')
        || cmd.contains(" | ")
        || cmd.contains("|(")
        || (cmd.contains('|') && !cmd.contains("|tee"))
        || cmd.ends_with('&')
        || cmd.contains(" & ")
}

/// Split a compound command into its component parts.
fn split_compound(cmd: &str) -> Vec<&str> {
    // Naive split on ; | && || & — good enough for the safety check.
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b';' | b'|' | b'&' => {
                parts.push(&cmd[start..i]);
                // Skip over multi-char separators.
                if i + 1 < bytes.len() && (bytes[i + 1] == b'|' || bytes[i + 1] == b'&') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < cmd.len() {
        parts.push(&cmd[start..]);
    }
    parts
}

/// Returns `true` if the command contains $RANDOM, `date +%N`, or `uuidgen`.
fn has_nondeterminism(cmd: &str) -> bool {
    cmd.contains("$RANDOM")
        || cmd.contains("date +%N")
        || cmd.contains("uuidgen")
        || cmd.contains("$(")  // command substitution — conservatively non-deterministic
        || cmd.contains("${RANDOM")
}

/// `<prog> --version` or `<prog> -V` — always read-only.
fn is_version_flag_only(cmd: &str) -> bool {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    parts.len() >= 2
        && (parts[1] == "--version" || parts[1] == "-V" || parts[1] == "version")
}

// ─── TTL defaults ──────────────────────────────────────────────────────────────

/// Returns the default TTL in seconds for a command.
#[must_use]
pub fn default_ttl(command: &str) -> u64 {
    let effective = strip_wrappers(command.trim());
    let token = leading_token(effective);

    if token == "git" {
        let rest = after_token(effective, "git");
        let sub = leading_token(rest);
        return match sub {
            "status" | "diff" => 60,
            "log" | "show" | "ls-files" => 600,
            _ => 300,
        };
    }

    if token == "cargo" || token == "npm" || token == "pnpm" || token == "yarn" {
        let rest = after_token(effective, token);
        let sub = leading_token(rest);
        if sub == "tree" || sub == "list" || sub == "ls" || sub == "metadata" {
            return 1800;
        }
    }

    match token {
        "ls" | "find" => 300,
        "cat" | "head" | "tail" | "wc" => 600,
        _ => 300,
    }
}

// ─── touched_files heuristic ──────────────────────────────────────────────────

/// Infer which files a command reads so we can check their mtimes at lookup.
#[must_use]
pub fn infer_touched_files(command: &str, cwd: &Path) -> Vec<PathBuf> {
    let effective = strip_wrappers(command.trim());
    let token = leading_token(effective);
    let rest = after_token(effective, token);

    match token {
        "cat" | "head" | "tail" | "wc" => {
            // Collect non-flag arguments as file paths.
            rest.split_whitespace()
                .filter(|a| !a.starts_with('-'))
                .map(|p| {
                    let pb = PathBuf::from(p);
                    if pb.is_absolute() { pb } else { cwd.join(p) }
                })
                .collect()
        }
        "git" => {
            let sub = leading_token(rest);
            match sub {
                "status" | "diff" => vec![
                    cwd.join(".git/HEAD"),
                    cwd.join(".git/index"),
                ],
                "log" | "show" | "rev-parse" | "branch" | "tag" => vec![
                    cwd.join(".git/HEAD"),
                    cwd.join(".git/refs"),
                ],
                _ => vec![],
            }
        }
        "cargo" => {
            let sub = leading_token(rest);
            if sub == "tree" || sub == "metadata" {
                vec![
                    cwd.join("Cargo.toml"),
                    cwd.join("Cargo.lock"),
                ]
            } else {
                vec![]
            }
        }
        "npm" | "pnpm" | "yarn" => {
            let sub = leading_token(rest);
            if matches!(sub, "ls" | "list" | "view" | "info") {
                vec![
                    cwd.join("package.json"),
                    cwd.join("package-lock.json"),
                ]
            } else {
                vec![]
            }
        }
        "find" | "ls" => {
            let first_non_flag = rest
                .split_whitespace()
                .find(|a| !a.starts_with('-'))
                .map(|p| {
                    let pb = PathBuf::from(p);
                    if pb.is_absolute() { pb } else { cwd.join(p) }
                });
            first_non_flag.map(|p| vec![p]).unwrap_or_default()
        }
        _ => vec![],
    }
}

// ─── Key computation ───────────────────────────────────────────────────────────

/// Normalise a command string: trim + collapse internal whitespace.
fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute a 16-hex-char hash that identifies `(command, cwd)` uniquely.
fn command_hash(command: &str, cwd: &Path) -> String {
    let canonical = normalize_command(command);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher.update(b"\0");
    hasher.update(cwd.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    hex_encode(&result[..8])
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── CommandCacheManager ──────────────────────────────────────────────────────

/// Manages the on-disk command-output cache for a single project.
pub struct CommandCacheManager {
    pub project_root: PathBuf,
    pub cache_dir: PathBuf,
}

impl CommandCacheManager {
    /// Create a new manager for `project_root`.
    ///
    /// The cache directory (`~/.anvil/projects/<hash>/cmd-cache/`) is created
    /// lazily on first `store` call, not here.
    pub fn new(project_root: PathBuf) -> Result<Self, CommandCacheError> {
        use crate::memory::project_path_hash;
        let canonical = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone());
        let hash = project_path_hash(&canonical);
        let cache_dir = default_config_home()
            .join("projects")
            .join(hash)
            .join("cmd-cache");
        Ok(Self { project_root, cache_dir })
    }

    /// Look up a cached result for `(command, cwd)`.
    ///
    /// Returns `Some` only when:
    /// - the entry exists on disk,
    /// - it has not exceeded its TTL, and
    /// - none of its `touched_files` have been modified since the entry was stored.
    ///
    /// Records a session invocation for hot-path detection.
    ///
    /// Phase 3.5 (SECURITY): silently returns `Ok(None)` when `cwd` is
    /// inside the vault directory.
    pub fn lookup(
        &self,
        command: &str,
        cwd: &Path,
    ) -> Result<Option<CommandCacheEntry>, CommandCacheError> {
        if crate::file_cache::is_l5_path(cwd) {
            return Ok(None);
        }
        let canonical = normalize_command(command);
        record_cacheable_invocation(&canonical, cwd);

        let path = self.entry_path(&canonical, cwd);
        if !path.exists() {
            return Ok(None);
        }

        let raw = std::fs::read_to_string(&path)?;
        let mut entry: CommandCacheEntry = serde_json::from_str(&raw)?;

        // TTL check.
        let now = now_secs();
        if now.saturating_sub(entry.captured_at) >= entry.stale_after_secs {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }

        // cwd must match exactly.
        if entry.cwd != cwd {
            return Ok(None);
        }

        // File-watch invalidation.
        for file in &entry.touched_files {
            if let Ok(meta) = std::fs::metadata(file) {
                if let Ok(modified) = meta.modified() {
                    let mtime = modified
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if mtime > entry.captured_at {
                        let _ = std::fs::remove_file(&path);
                        return Ok(None);
                    }
                }
            }
        }

        // Cache hit — increment counter and persist.
        entry.hits = entry.hits.saturating_add(1);
        // Best-effort persist of updated hits; ignore errors.
        if let Ok(updated) = serde_json::to_string_pretty(&entry) {
            let _ = atomic_write(&path, updated.as_bytes());
        }

        eprintln!("[command-cache] hit: {canonical}");
        Ok(Some(entry))
    }

    /// Store a command result in the cache.
    ///
    /// Automatically infers `touched_files` and `stale_after_secs`.
    ///
    /// Phase 3.5 (SECURITY): silently skips storage when `cwd` is inside
    /// the vault directory. The `touched_files` list is also filtered so
    /// no L5 path ends up in any cache entry — a `debug_assert!` enforces
    /// this in debug builds so a misclassified touched_file caller is
    /// caught immediately.
    pub fn store(
        &self,
        command: &str,
        cwd: &Path,
        stdout: String,
        stderr: String,
        exit_code: i32,
        duration_ms: u64,
    ) -> Result<CommandCacheEntry, CommandCacheError> {
        if crate::file_cache::is_l5_path(cwd) {
            // Silent skip — return a stub entry so the caller's contract
            // (Ok(entry)) is preserved without writing anything to disk.
            return Ok(CommandCacheEntry {
                command: normalize_command(command),
                cwd: cwd.to_path_buf(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 0,
                captured_at: 0,
                touched_files: Vec::new(),
                stale_after_secs: 0,
                hits: 0,
            });
        }
        let canonical = normalize_command(command);
        let mut touched_files = infer_touched_files(&canonical, cwd);
        // Filter any L5 paths defensively. If a caller hands us an L5
        // touched_file, scrub it before the entry hits disk.
        touched_files.retain(|p| !crate::file_cache::is_l5_path(p));
        let stale_after_secs = default_ttl(&canonical);

        let entry = CommandCacheEntry {
            command: canonical.clone(),
            cwd: cwd.to_path_buf(),
            stdout,
            stderr,
            exit_code,
            duration_ms,
            captured_at: now_secs(),
            touched_files,
            stale_after_secs,
            hits: 0,
        };

        // L7 invariant: no L5 path in touched_files. The filter above
        // SHOULD have caught all cases; this assert catches any future
        // bug that bypasses the filter.
        debug_assert!(
            !entry.touched_files.iter().any(|p| crate::file_cache::is_l5_path(p)),
            "L7 cache stored entry with L5 path in touched_files: {:?}",
            entry.touched_files
        );

        std::fs::create_dir_all(&self.cache_dir).map_err(CommandCacheError::DirCreate)?;
        let path = self.entry_path(&canonical, cwd);
        let serialized = serde_json::to_string_pretty(&entry)?;
        atomic_write(&path, serialized.as_bytes())?;

        // Phase 4.2 (L7 §9): enforce size cap after every store. Failures
        // are non-fatal — the cache is advisory.
        if let Some(cap) = Self::size_cap_bytes() {
            if let Ok(count) = self.enforce_size_cap(cap) {
                if count > 0 {
                    eprintln!("[command-cache] LRU eviction: removed {count} entry/entries to fit {cap}-byte cap");
                }
            }
        }

        Ok(entry)
    }

    /// Remove the cached entry for `(command, cwd)` if it exists.
    pub fn forget(&self, command: &str, cwd: &Path) -> Result<(), CommandCacheError> {
        let canonical = normalize_command(command);
        let path = self.entry_path(&canonical, cwd);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// List all entries (including stale) in the cache directory.
    pub fn list(&self) -> Result<Vec<CommandCacheEntry>, CommandCacheError> {
        if !self.cache_dir.exists() {
            return Ok(vec![]);
        }
        let mut entries = Vec::new();
        for de in std::fs::read_dir(&self.cache_dir)? {
            let de = de?;
            if de.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read_to_string(de.path())?;
            if let Ok(entry) = serde_json::from_str::<CommandCacheEntry>(&raw) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Return aggregate stats for the cache.
    pub fn stats(&self) -> Result<CommandCacheStats, CommandCacheError> {
        let entries = self.list()?;
        let now = now_secs();
        let mut stats = CommandCacheStats::default();
        stats.total_entries = entries.len();
        for entry in &entries {
            stats.total_hits += entry.hits;
            if now.saturating_sub(entry.captured_at) >= entry.stale_after_secs {
                stats.stale_entries += 1;
            }
        }
        // Compute directory size.
        if let Ok(rd) = std::fs::read_dir(&self.cache_dir) {
            for de in rd.flatten() {
                if let Ok(meta) = de.metadata() {
                    stats.total_size_bytes += meta.len();
                }
            }
        }
        Ok(stats)
    }

    /// Phase 4.2 (L7 §9): resolve the size cap (in bytes) from
    /// `ANVIL_CMD_CACHE_MAX_MB` (default 50 MB). A value of `0` returns
    /// `None`, meaning size-cap eviction is disabled.
    #[must_use]
    pub fn size_cap_bytes() -> Option<u64> {
        let mb = std::env::var(CMD_CACHE_MAX_MB_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CMD_CACHE_MAX_MB);
        if mb == 0 {
            None
        } else {
            Some(mb.saturating_mul(1024 * 1024))
        }
    }

    /// Phase 4.2 (L7 §9): enforce a size cap on the on-disk cache,
    /// evicting oldest-by-`captured_at` entries until the total file
    /// size fits below `max_bytes`. Returns the number of entries
    /// evicted.
    ///
    /// SECURITY: defense-in-depth assert against Phase 3.5 L5/L7
    /// invariant — vault `cwd` should already be silent-skipped by
    /// `store`, but if any entry's `cwd` ever lands here we want a
    /// debug-build panic for fast detection.
    pub fn enforce_size_cap(&self, max_bytes: u64) -> Result<usize, CommandCacheError> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        // Collect (path, entry, file_size).
        let mut rows: Vec<(PathBuf, CommandCacheEntry, u64)> = Vec::new();
        let mut total: u64 = 0;
        for de in std::fs::read_dir(&self.cache_dir)? {
            let de = de?;
            let path = de.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let meta = match de.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = meta.len();
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let entry: CommandCacheEntry = match serde_json::from_str(&raw) {
                Ok(e) => e,
                Err(_) => continue,
            };
            total = total.saturating_add(size);
            rows.push((path, entry, size));
        }
        if total <= max_bytes {
            return Ok(0);
        }
        // Sort oldest-first by captured_at (ascending).
        rows.sort_by_key(|(_, e, _)| e.captured_at);
        let mut running = total;
        let mut evicted = 0_usize;
        for (path, entry, size) in rows {
            if running <= max_bytes {
                break;
            }
            debug_assert!(
                !crate::file_cache::is_l5_path(&entry.cwd),
                "L7 cache contains entry with L5 cwd {:?} — Phase 3.5 gate bypassed",
                entry.cwd,
            );
            let _ = std::fs::remove_file(&path);
            running = running.saturating_sub(size);
            evicted += 1;
        }
        Ok(evicted)
    }

    /// Phase 4.2: convenience wrapper that reads
    /// `ANVIL_CMD_CACHE_MAX_MB` and applies the cap. Returns 0 when the
    /// cap is disabled.
    pub fn enforce_size_cap_from_env(&self) -> Result<usize, CommandCacheError> {
        match Self::size_cap_bytes() {
            Some(cap) => self.enforce_size_cap(cap),
            None => Ok(0),
        }
    }

    /// Remove all entries whose TTL has expired.  Returns the number removed.
    pub fn prune_stale(&self) -> Result<usize, CommandCacheError> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        let now = now_secs();
        let mut removed = 0_usize;
        for de in std::fs::read_dir(&self.cache_dir)? {
            let de = de?;
            let path = de.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)?;
            if let Ok(entry) = serde_json::from_str::<CommandCacheEntry>(&raw) {
                if now.saturating_sub(entry.captured_at) >= entry.stale_after_secs {
                    let _ = std::fs::remove_file(&path);
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    fn entry_path(&self, canonical: &str, cwd: &Path) -> PathBuf {
        let hash = command_hash(canonical, cwd);
        self.cache_dir.join(format!("{hash}.json"))
    }
}

/// Write `data` to `path` atomically via a sibling `.tmp` file.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── is_cacheable tests ─────────────────────────────────────────────────────

    #[test]
    fn is_cacheable_recognizes_ls_cat_grep() {
        assert!(is_cacheable("ls -la"));
        assert!(is_cacheable("cat Cargo.toml"));
        assert!(is_cacheable("grep -r TODO src/"));
        assert!(is_cacheable("wc -l src/lib.rs"));
        assert!(is_cacheable("head -n 20 README.md"));
        assert!(is_cacheable("tail -f /dev/null")); // tail is read-only even -f
        assert!(is_cacheable("rg 'fn main'"));
    }

    #[test]
    fn is_cacheable_rejects_rm_mv_cp() {
        assert!(!is_cacheable("rm -rf /tmp/test"));
        assert!(!is_cacheable("mv old.txt new.txt"));
        assert!(!is_cacheable("cp src dst"));
        assert!(!is_cacheable("mkdir /tmp/newdir"));
        assert!(!is_cacheable("rmdir /tmp/newdir"));
        assert!(!is_cacheable("touch file.txt"));
        assert!(!is_cacheable("kill 1234"));
        assert!(!is_cacheable("dd if=/dev/zero of=/dev/null"));
    }

    #[test]
    fn is_cacheable_rejects_redirect_writes() {
        assert!(!is_cacheable("echo hello > file.txt"));
        assert!(!is_cacheable("echo hello >> file.txt"));
        assert!(!is_cacheable("ls | tee out.txt"));
        assert!(!is_cacheable("cat file | tee copy.txt"));
        assert!(!is_cacheable("grep foo bar --write"));
    }

    #[test]
    fn is_cacheable_rejects_compound_with_mutator() {
        assert!(!is_cacheable("ls && rm -rf /tmp/foo"));
        assert!(!is_cacheable("echo x; mkdir /tmp/y"));
        assert!(!is_cacheable("cat f || mv a b"));
    }

    #[test]
    fn is_cacheable_strips_wrappers_sudo_time_nohup() {
        assert!(is_cacheable("sudo ls -la"));
        assert!(is_cacheable("time cat big_file.txt"));
        assert!(is_cacheable("nohup grep -r pattern ."));
        assert!(is_cacheable("nice ls"));
        assert!(is_cacheable("timeout 5 git status"));
    }

    #[test]
    fn is_cacheable_rejects_npm_install_cargo_build() {
        assert!(!is_cacheable("npm install"));
        assert!(!is_cacheable("npm run dev"));
        assert!(!is_cacheable("cargo build"));
        assert!(!is_cacheable("cargo build --release"));
        assert!(!is_cacheable("cargo test"));
        assert!(!is_cacheable("cargo run"));
        assert!(!is_cacheable("make all"));
    }

    #[test]
    fn is_cacheable_allows_cargo_tree_and_metadata() {
        assert!(is_cacheable("cargo tree"));
        assert!(is_cacheable("cargo metadata"));
        assert!(is_cacheable("cargo --version"));
    }

    #[test]
    fn is_cacheable_rejects_git_push_pull_fetch_commit() {
        assert!(!is_cacheable("git push origin main"));
        assert!(!is_cacheable("git pull"));
        assert!(!is_cacheable("git fetch --all"));
        assert!(!is_cacheable("git commit -m 'msg'"));
        assert!(!is_cacheable("git merge feature"));
        assert!(!is_cacheable("git rebase main"));
        assert!(!is_cacheable("git reset --hard HEAD"));
        assert!(!is_cacheable("git checkout main"));
        assert!(!is_cacheable("git stash"));
        assert!(!is_cacheable("git apply patch.diff"));
    }

    #[test]
    fn is_cacheable_allows_git_read_commands() {
        assert!(is_cacheable("git status"));
        assert!(is_cacheable("git log --oneline -5"));
        assert!(is_cacheable("git diff HEAD"));
        assert!(is_cacheable("git show HEAD"));
        assert!(is_cacheable("git ls-files"));
        assert!(is_cacheable("git config --get user.name"));
        assert!(is_cacheable("git remote -v"));
        assert!(is_cacheable("git rev-parse HEAD"));
        assert!(is_cacheable("git branch -a"));
        assert!(is_cacheable("git tag -l"));
    }

    #[test]
    fn is_cacheable_rejects_random_or_uuidgen() {
        assert!(!is_cacheable("echo $RANDOM"));
        assert!(!is_cacheable("uuidgen"));
        assert!(!is_cacheable("date +%N"));
    }

    // ── Lookup / store / forget tests ──────────────────────────────────────────

    fn make_manager(dir: &TempDir) -> CommandCacheManager {
        CommandCacheManager {
            project_root: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cmd-cache"),
        }
    }

    #[test]
    fn lookup_returns_none_when_no_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let result = mgr.lookup("ls -la", dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn store_then_lookup_returns_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        mgr.store(
            "ls -la",
            cwd,
            "file1\nfile2".to_string(),
            String::new(),
            0,
            10,
        )
        .unwrap();

        let entry = mgr.lookup("ls -la", cwd).unwrap();
        assert!(entry.is_some());
        let e = entry.unwrap();
        assert_eq!(e.stdout, "file1\nfile2");
        assert_eq!(e.exit_code, 0);
    }

    #[test]
    fn lookup_returns_none_when_ttl_expired() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        // Store with TTL = 0 so it's immediately stale.
        let canonical = normalize_command("ls -la");
        let path = mgr.entry_path(&canonical, cwd);
        std::fs::create_dir_all(&mgr.cache_dir).unwrap();

        let entry = CommandCacheEntry {
            command: canonical.clone(),
            cwd: cwd.to_path_buf(),
            stdout: "data".to_string(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 5,
            captured_at: now_secs() - 100, // old
            touched_files: vec![],
            stale_after_secs: 60,          // expired: 100s > 60s TTL
            hits: 0,
        };
        let json = serde_json::to_string_pretty(&entry).unwrap();
        std::fs::write(&path, json).unwrap();

        let result = mgr.lookup("ls -la", cwd).unwrap();
        assert!(result.is_none(), "expired entry should be a cache miss");
    }

    #[test]
    fn lookup_returns_none_when_touched_file_changed() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        // Create a file and note its mtime.
        let watched = dir.path().join("watched.txt");
        std::fs::write(&watched, b"initial").unwrap();

        let before = now_secs();

        // Store entry referencing the file, captured "before".
        let canonical = normalize_command("cat watched.txt");
        std::fs::create_dir_all(&mgr.cache_dir).unwrap();
        let path = mgr.entry_path(&canonical, cwd);

        let entry = CommandCacheEntry {
            command: canonical.clone(),
            cwd: cwd.to_path_buf(),
            stdout: "initial".to_string(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1,
            captured_at: before,
            touched_files: vec![watched.clone()],
            stale_after_secs: 6000,
            hits: 0,
        };
        let json = serde_json::to_string_pretty(&entry).unwrap();
        std::fs::write(&path, json).unwrap();

        // Modify the watched file so its mtime is > captured_at.
        // Sleep 1s to guarantee mtime difference on coarse filesystems.
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(&watched, b"modified").unwrap();

        let result = mgr.lookup("cat watched.txt", cwd).unwrap();
        assert!(result.is_none(), "modified touched file should invalidate cache");
    }

    #[test]
    fn lookup_returns_none_when_cwd_differs() {
        let dir = TempDir::new().unwrap();
        let other_dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        mgr.store("ls", cwd, "file1".to_string(), String::new(), 0, 5)
            .unwrap();

        // Lookup with a different cwd.
        let result = mgr.lookup("ls", other_dir.path()).unwrap();
        assert!(result.is_none(), "different cwd should be a cache miss");
    }

    #[test]
    fn lookup_normalizes_whitespace_in_command() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        mgr.store("ls  -la", cwd, "out".to_string(), String::new(), 0, 5)
            .unwrap();

        // Look up with differently spaced version.
        let result = mgr.lookup("ls -la", cwd).unwrap();
        assert!(result.is_some(), "normalised whitespace should match");
    }

    #[test]
    fn forget_drops_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        mgr.store("ls", cwd, "data".to_string(), String::new(), 0, 5)
            .unwrap();
        assert!(mgr.lookup("ls", cwd).unwrap().is_some());

        mgr.forget("ls", cwd).unwrap();
        assert!(mgr.lookup("ls", cwd).unwrap().is_none());
    }

    #[test]
    fn prune_stale_removes_expired_entries() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();
        std::fs::create_dir_all(&mgr.cache_dir).unwrap();

        // Plant two entries: one stale, one fresh.
        let stale_cmd = normalize_command("git log");
        let fresh_cmd = normalize_command("git status");

        let stale_entry = CommandCacheEntry {
            command: stale_cmd.clone(),
            cwd: cwd.to_path_buf(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1,
            captured_at: now_secs() - 1000,
            touched_files: vec![],
            stale_after_secs: 60, // expired
            hits: 0,
        };
        let fresh_entry = CommandCacheEntry {
            command: fresh_cmd.clone(),
            cwd: cwd.to_path_buf(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1,
            captured_at: now_secs(),
            touched_files: vec![],
            stale_after_secs: 6000, // fresh
            hits: 0,
        };

        let stale_path = mgr.entry_path(&stale_cmd, cwd);
        let fresh_path = mgr.entry_path(&fresh_cmd, cwd);
        std::fs::write(&stale_path, serde_json::to_string_pretty(&stale_entry).unwrap()).unwrap();
        std::fs::write(&fresh_path, serde_json::to_string_pretty(&fresh_entry).unwrap()).unwrap();

        let removed = mgr.prune_stale().unwrap();
        assert_eq!(removed, 1, "should prune exactly one stale entry");
        assert!(!stale_path.exists(), "stale entry should be removed");
        assert!(fresh_path.exists(), "fresh entry should remain");
    }

    // ── touched_files tests ────────────────────────────────────────────────────

    #[test]
    fn touched_files_inferred_for_cat_head_tail() {
        let cwd = Path::new("/tmp/proj");
        let files = infer_touched_files("cat src/main.rs", cwd);
        assert!(files.iter().any(|f| f.ends_with("main.rs")));

        let files = infer_touched_files("head -n 10 README.md", cwd);
        assert!(files.iter().any(|f| f.ends_with("README.md")));

        let files = infer_touched_files("tail -f logfile.txt", cwd);
        assert!(files.iter().any(|f| f.ends_with("logfile.txt")));
    }

    #[test]
    fn touched_files_inferred_for_git_status() {
        let cwd = Path::new("/tmp/proj");
        let files = infer_touched_files("git status", cwd);
        assert!(files.iter().any(|f| f.ends_with(".git/HEAD")));
        assert!(files.iter().any(|f| f.ends_with(".git/index")));
    }

    #[test]
    fn touched_files_inferred_for_cargo_tree() {
        let cwd = Path::new("/tmp/proj");
        let files = infer_touched_files("cargo tree", cwd);
        assert!(files.iter().any(|f| f.ends_with("Cargo.toml")));
        assert!(files.iter().any(|f| f.ends_with("Cargo.lock")));
    }

    // ── TTL defaults tests ─────────────────────────────────────────────────────

    #[test]
    fn ttl_default_60s_for_git_status() {
        assert_eq!(default_ttl("git status"), 60);
        assert_eq!(default_ttl("git diff HEAD"), 60);
    }

    #[test]
    fn ttl_defaults_for_other_commands() {
        assert_eq!(default_ttl("git log --oneline"), 600);
        assert_eq!(default_ttl("git show HEAD"), 600);
        assert_eq!(default_ttl("ls -la"), 300);
        assert_eq!(default_ttl("cat file.txt"), 600);
        assert_eq!(default_ttl("cargo tree"), 1800);
        assert_eq!(default_ttl("npm ls"), 1800);
    }

    // ── Hits counter test ──────────────────────────────────────────────────────

    #[test]
    fn hits_counter_increments_on_each_lookup_hit() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let cwd = dir.path();

        mgr.store("ls", cwd, "out".to_string(), String::new(), 0, 5)
            .unwrap();

        let e1 = mgr.lookup("ls", cwd).unwrap().unwrap();
        assert_eq!(e1.hits, 1);

        let e2 = mgr.lookup("ls", cwd).unwrap().unwrap();
        assert_eq!(e2.hits, 2);
    }

    // ── Concurrent store atomicity test ───────────────────────────────────────

    #[test]
    fn concurrent_store_atomic() {
        use std::sync::Arc;
        let dir = Arc::new(TempDir::new().unwrap());
        let cwd = dir.path().to_path_buf();
        // Pre-create the cache dir so threads don't race on mkdir.
        std::fs::create_dir_all(dir.path().join("cmd-cache")).unwrap();

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let dir_clone = Arc::clone(&dir);
                let cwd_clone = cwd.clone();
                std::thread::spawn(move || {
                    let mgr = make_manager(&dir_clone);
                    // Ignore errors: concurrent rename races are expected.
                    let _ = mgr.store(
                        "ls",
                        &cwd_clone,
                        format!("output-{i}"),
                        String::new(),
                        0,
                        5,
                    );
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }

        // Exactly one valid entry should be on disk (last writer wins via rename).
        let mgr = make_manager(&dir);
        let entries = mgr.list().unwrap();
        assert_eq!(entries.len(), 1, "atomic rename ensures one valid entry");
    }

    // ── Project isolation test ─────────────────────────────────────────────────

    #[test]
    fn project_isolation() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let mgr_a = make_manager(&dir_a);
        let mgr_b = make_manager(&dir_b);

        mgr_a
            .store("ls", dir_a.path(), "project-a".to_string(), String::new(), 0, 5)
            .unwrap();

        // Cache stored via mgr_a should NOT be visible via mgr_b's cache_dir.
        let result = mgr_b
            .lookup("ls", dir_b.path())
            .unwrap();
        assert!(result.is_none(), "different project should have isolated cache");
    }

    // ── Auto-promotion hot-path test ───────────────────────────────────────────

    #[test]
    fn auto_promotion_threshold_3_invocations_logs_hot_path() {
        // We can't easily capture stderr in a unit test, but we can verify that
        // record_cacheable_invocation doesn't panic and the counter works.
        // Reset the session counter by using a unique command name.
        let unique_cmd = format!("ls /unique-hot-path-test-{}", std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos());
        let cwd = Path::new("/tmp");

        // Three invocations — the third triggers the hot-path log.
        record_cacheable_invocation(&unique_cmd, cwd);
        record_cacheable_invocation(&unique_cmd, cwd);
        record_cacheable_invocation(&unique_cmd, cwd); // triggers log

        let map = session_counter().lock().unwrap();
        let key = format!("{}|{}", unique_cmd, cwd.display());
        assert_eq!(*map.get(&key).unwrap(), 3);
    }

    // ── Phase 4.2 (L7 §9) size-cap + LRU tests ────────────────────────────

    fn write_raw_cmd_entry(mgr: &CommandCacheManager, name: &str, captured_at: u64, body_size: usize) {
        std::fs::create_dir_all(&mgr.cache_dir).unwrap();
        let entry = CommandCacheEntry {
            command: format!("echo {name}"),
            cwd: mgr.project_root.clone(),
            stdout: "x".repeat(body_size),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1,
            captured_at,
            touched_files: vec![],
            stale_after_secs: 3600,
            hits: 0,
        };
        let path = mgr.cache_dir.join(format!("{name}.json"));
        let json = serde_json::to_string(&entry).unwrap();
        std::fs::write(path, json).unwrap();
    }

    fn cmd_dir_size(mgr: &CommandCacheManager) -> u64 {
        let mut total = 0u64;
        if let Ok(rd) = std::fs::read_dir(&mgr.cache_dir) {
            for de in rd.flatten() {
                if let Ok(m) = de.metadata() {
                    total += m.len();
                }
            }
        }
        total
    }

    #[test]
    fn cmd_enforce_size_cap_evicts_oldest_first() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        // Three entries with 64 KB bodies, distinct captured_at order.
        write_raw_cmd_entry(&mgr, "old",   100, 64 * 1024); // oldest
        write_raw_cmd_entry(&mgr, "mid",   200, 64 * 1024);
        write_raw_cmd_entry(&mgr, "fresh", 300, 64 * 1024);
        let total_before = cmd_dir_size(&mgr);
        assert!(total_before > 128 * 1024);

        // Cap at ~ 96 KB → at least one entry must go (the oldest).
        let cap = 96 * 1024;
        let evicted = mgr.enforce_size_cap(cap).unwrap();
        assert!(evicted >= 1, "must evict at least one entry; got {evicted}");
        assert!(cmd_dir_size(&mgr) <= cap);
        // The newest (captured_at=300) must survive.
        let survivors = std::fs::read_dir(&mgr.cache_dir)
            .unwrap()
            .filter_map(|r| r.ok())
            .map(|de| de.file_name())
            .collect::<Vec<_>>();
        assert!(
            survivors.iter().any(|n| n.to_string_lossy() == "fresh.json"),
            "newest entry must survive eviction: {survivors:?}",
        );
    }

    #[test]
    fn cmd_enforce_size_cap_noop_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        // Don't create cache_dir.
        let evicted = mgr.enforce_size_cap(0).unwrap();
        assert_eq!(evicted, 0);
    }

    #[test]
    fn cmd_size_cap_env_disables_with_zero() {
        unsafe { std::env::set_var(CMD_CACHE_MAX_MB_ENV, "0"); }
        assert!(CommandCacheManager::size_cap_bytes().is_none());
        unsafe { std::env::set_var(CMD_CACHE_MAX_MB_ENV, "20"); }
        assert_eq!(
            CommandCacheManager::size_cap_bytes(),
            Some(20 * 1024 * 1024),
        );
        unsafe { std::env::remove_var(CMD_CACHE_MAX_MB_ENV); }
        assert_eq!(
            CommandCacheManager::size_cap_bytes(),
            Some(DEFAULT_CMD_CACHE_MAX_MB * 1024 * 1024),
        );
    }
}
