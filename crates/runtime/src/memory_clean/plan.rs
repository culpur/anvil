/// memory_clean::plan — Phase 6.5b (continued)
///
/// Builds a `RewritePlan` from a set of scanned entries.
///
/// The plan describes what each entry will look like after cleaning without
/// committing any changes to disk.  This is used by `--dry-run` mode and the
/// interactive preview.

use std::path::PathBuf;

use crate::memory_clean::scan::ScannedEntry;

// ── RewritePlan ───────────────────────────────────────────────────────────────

/// A complete plan describing what `anvil memory clean` will do.
#[derive(Debug, Clone, Default)]
pub struct RewritePlan {
    /// Entries that will be rewritten.
    pub entries: Vec<PlannedEntry>,
    /// Entries that are already clean (no rewriter action needed) and will
    /// be skipped.  Note: these are NOT the same as progress-skipped (hash
    /// gate) entries; those never appear in the plan at all.
    pub clean_entries: Vec<PathBuf>,
    /// RFC 3339 timestamp when this plan was built.
    pub planned_at: String,
}

impl RewritePlan {
    /// Return the number of entries that will be rewritten.
    pub fn rewrite_count(&self) -> usize {
        self.entries.len()
    }

    /// Build a human-readable summary string for `--dry-run` preview.
    pub fn dry_run_summary(&self) -> String {
        let n = self.entries.len();
        let skip = self.clean_entries.len();
        let mut lines = vec![
            format!("[DRY RUN] anvil memory clean — {n} entr{} to rewrite, {skip} already clean",
                if n == 1 { "y" } else { "ies" })
        ];
        for pe in &self.entries {
            let name = pe.path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| pe.path.display().to_string());
            lines.push(format!("  {name}:"));
            for reason in &pe.anticipated_changes {
                lines.push(format!("    - {reason}"));
            }
        }
        lines.join("\n")
    }
}

/// A single entry in the rewrite plan.
#[derive(Debug, Clone)]
pub struct PlannedEntry {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Content hash before rewrite (used for idempotency gating in apply.rs).
    pub pre_clean_hash: String,
    /// Anticipated change descriptions (derived from static analysis of the
    /// body — not from the LLM which runs during apply).
    pub anticipated_changes: Vec<String>,
    /// Provenance fields that will be PRESERVED verbatim.
    pub provenance: ProvenanceFields,
}

/// Frontmatter fields that must survive the rewrite untouched.
#[derive(Debug, Clone)]
pub struct ProvenanceFields {
    pub imported_from: String,
    pub imported_at: Option<String>,
    pub source_path: Option<String>,
    /// The original content_hash (pre-import).
    pub original_content_hash: Option<String>,
    /// The raw frontmatter block, stored so apply.rs can splice in the new
    /// `rewritten_*` fields without re-parsing.
    pub raw_frontmatter: String,
}

// ── Plan builder ──────────────────────────────────────────────────────────────

/// Build a [`RewritePlan`] from a slice of scanned entries.
///
/// Does NOT call the LLM — it performs static analysis to produce
/// `anticipated_changes` hinting at what the LLM will likely do.  The actual
/// changes are determined by the rewriter during apply.
pub fn build_plan(entries: &[ScannedEntry]) -> RewritePlan {
    let planned_at = crate::import::now_rfc3339();
    let mut plan = RewritePlan {
        planned_at,
        ..Default::default()
    };

    for entry in entries {
        let anticipated = anticipate_changes(&entry.body);

        if anticipated.is_empty() {
            // Body looks clean already; mark as clean (still runs through the
            // rewriter in non-dry-run mode to be sure, but skip in plan preview).
            plan.clean_entries.push(entry.path.clone());
            continue;
        }

        // Extract the original content_hash from the frontmatter.
        let original_content_hash = extract_frontmatter_value(&entry.raw_frontmatter, "content_hash");

        plan.entries.push(PlannedEntry {
            path: entry.path.clone(),
            pre_clean_hash: entry.content_hash.clone(),
            anticipated_changes: anticipated,
            provenance: ProvenanceFields {
                imported_from: entry.imported_from.clone(),
                imported_at: entry.imported_at.clone(),
                source_path: entry.source_path.clone(),
                original_content_hash,
                raw_frontmatter: entry.raw_frontmatter.clone(),
            },
        });
    }

    plan
}

/// Static analysis hinting at what changes the LLM will apply.
///
/// Returns an empty Vec when the body looks clean and no changes are
/// anticipated.
fn anticipate_changes(body: &str) -> Vec<String> {
    let mut hints = Vec::new();

    let cc_count = body.matches("Claude Code").count();
    if cc_count > 0 {
        hints.push(format!("normalize 'CC' ({cc_count} occurrence(s) of 'Claude Code')"));
    }

    // Count `.claude/` occurrences that look like config paths (not citations).
    // A citation looks like: "source_path: ~/.claude/..." — we skip those.
    let dot_claude_non_cite = count_non_citation_dot_claude(body);
    if dot_claude_non_cite > 0 {
        hints.push(format!("replace .claude/ → .anvil/ ({dot_claude_non_cite} likely config reference(s))"));
    }

    // Identity preambles.
    let preambles = ["As an AI assistant", "I am Claude", "As Claude,"];
    for p in &preambles {
        if body.contains(p) {
            hints.push(format!("strip identity preamble: '{p}'"));
        }
    }

    hints
}

