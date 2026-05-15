// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// memory_clean::apply — Phase 6.5e
///
/// Commits rewrites to disk.  For each planned entry:
///
/// 1. Invoke the rewriter on the body.
/// 2. Splice the new `rewritten_at`, `rewritten_by`, `rewritten_from_hash`
///    frontmatter fields into the existing frontmatter — after the provenance
///    fields, before everything else.  `imported_from`, `imported_at`,
///    `source_path`, and the original `content_hash` are preserved verbatim.
/// 3. Compute the new `content_hash` of the rewritten document.
/// 4. Write atomically via a temp-file + rename.
/// 5. Record the entry in the progress store.
///
/// # Dry-run safety
///
/// When `dry_run` is `true`, no writes occur.  The function returns
/// [`ApplyRecord`]s with `status: DryRun` and the anticipated rewritten
/// content for preview.
///
/// # Idempotency
///
/// Before processing an entry, the progress store is checked.  If the entry's
/// `pre_clean_hash` matches a previously-completed hash, the entry is skipped
/// (`status: AlreadyClean`).

use std::path::{Path, PathBuf};

use crate::import::{now_rfc3339, sha256_hex};
use crate::memory_clean::plan::PlannedEntry;
use crate::memory_clean::progress::CleanProgress;
use crate::memory_clean::rewriter::MemoryRewriter;

/// Version string stamped into `rewritten_by`.
pub const REWRITER_VERSION: &str = "anvil memory clean v2.2.14";

// ── ApplyRecord ───────────────────────────────────────────────────────────────

/// Outcome for a single planned entry.
#[derive(Debug, Clone)]
pub struct ApplyRecord {
    pub path: PathBuf,
    pub status: ApplyStatus,
    /// Human-readable changes applied (from the rewriter).
    pub changes: Vec<String>,
    /// New content hash (post-rewrite).  Empty on error or dry-run without LLM.
    pub new_content_hash: String,
    /// Human-readable reason (populated for `Skipped` and `Failed`).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStatus {
    /// Entry was rewritten and written to disk.
    Rewritten,
    /// Entry was rewritten in memory (dry-run preview only).
    DryRun,
    /// Entry's hash was already in the progress store — no-op.
    AlreadyClean,
    /// Rewriter returned an error or I/O failed.
    Failed,
}

// ── apply_plan ────────────────────────────────────────────────────────────────

/// Apply rewrites for all entries in `planned_entries`.
///
/// `progress` is read before each entry and updated after each successful
/// write.  Pass a `CleanProgress` loaded from `~/.anvil/.memory-clean-progress.json`.
///
/// When `dry_run` is `true`, no files are written; the rewriter is still
/// called so the preview contains the actual LLM output.  Pass `None` for
/// `rewriter` to skip the LLM call in dry-run mode (preview uses
/// `anticipated_changes` from the plan instead).
pub fn apply_plan(
    planned_entries: &[PlannedEntry],
    rewriter: &dyn MemoryRewriter,
    progress: &mut CleanProgress,
    dry_run: bool,
) -> Vec<ApplyRecord> {
    let rewritten_at = now_rfc3339();
    let mut records = Vec::new();

    for pe in planned_entries {
        // Idempotency gate.
        if progress.is_done(&pe.pre_clean_hash) {
            records.push(ApplyRecord {
                path: pe.path.clone(),
                status: ApplyStatus::AlreadyClean,
                changes: vec!["already cleaned on a previous run".to_string()],
                new_content_hash: String::new(),
                reason: Some("hash matches completed entry".to_string()),
            });
            continue;
        }

        // Read the current file content.
        let file_text = match std::fs::read_to_string(&pe.path) {
            Ok(t) => t,
            Err(e) => {
                records.push(ApplyRecord {
                    path: pe.path.clone(),
                    status: ApplyStatus::Failed,
                    changes: vec![],
                    new_content_hash: String::new(),
                    reason: Some(format!("read failed: {e}")),
                });
                continue;
            }
        };

        // Extract the body (re-parse to be sure we have the current body).
        let body = extract_body_from_file(&file_text);

        // Invoke the rewriter.
        let rw_result = match rewriter.rewrite(&body) {
            Ok(r) => r,
            Err(e) => {
                records.push(ApplyRecord {
                    path: pe.path.clone(),
                    status: ApplyStatus::Failed,
                    changes: vec![],
                    new_content_hash: String::new(),
                    reason: Some(format!("rewriter failed: {e}")),
                });
                continue;
            }
        };

        // Rebuild the file with updated frontmatter + rewritten body.
        let new_doc = splice_rewrite_fields(
            &pe.provenance.raw_frontmatter,
            &pe.pre_clean_hash,
            &rewritten_at,
            REWRITER_VERSION,
            &rw_result.rewritten,
        );

        let new_hash = sha256_hex(new_doc.as_bytes());

        if dry_run {
            records.push(ApplyRecord {
                path: pe.path.clone(),
                status: ApplyStatus::DryRun,
                changes: rw_result.changes,
                new_content_hash: new_hash,
                reason: None,
            });
            continue;
        }

        // Atomic write: temp file + rename.
        if let Err(e) = atomic_write(&pe.path, new_doc.as_bytes()) {
            records.push(ApplyRecord {
                path: pe.path.clone(),
                status: ApplyStatus::Failed,
                changes: rw_result.changes,
                new_content_hash: String::new(),
                reason: Some(format!("write failed: {e}")),
            });
            continue;
        }

        // Update progress.
        progress.mark_done(pe.pre_clean_hash.clone());

        records.push(ApplyRecord {
            path: pe.path.clone(),
            status: ApplyStatus::Rewritten,
            changes: rw_result.changes,
            new_content_hash: new_hash,
            reason: None,
        });
    }

    records
}

