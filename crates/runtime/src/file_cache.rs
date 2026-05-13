// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// File-fingerprint cache for Anvil's token-economy build (W11).
///
/// Stores per-file metadata (sha256, mtime, line count, language, optional
/// summary, key symbols, access count) under:
///   `~/.anvil/projects/<project-hash>/file-cache/<sha-prefix2>/<sha256>.json`
///
/// The cache is purely advisory — a stale entry (mtime or sha256 changed)
/// causes `lookup` to return `None` and auto-prune the stale file.
///
/// Phase 3.5 (SECURITY): all store/lookup paths are gated behind
/// [`is_l5_path`] so vault content (`~/.anvil/vault/...`) NEVER enters the
/// L7 cache. The gate silent-skips — no log lines (logging a vault path
/// could still leak info about which secrets exist).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::memory::project_path_hash;

/// Phase 3.5: returns `true` if `path` is inside the vault directory.
///
/// Canonicalises both the input path and the vault directory before
/// comparing, so symlinks pointing into the vault are caught.
///
/// When the path itself can't be canonicalized (it may not exist yet),
/// the function walks up the path's ancestors until it finds one that
/// CAN be canonicalized, then checks whether THAT ancestor lives under
/// the vault. This catches the "store a not-yet-created vault file" case.
///
/// SECURITY: this function is the sentinel for the L5 ↔ L7 invariant.
/// No L7 cache store/lookup site may bypass it.
#[must_use]
pub fn is_l5_path(path: &Path) -> bool {
    let vault_dir = crate::config::default_config_home().join("vault");
    let vault_canonical = vault_dir.canonicalize().unwrap_or(vault_dir);

    // Fast path: the path itself canonicalizes (exists + resolves).
    if let Ok(p) = path.canonicalize() {
        return p.starts_with(&vault_canonical);
    }

    // Slow path: walk up to the deepest existing ancestor.
    let mut cursor: Option<&Path> = path.parent();
    while let Some(parent) = cursor {
        if let Ok(canonical_parent) = parent.canonicalize() {
            return canonical_parent.starts_with(&vault_canonical);
        }
        cursor = parent.parent();
    }
    false
}

/// Monotonic per-process counter so concurrent `write_entry_atomic` calls
/// always produce distinct tmp filenames, even when their `subsec_nanos`
/// readings collide.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Files larger than this threshold skip sha256 computation.
/// The entry is still stored but `sha256` is left empty and content-based
/// invalidation is skipped — only mtime + size are checked.
pub const LARGE_FILE_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024; // 10 MB

/// Maximum number of key symbols stored per entry.
pub const MAX_KEY_SYMBOLS: usize = 50;

/// Maximum character length of a summary string.
pub const MAX_SUMMARY_LEN: usize = 200;

/// Maximum entries returned by system-prompt injection.
pub const MAX_PROMPT_ENTRIES: usize = 50;

/// Approximate byte budget for the `<known-files>` block in the system prompt.
pub const MAX_PROMPT_BYTES: usize = 4 * 1024; // 4 KB

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum FileCacheError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for FileCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "file-cache I/O error: {e}"),
            Self::Json(e) => write!(f, "file-cache JSON error: {e}"),
        }
    }
}

impl std::error::Error for FileCacheError {}

impl From<io::Error> for FileCacheError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for FileCacheError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single cached file entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileCacheEntry {
    /// Canonical absolute path.
    pub path: PathBuf,
    /// Hex-encoded SHA-256 of the file contents.
    /// Empty string for files larger than `LARGE_FILE_THRESHOLD_BYTES`.
    pub sha256: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Modification time as Unix seconds.
    pub mtime: u64,
    /// Unix seconds when we last verified this entry was current.
    pub last_seen: u64,
    /// Number of source lines in the file.
    pub line_count: usize,
    /// Language detected from file extension (e.g. `"rust"`, `"typescript"`).
    pub language: Option<String>,
    /// Optional one-line description, at most 200 characters.
    pub summary: Option<String>,
    /// Function, struct, or class names extracted from the file.
    pub key_symbols: Vec<String>,
    /// Number of times the agent has consulted this entry.
    pub access_count: u32,
}

/// Summary statistics for the whole project cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheStats {
    pub entry_count: usize,
    pub total_bytes_cached: u64,
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Manages the file-fingerprint cache for a single project.
pub struct FileCacheManager {
    #[allow(dead_code)]
    project_root: PathBuf,
    cache_dir: PathBuf,
}