/// Count `.claude/` occurrences that are NOT in citation/provenance lines.
///
/// A line is considered a citation if it contains `source_path:`,
/// `imported_from:`, or the literal `~/.claude/projects/`.
fn count_non_citation_dot_claude(body: &str) -> usize {
    let mut count = 0;
    for line in body.lines() {
        if line.contains("source_path:")
            || line.contains("imported_from:")
            || line.contains("~/.claude/projects/")
        {
            continue;
        }
        count += line.matches(".claude/").count();
    }
    count
}

/// Extract a scalar YAML value from a raw frontmatter block.
fn extract_frontmatter_value(raw_fm: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in raw_fm.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_clean::scan::ScannedEntry;
    use std::path::PathBuf;

    fn make_entry(name: &str, body: &str) -> ScannedEntry {
        let raw_frontmatter = format!(
            "---\nimported_from: claude_code\nimported_at: 2026-05-15T00:00:00Z\nsource_path: ~/.claude/memory/{name}\ncontent_hash: abc123\n---"
        );
        ScannedEntry {
            path: PathBuf::from(format!("/home/user/.anvil/memory/{name}")),
            content_hash: "deadbeef".to_string(),
            imported_from: "claude_code".to_string(),
            imported_at: Some("2026-05-15T00:00:00Z".to_string()),
            source_path: Some(format!("~/.claude/memory/{name}")),
            body: body.to_string(),
            raw_frontmatter,
            scanned_at: "2026-05-15T00:00:00Z".to_string(),
        }
    }

    // ── plan flags entries with CC occurrences ────────────────────────────────

    #[test]
    fn plan_flags_cc_occurrences() {
        let entry = make_entry("rule.md", "Use Claude Code for all tasks.");
        let plan = build_plan(&[entry]);
        assert_eq!(plan.rewrite_count(), 1, "should flag 1 entry");
        assert!(
            plan.entries[0].anticipated_changes.iter().any(|c| c.contains("CC")),
            "should anticipate CC normalization"
        );
    }

    // ── plan flags identity preamble ──────────────────────────────────────────

    #[test]
    fn plan_flags_identity_preamble() {
        let entry = make_entry("intro.md", "As an AI assistant, I help with coding.\n");
        let plan = build_plan(&[entry]);
        assert_eq!(plan.rewrite_count(), 1);
        assert!(
            plan.entries[0].anticipated_changes.iter().any(|c| c.contains("preamble")),
            "should flag identity preamble"
        );
    }

    // ── plan marks clean entries ──────────────────────────────────────────────

    #[test]
    fn plan_marks_clean_entries() {
        let entry = make_entry("clean.md", "Always prefer Rust over Python.\n");
        let plan = build_plan(&[entry]);
        assert_eq!(plan.rewrite_count(), 0, "no entries to rewrite");
        assert_eq!(plan.clean_entries.len(), 1, "one clean entry");
    }

    // ── plan preserves provenance fields ─────────────────────────────────────

    #[test]
    fn plan_preserves_provenance_fields() {
        let entry = make_entry("rule.md", "Use Claude Code daily.");
        let plan = build_plan(&[entry]);
        assert_eq!(plan.rewrite_count(), 1);
        let pe = &plan.entries[0];
        assert_eq!(pe.provenance.imported_from, "claude_code");
        assert_eq!(pe.provenance.imported_at.as_deref(), Some("2026-05-15T00:00:00Z"));
        assert!(pe.provenance.source_path.as_ref().unwrap().contains(".claude/memory/"));
    }

    // ── dry_run_summary format ────────────────────────────────────────────────

    #[test]
    fn dry_run_summary_format() {
        let entry = make_entry("rule.md", "Use Claude Code daily.");
        let plan = build_plan(&[entry]);
        let summary = plan.dry_run_summary();
        assert!(summary.contains("[DRY RUN]"), "must contain DRY RUN label");
        assert!(summary.contains("rule.md"), "must mention filename");
        assert!(summary.contains("CC"), "must mention CC normalization");
    }

    // ── citation lines not counted as dot-claude ──────────────────────────────

    #[test]
    fn citation_lines_not_flagged() {
        // A body that only references .claude/ in the source_path line
        // should not trigger the replacement hint.
        let body = "source_path: ~/.claude/projects/proj123/memory/rule.md\nOther content.\n";
        let entry = make_entry("rule.md", body);
        let plan = build_plan(&[entry]);
        // Should be clean (no claude_code normalization and no non-citation references).
        assert_eq!(plan.rewrite_count(), 0, "citation lines must not be flagged");
    }
}
