/// Report module — generate the user-facing import summary.
///
/// Phase 6.0: report schema and generator.  The report is written to
/// `~/.anvil/.import-report.md` after the Commit phase completes (Bucket 4
/// wires this into the full TUI flow).
///
/// Phase 6.4: full-format report with all required sections (Source, Bring,
/// Needs Review, Skipped with reasons, Next steps).  Both dry-run and live
/// runs produce a report; dry-run reports carry a `[DRY RUN]` prefix.
///
/// # Report sections
///
/// 1. Header (with optional `[DRY RUN]` prefix)
/// 2. Source block
/// 3. Bring (committed) — per artifact type counts
/// 4. Needs Review — items flagged for user action
/// 5. Skipped with reasons
/// 6. Next steps footer

use crate::import::manifest::{ImportEntryStatus, ImportManifest};

/// Generate a markdown-formatted import report from the final manifest.
#[must_use]
pub fn generate_report(manifest: &ImportManifest, source_label: &str) -> String {
    let committed = manifest.count_by_status(&ImportEntryStatus::Committed);
    let skipped = manifest.count_by_status(&ImportEntryStatus::Skipped);
    let failed = manifest.count_by_status(&ImportEntryStatus::Failed);
    let pending = manifest.count_by_status(&ImportEntryStatus::Pending);
    let staged = manifest.count_by_status(&ImportEntryStatus::Staged);

    let mut lines = Vec::new();

    lines.push(format!(
        "# Import Report — {} (Anvil v{})",
        source_label, manifest.pipeline_version
    ));
    lines.push(String::new());

    // Summary table
    lines.push("## Summary".to_string());
    lines.push(String::new());
    lines.push("| Status    | Count |".to_string());
    lines.push("|-----------|------:|".to_string());
    lines.push(format!("| Committed | {committed:>5} |"));
    if staged > 0 {
        lines.push(format!("| Staged    | {staged:>5} |"));
    }
    lines.push(format!("| Skipped   | {skipped:>5} |"));
    lines.push(format!("| Failed    | {failed:>5} |"));
    if pending > 0 {
        lines.push(format!("| Pending   | {pending:>5} |"));
    }
    lines.push(String::new());

    // Skipped items
    let skipped_entries = manifest.entries_with_status(&ImportEntryStatus::Skipped);
    if !skipped_entries.is_empty() {
        lines.push("## Skipped (with reasons)".to_string());
        lines.push(String::new());
        for e in skipped_entries {
            let reason = e.skip_reason.as_deref().unwrap_or("no reason recorded");
            lines.push(format!(
                "- `{}` — {reason}",
                e.source_path.display()
            ));
        }
        lines.push(String::new());
    }

    // Failed items
    let failed_entries = manifest.entries_with_status(&ImportEntryStatus::Failed);
    if !failed_entries.is_empty() {
        lines.push("## Failed".to_string());
        lines.push(String::new());
        for e in failed_entries {
            let err = e.error.as_deref().unwrap_or("unknown error");
            lines.push(format!(
                "- `{}` — {err}",
                e.source_path.display()
            ));
        }
        lines.push(String::new());
    }

    // Footer
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(
        "Re-run `anvil import claude-code` at any time to import changed or new files. \
         Previously committed artifacts with unchanged content are automatically skipped."
            .to_string(),
    );

    lines.join("\n")
}

/// Write the report to `~/.anvil/.import-report.md`.
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns an error string on I/O failure.
pub fn write_report(manifest: &ImportManifest, source_label: &str) -> Result<std::path::PathBuf, String> {
    use crate::import::staging::anvil_config_home;

    let path = anvil_config_home().join(".import-report.md");
    let content = generate_report(manifest, source_label);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create report dir: {e}"))?;
    }
    std::fs::write(&path, content.as_bytes())
        .map_err(|e| format!("write report: {e}"))?;

    Ok(path)
}

// ── Phase 6.4 — Full-format report ──────────────────────────────────────────

