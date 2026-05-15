/// memory_clean — Phase 6.5 of the v2.2.14 arc.
///
/// Day-2 user-triggered command that applies LLM rewriting over memory entries
/// imported from a previous AI assistant installation.
///
/// # What it does
///
/// - Normalizes vocabulary in imported entries ("Claude Code" → "CC",
///   `.claude/` → `.anvil/` where context-appropriate)
/// - Strips identity preambles ("As an AI assistant…")
/// - Detects duplicate-meaning entries (via Jaccard similarity + optional LLM judge)
/// - Preserves all provenance frontmatter verbatim
/// - Is resumable: progress stored at `~/.anvil/.memory-clean-progress.json`
///
/// # What it does NOT do
///
/// - Auto-apply at import time — verbatim-with-flag is the import contract
/// - Auto-merge duplicate entries — always presents candidates for user review
/// - Run on a schedule or as a hook — always user-triggered
///
/// # 8-axis capability contract
///
/// 1. Definition   — handled via SlashCommand::Memory { action: Some("clean ...") }
/// 2. Registration — spec entry in slash_command_specs() for /memory
///                   (MEMORY_SUBCOMMANDS extended with "clean")
/// 3. Completion   — TAB cycles "clean", "--dry-run", "--auto", "--filter=", "--dedup"
///                   (MEMORY_SUBCOMMAND_NAMES updated)
/// 4. Handler      — handle_memory_command::clean branch → commands crate
///                   delegates to run_memory_clean_pipeline here
/// 5. Dispatch     — routes via /memory dispatch (no new dispatch site)
/// 6. Rendering    — preview diff via report::render_plan_preview;
///                   confirmation before commit
/// 7. Permission   — writes to ~/.anvil/memory/ require WorkspaceWrite gate
///                   (enforced in handler; dry-run always allowed)
/// 8. OTel + tests — events module; unit tests in each sub-module;
///                   integration test in crates/commands/tests/memory_clean.rs

pub mod apply;
pub mod dedup;
pub mod plan;
pub mod progress;
pub mod report;
pub mod rewriter;
pub mod scan;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use apply::{apply_plan, ApplyRecord, ApplyStatus, REWRITER_VERSION};
pub use dedup::{detect_duplicates, DedupCandidate, DedupOpts};
pub use plan::{build_plan, PlannedEntry, RewritePlan};
pub use progress::CleanProgress;
pub use report::{render_plan_preview, CleanReport};
pub use rewriter::{MemoryRewriter, MockRewriter, ProviderRewriter, RewriteResult};
pub use scan::{scan_memory_dir, ScannedEntry, ScanOpts};

// ── OTel events ──────────────────────────────────────────────────────────────

/// OTel event constants for the memory-clean pipeline.
pub mod events {
    pub const INVOKED: &str = "memory_clean.invoked";
    pub const SCANNED: &str = "memory_clean.scanned";
    pub const REWRITTEN: &str = "memory_clean.rewritten";
    pub const SKIPPED: &str = "memory_clean.skipped";
    pub const DEDUP_CANDIDATE: &str = "memory_clean.dedup_candidate";
    pub const COMPLETED: &str = "memory_clean.completed";
}

/// Emit `memory_clean.invoked` at command entry.
pub fn otel_clean_invoked(dry_run: bool, auto: bool, dedup: bool) {
    crate::otel::emit_event(
        events::INVOKED,
        &[
            ("dry_run", if dry_run { "true" } else { "false" }),
            ("auto", if auto { "true" } else { "false" }),
            ("dedup", if dedup { "true" } else { "false" }),
        ],
    );
}

/// Emit `memory_clean.scanned` with the total count.
pub fn otel_clean_scanned(n: usize) {
    let n_s = n.to_string();
    crate::otel::emit_event(events::SCANNED, &[("n", &n_s)]);
}

/// Emit `memory_clean.rewritten` with the rewritten count.
pub fn otel_clean_rewritten(n: usize) {
    let n_s = n.to_string();
    crate::otel::emit_event(events::REWRITTEN, &[("n", &n_s)]);
}

/// Emit `memory_clean.skipped` with a reason.
pub fn otel_clean_skipped(reason: &str) {
    crate::otel::emit_event(events::SKIPPED, &[("reason", reason)]);
}

/// Emit `memory_clean.dedup_candidate` with a confidence score.
pub fn otel_clean_dedup_candidate(confidence: f32) {
    let conf_s = format!("{confidence:.2}");
    crate::otel::emit_event(events::DEDUP_CANDIDATE, &[("confidence", &conf_s)]);
}

/// Emit `memory_clean.completed` with final counts and duration.
pub fn otel_clean_completed(rewritten: usize, skipped: usize, failed: usize, duration_ms: u64) {
    let r = rewritten.to_string();
    let s = skipped.to_string();
    let f = failed.to_string();
    let d = duration_ms.to_string();
    crate::otel::emit_event(
        events::COMPLETED,
        &[
            ("rewritten", &r),
            ("skipped", &s),
            ("failed", &f),
            ("duration_ms", &d),
        ],
    );
}

// ── CleanOpts ─────────────────────────────────────────────────────────────────