// ── Frontmatter splicing ──────────────────────────────────────────────────────

/// Build the final document string from the existing frontmatter, the new
/// `rewritten_*` fields, and the rewritten body.
///
/// The provenance fields (`imported_from`, `imported_at`, `source_path`,
/// `content_hash`) are preserved verbatim from `raw_frontmatter`.
///
/// The new fields are appended into the frontmatter block.  If a
/// `rewritten_at` field already exists (the entry was previously cleaned),
/// it is replaced; otherwise it is added.
pub fn splice_rewrite_fields(
    raw_frontmatter: &str,
    pre_clean_hash: &str,
    rewritten_at: &str,
    rewritten_by: &str,
    rewritten_body: &str,
) -> String {
    // Strip the `---` delimiters from the raw frontmatter.
    let fm_inner = raw_frontmatter
        .strip_prefix("---\n")
        .or_else(|| raw_frontmatter.strip_prefix("---\r\n"))
        .unwrap_or(raw_frontmatter);
    let fm_inner = fm_inner
        .strip_suffix("\n---")
        .or_else(|| fm_inner.strip_suffix("\r\n---"))
        .or_else(|| fm_inner.strip_suffix("---"))
        .unwrap_or(fm_inner);

    // Remove any existing rewritten_* lines so we can re-stamp them.
    let clean_inner: String = fm_inner
        .lines()
        .filter(|l| {
            !l.trim_start().starts_with("rewritten_at:")
                && !l.trim_start().starts_with("rewritten_by:")
                && !l.trim_start().starts_with("rewritten_from_hash:")
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Also remove the OLD content_hash line (will be replaced by the new one below).
    let clean_inner_no_hash: String = clean_inner
        .lines()
        .filter(|l| !l.trim_start().starts_with("content_hash:"))
        .collect::<Vec<_>>()
        .join("\n");

    // Compute new content hash for the final document (we'll compute it after
    // building the string — placeholder replaced afterwards).
    // For now, build the frontmatter with the pre-clean hash as `rewritten_from_hash`
    // and a placeholder for the new `content_hash`.
    let new_hash_placeholder = "__CONTENT_HASH__";

    let new_fm = format!(
        "---\n\
         {clean_inner_no_hash}\n\
         content_hash: {new_hash_placeholder}\n\
         rewritten_at: {rewritten_at}\n\
         rewritten_by: {rewritten_by}\n\
         rewritten_from_hash: {pre_clean_hash}\n\
         ---"
    );

    let draft = format!("{new_fm}\n{rewritten_body}");

    // Compute the real hash of the final document and substitute.
    let real_hash = sha256_hex(
        draft.replace(new_hash_placeholder, "").as_bytes()
    );
    draft.replace(new_hash_placeholder, &real_hash)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the body text from a file with YAML frontmatter.
///
/// Returns everything after the closing `---` delimiter.
/// Returns the whole text if no frontmatter is found.
fn extract_body_from_file(text: &str) -> String {
    if !text.starts_with("---") {
        return text.to_string();
    }
    let after_opener = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .unwrap_or(text);
    let close_offset = match after_opener.find("\n---") {
        Some(i) => i,
        None => return text.to_string(),
    };
    let after_close = &after_opener[close_offset + 1..];
    after_close
        .strip_prefix("---\n")
        .or_else(|| after_close.strip_prefix("---\r\n"))
        .or_else(|| after_close.strip_prefix("---"))
        .unwrap_or(after_close)
        .to_string()
}

/// Atomic file write: write to a temp file first, then rename.
fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_clean::plan::{PlannedEntry, ProvenanceFields};
    use crate::memory_clean::progress::CleanProgress;
    use crate::memory_clean::rewriter::MockRewriter;
    use serial_test::serial;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_planned_entry(dir: &Path, name: &str, body: &str) -> PlannedEntry {
        let raw_fm = format!(
            "---\nimported_from: claude_code\nimported_at: 2026-05-15T00:00:00Z\nsource_path: ~/.claude/memory/{name}\ncontent_hash: abc123\n---"
        );
        let content = format!("{raw_fm}\n{body}");
        let path = dir.join(name);
        std::fs::write(&path, &content).expect("write entry");
        let hash = sha256_hex(content.as_bytes());

        PlannedEntry {
            path,
            pre_clean_hash: hash,
            anticipated_changes: vec!["normalize 'CC'".to_string()],
            provenance: ProvenanceFields {
                imported_from: "claude_code".to_string(),
                imported_at: Some("2026-05-15T00:00:00Z".to_string()),
                source_path: Some(format!("~/.claude/memory/{name}")),
                original_content_hash: Some("abc123".to_string()),
                raw_frontmatter: raw_fm,
            },
        }
    }

    // ── dry-run produces no file changes ─────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn dry_run_produces_no_file_changes() {
        let dir = TempDir::new().expect("tmpdir");
        let entry = make_planned_entry(dir.path(), "test.md", "Use Claude Code here.\n");
        let original = std::fs::read_to_string(&entry.path).expect("read");

        let rewriter = MockRewriter;
        let mut progress = CleanProgress::default();
        let records = apply_plan(&[entry.clone()], &rewriter, &mut progress, true);

        let after = std::fs::read_to_string(&entry.path).expect("read after");
        assert_eq!(original, after, "dry-run must not modify files");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, ApplyStatus::DryRun);
    }

    // ── live run rewrites file content ────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn live_run_rewrites_file() {
        let dir = TempDir::new().expect("tmpdir");
        let entry = make_planned_entry(dir.path(), "rule.md", "Use Claude Code.\n");
        let original = std::fs::read_to_string(&entry.path).expect("read");

        let rewriter = MockRewriter;
        let mut progress = CleanProgress::default();
        let records = apply_plan(&[entry.clone()], &rewriter, &mut progress, false);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, ApplyStatus::Rewritten);

        let after = std::fs::read_to_string(&entry.path).expect("read after");
        assert_ne!(original, after, "file must change after live run");
        assert!(after.contains("CC"), "should contain CC");
        assert!(!after.contains("Claude Code"), "should not contain original text");
    }

    // ── idempotent re-run is a no-op ──────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn idempotent_rerun_is_noop() {
        let dir = TempDir::new().expect("tmpdir");
        let entry = make_planned_entry(dir.path(), "rule.md", "Use Claude Code.\n");

        let rewriter = MockRewriter;
        let mut progress = CleanProgress::default();

        // First run.
        let first = apply_plan(&[entry.clone()], &rewriter, &mut progress, false);
        assert_eq!(first[0].status, ApplyStatus::Rewritten);

        // Second run with the same pre_clean_hash in progress.
        let second = apply_plan(&[entry.clone()], &rewriter, &mut progress, false);
        assert_eq!(
            second[0].status,
            ApplyStatus::AlreadyClean,
            "second run must be a no-op"
        );
    }

    // ── provenance frontmatter is preserved verbatim ──────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn provenance_frontmatter_preserved() {
        let dir = TempDir::new().expect("tmpdir");
        let entry = make_planned_entry(dir.path(), "provenance.md", "Use Claude Code.\n");

        let rewriter = MockRewriter;
        let mut progress = CleanProgress::default();
        let _ = apply_plan(&[entry.clone()], &rewriter, &mut progress, false);

        let after = std::fs::read_to_string(&entry.path).expect("read");
        assert!(after.contains("imported_from: claude_code"), "imported_from must be preserved");
        assert!(after.contains("imported_at: 2026-05-15T00:00:00Z"), "imported_at must be preserved");
        assert!(after.contains("source_path: ~/.claude/memory/provenance.md"), "source_path must be preserved");
        assert!(after.contains("rewritten_at:"), "rewritten_at must be added");
        assert!(after.contains("rewritten_by:"), "rewritten_by must be added");
        assert!(after.contains("rewritten_from_hash:"), "rewritten_from_hash must be added");
    }

    // ── splice_rewrite_fields test ────────────────────────────────────────────

    #[test]
    fn splice_rewrite_fields_round_trip() {
        let raw_fm = "---\nimported_from: claude_code\nimported_at: 2026-05-15T00:00:00Z\nsource_path: ~/.claude/memory/rule.md\ncontent_hash: abc123\n---";
        let result = splice_rewrite_fields(
            raw_fm,
            "abc123",
            "2026-05-15T01:00:00Z",
            "anvil memory clean v2.2.14",
            "Prefer CC.\n",
        );
        assert!(result.contains("imported_from: claude_code"), "must preserve imported_from");
        assert!(result.contains("imported_at: 2026-05-15T00:00:00Z"), "must preserve imported_at");
        assert!(result.contains("source_path: ~/.claude/memory/rule.md"), "must preserve source_path");
        assert!(result.contains("rewritten_at: 2026-05-15T01:00:00Z"), "must add rewritten_at");
        assert!(result.contains("rewritten_by: anvil memory clean v2.2.14"), "must add rewritten_by");
        assert!(result.contains("rewritten_from_hash: abc123"), "must add rewritten_from_hash");
        assert!(result.contains("content_hash:"), "must have content_hash");
        assert!(result.contains("Prefer CC."), "must contain rewritten body");
    }
}
