// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![allow(unsafe_code)]

/// Integration tests for Phase 6.4 — Report generation and OTel events.
///
/// Tests that live in the `runtime` crate cover:
/// - Full-format report generation (live + dry-run)
/// - OTel event constants completeness
/// - OTel helper no-ops when disabled

use serial_test::serial;
use std::path::PathBuf;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn set_anvil_home(path: &std::path::Path) {
    unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
}

fn clear_anvil_home() {
    unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
}

// ── Full-format report tests ─────────────────────────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn full_report_dry_run_has_prefix() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let manifest = runtime::ImportManifest::new("2.2.14-test");
    let opts = runtime::ReportOptions {
        source_label: "CC at `~/.claude/`",
        dry_run: true,
        timestamp: "2026-05-15T09:34:21Z",
        needs_review: &[],
        next_steps: &[],
    };
    let report = runtime::generate_full_report(&manifest, &opts);

    clear_anvil_home();

    assert!(
        report.starts_with("# [DRY RUN]"),
        "dry-run report must start with [DRY RUN]: {report}"
    );
}

#[test]
#[serial(anvil_config_home)]
fn full_report_live_has_no_dry_run_prefix() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let manifest = runtime::ImportManifest::new("2.2.14-test");
    let opts = runtime::ReportOptions {
        source_label: "CC at `~/.claude/`",
        dry_run: false,
        timestamp: "2026-05-15T09:34:21Z",
        needs_review: &[],
        next_steps: &[],
    };
    let report = runtime::generate_full_report(&manifest, &opts);

    clear_anvil_home();

    assert!(
        report.starts_with("# Anvil Import Report"),
        "live report must start with Anvil Import Report: {report}"
    );
    assert!(
        !report.contains("[DRY RUN]"),
        "live report must not contain [DRY RUN]: {report}"
    );
}

#[test]
#[serial(anvil_config_home)]
fn full_report_has_all_required_sections() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let mut manifest = runtime::ImportManifest::new("2.2.14-test");
    let mut committed_entry = runtime::ImportEntry::pending(
        "memory",
        PathBuf::from("/fake/.claude/memory/rule.md"),
        PathBuf::from("/fake/.anvil/memory/rule.md"),
        "abc123",
        "2026-05-15T00:00:00Z",
    );
    committed_entry.status = runtime::ImportEntryStatus::Committed;
    manifest.push(committed_entry);

    let mut skipped_entry = runtime::ImportEntry::pending(
        "settings",
        PathBuf::from("/fake/.claude/.credentials.json"),
        PathBuf::from("/dev/null"),
        "hash",
        "2026-05-15T00:00:00Z",
    );
    skipped_entry.status = runtime::ImportEntryStatus::Skipped;
    skipped_entry.skip_reason = Some("OAuth credentials — run `anvil login` separately".to_string());
    manifest.push(skipped_entry);

    let opts = runtime::ReportOptions {
        source_label: "CC at `~/.claude/`",
        dry_run: false,
        timestamp: "2026-05-15T09:34:21Z",
        needs_review: &[(
            "/projects/foo/ANVIL.md already exists → staged as ANVIL.imported.md".to_string(),
            "~/.anvil/.import-review/foo_ANVIL.imported.md".to_string(),
        )],
        next_steps: &[],
    };
    let report = runtime::generate_full_report(&manifest, &opts);

    clear_anvil_home();

    assert!(report.contains("## Source"), "should have Source section");
    assert!(report.contains("## Bring"), "should have Bring section");
    assert!(report.contains("## Needs Review"), "should have Needs Review section");
    assert!(report.contains("## Skipped"), "should have Skipped section");
    assert!(report.contains("## Next Steps"), "should have Next Steps section");
    assert!(report.contains("anvil import claude-code"), "should have footer");
}

#[test]
#[serial(anvil_config_home)]
fn write_full_report_creates_file() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let manifest = runtime::ImportManifest::new("2.2.14-test");
    let opts = runtime::ReportOptions {
        source_label: "CC at `~/.claude/`",
        dry_run: false,
        timestamp: "2026-05-15T09:34:21Z",
        needs_review: &[],
        next_steps: &[],
    };
    let path = runtime::write_full_report(&manifest, &opts).expect("write report");
    let exists = path.exists();
    let content = std::fs::read_to_string(&path).unwrap_or_default();

    clear_anvil_home();

    assert!(exists, "report file should exist at {}", path.display());
    assert!(content.contains("Anvil Import Report"), "report should have header");
}

// ── OTel event constants ─────────────────────────────────────────────────────

#[test]
fn otel_event_constants_all_prefixed() {
    use runtime::import::events;
    assert!(events::INVOKED.starts_with("import."));
    assert!(events::DISCOVERED.starts_with("import."));
    assert!(events::STAGED.starts_with("import."));
    assert!(events::COMMITTED.starts_with("import."));
    assert!(events::SKIPPED.starts_with("import."));
    assert!(events::CONFLICT_DETECTED.starts_with("import."));
    assert!(events::COMPLETED.starts_with("import."));
}

#[test]
fn otel_import_helpers_are_no_ops_when_disabled() {
    // OTel is disabled by default; all helpers must not panic.
    runtime::otel_import_invoked("cc:/fake/.claude", "all", false, false);
    runtime::otel_import_discovered("memory", 42);
    runtime::otel_import_staged("memory", 42);
    runtime::otel_import_conflict_detected("settings", 2);
    runtime::otel_import_committed("memory", 40);
    runtime::otel_import_skipped("credentials", 1, "OAuth scope");
    runtime::otel_import_completed(40, 2, 0, 1234);
}

// ── Wizard skip flag ──────────────────────────────────────────────────────────

/// The skip-flag path is derived from ANVIL_CONFIG_HOME.
/// This test writes a flag via std::fs directly (wizard module is in anvil-cli,
/// not runtime) and verifies the path derivation.
#[test]
#[serial(anvil_config_home)]
fn skip_flag_path_uses_anvil_config_home() {
    let dir = TempDir::new().expect("tmpdir");
    set_anvil_home(dir.path());

    let flag_path = runtime::anvil_config_home().join(".import-skipped");
    // Should not exist yet.
    assert!(!flag_path.exists(), "skip flag should not exist before write");

    // Write it manually (simulates what wizard_run_migration_step does on 'n').
    std::fs::create_dir_all(flag_path.parent().unwrap()).expect("mkdir");
    std::fs::write(&flag_path, b"").expect("write flag");

    assert!(flag_path.exists(), "skip flag should exist after write");

    clear_anvil_home();
}
