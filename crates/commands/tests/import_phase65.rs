// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![allow(unsafe_code)]

/// Integration acceptance test — Phase 6 composer: all 5 pipelines.
///
/// Fixture: temp directory with one populated sub-dir for each pipeline:
///   - 2 memory markdown entries
///   - 1 global CLAUDE.md
///   - 1 settings.json (no existing Anvil settings → no conflict)
///   - 1 skill directory with SKILL.md
///   - 1 plugin (installed_plugins.json + plugin.json)
///
/// The test exercises `run_import_pipeline_headless` in both dry-run and
/// live-commit modes and asserts the summary string covers all 5 categories.

use serial_test::serial;
use std::path::Path;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn set_anvil_home(path: &Path) {
    unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
}

fn clear_anvil_home() {
    unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
}

/// Populate `cc_dir` with one artifact from each of the 5 pipeline kinds.
fn build_cc_fixture(cc_dir: &Path) {
    // 1. Memory entries (2 files under projects/<id>/memory/)
    let memory_dir = cc_dir
        .join("projects")
        .join("proj_test123")
        .join("memory");
    std::fs::create_dir_all(&memory_dir).expect("mkdir memory");
    std::fs::write(
        memory_dir.join("rule_commit.md"),
        b"---\ntitle: Commit Rule\n---\n\nAlways commit after finishing work.\n",
    )
    .expect("write rule_commit.md");
    std::fs::write(
        memory_dir.join("rule_deploy.md"),
        b"---\ntitle: Deploy Rule\n---\n\nNever use rsync to deploy.\n",
    )
    .expect("write rule_deploy.md");

    // 2. Global CLAUDE.md → ANVIL.md
    std::fs::write(
        cc_dir.join("CLAUDE.md"),
        b"# Global CC Instructions\n\nLead the team, don't stop for approval.\n",
    )
    .expect("write CLAUDE.md");

    // 3. settings.json
    let settings = serde_json::json!({
        "theme": "dark",
        "outputStyle": "concise"
    });
    std::fs::write(
        cc_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).expect("serialize settings"),
    )
    .expect("write settings.json");

    // 4. Skill with valid SKILL.md
    let skill_dir = cc_dir.join("skills").join("audit-helper");
    std::fs::create_dir_all(&skill_dir).expect("mkdir skill");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        b"---\nname: audit-helper\ndescription: Helps with security audits\n---\n\nRun audit checks.\n",
    )
    .expect("write SKILL.md");

    // 5. Plugin: installed_plugins.json + marketplace plugin.json
    let plugins_dir = cc_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).expect("mkdir plugins");
    let installed = serde_json::json!([
        { "id": "security-scan", "marketplace": "default" }
    ]);
    std::fs::write(
        plugins_dir.join("installed_plugins.json"),
        serde_json::to_vec_pretty(&installed).expect("serialize installed_plugins"),
    )
    .expect("write installed_plugins.json");

    let plugin_manifest = serde_json::json!({
        "name": "security-scan",
        "version": "2.0.0",
        "description": "Automated security scanning",
        "permissions": [],
        "defaultEnabled": false,
        "hooks": {},
        "tools": [],
        "commands": []
    });
    let plugin_dir = plugins_dir
        .join("marketplaces")
        .join("default")
        .join("security-scan");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::to_vec_pretty(&plugin_manifest).expect("serialize plugin manifest"),
    )
    .expect("write plugin.json");
}

// ── Dry-run: all 5 categories appear, DRY RUN label present ──────────────────

#[test]
#[serial(anvil_config_home)]
fn dry_run_summary_covers_all_5_pipelines() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil home");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc dir");
    set_anvil_home(&anvil_home);

    build_cc_fixture(&cc_dir);

    let source = runtime::ImportSource::ClaudeCode {
        profile_dir: cc_dir.clone(),
    };

    let result =
        commands::handlers::run_import_pipeline_headless(&source, true, false);

    clear_anvil_home();

    let summary = result.expect("dry-run must succeed");

    assert!(
        summary.contains("DRY RUN"),
        "dry-run summary must contain DRY RUN label;\ngot: {summary}"
    );
    assert!(
        summary.contains("Memory entries:"),
        "must have Memory entries line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Instructions:"),
        "must have Instructions line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Settings:"),
        "must have Settings line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Skills:"),
        "must have Skills line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Plugins:"),
        "must have Plugins line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Report:"),
        "must mention report path;\ngot: {summary}"
    );

    // Memory: 2 entries in the fixture
    assert!(
        summary.contains("2 found"),
        "memory should show 2 found;\ngot: {summary}"
    );
}

// ── Live commit: all 5 categories appear, no DRY RUN label ───────────────────

#[test]
#[serial(anvil_config_home)]
fn live_commit_covers_all_5_pipelines() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil home");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc dir");
    set_anvil_home(&anvil_home);

    build_cc_fixture(&cc_dir);

    let source = runtime::ImportSource::ClaudeCode {
        profile_dir: cc_dir.clone(),
    };

    let result =
        commands::handlers::run_import_pipeline_headless(&source, false, false);

    clear_anvil_home();

    let summary = result.expect("live commit must succeed");

    assert!(
        !summary.contains("DRY RUN"),
        "live summary must NOT contain DRY RUN;\ngot: {summary}"
    );
    assert!(
        summary.contains("Memory entries:"),
        "must have Memory entries line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Instructions:"),
        "must have Instructions line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Settings:"),
        "must have Settings line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Skills:"),
        "must have Skills line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Plugins:"),
        "must have Plugins line;\ngot: {summary}"
    );
    assert!(
        summary.contains("Report:"),
        "must mention report path;\ngot: {summary}"
    );
}

// ── Empty CC dir: succeeds, all category lines present, counts are 0 ──────────

#[test]
#[serial(anvil_config_home)]
fn empty_cc_dir_succeeds_with_zero_found() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc_empty");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil home");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc empty dir");
    set_anvil_home(&anvil_home);

    let source = runtime::ImportSource::ClaudeCode {
        profile_dir: cc_dir.clone(),
    };

    let result =
        commands::handlers::run_import_pipeline_headless(&source, true, false);

    clear_anvil_home();

    let summary = result.expect("empty cc dir pipeline must succeed");

    // All 5 category lines must appear even when counts are 0.
    assert!(summary.contains("Memory entries:"), "memory line must appear");
    assert!(summary.contains("Instructions:"), "instructions line must appear");
    assert!(summary.contains("Settings:"), "settings line must appear");
    assert!(summary.contains("Skills:"), "skills line must appear");
    assert!(summary.contains("Plugins:"), "plugins line must appear");
}