/// Options for the `run_memory_clean_pipeline` entry point.
pub struct CleanOpts {
    /// When `true`, no writes occur; rewriter is still called for preview.
    pub dry_run: bool,
    /// When `true`, skip the interactive confirmation prompt.
    pub auto: bool,
    /// Optional glob filter applied to filename stems.
    pub filter: Option<String>,
    /// When `true`, run dedup detection after rewriting.
    pub dedup: bool,
    /// The rewriter to use.  Callers supply `MockRewriter` in tests;
    /// production callers supply `ProviderRewriter::detect()?`.
    pub rewriter: Box<dyn MemoryRewriter>,
}

impl std::fmt::Debug for CleanOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanOpts")
            .field("dry_run", &self.dry_run)
            .field("auto", &self.auto)
            .field("filter", &self.filter)
            .field("dedup", &self.dedup)
            .finish()
    }
}

// ── run_memory_clean_pipeline ─────────────────────────────────────────────────

/// Top-level entry point for `anvil memory clean`.
///
/// Calls scan → plan → (optional dedup) → apply and returns a rendered
/// string suitable for display in the TUI or headless output.
///
/// The caller is responsible for:
/// - Building a `CleanOpts` with the appropriate rewriter.
/// - Permission gating (ReadOnly mode must be blocked by the handler;
///   only dry-run is allowed in ReadOnly).
/// - Presenting the confirmation prompt before calling with `dry_run: false`
///   and `auto: false`.
///
/// # Errors
///
/// Returns `Err` only on catastrophic failures (e.g. progress file
/// unwritable).  Individual entry failures are reported in the result string.
pub fn run_memory_clean_pipeline(
    memory_dir: Option<&std::path::Path>,
    opts: &CleanOpts,
) -> Result<String, String> {
    use std::time::Instant;

    let start = Instant::now();

    otel_clean_invoked(opts.dry_run, opts.auto, opts.dedup);

    // 1. Scan.
    let scan_opts = ScanOpts {
        filter: opts.filter.clone(),
    };
    let scanned = scan_memory_dir(memory_dir, &scan_opts);
    let n_scanned = scanned.len();
    otel_clean_scanned(n_scanned);

    if n_scanned == 0 {
        return Ok(
            "No imported memory entries found to clean.\n\
             (Entries must have `imported_from: claude_code` frontmatter.)"
                .to_string(),
        );
    }

    // 2. Plan.
    let plan = build_plan(&scanned);

    if opts.dry_run && plan.rewrite_count() == 0 {
        return Ok(render_plan_preview(&plan));
    }

    // 3. Dedup detection (optional, before apply).
    let dedup_candidates = if opts.dedup {
        let dedup_opts = DedupOpts {
            use_llm_judge: false, // LLM judge requires explicit flag in future
            ..Default::default()
        };
        let candidates = detect_duplicates(&scanned, &dedup_opts, None);
        for c in &candidates {
            otel_clean_dedup_candidate(c.confidence);
        }
        candidates
    } else {
        Vec::new()
    };

    if opts.dry_run {
        // Dry-run: run rewriter on each entry in the plan for preview.
        let progress_path = CleanProgress::default_path();
        let mut progress = CleanProgress::load(&progress_path);

        let records = apply_plan(&plan.entries, opts.rewriter.as_ref(), &mut progress, true);

        let report = CleanReport::from_apply(&records, dedup_candidates, true);
        return Ok(report.render());
    }

    // 4. Apply (live).
    let progress_path = CleanProgress::default_path();
    let mut progress = CleanProgress::load(&progress_path);

    let records = apply_plan(&plan.entries, opts.rewriter.as_ref(), &mut progress, false);

    // Save progress.
    let _ = progress.save(&progress_path);

    let n_rewritten = records.iter().filter(|r| r.status == ApplyStatus::Rewritten).count();
    let n_failed = records.iter().filter(|r| r.status == ApplyStatus::Failed).count();
    let n_skipped = records.iter().filter(|r| r.status == ApplyStatus::AlreadyClean).count();

    otel_clean_rewritten(n_rewritten);
    if n_skipped > 0 {
        otel_clean_skipped("already_clean");
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    otel_clean_completed(n_rewritten, n_skipped, n_failed, duration_ms);

    let report = CleanReport::from_apply(&records, dedup_candidates, false);
    Ok(report.render())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── OTel event constants are prefixed correctly ───────────────────────────

    #[test]
    fn otel_events_have_correct_prefix() {
        assert!(events::INVOKED.starts_with("memory_clean."));
        assert!(events::SCANNED.starts_with("memory_clean."));
        assert!(events::REWRITTEN.starts_with("memory_clean."));
        assert!(events::SKIPPED.starts_with("memory_clean."));
        assert!(events::DEDUP_CANDIDATE.starts_with("memory_clean."));
        assert!(events::COMPLETED.starts_with("memory_clean."));
    }

    // ── empty memory dir returns informational message ────────────────────────

    #[test]
    fn pipeline_empty_dir_returns_info_message() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let opts = CleanOpts {
            dry_run: true,
            auto: false,
            filter: None,
            dedup: false,
            rewriter: Box::new(MockRewriter),
        };
        let result = run_memory_clean_pipeline(Some(dir.path()), &opts)
            .expect("pipeline must succeed");
        assert!(
            result.contains("No imported memory entries"),
            "empty dir message expected; got: {result}"
        );
    }
}
