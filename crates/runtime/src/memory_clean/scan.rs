/// memory_clean::scan — Phase 6.5b
///
/// Discover `~/.anvil/memory/*.md` files that carry
/// `imported_from: claude_code` frontmatter and are therefore eligible for
/// the Day-2 clean-up pass.
///
/// # Design
///
/// - Reads files one at a time — never loads all entries into memory at once.
/// - Uses the `imported_from` frontmatter field as the gate.  Entries without
///   it (user-authored entries) are silently skipped.
/// - Supports glob-style `--filter` matching against the filename stem.
/// - Resumability is handled by the caller via the progress file; scan itself
///   is stateless.

use std::path::{Path, PathBuf};

use crate::import::{now_rfc3339, sha256_hex};
use crate::import::staging::anvil_config_home;

// ── ScannedEntry ──────────────────────────────────────────────────────────────

/// A single memory file eligible for clean-up.
#[derive(Debug, Clone)]
pub struct ScannedEntry {
    /// Absolute path to the `.md` file under `~/.anvil/memory/`.
    pub path: PathBuf,
    /// SHA-256 of the current file contents (used for idempotency gating).
    pub content_hash: String,
    /// The `imported_from` frontmatter value (always `"claude_code"` for
    /// imported entries, but we store whatever we found).
    pub imported_from: String,
    /// The `imported_at` frontmatter value (RFC 3339), if present.
    pub imported_at: Option<String>,
    /// The `source_path` frontmatter value, if present.
    pub source_path: Option<String>,
    /// Body text (everything after the closing `---` of the frontmatter).
    pub body: String,
    /// Full raw frontmatter block (including delimiters) so the apply phase
    /// can round-trip it exactly.
    pub raw_frontmatter: String,
    /// Timestamp of the scan (RFC 3339), injected by `scan_memory_dir`.
    pub scanned_at: String,
}

// ── Scan options ──────────────────────────────────────────────────────────────

/// Options for [`scan_memory_dir`].
#[derive(Debug, Clone, Default)]
pub struct ScanOpts {
    /// Optional glob-style filter applied against the filename stem.
    ///
    /// Supports `*` as a wildcard.  `None` means "match all".
    pub filter: Option<String>,
}

// ── scan_memory_dir ───────────────────────────────────────────────────────────

/// Scan `~/.anvil/memory/` (or `memory_dir` when supplied directly for tests)
/// and return all entries that carry `imported_from: claude_code` in their
/// frontmatter, subject to `opts.filter`.
///
/// Files are processed one at a time — the Vec contains only lightweight
/// metadata plus the body string.  For very large memory dirs, callers should
/// process entries in a streaming fashion rather than loading all bodies.
///
/// # Errors
///
/// Never returns `Err` — unreadable or non-conformant files are silently
/// skipped.  The returned slice contains only successfully parsed entries.
pub fn scan_memory_dir(memory_dir: Option<&Path>, opts: &ScanOpts) -> Vec<ScannedEntry> {
    let dir = memory_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| anvil_config_home().join("memory"));

    if !dir.is_dir() {
        return Vec::new();
    }

    let scanned_at = now_rfc3339();

    let mut entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut result: Vec<ScannedEntry> = Vec::new();

    while let Some(Ok(de)) = entries.next() {
        let path = de.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // Apply filename filter if provided.
        if let Some(ref pattern) = opts.filter {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !glob_match(pattern, &stem) {
                continue;
            }
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let text = match std::str::from_utf8(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Parse frontmatter.
        let parsed = match parse_frontmatter(text) {
            Some(p) => p,
            None => continue,
        };

        // Only keep entries with `imported_from: claude_code`.
        if parsed.imported_from.as_deref() != Some("claude_code") {
            continue;
        }

        let content_hash = sha256_hex(&bytes);

        result.push(ScannedEntry {
            path,
            content_hash,
            imported_from: parsed.imported_from.unwrap_or_default(),
            imported_at: parsed.imported_at,
            source_path: parsed.source_path,
            body: parsed.body,
            raw_frontmatter: parsed.raw_frontmatter,
            scanned_at: scanned_at.clone(),
        });
    }

    // Sort for deterministic ordering.
    result.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

// ── Frontmatter parsing ───────────────────────────────────────────────────────

struct ParsedFrontmatter {
    raw_frontmatter: String,
    imported_from: Option<String>,
    imported_at: Option<String>,
    source_path: Option<String>,
    body: String,
}

/// Parse YAML frontmatter from a markdown document.
///
/// Returns `None` if the document does not start with `---`.
fn parse_frontmatter(text: &str) -> Option<ParsedFrontmatter> {
    if !text.starts_with("---") {
        return None;
    }

    // Find the closing `---` (after the first line).
    let after_opener = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))?;

    let close_offset = after_opener.find("\n---")
        .or_else(|| after_opener.find("\r\n---"))?;

    let fm_body = &after_opener[..close_offset];
    let raw_frontmatter = format!("---\n{fm_body}\n---");

    // Everything after the closing delimiter is the body.
    // Skip the `\n---` itself plus the newline that follows.
    let after_close = &after_opener[close_offset + 1..]; // skip leading \n
    let body = after_close
        .strip_prefix("---\n")
        .or_else(|| after_close.strip_prefix("---\r\n"))
        .or_else(|| after_close.strip_prefix("---"))
        .unwrap_or(after_close)
        .to_string();

    // Extract individual fields by scanning YAML lines.
    let imported_from = find_yaml_value(fm_body, "imported_from");
    let imported_at = find_yaml_value(fm_body, "imported_at");
    let source_path = find_yaml_value(fm_body, "source_path");

    Some(ParsedFrontmatter {
        raw_frontmatter,
        imported_from,
        imported_at,
        source_path,
        body,
    })
}