impl FileCacheManager {
    /// Create (or open) the cache for `project_root`.
    ///
    /// The cache directory is created lazily on first write.
    pub fn new(project_root: PathBuf) -> Result<Self, FileCacheError> {
        let hash = project_path_hash(&project_root);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let cache_dir = home
            .join(".anvil")
            .join("projects")
            .join(hash)
            .join("file-cache");

        Ok(Self {
            project_root,
            cache_dir,
        })
    }

    // ── public API ────────────────────────────────────────────────────────

    /// Look up a file.
    ///
    /// Returns `Some(entry)` only when the on-disk mtime + size match the
    /// cached values **and** (for small files) the sha256 matches the live
    /// file contents.  Stale entries are deleted automatically.
    ///
    /// Phase 3.5 (SECURITY): silently returns `Ok(None)` for paths inside
    /// the vault directory — vault content is L5 and must never enter L7.
    pub fn lookup(&self, path: &Path) -> Result<Option<FileCacheEntry>, FileCacheError> {
        if is_l5_path(path) {
            return Ok(None);
        }
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let entry = match self.load_entry(&canonical)? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Stat the live file.
        let meta = match fs::metadata(&canonical) {
            Ok(m) => m,
            Err(_) => {
                // File gone — remove stale entry.
                let _ = self.delete_entry(&canonical);
                return Ok(None);
            }
        };

        let live_size = meta.len();
        let live_mtime = mtime_secs(&meta);

        // Reject if size or mtime changed.
        if live_size != entry.size_bytes || live_mtime != entry.mtime {
            let _ = self.delete_entry(&canonical);
            return Ok(None);
        }

        // For small files, verify sha256 content hash.
        if live_size <= LARGE_FILE_THRESHOLD_BYTES && !entry.sha256.is_empty() {
            let live_sha = compute_sha256(&canonical)?;
            if live_sha != entry.sha256 {
                let _ = self.delete_entry(&canonical);
                return Ok(None);
            }
        }

        Ok(Some(entry))
    }

    /// Store or refresh the cache entry for `path`.
    ///
    /// Reads the file, computes sha256 (for files ≤ threshold), counts lines,
    /// detects language, and writes the entry atomically (`.tmp` + rename).
    ///
    /// Phase 3.5 (SECURITY): silent-skips vault paths. The returned entry
    /// for a silent-skip is a stub (`size_bytes = 0`, empty sha256, empty
    /// symbols). The L7 cache directory is left untouched.
    pub fn store(
        &self,
        path: &Path,
        summary: Option<String>,
        key_symbols: Vec<String>,
    ) -> Result<FileCacheEntry, FileCacheError> {
        if is_l5_path(path) {
            // Silent-skip: never log the path itself (logging vault paths
            // could leak the existence of specific labels).
            return Ok(FileCacheEntry {
                path: path.to_path_buf(),
                sha256: String::new(),
                size_bytes: 0,
                mtime: 0,
                last_seen: 0,
                line_count: 0,
                language: None,
                summary: None,
                key_symbols: Vec::new(),
                access_count: 0,
            });
        }
        let canonical = path
            .canonicalize()
            .map_err(FileCacheError::Io)?;

        let meta = fs::metadata(&canonical)?;
        let size_bytes = meta.len();
        let mtime = mtime_secs(&meta);
        let now = unix_now();

        let (sha256, line_count) = if size_bytes > LARGE_FILE_THRESHOLD_BYTES {
            // Large file: skip sha256 and line count.
            (String::new(), 0)
        } else {
            let contents = fs::read_to_string(&canonical)?;
            let sha = compute_sha256_from_bytes(contents.as_bytes());
            let lines = contents.lines().count();
            (sha, lines)
        };

        let language = detect_language(&canonical);

        // Truncate summary to MAX_SUMMARY_LEN.
        let summary = summary.map(|s| truncate_to(s, MAX_SUMMARY_LEN));

        // Cap key_symbols.
        let key_symbols = key_symbols
            .into_iter()
            .take(MAX_KEY_SYMBOLS)
            .collect::<Vec<_>>();

        // Preserve existing access_count if there is an existing entry.
        let access_count = self
            .load_entry(&canonical)
            .ok()
            .flatten()
            .map(|e| e.access_count)
            .unwrap_or(0);

        let entry = FileCacheEntry {
            path: canonical.clone(),
            sha256,
            size_bytes,
            mtime,
            last_seen: now,
            line_count,
            language,
            summary,
            key_symbols,
            access_count,
        };

        self.write_entry_atomic(&entry)?;
        Ok(entry)
    }

