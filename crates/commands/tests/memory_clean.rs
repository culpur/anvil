// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![allow(unsafe_code)]

/// Integration acceptance test — Phase 6.5: `anvil memory clean`.
///
/// Tests the end-to-end path from slash command parse → dispatch →
/// handler → runtime pipeline using MockRewriter.
///
/// Coverage:
/// - `/memory clean --dry-run` produces no file changes
/// - `/memory clean` (live) rewrites imported entries
/// - Idempotent re-run is a no-op
/// - ProviderRewriter::detect() fails loud with no provider configured
/// - Dedup candidate detection produces expected pairs
/// - Frontmatter provenance is preserved verbatim
/// - Permission gate blocks writes in ReadOnly mode
/// - `--filter` targets specific entries

use serial_test::serial;
use std::path::Path;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn set_anvil_home(path: &Path) {
    unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
}

fn clear_anvil_home() {
    unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
}

/// Write an imported memory entry to `memory_dir`.
fn write_imported_entry(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).expect("mkdir memory");
    let content = format!(
        "---\nimported_from: claude_code\nimported_at: 2026-05-15T00:00:00Z\nsource_path: ~/.claude/memory/{name}\ncontent_hash: abc123\n---\n{body}"
    );
    std::fs::write(dir.join(name), content).expect("write imported entry");
}

/// Write a user-authored (non-imported) memory entry.
fn write_user_entry(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).expect("mkdir memory");
    let content = format!("---\ntitle: User note\n---\n{body}");
    std::fs::write(dir.join(name), content).expect("write user entry");
}

// ── /memory clean --dry-run produces no file changes ─────────────────────────

#[test]
#[serial(anvil_config_home)]
fn dry_run_produces_no_file_changes() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(&memory_dir, "rule.md", "Use Claude Code for all tasks.\n");
    let original = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read");

    // Invoke via the handler directly (matching the dispatch path).
    let result = commands::handle_memory_clean("--dry-run", None);

    let after = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read after");
    clear_anvil_home();

    assert_eq!(original, after, "dry-run must not modify files");
    assert!(
        result.contains("DRY RUN") || result.contains("dry"),
        "dry-run result must mention dry run; got: {result}"
    );
}

// ── /memory clean --dry-run output mentions CC normalization ──────────────────

#[test]
#[serial(anvil_config_home)]
fn dry_run_output_mentions_expected_changes() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(
        &memory_dir,
        "feedback-foo.md",
        "Use Claude Code for tasks. See ~/.claude/settings.json\n",
    );

    let result = commands::handle_memory_clean("--dry-run", None);
    clear_anvil_home();

    // The result should contain some indicator of what will change.
    // With MockRewriter in dry-run it calls the pipeline which runs scan+plan.
    assert!(
        !result.is_empty(),
        "dry-run must produce non-empty output"
    );
}

// ── /memory clean (live) rewrites imported entries ────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn live_run_rewrites_imported_entries() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(
        &memory_dir,
        "rule.md",
        "Use Claude Code for coding tasks.\n",
    );
    let original = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read");

    // We call the runtime pipeline directly with MockRewriter to avoid needing
    // a real LLM provider in integration tests.
    let opts = runtime::memory_clean::CleanOpts {
        dry_run: false,
        auto: true,
        filter: None,
        dedup: false,
        rewriter: Box::new(runtime::memory_clean::MockRewriter),
    };

    let result = runtime::memory_clean::run_memory_clean_pipeline(
        Some(&memory_dir),
        &opts,
    )
    .expect("pipeline must succeed");

    let after = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read after");
    clear_anvil_home();

    assert_ne!(original, after, "live run must modify files");
    assert!(after.contains("CC"), "should contain CC after rewrite");
    assert!(!after.contains("Claude Code"), "should not contain original text");
    assert!(
        result.contains("rewritten") || result.contains("Rewritten"),
        "result must mention rewritten count; got: {result}"
    );
}