/// Options for the Phase 6.4 full-format import report.
pub struct ReportOptions<'a> {
    /// Import source label (e.g. `"CC at ~/.claude/"`).
    pub source_label: &'a str,
    /// When `true`, the header carries `[DRY RUN]` and the "Bring" section
    /// says "would be committed" instead of "committed".
    pub dry_run: bool,
    /// Timestamp for the report header.  Pass `now_rfc3339()` for live runs;
    /// inject a fixed value in tests.
    pub timestamp: &'a str,
    /// Items that need user review before they are fully usable.
    /// Each entry is `(description, path)`.
    pub needs_review: &'a [(String, String)],
    /// Next-step bullets to append after the main sections.
    /// When empty, a standard set of bullets is used.
    pub next_steps: &'a [String],
}

/// Generate a full-format Phase 6.4 import report.
///
/// Always written — even on dry-run (with `[DRY RUN]` prefix).
#[must_use]
pub fn generate_full_report(manifest: &ImportManifest, opts: &ReportOptions<'_>) -> String {
    let dry_prefix = if opts.dry_run { "[DRY RUN] " } else { "" };
    let committed_verb = if opts.dry_run { "would be committed" } else { "committed" };

    let committed = manifest.count_by_status(&ImportEntryStatus::Committed);
    let skipped = manifest.count_by_status(&ImportEntryStatus::Skipped);
    let failed = manifest.count_by_status(&ImportEntryStatus::Failed);

    let mut lines: Vec<String> = Vec::new();

    // Header
    lines.push(format!(
        "# {dry_prefix}Anvil Import Report — {}",
        opts.timestamp
    ));
    lines.push(String::new());

    // Source
    lines.push("## Source".to_string());
    lines.push(String::new());
    lines.push(format!("{}", opts.source_label));
    lines.push(String::new());

    // Bring (committed)
    lines.push(format!("## Bring ({committed_verb})"));
    lines.push(String::new());

    if committed == 0 && !opts.dry_run {
        lines.push("- Nothing committed (all artifacts skipped or failed)".to_string());
    } else {
        // Group by artifact kind.
        let committed_entries = manifest.entries_with_status(&ImportEntryStatus::Committed);
        let mut by_kind: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for e in &committed_entries {
            *by_kind.entry(e.artifact.as_str()).or_default() += 1;
        }
        if by_kind.is_empty() && opts.dry_run {
            lines.push("- (dry-run: no artifacts staged yet)".to_string());
        } else {
            for (kind, count) in &by_kind {
                lines.push(format!("- {count} {kind} artifact{}", if *count == 1 { "" } else { "s" }));
            }
        }
    }
    lines.push(String::new());

    // Needs Review
    if !opts.needs_review.is_empty() {
        lines.push("## Needs Review".to_string());
        lines.push(String::new());
        for (desc, path) in opts.needs_review {
            lines.push(format!("- {desc}"));
            if !path.is_empty() {
                lines.push(format!("  Inspect: `{path}`"));
            }
        }
        lines.push(String::new());
    }

    // Skipped with reasons
    let skipped_entries = manifest.entries_with_status(&ImportEntryStatus::Skipped);
    if !skipped_entries.is_empty() {
        lines.push("## Skipped (with reasons)".to_string());
        lines.push(String::new());
        for e in skipped_entries {
            let reason = e.skip_reason.as_deref().unwrap_or("no reason recorded");
            lines.push(format!("- `{}` — {reason}", e.source_path.display()));
        }
        lines.push(String::new());
    }

    // Failed
    let failed_entries = manifest.entries_with_status(&ImportEntryStatus::Failed);
    if !failed_entries.is_empty() {
        lines.push("## Failed".to_string());
        lines.push(String::new());
        for e in failed_entries {
            let err = e.error.as_deref().unwrap_or("unknown error");
            lines.push(format!("- `{}` — {err}", e.source_path.display()));
        }
        lines.push(String::new());
    }

    // Summary line
    lines.push("## Summary".to_string());
    lines.push(String::new());
    lines.push(format!("- {committed} {committed_verb}"));
    lines.push(format!("- {skipped} skipped"));
    if failed > 0 {
        lines.push(format!("- {failed} failed"));
    }
    lines.push(String::new());

    // Next steps
    lines.push("## Next Steps".to_string());
    lines.push(String::new());
    if opts.next_steps.is_empty() {
        lines.push("- Review any files in `~/.anvil/.import-review/` if needed".to_string());
        lines.push("- Restart Anvil to pick up new ANVIL.md instructions".to_string());
        lines.push("- Run `/memory show semantic` to see imported memory entries".to_string());
        lines.push(
            "- Run `/import claude-code --include-sessions` to summarize past sessions (~$5 in tokens)".to_string()
        );
    } else {
        for step in opts.next_steps {
            lines.push(format!("- {step}"));
        }
    }
    lines.push(String::new());

    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(
        "Re-run `anvil import claude-code` at any time to import changed or new files. \
         Previously committed artifacts with unchanged content are automatically skipped."
            .to_string(),
    );

    lines.join("\n")
}