/// Extract a plain scalar value from a YAML-like string.
///
/// Looks for a line starting with `key: ` (possibly quoted).
fn find_yaml_value(yaml: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in yaml.lines() {
        let line = line.trim();
        if line.starts_with(&prefix) {
            let raw = line[prefix.len()..].trim();
            // Strip surrounding quotes if present.
            let val = raw
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(raw);
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

// ── Glob matching ─────────────────────────────────────────────────────────────

/// Minimal glob matching: `*` matches any sequence of characters.
///
/// Case-sensitive.  Used for `--filter` matching against filename stems.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let search_in = &text[pos..];
        if i == 0 {
            // First part must match at the start.
            if !search_in.starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            // Last part must match at the end.
            return search_in.ends_with(part);
        } else {
            match search_in.find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_imported_entry(dir: &Path, name: &str, body: &str) {
        let content = format!(
            "---\nimported_from: claude_code\nimported_at: 2026-05-15T00:00:00Z\nsource_path: ~/.claude/memory/{name}\ncontent_hash: abc123\n---\n{body}"
        );
        std::fs::write(dir.join(name), content).expect("write entry");
    }

    fn write_user_entry(dir: &Path, name: &str, body: &str) {
        let content = format!("---\ntitle: User entry\n---\n{body}");
        std::fs::write(dir.join(name), content).expect("write user entry");
    }

    fn write_bare_entry(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write bare entry");
    }

    // ── scan finds imported entries ───────────────────────────────────────────

    #[test]
    fn scan_finds_imported_entries() {
        let dir = TempDir::new().expect("tmpdir");
        write_imported_entry(dir.path(), "feedback-foo.md", "Some feedback.\n");
        write_imported_entry(dir.path(), "feedback-bar.md", "More feedback.\n");
        write_user_entry(dir.path(), "user-note.md", "User note.\n");

        let opts = ScanOpts::default();
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert_eq!(entries.len(), 2, "should find 2 imported entries");
    }

    // ── scan skips user-authored entries ─────────────────────────────────────

    #[test]
    fn scan_skips_user_entries() {
        let dir = TempDir::new().expect("tmpdir");
        write_user_entry(dir.path(), "user-note.md", "User content.\n");

        let opts = ScanOpts::default();
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert!(entries.is_empty(), "user entries must not appear in scan");
    }

    // ── scan skips bare files (no frontmatter) ────────────────────────────────

    #[test]
    fn scan_skips_bare_files() {
        let dir = TempDir::new().expect("tmpdir");
        write_bare_entry(dir.path(), "bare.md", "# No frontmatter\n\nJust text.\n");

        let opts = ScanOpts::default();
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert!(entries.is_empty(), "bare files must be skipped");
    }

    // ── scan respects filter glob ─────────────────────────────────────────────

    #[test]
    fn scan_filter_glob_matches_stems() {
        let dir = TempDir::new().expect("tmpdir");
        write_imported_entry(dir.path(), "feedback-foo.md", "foo.\n");
        write_imported_entry(dir.path(), "feedback-bar.md", "bar.\n");
        write_imported_entry(dir.path(), "other-thing.md", "other.\n");

        let opts = ScanOpts {
            filter: Some("feedback-*".to_string()),
        };
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert_eq!(entries.len(), 2, "filter should match 2 entries");
        assert!(entries.iter().all(|e| {
            e.path.file_name().unwrap().to_string_lossy().starts_with("feedback-")
        }));
    }

    // ── scan on empty dir returns empty ───────────────────────────────────────

    #[test]
    fn scan_empty_dir_returns_empty() {
        let dir = TempDir::new().expect("tmpdir");
        let opts = ScanOpts::default();
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert!(entries.is_empty(), "empty dir must return empty vec");
    }

    // ── ScannedEntry carries correct metadata ─────────────────────────────────

    #[test]
    fn scan_entry_carries_metadata() {
        let dir = TempDir::new().expect("tmpdir");
        write_imported_entry(dir.path(), "test-entry.md", "Body content.\n");

        let opts = ScanOpts::default();
        let entries = scan_memory_dir(Some(dir.path()), &opts);
        assert_eq!(entries.len(), 1);

        let e = &entries[0];
        assert_eq!(e.imported_from, "claude_code");
        assert_eq!(e.imported_at.as_deref(), Some("2026-05-15T00:00:00Z"));
        assert!(e.body.contains("Body content."));
        assert!(!e.content_hash.is_empty());
        assert!(e.raw_frontmatter.starts_with("---"));
    }

    // ── glob_match tests ──────────────────────────────────────────────────────

    #[test]
    fn glob_match_exact() {
        assert!(glob_match("feedback-foo", "feedback-foo"));
        assert!(!glob_match("feedback-foo", "feedback-bar"));
    }

    #[test]
    fn glob_match_wildcard() {
        assert!(glob_match("feedback-*", "feedback-foo"));
        assert!(glob_match("feedback-*", "feedback-bar-baz"));
        assert!(!glob_match("feedback-*", "other-foo"));
    }

    #[test]
    fn glob_match_leading_wildcard() {
        assert!(glob_match("*foo", "something-foo"));
        assert!(!glob_match("*foo", "something-bar"));
    }

    #[test]
    fn glob_match_all_wildcard() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }
}