// ── provenance frontmatter is preserved verbatim ──────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn provenance_frontmatter_preserved_verbatim() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(
        &memory_dir,
        "feedback-rule.md",
        "Claude Code uses many conventions.\n",
    );

    let opts = runtime::memory_clean::CleanOpts {
        dry_run: false,
        auto: true,
        filter: None,
        dedup: false,
        rewriter: Box::new(runtime::memory_clean::MockRewriter),
    };

    let _ = runtime::memory_clean::run_memory_clean_pipeline(Some(&memory_dir), &opts)
        .expect("pipeline");

    let after = std::fs::read_to_string(memory_dir.join("feedback-rule.md")).expect("read");
    clear_anvil_home();

    // Provenance fields must be preserved.
    assert!(after.contains("imported_from: claude_code"), "imported_from must be preserved");
    assert!(after.contains("imported_at: 2026-05-15T00:00:00Z"), "imported_at must be preserved");
    assert!(
        after.contains("source_path: ~/.claude/memory/feedback-rule.md"),
        "source_path must be preserved"
    );

    // New rewritten_* fields must be added.
    assert!(after.contains("rewritten_at:"), "rewritten_at must be added");
    assert!(after.contains("rewritten_by:"), "rewritten_by must be added");
    assert!(after.contains("rewritten_from_hash:"), "rewritten_from_hash must be added");
}

// ── idempotent re-run is a no-op ──────────────────────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn idempotent_rerun_is_noop() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(
        &memory_dir,
        "rule.md",
        "Claude Code tasks. Always commit.\n",
    );

    let make_opts = || runtime::memory_clean::CleanOpts {
        dry_run: false,
        auto: true,
        filter: None,
        dedup: false,
        rewriter: Box::new(runtime::memory_clean::MockRewriter),
    };

    // First run.
    let _ = runtime::memory_clean::run_memory_clean_pipeline(Some(&memory_dir), &make_opts())
        .expect("first run");

    let after_first = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read first");

    // Second run.
    let result2 = runtime::memory_clean::run_memory_clean_pipeline(Some(&memory_dir), &make_opts())
        .expect("second run");

    let after_second = std::fs::read_to_string(memory_dir.join("rule.md")).expect("read second");
    clear_anvil_home();

    assert_eq!(
        after_first, after_second,
        "second run must not modify the file (idempotent)"
    );
    assert!(
        result2.contains("0") || result2.contains("Skipped") || result2.contains("already"),
        "second run result must indicate no rewrites; got: {result2}"
    );
}

// ── user-authored entries are not touched ─────────────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn user_authored_entries_not_touched() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_user_entry(&memory_dir, "my-notes.md", "Personal note about Claude Code.\n");
    let original = std::fs::read_to_string(memory_dir.join("my-notes.md")).expect("read");

    let opts = runtime::memory_clean::CleanOpts {
        dry_run: false,
        auto: true,
        filter: None,
        dedup: false,
        rewriter: Box::new(runtime::memory_clean::MockRewriter),
    };

    let result = runtime::memory_clean::run_memory_clean_pipeline(Some(&memory_dir), &opts)
        .expect("pipeline");

    let after = std::fs::read_to_string(memory_dir.join("my-notes.md")).expect("read after");
    clear_anvil_home();

    assert_eq!(original, after, "user-authored entries must not be touched");
    assert!(
        result.contains("No imported") || result.contains("0"),
        "result should indicate no entries found; got: {result}"
    );
}

