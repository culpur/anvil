/// Report module — generate the user-facing import summary.
///
/// Phase 6.0: report schema and generator.  The report is written to
/// `~/.anvil/.import-report.md` after the Commit phase completes (Bucket 4
/// wires this into the full TUI flow).
///
/// # Report sections
///
/// 1. Summary line: "Imported N artifacts from CC (version: X.Y.Z)"
/// 2. Per-status table: Committed / Staged / Skipped / Failed
/// 3. Skipped with reasons (from `ImportEntryStatus::Skipped`)
/// 4. Failed with errors (from `ImportEntryStatus::Failed`)
/// 5. Needs-review items (future: flagged during Translate)
/// 6. Footer: "Run `anvil import claude-code` again to re-import changed files."

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
