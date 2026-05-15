/// memory_clean::report — Phase 6.5
///
/// Human-readable diff/summary for `anvil memory clean`.
///
/// Used to render the preview (before commit) and the completion report
/// (after commit).

use crate::memory_clean::apply::{ApplyRecord, ApplyStatus};
use crate::memory_clean::dedup::DedupCandidate;
use crate::memory_clean::plan::RewritePlan;

// ── CleanReport ───────────────────────────────────────────────────────────────

/// Report emitted after a clean run.
#[derive(Debug, Clone)]
pub struct CleanReport {
    pub total_scanned: usize,
    pub total_rewritten: usize,
    pub total_skipped: usize,
    pub total_failed: usize,
    pub dedup_candidates: Vec<DedupCandidate>,
    pub dry_run: bool,
}

impl CleanReport {
    /// Build a `CleanReport` from apply results.
    pub fn from_apply(
        records: &[ApplyRecord],
        dedup_candidates: Vec<DedupCandidate>,
        dry_run: bool,
    ) -> Self {
        let total_scanned = records.len();
        let total_rewritten = records
            .iter()
            .filter(|r| matches!(r.status, ApplyStatus::Rewritten | ApplyStatus::DryRun))
            .count();
        let total_skipped = records
            .iter()
            .filter(|r| r.status == ApplyStatus::AlreadyClean)
            .count();
        let total_failed = records
            .iter()
            .filter(|r| r.status == ApplyStatus::Failed)
            .count();
        Self {
            total_scanned,
            total_rewritten,
            total_skipped,
            total_failed,
            dedup_candidates,
            dry_run,
        }
    }

    /// Render the report as a markdown string.
    pub fn render(&self) -> String {
        let mode_label = if self.dry_run { " [DRY RUN]" } else { "" };
        let action_word = if self.dry_run { "would rewrite" } else { "rewritten" };

        let mut lines = vec![
            format!("=== anvil memory clean{mode_label} ==="),
            format!(
                "  Scanned:   {}\n  {}:  {}\n  Skipped:   {}\n  Failed:    {}",
                self.total_scanned,
                action_word,
                self.total_rewritten,
                self.total_skipped,
                self.total_failed,
            ),
        ];

        if !self.dedup_candidates.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Dedup candidates: {} pair(s) detected — review and merge manually.",
                self.dedup_candidates.len()
            ));
            for (i, c) in self.dedup_candidates.iter().enumerate() {
                let name_a = c.entries.0.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| c.entries.0.display().to_string());
                let name_b = c.entries.1.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| c.entries.1.display().to_string());
                lines.push(format!("  {}: {name_a} ↔ {name_b}  ({})", i + 1, c.reason));
            }
        }

        lines.join("\n")
    }
}

/// Render a plan preview (before applying).
pub fn render_plan_preview(plan: &RewritePlan) -> String {
    let summary = plan.dry_run_summary();
    let dedup_hint = if !plan.entries.is_empty() {
        "\nRun `/memory clean` to apply, or `/memory clean --dry-run` to preview again.\n\
         Add `--dedup` to also detect potential duplicate entries."
    } else {
        "\nAll imported entries look clean. Nothing to do."
    };
    format!("{summary}{dedup_hint}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_clean::apply::{ApplyRecord, ApplyStatus};
    use crate::memory_clean::dedup::DedupCandidate;
    use std::path::PathBuf;

    fn make_record(name: &str, status: ApplyStatus) -> ApplyRecord {
        ApplyRecord {
            path: PathBuf::from(format!("/home/.anvil/memory/{name}")),
            status,
            changes: vec!["normalized 'CC'".to_string()],
            new_content_hash: "newhash".to_string(),
            reason: None,
        }
    }

    #[test]
    fn report_counts_are_correct() {
        let records = vec![
            make_record("a.md", ApplyStatus::Rewritten),
            make_record("b.md", ApplyStatus::Rewritten),
            make_record("c.md", ApplyStatus::AlreadyClean),
            make_record("d.md", ApplyStatus::Failed),
        ];
        let report = CleanReport::from_apply(&records, vec![], false);
        assert_eq!(report.total_scanned, 4);
        assert_eq!(report.total_rewritten, 2);
        assert_eq!(report.total_skipped, 1);
        assert_eq!(report.total_failed, 1);
    }

    #[test]
    fn report_render_contains_summary() {
        let records = vec![make_record("a.md", ApplyStatus::Rewritten)];
        let report = CleanReport::from_apply(&records, vec![], false);
        let rendered = report.render();
        assert!(rendered.contains("anvil memory clean"));
        assert!(rendered.contains("Scanned:"));
        assert!(rendered.contains("rewritten:") || rendered.contains("Rewritten:"));
    }

    #[test]
    fn report_dry_run_label() {
        let records = vec![make_record("a.md", ApplyStatus::DryRun)];
        let report = CleanReport::from_apply(&records, vec![], true);
        let rendered = report.render();
        assert!(rendered.contains("DRY RUN"));
        assert!(rendered.contains("would rewrite"));
    }

    #[test]
    fn report_includes_dedup_candidates() {
        let candidates = vec![DedupCandidate {
            entries: (
                PathBuf::from("/home/.anvil/memory/rule-a.md"),
                PathBuf::from("/home/.anvil/memory/rule-b.md"),
            ),
            confidence: 0.85,
            reason: "Jaccard similarity 0.85".to_string(),
        }];
        let report = CleanReport::from_apply(&[], candidates, false);
        let rendered = report.render();
        assert!(rendered.contains("Dedup candidates:"));
        assert!(rendered.contains("rule-a.md"));
        assert!(rendered.contains("rule-b.md"));
    }
}