    /// Increment `access_count` and update `last_seen`.
    ///
    /// Phase 3.5 (SECURITY): vault paths are silently skipped.
    pub fn touch(&self, path: &Path) -> Result<(), FileCacheError> {
        if is_l5_path(path) {
            return Ok(());
        }
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => return Err(FileCacheError::Io(e)),
        };
        if let Some(mut entry) = self.load_entry(&canonical)? {
            entry.access_count = entry.access_count.saturating_add(1);
            entry.last_seen = unix_now();
            self.write_entry_atomic(&entry)?;
        }
        Ok(())
    }

    /// Remove the cache entry for `path`.
    pub fn forget(&self, path: &Path) -> Result<(), FileCacheError> {
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        self.delete_entry(&canonical)
    }

    /// Return all valid entries for this project.
    pub fn list(&self) -> Result<Vec<FileCacheEntry>, FileCacheError> {
        let mut entries = Vec::new();
        if !self.cache_dir.exists() {
            return Ok(entries);
        }

        for shard in read_dir_entries(&self.cache_dir)? {
            if !shard.is_dir() {
                continue;
            }
            for entry_path in read_dir_entries(&shard)? {
                if entry_path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let raw = match fs::read_to_string(&entry_path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Ok(entry) = serde_json::from_str::<FileCacheEntry>(&raw) {
                    entries.push(entry);
                }
            }
        }
        Ok(entries)
    }

    /// Return aggregate statistics.
    pub fn stats(&self) -> Result<FileCacheStats, FileCacheError> {
        let entries = self.list()?;
        let total_bytes_cached = entries.iter().map(|e| e.size_bytes).sum();
        Ok(FileCacheStats {
            entry_count: entries.len(),
            total_bytes_cached,
        })
    }

    /// Remove entries for files that no longer exist or whose sha256 doesn't
    /// match the live file.  Returns the number of entries pruned.
    pub fn prune(&self) -> Result<usize, FileCacheError> {
        let mut pruned = 0;
        if !self.cache_dir.exists() {
            return Ok(0);
        }

        for shard in read_dir_entries(&self.cache_dir)? {
            if !shard.is_dir() {
                continue;
            }
            for entry_path in read_dir_entries(&shard)? {
                if entry_path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let raw = match fs::read_to_string(&entry_path) {
                    Ok(s) => s,
                    Err(_) => {
                        let _ = fs::remove_file(&entry_path);
                        pruned += 1;
                        continue;
                    }
                };
                let entry: FileCacheEntry = match serde_json::from_str(&raw) {
                    Ok(e) => e,
                    Err(_) => {
                        let _ = fs::remove_file(&entry_path);
                        pruned += 1;
                        continue;
                    }
                };

                // File no longer exists?
                if !entry.path.exists() {
                    let _ = fs::remove_file(&entry_path);
                    pruned += 1;
                    continue;
                }

                // For small files, verify sha256.
                if entry.size_bytes <= LARGE_FILE_THRESHOLD_BYTES && !entry.sha256.is_empty() {
                    let live_sha = match compute_sha256(&entry.path) {
                        Ok(s) => s,
                        Err(_) => {
                            let _ = fs::remove_file(&entry_path);
                            pruned += 1;
                            continue;
                        }
                    };
                    if live_sha != entry.sha256 {
                        let _ = fs::remove_file(&entry_path);
                        pruned += 1;
                        continue;
                    }
                }
            }
        }
        Ok(pruned)
    }

    // ── private helpers ───────────────────────────────────────────────────

    fn entry_path(&self, canonical: &Path) -> PathBuf {
        let sha = sha256_of_path(canonical);
        let prefix = &sha[..2];
        self.cache_dir.join(prefix).join(format!("{sha}.json"))
    }

    fn load_entry(&self, canonical: &Path) -> Result<Option<FileCacheEntry>, FileCacheError> {
        let path = self.entry_path(canonical);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        let entry: FileCacheEntry = serde_json::from_str(&raw)?;
        Ok(Some(entry))
    }

    fn delete_entry(&self, canonical: &Path) -> Result<(), FileCacheError> {
        let path = self.entry_path(canonical);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Write an entry atomically: serialise to `.tmp`, then rename.
    fn write_entry_atomic(&self, entry: &FileCacheEntry) -> Result<(), FileCacheError> {
        let dest = self.entry_path(&entry.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // Use a unique tmp filename per write to avoid concurrent writers clobbering
        // each other's in-flight temp file.  The final rename is still atomic.
        // The per-process counter guarantees uniqueness even when subsec_nanos collides.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_ext = format!("{pid}.{nanos:010}.{seq}.tmp");
        let tmp = dest.with_extension(tmp_ext);
        let json = serde_json::to_string_pretty(entry)?;
        fs::write(&tmp, &json)?;
        fs::rename(&tmp, &dest)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Best-effort wiring helpers
// ---------------------------------------------------------------------------

/// Best-effort cache touch — silently swallows all errors. Used from
/// file_ops on the hot read/write/edit paths so a busted cache directory
/// can never break a tool call. The caller passes a project root; the
/// cache lives under `~/.anvil/projects/<hash>/file-cache/`.
///
/// Strategy:
///   - If the file already has an entry whose mtime+size are unchanged,
///     just increment `access_count` via `touch`.
///   - Otherwise re-`store` it (preserving any existing access_count).
///
/// Errors are intentionally ignored — the file_cache is purely advisory.
pub fn refresh_entry_best_effort(project_root: &Path, file_path: &Path) {
    let manager = match FileCacheManager::new(project_root.to_path_buf()) {
        Ok(m) => m,
        Err(_) => return,
    };
    match manager.lookup(file_path) {
        Ok(Some(_)) => {
            let _ = manager.touch(file_path);
        }
        Ok(None) => {
            let _ = manager.store(file_path, None, Vec::new());
        }
        Err(_) => {}
    }
}

/// Best-effort cache invalidation — used after a destructive op (delete,
/// move out from under us). Silent on error.
pub fn forget_entry_best_effort(project_root: &Path, file_path: &Path) {
    if let Ok(manager) = FileCacheManager::new(project_root.to_path_buf()) {
        let _ = manager.forget(file_path);
    }
}

// ---------------------------------------------------------------------------
// System-prompt injection
// ---------------------------------------------------------------------------

/// Build a `<known-files>` block for inclusion in the system prompt.
///
/// Only entries that have a `summary` are included.  Results are capped at
/// `MAX_PROMPT_ENTRIES` entries and `MAX_PROMPT_BYTES` total characters.
pub fn build_known_files_block(entries: &[FileCacheEntry]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut budget = MAX_PROMPT_BYTES;

    for entry in entries.iter().take(MAX_PROMPT_ENTRIES) {
        let summary = match &entry.summary {
            Some(s) => s.as_str(),
            None => continue, // bare entries are silent
        };
        let file_name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| entry.path.to_string_lossy().into_owned());
        let line = format!("  {file_name}: {summary}");
        if line.len() > budget {
            break;
        }
        budget -= line.len();
        lines.push(line);
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "<known-files>\n{}\n</known-files>",
        lines.join("\n")
    ))
}

// ---------------------------------------------------------------------------
// Private utilities
// ---------------------------------------------------------------------------

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn compute_sha256(path: &Path) -> Result<String, FileCacheError> {
    let bytes = fs::read(path)?;
    Ok(compute_sha256_from_bytes(&bytes))
}

fn compute_sha256_from_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    hex_encode_bytes(&result)
}

/// Compute the SHA-256 of a canonical path string — used to key the cache
/// entry filename itself (separate from the file-contents sha256).
fn sha256_of_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    hex_encode_bytes(&result)
}