/// Write the full-format Phase 6.4 report to `~/.anvil/.import-report.md`.
///
/// Creates parent directories as needed.
/// Always written — even on dry-run (with `[DRY RUN]` prefix in the header).
///
/// # Errors
///
/// Returns an error string on I/O failure.
pub fn write_full_report(
    manifest: &ImportManifest,
    opts: &ReportOptions<'_>,
) -> Result<std::path::PathBuf, String> {
    use crate::import::staging::anvil_config_home;

    let path = anvil_config_home().join(".import-report.md");
    let content = generate_full_report(manifest, opts);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create report dir: {e}"))?;
    }
    std::fs::write(&path, content.as_bytes())
        .map_err(|e| format!("write report: {e}"))?;

    Ok(path)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::manifest::{ImportEntry, ImportEntryStatus, ImportManifest};
    use std::path::PathBuf;

    #[test]
    fn generate_report_contains_committed_count() {
        let mut m = ImportManifest::new("2.2.14-test");
        let mut e = ImportEntry::pending(
            "memory",
            PathBuf::from("/fake/.claude/memory/rule.md"),
            PathBuf::from("/fake/.anvil/memory/rule.md"),
            "abc",
            "2026-05-15T00:00:00Z",
        );
        e.status = ImportEntryStatus::Committed;
        m.push(e);

        let report = generate_report(&m, "cc:/home/user/.claude");
        assert!(report.contains("Import Report"), "should have header");
        assert!(report.contains("Committed"), "should mention committed");
        assert!(report.contains('1'), "should have count 1");
    }

    #[test]
    fn generate_report_shows_skip_reasons() {
        let mut m = ImportManifest::new("2.2.14-test");
        let mut e = ImportEntry::pending(
            "settings",
            PathBuf::from("/fake/.claude/settings.json"),
            PathBuf::from("/fake/.anvil/settings.json"),
            "hash",
            "2026-05-15T00:00:00Z",
        );
        e.status = ImportEntryStatus::Skipped;
        e.skip_reason = Some("credentials are not portable".into());
        m.push(e);

        let report = generate_report(&m, "cc:/home/user/.claude");
        assert!(report.contains("Skipped"), "should have skipped section");
        assert!(report.contains("credentials are not portable"));
    }

    #[test]
    fn generate_report_shows_failed_errors() {
        let mut m = ImportManifest::new("2.2.14-test");
        let mut e = ImportEntry::pending(
            "memory",
            PathBuf::from("/fake/.claude/memory/rule.md"),
            PathBuf::from("/fake/.anvil/memory/rule.md"),
            "hash",
            "2026-05-15T00:00:00Z",
        );
        e.status = ImportEntryStatus::Failed;
        e.error = Some("disk full".into());
        m.push(e);

        let report = generate_report(&m, "cc:/home/user/.claude");
        assert!(report.contains("Failed"), "should have failed section");
        assert!(report.contains("disk full"));
    }

    #[test]
    fn generate_report_contains_footer_rerun_hint() {
        let m = ImportManifest::new("2.2.14-test");
        let report = generate_report(&m, "cc:/home/user/.claude");
        assert!(report.contains("anvil import claude-code"));
    }
}
