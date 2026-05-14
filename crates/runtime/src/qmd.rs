// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
// Rust 2024 gates env mutation behind `unsafe`; the crate-wide
// `#![forbid(unsafe_code)]` lint would otherwise block it.
#![cfg_attr(test, allow(unsafe_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use serde::Deserialize;

/// A single result returned by the QMD CLI.
#[derive(Debug, Clone)]
pub struct QmdResult {
    pub file: String,
    pub title: String,
    pub score: f64,
    pub snippet: String,
}

// f64 does not implement Eq, so we implement PartialEq manually and provide a
// trivially-consistent Eq to satisfy the `SystemPromptBuilder` derive.
impl PartialEq for QmdResult {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.title == other.title
            && self.snippet == other.snippet
            // Score comparison: bitwise equality so NaN != NaN (acceptable for
            // our caching / dedup purposes).
            && self.score.to_bits() == other.score.to_bits()
    }
}

impl Eq for QmdResult {}

/// Index statistics returned by `qmd status`.
#[derive(Debug, Clone)]
pub struct QmdStatus {
    pub total_docs: u32,
    pub total_vectors: u32,
    pub size_mb: f64,
}

/// Wire-format for a single JSON result object from `qmd search --json`.
#[derive(Debug, Deserialize)]
struct RawQmdResult {
    file: String,
    #[serde(default)]
    title: String,
    score: f64,
    #[serde(default)]
    snippet: String,
}

/// Wire-format for `qmd status --json` (best-effort; the CLI may not support
/// machine-readable status yet, so we parse what we can).
#[derive(Debug, Deserialize)]
struct RawQmdStatus {
    total_docs: Option<u32>,
    total_vectors: Option<u32>,
    size_mb: Option<f64>,
}

/// Client that wraps the `qmd` CLI binary.
///
/// If the binary cannot be found the client is created in a disabled state and
/// every method returns an empty result without error, so callers can treat QMD
/// as a pure enhancement rather than a hard dependency.
pub struct QmdClient {
    qmd_path: Option<PathBuf>,
    /// Per-session query cache: query string → results.
    cache: Mutex<HashMap<String, Vec<QmdResult>>>,
}