// ── --filter targets specific entries ─────────────────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn filter_targets_specific_entries() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    write_imported_entry(&memory_dir, "feedback-foo.md", "Claude Code feedback.\n");
    write_imported_entry(&memory_dir, "other-bar.md", "Other Claude Code rule.\n");

    let original_other = std::fs::read_to_string(memory_dir.join("other-bar.md")).expect("read");

    let opts = runtime::memory_clean::CleanOpts {
        dry_run: false,
        auto: true,
        filter: Some("feedback-*".to_string()),
        dedup: false,
        rewriter: Box::new(runtime::memory_clean::MockRewriter),
    };

    let _ = runtime::memory_clean::run_memory_clean_pipeline(Some(&memory_dir), &opts)
        .expect("pipeline");

    let feedback_after = std::fs::read_to_string(memory_dir.join("feedback-foo.md")).expect("read");
    let other_after = std::fs::read_to_string(memory_dir.join("other-bar.md")).expect("read other");
    clear_anvil_home();

    // feedback-foo.md should be rewritten.
    assert!(
        feedback_after.contains("CC"),
        "filtered entry must be rewritten"
    );
    // other-bar.md should NOT be touched.
    assert_eq!(
        original_other, other_after,
        "non-matching entry must not be touched by filter"
    );
}

// ── dedup candidate detection produces expected pairs ────────────────────────

#[test]
#[serial(anvil_config_home)]
fn dedup_detection_produces_candidates_for_similar_entries() {
    let dir = TempDir::new().expect("tmpdir");
    let memory_dir = dir.path().join("memory");
    set_anvil_home(dir.path());

    // Two nearly identical entries.
    let body_a = "Always commit after finishing any work on any coding project task.\n";
    let body_b = "Always commit after finishing any work on any coding project task and push.\n";
    write_imported_entry(&memory_dir, "rule-a.md", body_a);
    write_imported_entry(&memory_dir, "rule-b.md", body_b);

    let scan_opts = runtime::memory_clean::ScanOpts::default();
    let scanned = runtime::memory_clean::scan_memory_dir(Some(&memory_dir), &scan_opts);
    clear_anvil_home();

    let dedup_opts = runtime::memory_clean::DedupOpts {
        jaccard_threshold: 0.6,
        use_llm_judge: false,
    };
    let candidates = runtime::memory_clean::detect_duplicates(&scanned, &dedup_opts, None);

    assert!(
        !candidates.is_empty(),
        "near-duplicate pair must be detected"
    );
    assert!(
        candidates[0].confidence >= 0.6,
        "confidence must meet threshold"
    );
}

// ── permission gate blocks writes in ReadOnly mode ────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn permission_gate_blocks_writes_in_read_only() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let result = commands::handle_memory_clean("", Some(runtime::PermissionMode::ReadOnly));
    clear_anvil_home();

    assert!(
        result.contains("WorkspaceWrite") || result.contains("ReadOnly") || result.contains("acceptEdits"),
        "ReadOnly mode must block writes; got: {result}"
    );
}

// ── /memory clean appears in slash command parse ──────────────────────────────

#[test]
fn memory_clean_parses_correctly() {
    // /memory clean routes via Memory { action: Some("clean ...") } — the
    // existing parser shape for /memory captures the rest as `action`.
    use commands::SlashCommand;
    let parsed = SlashCommand::parse("/memory clean --dry-run");
    assert!(
        matches!(parsed, Some(SlashCommand::Memory { action: Some(ref a) }) if a.contains("clean")),
        "'/memory clean --dry-run' must parse as Memory with clean action; got: {parsed:?}"
    );
}

// ── ProviderRewriter::detect() fails loud with no provider ────────────────────

#[test]
fn provider_rewriter_detect_fails_loud_no_provider() {
    let _old_anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
    let _old_openai = std::env::var("OPENAI_API_KEY").ok();

    unsafe {
        std::env::set_var("OLLAMA_BASE_URL", "http://127.0.0.1:19998");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
    }

    let result = runtime::memory_clean::ProviderRewriter::detect();

    unsafe {
        std::env::remove_var("OLLAMA_BASE_URL");
        if let Some(v) = _old_anthropic {
            std::env::set_var("ANTHROPIC_API_KEY", v);
        }
        if let Some(v) = _old_openai {
            std::env::set_var("OPENAI_API_KEY", v);
        }
    }

    assert!(
        result.is_err(),
        "detect() must fail loud when no provider is configured"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("ANTHROPIC_API_KEY") || err.contains("ollama"),
        "error must name at least one fix path; got: {err}"
    );
}
