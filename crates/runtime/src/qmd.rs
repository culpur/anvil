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
    /// Checks `/opt/homebrew/bin/qmd` first, then falls back to `which qmd`.
    /// If neither is found the client starts disabled.
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
    pub fn is_enabled(&self) -> bool {
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
        if let Ok(cache) = self.cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
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
        if let Ok(cache) = self.cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
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
        // 1. Well-known Homebrew location on macOS.
        let homebrew = PathBuf::from("/opt/homebrew/bin/qmd");
        if homebrew.is_file() {
            return Some(homebrew);
        }

        // 2. Resolve via PATH using `which`.
        let which_out = Command::new("which").arg("qmd").output().ok()?;
        if which_out.status.success() {
            let path_str = String::from_utf8(which_out.stdout).ok()?;
            let trimmed = path_str.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
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
        if let Some(after_collection) = after_scheme.splitn(2, '/').nth(1) {
            return after_collection.to_string();
        }
    }
    file.to_string()
}

/// Clean up the diff-style snippet QMD returns into readable prose.
fn clean_snippet(raw: &str) -> String {
    // Snippets look like: "@@ -24,4 @@ (23 before, 110 after)\n<content>"
    // Drop the leading `@@ ... @@` line if present.
    let cleaned = if let Some(body) = raw.find("\n") {
        let first_line = raw[..body].trim();
        if first_line.starts_with("@@") {
            raw[body + 1..].trim()
        } else {
            raw.trim()
        }
    } else {
        raw.trim()
    };

    // Limit to a reasonable snippet length (~300 chars).
    const MAX_SNIPPET: usize = 300;
    if cleaned.chars().count() > MAX_SNIPPET {
        let mut truncated: String = cleaned.chars().take(MAX_SNIPPET).collect();
        truncated.push_str("...");
        truncated
    } else {
        cleaned.to_string()
    }
}

/// Scrape total_docs from plain-text `qmd status` output.
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
}