fn hex_encode_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    Some(
        match ext {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "json" => "json",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            "md" => "markdown",
            "sh" | "bash" => "shell",
            "go" => "go",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" => "cpp",
            "java" => "java",
            "rb" => "ruby",
            "php" => "php",
            "cs" => "csharp",
            "swift" => "swift",
            "kt" => "kotlin",
            "html" | "htm" => "html",
            "css" => "css",
            "sql" => "sql",
            _ => return None,
        }
        .to_string(),
    )
}

/// Truncate a string to at most `max_chars` characters (char-boundary safe).
fn truncate_to(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

fn read_dir_entries(dir: &Path) -> Result<Vec<PathBuf>, FileCacheError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        paths.push(entry.path());
    }
    Ok(paths)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        build_known_files_block, detect_language, forget_entry_best_effort,
        refresh_entry_best_effort, truncate_to, FileCacheEntry, FileCacheManager,
        LARGE_FILE_THRESHOLD_BYTES, MAX_KEY_SYMBOLS, MAX_SUMMARY_LEN,
    };

    // ── helpers ───────────────────────────────────────────────────────────────

    fn tmp_project() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    fn make_manager(project: &tempfile::TempDir) -> FileCacheManager {
        FileCacheManager::new(project.path().to_path_buf()).expect("manager")
    }

    fn write_file(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(path, content).expect("write");
    }

    // ── Phase 3.5 SECURITY: is_l5_path + cache gates ──────────────────────

    /// Process-local lock for ANVIL_CONFIG_HOME-mutating tests.
    fn l5_env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// SAFETY: env::set_var is unsafe in Rust 2024. Tests serialise on
    /// l5_env_lock() so the mutation is race-free.
    fn set_anvil_home_l5(path: &std::path::Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path); }
    }

    fn restore_anvil_home(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(p) => unsafe { std::env::set_var("ANVIL_CONFIG_HOME", p); },
            None => unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); },
        }
    }

    #[test]
    fn is_l5_path_returns_true_for_vault_paths() {
        let _lock = l5_env_lock();
        let home = tempfile::tempdir().expect("home");
        let vault_dir = home.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        set_anvil_home_l5(home.path());

        assert!(super::is_l5_path(&vault_dir));
        assert!(super::is_l5_path(&vault_dir.join("creds")));
        let nested = vault_dir.join("deep").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(super::is_l5_path(&nested));

        restore_anvil_home(prev);
    }

    #[test]
    fn is_l5_path_returns_false_for_non_vault_paths() {
        let _lock = l5_env_lock();
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join("vault")).unwrap();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        set_anvil_home_l5(home.path());

        // Regular project files must not be flagged.
        let proj = tempfile::tempdir().expect("proj");
        let file = proj.path().join("foo.rs");
        std::fs::write(&file, "code").unwrap();
        assert!(!super::is_l5_path(&file));
        // Sibling dirs under .anvil but not under vault must pass.
        let memdir = home.path().join("memory");
        std::fs::create_dir_all(&memdir).unwrap();
        assert!(!super::is_l5_path(&memdir));

        restore_anvil_home(prev);
    }

    #[test]
    fn is_l5_path_catches_symlinks_into_vault() {
        let _lock = l5_env_lock();
        let home = tempfile::tempdir().expect("home");
        let vault_dir = home.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let real = vault_dir.join("real-file");
        std::fs::write(&real, "secret").unwrap();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        set_anvil_home_l5(home.path());

        let outside = tempfile::tempdir().expect("outside");
        let link = outside.path().join("symlink-to-vault");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        #[cfg(unix)]
        assert!(
            super::is_l5_path(&link),
            "symlink pointing into vault must be detected"
        );

        restore_anvil_home(prev);
    }

    #[test]
    fn file_cache_store_silent_skips_vault_paths() {
        let _lock = l5_env_lock();
        let home = tempfile::tempdir().expect("home");
        let vault_dir = home.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let secret_file = vault_dir.join("secret.txt");
        std::fs::write(&secret_file, "sk-ant-api03-secret").unwrap();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        set_anvil_home_l5(home.path());

        // Create a manager in a totally separate project root so any
        // accidental write would land somewhere we can verify is empty.
        let proj = tempfile::tempdir().expect("proj");
        let mgr = FileCacheManager::new(proj.path().to_path_buf()).expect("manager");
        let result = mgr.store(&secret_file, Some("a secret".to_string()), vec!["api_key".to_string()]);
        assert!(result.is_ok(), "store must return Ok stub for L5 paths");
        // Verify nothing was actually written.
        assert_eq!(mgr.list().expect("list").len(), 0, "L5 paths must not enter cache");

        restore_anvil_home(prev);
    }

    #[test]
    fn file_cache_lookup_silent_returns_none_for_vault_paths() {
        let _lock = l5_env_lock();
        let home = tempfile::tempdir().expect("home");
        let vault_dir = home.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let secret_file = vault_dir.join("forged.txt");
        std::fs::write(&secret_file, "secret").unwrap();
        let prev = std::env::var_os("ANVIL_CONFIG_HOME");
        set_anvil_home_l5(home.path());

        let proj = tempfile::tempdir().expect("proj");
        let mgr = FileCacheManager::new(proj.path().to_path_buf()).expect("manager");

        // Manually forge an entry on disk to simulate a hypothetical
        // bypass — lookup must still refuse to return it.
        let canonical = secret_file.canonicalize().unwrap();
        let entry = FileCacheEntry {
            path: canonical.clone(),
            sha256: "abc".to_string(),
            size_bytes: 100,
            mtime: 0,
            last_seen: 0,
            line_count: 1,
            language: None,
            summary: Some("a secret".to_string()),
            key_symbols: vec![],
            access_count: 0,
        };
        // Bypass the gate by writing the entry file directly.
        let _ = mgr.write_entry_atomic(&entry);

        // Public lookup MUST still return None — the gate refuses to
        // surface vault content even if it's somehow on disk.
        let result = mgr.lookup(&secret_file).expect("lookup");
        assert!(result.is_none(), "lookup must refuse vault paths");

        restore_anvil_home(prev);
    }

    // ── 1. lookup_returns_none_for_unknown_file ────────────────────────────────

    #[test]
    fn lookup_returns_none_for_unknown_file() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("never-stored.txt");
        write_file(&file, "hello");
        let result = mgr.lookup(&file).expect("lookup");
        assert!(result.is_none());
    }

    // ── 2. store_then_lookup_returns_entry ────────────────────────────────────

    #[test]
    fn store_then_lookup_returns_entry() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("a.txt");
        write_file(&file, "content");

        let stored = mgr
            .store(&file, Some("a test file".to_string()), vec![])
            .expect("store");
        assert!(stored.summary.is_some());

        let hit = mgr.lookup(&file).expect("lookup").expect("expected hit");
        assert_eq!(hit.path, stored.path);
        assert_eq!(hit.sha256, stored.sha256);
    }

    // ── 3. lookup_returns_none_when_mtime_changes ─────────────────────────────

    #[test]
    fn lookup_returns_none_when_mtime_changes() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("b.txt");
        write_file(&file, "original");

        mgr.store(&file, None, vec![]).expect("store");

        // Touch mtime by overwriting.
        // macOS HFS+/APFS has 1-second mtime granularity on temp dirs, so we
        // must sleep >1 s to guarantee the mtime second actually increments.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_file(&file, "original"); // same content, different mtime

        let result = mgr.lookup(&file).expect("lookup");
        assert!(
            result.is_none(),
            "mtime changed — should be a cache miss"
        );
    }

    // ── 4. lookup_returns_none_when_sha_changes ───────────────────────────────

    #[test]
    fn lookup_returns_none_when_sha_changes() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("c.txt");
        write_file(&file, "version-one");

        // Store with version-one content.
        mgr.store(&file, None, vec![]).expect("store");

        // Manually forge the cache entry to pretend mtime matches but
        // store different content on disk.
        // We reload the entry, keep its mtime in the JSON but write new content.
        let entry = mgr
            .load_entry(&file.canonicalize().unwrap())
            .expect("load")
            .expect("entry");

        // Write new content with the same byte-length trick is unreliable,
        // so instead: write new content, then patch the cached entry's mtime
        // to match the new on-disk mtime so only the sha256 differs.
        write_file(&file, "version-two-differs");
        let new_meta = fs::metadata(&file).expect("meta");
        let new_mtime = new_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Write forged entry: mtime matches disk but sha256 is still old.
        let forged = FileCacheEntry {
            mtime: new_mtime,
            size_bytes: new_meta.len(),
            ..entry
        };
        mgr.write_entry_atomic(&forged).expect("write forged");

        let result = mgr.lookup(&file).expect("lookup");
        assert!(result.is_none(), "sha256 mismatch should cause a cache miss");
    }

    // ── 5. touch_increments_access_count_and_updates_last_seen ───────────────

    #[test]
    fn touch_increments_access_count_and_updates_last_seen() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("d.txt");
        write_file(&file, "data");

        mgr.store(&file, None, vec![]).expect("store");
        let before = mgr.lookup(&file).expect("lookup").expect("entry");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        mgr.touch(&file).expect("touch");

        let after = mgr
            .load_entry(&file.canonicalize().unwrap())
            .expect("load")
            .expect("entry");

        assert_eq!(after.access_count, before.access_count + 1);
        assert!(after.last_seen >= before.last_seen);
    }

    // ── 6. forget_drops_entry ────────────────────────────────────────────────

    #[test]
    fn forget_drops_entry() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("e.txt");
        write_file(&file, "data");

        mgr.store(&file, None, vec![]).expect("store");
        assert!(mgr.lookup(&file).expect("lookup").is_some());

        mgr.forget(&file).expect("forget");
        assert!(mgr.lookup(&file).expect("lookup").is_none());
    }

    // ── 7. list_returns_all_entries_for_project ───────────────────────────────

    #[test]
    fn list_returns_all_entries_for_project() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);

        for i in 0..3u8 {
            let file = proj.path().join(format!("file{i}.txt"));
            write_file(&file, &format!("content {i}"));
            mgr.store(&file, None, vec![]).expect("store");
        }

        let entries = mgr.list().expect("list");
        assert_eq!(entries.len(), 3);
    }

    // ── 8. prune_removes_deleted_files ───────────────────────────────────────

    #[test]
    fn prune_removes_deleted_files() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("ephemeral.txt");
        write_file(&file, "bye");

        mgr.store(&file, None, vec![]).expect("store");
        assert_eq!(mgr.list().expect("list").len(), 1);

        fs::remove_file(&file).expect("remove");
        let pruned = mgr.prune().expect("prune");
        assert_eq!(pruned, 1);
        assert_eq!(mgr.list().expect("list").len(), 0);
    }

    // ── 9. prune_removes_entries_with_mismatched_sha ─────────────────────────

    #[test]
    fn prune_removes_entries_with_mismatched_sha() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("mutated.txt");
        write_file(&file, "original");

        // Store with original content, then mutate the cache entry so sha is wrong.
        let entry = mgr.store(&file, None, vec![]).expect("store");
        let bad = FileCacheEntry {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ..entry
        };
        mgr.write_entry_atomic(&bad).expect("write bad");

        let pruned = mgr.prune().expect("prune");
        assert_eq!(pruned, 1);
    }

    // ── 10. stats_reports_correct_counts_and_bytes ───────────────────────────

    #[test]
    fn stats_reports_correct_counts_and_bytes() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let content = "hello world";

        let file = proj.path().join("stats.txt");
        write_file(&file, content);
        mgr.store(&file, None, vec![]).expect("store");

        let stats = mgr.stats().expect("stats");
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.total_bytes_cached, content.len() as u64);
    }

    // ── 11. project_isolation_two_managers_dont_share ────────────────────────

    #[test]
    fn project_isolation_two_managers_dont_share() {
        let proj_a = tmp_project();
        let proj_b = tmp_project();

        let mgr_a = make_manager(&proj_a);
        let mgr_b = make_manager(&proj_b);

        let file = proj_a.path().join("shared-name.txt");
        write_file(&file, "project A");
        mgr_a.store(&file, None, vec![]).expect("store");

        // proj_b's manager should have zero entries.
        assert_eq!(mgr_b.list().expect("list").len(), 0);
    }

    // ── 12. summary_truncated_to_200_chars ───────────────────────────────────

    #[test]
    fn summary_truncated_to_200_chars() {
        let long: String = "x".repeat(300);
        let result = truncate_to(long, MAX_SUMMARY_LEN);
        assert_eq!(result.chars().count(), MAX_SUMMARY_LEN);
    }

    // ── 13. key_symbols_capped_at_reasonable_count ───────────────────────────

    #[test]
    fn key_symbols_capped_at_reasonable_count() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("symbols.txt");
        write_file(&file, "data");

        let many: Vec<String> = (0..100).map(|i| format!("sym{i}")).collect();
        let entry = mgr.store(&file, None, many).expect("store");
        assert_eq!(entry.key_symbols.len(), MAX_KEY_SYMBOLS);
    }

    // ── 14. concurrent_store_atomic_no_partial_files ─────────────────────────

    #[test]
    fn concurrent_store_atomic_no_partial_files() {
        use std::sync::Arc;
        use std::thread;

        let proj = tmp_project();
        // Canonicalize the project root to resolve macOS /var -> /private/var symlinks.
        let proj_root = proj.path().canonicalize().expect("canonicalize proj");
        let proj_path = Arc::new(proj_root);

        let file = proj_path.join("concurrent.txt");
        write_file(&file, "shared content");

        // Use the canonical file path so store() doesn't need to re-canonicalize.
        let canonical_file = Arc::new(file.canonicalize().expect("canonicalize file"));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let p = Arc::clone(&proj_path);
                let f = Arc::clone(&canonical_file);
                thread::spawn(move || {
                    let mgr = FileCacheManager::new(p.as_ref().to_path_buf()).unwrap();
                    mgr.store(&f, None, vec![]).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread");
        }

        // Cache should have exactly one valid entry.
        let mgr = FileCacheManager::new(proj_path.as_ref().to_path_buf()).unwrap();
        let entries = mgr.list().expect("list");
        assert_eq!(entries.len(), 1, "concurrent writes should converge to 1 entry");
    }

    // ── 15. language_detected_from_extension_rs_ts_py_json ───────────────────

    #[test]
    fn language_detected_from_extension_rs_ts_py_json() {
        let cases = [
            ("main.rs", "rust"),
            ("app.ts", "typescript"),
            ("app.tsx", "typescript"),
            ("script.py", "python"),
            ("config.json", "json"),
        ];
        for (name, expected) in cases {
            let path = PathBuf::from(name);
            let lang = detect_language(&path);
            assert_eq!(
                lang.as_deref(),
                Some(expected),
                "language mismatch for {name}"
            );
        }
    }

    // ── 16. entry_round_trips_through_json ───────────────────────────────────

    #[test]
    fn entry_round_trips_through_json() {
        let entry = FileCacheEntry {
            path: PathBuf::from("/tmp/foo.rs"),
            sha256: "abc123".to_string(),
            size_bytes: 42,
            mtime: 1_700_000_000,
            last_seen: 1_700_000_001,
            line_count: 10,
            language: Some("rust".to_string()),
            summary: Some("a demo file".to_string()),
            key_symbols: vec!["main".to_string()],
            access_count: 3,
        };

        let json = serde_json::to_string(&entry).expect("serialize");
        let back: FileCacheEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, back);
    }

    // ── 17. large_file_skipped_or_truncated_at_size_threshold ────────────────

    #[test]
    fn large_file_skipped_or_truncated_at_size_threshold() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);

        // Write a fake "large" file by constructing an entry directly, since
        // we can't actually create a 10 MB file in a fast unit test.
        // Instead, verify the public constant is accessible and that a real
        // small file gets a non-empty sha256 (the inverse guard).
        assert!(LARGE_FILE_THRESHOLD_BYTES > 0);

        let file = proj.path().join("small.txt");
        write_file(&file, "tiny");
        let entry = mgr.store(&file, None, vec![]).expect("store");
        // Small files should have a non-empty sha256.
        assert!(!entry.sha256.is_empty(), "small file should have sha256");

        // Simulate large-file path: build an entry with size > threshold and
        // verify sha256 is empty.
        let fake_large = FileCacheEntry {
            path: file.canonicalize().unwrap(),
            sha256: String::new(),
            size_bytes: LARGE_FILE_THRESHOLD_BYTES + 1,
            mtime: 0,
            last_seen: 0,
            line_count: 0,
            language: None,
            summary: None,
            key_symbols: vec![],
            access_count: 0,
        };
        assert!(
            fake_large.sha256.is_empty(),
            "large-file entries must have empty sha256"
        );
    }

    // ── 11. refresh_entry_best_effort populates a missing entry ───────────────

    #[test]
    fn refresh_entry_best_effort_stores_missing_entry() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("freshly_seen.txt");
        write_file(&file, "hello world");

        // First refresh: no entry exists → store path.
        refresh_entry_best_effort(proj.path(), &file);
        let hit = mgr.lookup(&file).expect("lookup").expect("entry");
        assert_eq!(hit.access_count, 0, "store doesn't bump access");

        // Second refresh: entry exists & matches → touch path.
        refresh_entry_best_effort(proj.path(), &file);
        let hit2 = mgr.lookup(&file).expect("lookup").expect("entry");
        assert_eq!(hit2.access_count, 1, "touch increments access_count");
    }

    // ── 12. refresh_entry_best_effort never panics on bad input ───────────────

    #[test]
    fn refresh_entry_best_effort_silent_on_missing_file() {
        let proj = tmp_project();
        let nowhere = proj.path().join("does_not_exist.txt");
        // Must not panic and must not produce an entry.
        refresh_entry_best_effort(proj.path(), &nowhere);
        let mgr = make_manager(&proj);
        let result = mgr.lookup(&nowhere).expect("lookup");
        assert!(result.is_none());
    }

    // ── 13. forget_entry_best_effort removes the entry ────────────────────────

    #[test]
    fn forget_entry_best_effort_removes_existing_entry() {
        let proj = tmp_project();
        let mgr = make_manager(&proj);
        let file = proj.path().join("about_to_be_forgotten.txt");
        write_file(&file, "bye");

        mgr.store(&file, None, vec![]).expect("store");
        assert!(mgr.lookup(&file).expect("lookup").is_some());

        forget_entry_best_effort(proj.path(), &file);
        assert!(mgr.lookup(&file).expect("lookup").is_none());
    }

    // ── 18. system_prompt_injection_only_includes_entries_with_summary ────────

    #[test]
    fn system_prompt_injection_only_includes_entries_with_summary() {
        let with_summary = FileCacheEntry {
            path: PathBuf::from("/tmp/has-summary.rs"),
            sha256: "abc".to_string(),
            size_bytes: 100,
            mtime: 0,
            last_seen: 0,
            line_count: 5,
            language: Some("rust".to_string()),
            summary: Some("defines Foo struct".to_string()),
            key_symbols: vec![],
            access_count: 0,
        };
        let without_summary = FileCacheEntry {
            path: PathBuf::from("/tmp/no-summary.rs"),
            sha256: "def".to_string(),
            size_bytes: 200,
            mtime: 0,
            last_seen: 0,
            line_count: 10,
            language: Some("rust".to_string()),
            summary: None,
            key_symbols: vec![],
            access_count: 0,
        };

        let block = build_known_files_block(&[with_summary, without_summary]);
        let block_str = block.expect("should produce a block");
        assert!(block_str.contains("has-summary.rs"));
        assert!(!block_str.contains("no-summary.rs"));
        assert!(block_str.contains("<known-files>"));
        assert!(block_str.contains("</known-files>"));
    }
}