impl QmdClient {
    /// Create a new client, auto-detecting the `qmd` binary.
    ///
    /// Uses the `which` crate for cross-platform PATH lookup (handles
    /// `PATHEXT` on Windows).  Falls back to well-known install locations on
    /// macOS (`/opt/homebrew/bin`) and Windows (`%LOCALAPPDATA%\Programs\qmd`,
    /// `%PROGRAMFILES%\qmd`).  If the binary cannot be found the client starts
    /// disabled and all search methods return empty results.
    #[must_use]
    pub fn new() -> Self {
        let qmd_path = Self::detect_binary();
        Self {
            qmd_path,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` when the QMD binary was found and the client is active.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.qmd_path.is_some()
    }

    /// Run `qmd search "<query>" -n <limit> --json` and return parsed results.
    ///
    /// This uses BM25 keyword search, which is fast and requires no model
    /// download.  Results with `score < min_score` are filtered out.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize, min_score: f64) -> Vec<QmdResult> {
        let Some(ref path) = self.qmd_path else {
            return Vec::new();
        };

        // Check the per-session cache first.
        let cache_key = format!("search:{limit}:{min_score:.2}:{query}");
        if let Ok(cache) = self.cache.lock()
            && let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }

        let output = Command::new(path)
            .args(["search", query, "-n", &limit.to_string(), "--json"])
            .output();

        let results = match output {
            Ok(out) if out.status.success() => {
                parse_results(&out.stdout, min_score)
            }
            _ => Vec::new(),
        };

        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, results.clone());
        }

        results
    }

    /// Return index statistics by running `qmd status --json`.
    ///
    /// Returns `None` if QMD is disabled or if the status command fails.
    #[must_use]
    pub fn status(&self) -> Option<QmdStatus> {
        let path = self.qmd_path.as_ref()?;

        let output = Command::new(path)
            .args(["status", "--json"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        // The current QMD CLI emits plain text for `status`.  Attempt JSON
        // first; fall back to scraping the plain-text output.
        if let Ok(raw) = serde_json::from_slice::<RawQmdStatus>(&output.stdout) {
            return Some(QmdStatus {
                total_docs: raw.total_docs.unwrap_or(0),
                total_vectors: raw.total_vectors.unwrap_or(0),
                size_mb: raw.size_mb.unwrap_or(0.0),
            });
        }

        // Plain-text scrape fallback.
        let text = String::from_utf8_lossy(&output.stdout);
        parse_status_plain(&text)
    }

    /// Search within a specific named QMD collection.
    ///
    /// Uses the `--collection` flag if the QMD binary supports it; falls back
    /// to a plain search otherwise.  Results with `score < min_score` are
    /// filtered out.
    #[must_use]
    pub fn search_collection(
        &self,
        collection: &str,
        query: &str,
        limit: usize,
        min_score: f64,
    ) -> Vec<QmdResult> {
        let Some(ref path) = self.qmd_path else {
            return Vec::new();
        };

        let cache_key = format!("col:{collection}:{limit}:{min_score:.2}:{query}");
        if let Ok(cache) = self.cache.lock()
            && let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }

        let output = Command::new(path)
            .args([
                "search",
                query,
                "--collection",
                collection,
                "-n",
                &limit.to_string(),
                "--json",
            ])
            .output();

        let results = match output {
            Ok(out) if out.status.success() => parse_results(&out.stdout, min_score),
            _ => Vec::new(),
        };

        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cache_key, results.clone());
        }

        results
    }

    /// Ensure the `anvil-semantic` collection exists. Best-effort: silently
    /// no-ops if QMD is not installed or the collection-add command fails.
    ///
    /// `semantic_dir` is the directory where promoted nomination markdown
    /// files live (typically `~/.anvil/semantic/`). On first call this
    /// directory is created so the QMD `collection add` glob has a target.
    pub fn ensure_semantic_indexed(&self, semantic_dir: &Path) {
        let Some(ref qmd_path) = self.qmd_path else {
            return;
        };

        // Create the directory if needed so the glob has somewhere to
        // resolve. Failing to create is fatal-silent — we cannot index a
        // dir we can't write to, but we don't crash the runtime either.
        if std::fs::create_dir_all(semantic_dir).is_err() {
            return;
        }

        // Probe by searching for "anvil-semantic" with a sentinel that
        // can't appear in real content. If the collection is missing the
        // search fails; we then add it.
        let probe = Command::new(qmd_path)
            .args([
                "search",
                "__anvil_semantic_probe__",
                "--collection",
                "anvil-semantic",
                "-n",
                "1",
                "--json",
            ])
            .output();

        let needs_add = probe.map_or(true, |out| !out.status.success());

        if needs_add {
            let glob = format!("{}/**/*.md", semantic_dir.display());
            let _ = Command::new(qmd_path)
                .args(["collection", "add", "anvil-semantic", &glob])
                .output();
        }

        // Make sure freshly written promoted-nomination files are embedded.
        let _ = Command::new(qmd_path).args(["update"]).output();

        self.invalidate_cache();
    }

    /// Ensure the `anvil-history` collection is registered with QMD and up to
    /// date.  This is a best-effort operation: failures are silently ignored
    /// because QMD is an optional enhancement.
    ///
    /// `history_dir` is the path to `~/.anvil/history/`.
    pub fn ensure_history_indexed(&self, history_dir: &Path) {
        let Some(ref qmd_path) = self.qmd_path else {
            return;
        };

        // Check whether the collection already exists by running a status
        // query.  We probe with `qmd search` on the collection; if it returns
        // a non-zero exit code we attempt to add the collection first.
        let probe = Command::new(qmd_path)
            .args(["search", "test", "--collection", "anvil-history", "-n", "1", "--json"])
            .output();

        let needs_add = probe.map_or(true, |out| !out.status.success());

        if needs_add {
            // `qmd collection add anvil-history <glob>`
            let glob = format!("{}/**/*.md", history_dir.display());
            let _ = Command::new(qmd_path)
                .args(["collection", "add", "anvil-history", &glob])
                .output();
        }

        // Always run `qmd update` so newly written archive files are embedded.
        let _ = Command::new(qmd_path).args(["update"]).output();

        self.invalidate_cache();
    }

    /// Invalidate the in-process query cache.  Call this when the index is
    /// known to have changed (e.g. after a memory write).
    pub fn invalidate_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    fn detect_binary() -> Option<PathBuf> {
        // 1. Use the `which` crate for cross-platform PATH lookup.
        //    On Windows this respects PATHEXT (.exe, .cmd, .bat) automatically.
        //    On Unix it behaves identically to the `which` shell built-in.
        if let Ok(p) = which::which("qmd") {
            return Some(p);
        }

        // 2. macOS Homebrew well-known location (may not be on PATH in some
        //    shell environments such as GUI-launched apps).
        #[cfg(target_os = "macos")]
        {
            let homebrew = PathBuf::from("/opt/homebrew/bin/qmd");
            if homebrew.is_file() {
                return Some(homebrew);
            }
        }

        // 3. Common Windows install locations.
        #[cfg(target_os = "windows")]
        {
            for candidate in [
                std::env::var("LOCALAPPDATA")
                    .ok()
                    .map(|d| PathBuf::from(d).join("Programs").join("qmd").join("qmd.exe")),
                std::env::var("PROGRAMFILES")
                    .ok()
                    .map(|d| PathBuf::from(d).join("qmd").join("qmd.exe")),
            ]
            .into_iter()
            .flatten()
            {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        None
    }
}

impl Default for QmdClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Phase 3.2: anvil-semantic collection (free-function helpers)
// ---------------------------------------------------------------------------

/// Return the on-disk directory for the `anvil-semantic` QMD collection.
///
/// Lives under `~/.anvil/semantic/`. Files are individual promoted-nomination
/// markdown documents with YAML frontmatter pointing back at the source ID.
#[must_use]
pub fn semantic_dir() -> PathBuf {
    crate::config::default_config_home().join("semantic")
}

/// Rename auto-generated `MEMORY.md` (if present) inside the semantic dir to
/// `_index.md` so it does not collide with project-level ANVIL.md/MEMORY.md.
/// If neither exists, create `_index.md` blank.
///
/// Returns the path of the index file on success. Best-effort: surfaces I/O
/// errors so the caller can ignore them.
pub fn normalize_semantic_index() -> std::io::Result<PathBuf> {
    let dir = semantic_dir();
    std::fs::create_dir_all(&dir)?;
    let memory_md = dir.join("MEMORY.md");
    let index_md = dir.join("_index.md");
    if memory_md.exists() && !index_md.exists() {
        std::fs::rename(&memory_md, &index_md)?;
    } else if !index_md.exists() {
        // Blank stub so the QMD glob has something to embed; the next
        // promote will overwrite or augment as needed.
        std::fs::write(&index_md, "# anvil-semantic\n\nPromoted nominations live here.\n")?;
    }
    Ok(index_md)
}

/// Best-effort: ensure the anvil-semantic collection is registered with QMD.
///
/// Returns `true` when QMD is enabled and the call ran (success is implicit
/// — collection-add is idempotent). Returns `false` when QMD is not on PATH.
pub fn ensure_semantic_collection() -> bool {
    let client = QmdClient::new();
    if !client.is_enabled() {
        return false;
    }
    // Normalize the index file before we add the collection so the first
    // `qmd update` has a non-empty glob target.
    let _ = normalize_semantic_index();
    client.ensure_semantic_indexed(&semantic_dir());
    true
}

/// Phase 3.2: index a promoted nomination into the anvil-semantic collection.
///
/// Writes a markdown file at `<semantic_dir>/<nomination-id>.md` with
/// frontmatter `nominated_from: <id>` and the body text. Then triggers a
/// `qmd update` so the embedding is fresh.
///
/// Returns `Ok(true)` when the file was written AND QMD was invoked.
/// Returns `Ok(false)` when QMD is not installed (the file is still written
/// so a later `qmd` install picks it up).
///
/// # Errors
/// Returns the I/O error if the file or directory cannot be created.
pub fn index_promoted_nomination(nomination_id: &str, body: &str) -> std::io::Result<bool> {
    let dir = semantic_dir();
    std::fs::create_dir_all(&dir)?;
    // Make sure the auto-generated MEMORY.md doesn't collide with our index.
    let _ = normalize_semantic_index();

    let target = dir.join(format!("{nomination_id}.md"));
    let frontmatter = format!(
        "---\nnominated_from: {nomination_id}\n---\n\n{}\n",
        body.trim_end()
    );
    // Atomic write: tmp + rename.
    let tmp = dir.join(format!("{nomination_id}.md.tmp"));
    std::fs::write(&tmp, frontmatter.as_bytes())?;
    std::fs::rename(&tmp, &target)?;

    let client = QmdClient::new();
    if !client.is_enabled() {
        return Ok(false);
    }
    // Re-run the collection-add + update path so the new file ends up
    // embedded. Best-effort — failures don't surface.
    client.ensure_semantic_indexed(&dir);
    Ok(true)
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_results(stdout: &[u8], min_score: f64) -> Vec<QmdResult> {
    let raw: Vec<RawQmdResult> = match serde_json::from_slice(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    raw.into_iter()
        .filter(|r| r.score >= min_score)
        .map(|r| QmdResult {
            file: strip_qmd_prefix(&r.file),
            title: r.title,
            score: r.score,
            snippet: clean_snippet(&r.snippet),
        })
        .collect()
}

/// Strip the `qmd://collection/` URI prefix so callers see a clean path.
fn strip_qmd_prefix(file: &str) -> String {
    // qmd://projects/some/path.md → some/path.md
    if let Some(after_scheme) = file.strip_prefix("qmd://") {
        // Drop the collection segment (first path component).
        if let Some(after_collection) = after_scheme.split_once('/').map(|x| x.1) {
            return after_collection.to_string();
        }
    }
    file.to_string()
}

/// Clean up the diff-style snippet QMD returns into readable prose.
fn clean_snippet(raw: &str) -> String {
    // Snippets look like: "@@ -24,4 @@ (23 before, 110 after)\n<content>"
    // Drop the leading `@@ ... @@` line if present.
    let cleaned = if let Some(body) = raw.find('\n') {
        let first_line = raw[..body].trim();
        if first_line.starts_with("@@") {
            raw[body + 1..].trim()
        } else {
            raw.trim()
        }
    } else {
        raw.trim()
    };

    #[allow(clippy::items_after_statements)]
    const MAX_SNIPPET: usize = 300; // Limit to a reasonable snippet length (~300 chars).
    if cleaned.chars().count() > MAX_SNIPPET {
        let mut truncated: String = cleaned.chars().take(MAX_SNIPPET).collect();
        truncated.push_str("...");
        truncated
    } else {
        cleaned.to_string()
    }
}

/// Scrape `total_docs` from plain-text `qmd status` output.
fn parse_status_plain(text: &str) -> Option<QmdStatus> {
    let mut total_docs: u32 = 0;
    let mut total_vectors: u32 = 0;
    let mut size_mb: f64 = 0.0;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Total:") {
            let val_str = rest.split_whitespace().next().unwrap_or("0");
            if let Ok(v) = val_str.replace(',', "").parse::<u32>() {
                // First "Total:" is docs, second is vectors.
                if total_docs == 0 {
                    total_docs = v;
                } else {
                    total_vectors = v;
                }
            }
        } else if let Some(rest) = line.strip_prefix("Size:") {
            let val_str = rest.split_whitespace().next().unwrap_or("0");
            let numeric: String = val_str.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            size_mb = numeric.parse::<f64>().unwrap_or(0.0);
        }
    }

    if total_docs > 0 {
        Some(QmdStatus { total_docs, total_vectors, size_mb })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Prompt rendering
// ---------------------------------------------------------------------------

/// Render QMD history search results as a `<history-context>` system-reminder block.
///
/// Returns an empty string if `results` is empty.
#[must_use]
pub fn render_history_context(results: &[QmdResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "# Historical Context (from previous sessions)".to_string(),
        String::new(),
        "<history-context>".to_string(),
    ];

    for result in results {
        lines.push(format!(
            r#"<document file="{}" score="{:.2}">"#,
            result.file, result.score
        ));
        lines.push(result.snippet.clone());
        lines.push("</document>".to_string());
    }

    lines.push("</history-context>".to_string());
    lines.join("\n")
}

/// Render QMD search results as a system prompt section.
///
/// Returns an empty string if `results` is empty.
#[must_use]
pub fn render_qmd_context(results: &[QmdResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "# Workspace Knowledge (from QMD)".to_string(),
        "The following documents are relevant to the current conversation:".to_string(),
        String::new(),
        "<qmd-context>".to_string(),
    ];

    for result in results {
        lines.push(format!(
            r#"<document file="{}" score="{:.2}">"#,
            result.file, result.score
        ));
        lines.push(result.snippet.clone());
        lines.push("</document>".to_string());
    }

    lines.push("</qmd-context>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn strip_qmd_prefix_removes_scheme_and_collection() {
        assert_eq!(
            strip_qmd_prefix("qmd://projects/ems-main/docs/cvs.md"),
            "ems-main/docs/cvs.md"
        );
    }

    #[test]
    fn strip_qmd_prefix_leaves_plain_paths_unchanged() {
        assert_eq!(strip_qmd_prefix("just/a/path.md"), "just/a/path.md");
    }

    #[test]
    fn clean_snippet_removes_diff_header() {
        let raw = "@@ -1,3 @@ (0 before, 10 after)\nactual content here";
        assert_eq!(clean_snippet(raw), "actual content here");
    }

    #[test]
    fn clean_snippet_truncates_long_content() {
        let long = "x".repeat(400);
        let result = clean_snippet(&long);
        assert!(result.ends_with("..."));
        assert!(result.len() < 320);
    }

    #[test]
    fn parse_results_filters_by_min_score() {
        let json = r#"[
            {"file": "qmd://projects/a.md", "title": "A", "score": 0.9, "snippet": "hi"},
            {"file": "qmd://projects/b.md", "title": "B", "score": 0.2, "snippet": "low"}
        ]"#;
        let results = parse_results(json.as_bytes(), 0.5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "A");
    }

    #[test]
    fn render_qmd_context_is_empty_for_no_results() {
        assert!(render_qmd_context(&[]).is_empty());
    }

    #[test]
    fn render_qmd_context_formats_xml_section() {
        let results = vec![QmdResult {
            file: "ems-main/docs/cvs.md".to_string(),
            title: "CVS Guide".to_string(),
            score: 0.92,
            snippet: "The credential vault...".to_string(),
        }];
        let rendered = render_qmd_context(&results);
        assert!(rendered.contains("# Workspace Knowledge (from QMD)"));
        assert!(rendered.contains("<qmd-context>"));
        assert!(rendered.contains(r#"file="ems-main/docs/cvs.md""#));
        assert!(rendered.contains("The credential vault..."));
        assert!(rendered.contains("</qmd-context>"));
    }

    #[test]
    fn parse_status_plain_extracts_counts() {
        let text = "Index: /tmp/index.sqlite\nSize:  106.6 MB\n\nDocuments\n  Total:    2489 files indexed\n  Vectors:  11882 embedded\n";
        let status = parse_status_plain(text).expect("should parse");
        assert_eq!(status.total_docs, 2489);
        assert!(status.size_mb > 100.0);
    }

    // ── Phase 3.2: anvil-semantic collection helpers ────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn index_promoted_nomination_writes_frontmatter_file() {
        let _lock = crate::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");
        // SAFETY: env mutation serialised on the crate-wide test lock.
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", tmp.path()); }

        let id = "nom-test-abc123";
        let body = "Always run cargo test before commit";
        // Returns true with qmd installed, false without; either way the
        // on-disk markdown file must be written.
        let _ = super::index_promoted_nomination(id, body).expect("index call");

        let semantic = tmp.path().join("semantic");
        let target = semantic.join(format!("{id}.md"));
        assert!(target.exists(), "promoted nomination md file must exist at {target:?}");

        let written = std::fs::read_to_string(&target).expect("read");
        assert!(written.contains("nominated_from: nom-test-abc123"),
                "frontmatter must record source id; got: {written}");
        assert!(written.contains("Always run cargo test before commit"),
                "body must be preserved; got: {written}");

        // _index.md should be created as part of normalize_semantic_index.
        assert!(semantic.join("_index.md").exists(),
                "_index.md stub must be created");

        if let Some(prev) = prev_home {
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", prev); }
        } else {
            unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        }
    }

    #[test]
    #[serial(anvil_config_home)]
    fn normalize_semantic_index_renames_memory_md() {
        let _lock = crate::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var_os("ANVIL_CONFIG_HOME");
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", tmp.path()); }

        // Seed an auto-generated MEMORY.md.
        let semantic_dir = tmp.path().join("semantic");
        std::fs::create_dir_all(&semantic_dir).unwrap();
        std::fs::write(semantic_dir.join("MEMORY.md"), "stale auto-index").unwrap();

        let result = super::normalize_semantic_index().expect("normalize");
        assert_eq!(result, semantic_dir.join("_index.md"));
        assert!(!semantic_dir.join("MEMORY.md").exists(),
                "MEMORY.md must be renamed away");
        assert!(semantic_dir.join("_index.md").exists(),
                "_index.md must exist after rename");
        // Content is preserved by rename (not dropped).
        let body = std::fs::read_to_string(semantic_dir.join("_index.md")).unwrap();
        assert_eq!(body, "stale auto-index");

        if let Some(prev) = prev_home {
            unsafe { std::env::set_var("ANVIL_CONFIG_HOME", prev); }
        } else {
            unsafe { std::env::remove_var("ANVIL_CONFIG_HOME"); }
        }
    }

    #[test]
    fn ensure_semantic_collection_does_not_panic() {
        // Whether or not the qmd binary is on PATH, the call must not panic.
        // Returns false when qmd is absent, true when present — both are valid.
        let _result = super::ensure_semantic_collection();
    }

    // ── Cross-platform binary detection ──────────────────────────────────

    /// Disabled client (no qmd on PATH, no fallback) must not be enabled.
    ///
    /// This test is best-effort: it passes on machines where `qmd` is not
    /// installed and skips gracefully when it is.
    #[test]
    fn client_disabled_when_no_binary() {
        // Only assert disabled when `which::which` itself also finds nothing.
        // On CI / developer machines that have qmd installed this test would
        // be a false negative, so we check the precondition first.
        if which::which("qmd").is_ok() {
            // qmd is installed — skip rather than fail.
            return;
        }
        // On macOS, also skip when the Homebrew path actually exists.
        #[cfg(target_os = "macos")]
        if std::path::PathBuf::from("/opt/homebrew/bin/qmd").is_file() {
            return;
        }
        let client = QmdClient::new();
        assert!(
            !client.is_enabled(),
            "client must be disabled when qmd binary is not available"
        );
    }

    /// When `qmd` exists in a directory that is on the PATH the `which` crate
    /// must locate it.  We use `which::which_in` with an explicit search path
    /// so we never mutate process environment state (which is `unsafe` in
    /// Rust 2024 and forbidden by the workspace `unsafe-code` lint).
    #[test]
    #[cfg(unix)]
    fn detect_binary_finds_qmd_via_which_in() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("qmd");
        // Write a minimal shell stub (contents irrelevant; only existence + x bit matter).
        fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write stub");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .expect("chmod");

        // Build a search path string containing only our temp dir so we get a
        // deterministic result regardless of what `qmd` installations exist on
        // the real PATH.
        let search_path = std::env::join_paths(std::iter::once(dir.path().to_path_buf()))
            .expect("join_paths");

        let found = which::which_in("qmd", Some(&search_path), dir.path())
            .expect("which_in must find the stub");

        assert_eq!(found, bin, "resolved path must point to our stub");
    }

    /// `which::which` path splitting must use the OS-appropriate separator.
    ///
    /// On Unix it is `:`, on Windows it is `;`.  We verify the constant is
    /// correct by round-tripping a two-element path through split/join.
    #[test]
    fn path_env_split_join_roundtrip() {
        let dir_a = std::path::PathBuf::from("/fake/dir/a");
        let dir_b = std::path::PathBuf::from("/fake/dir/b");
        let joined = std::env::join_paths([&dir_a, &dir_b]).expect("join");
        let split: Vec<_> = std::env::split_paths(&joined).collect();
        assert_eq!(split, vec![dir_a, dir_b], "split_paths(join_paths(…)) must roundtrip");

        // Verify the separator is not `/` (which would produce a single path).
        let joined_str = joined.to_string_lossy();
        assert!(
            joined_str.contains(':') || joined_str.contains(';'),
            "joined PATH must contain OS path separator"
        );
    }

    /// Windows fallback candidates are built from LOCALAPPDATA / PROGRAMFILES.
    ///
    /// We cannot create real `.exe` files in a Unix CI environment, so this
    /// test only verifies the path *construction* logic, not disk existence.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_fallback_paths_use_env_vars() {
        // If LOCALAPPDATA is set, the expected path must end with the known suffix.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let expected = std::path::PathBuf::from(&local)
                .join("Programs")
                .join("qmd")
                .join("qmd.exe");
            assert!(
                expected.to_string_lossy().contains("Programs"),
                "LOCALAPPDATA fallback path must contain Programs segment"
            );
        }
        if let Ok(pf) = std::env::var("PROGRAMFILES") {
            let expected = std::path::PathBuf::from(&pf).join("qmd").join("qmd.exe");
            assert!(
                expected.to_string_lossy().ends_with("qmd.exe"),
                "PROGRAMFILES fallback path must end with qmd.exe"
            );
        }
    }
}
