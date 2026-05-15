// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![allow(unsafe_code)]

/// Integration tests — Phase 6.3: sessions import pipeline.
///
/// Tests exercise `run_sessions_import` (via `run_import_pipeline_headless`)
/// against fixture CC directories, verifying:
///
/// 1. Idempotency: re-run produces no new artifacts when source is unchanged.
/// 2. Oversized-skip: sessions > 50 MB are counted but not summarized.
/// 3. Chunked summarization: large sessions produce a valid SummarizedSession.
/// 4. Resumable cancellation: pre-seeded progress.json skips already-processed.
/// 5. Lesson nominations land in staging with correct import stamps.
/// 6. --include-sessions=false leaves sessions untouched.
///
/// All tests use `MockSummarizer` (no network calls).

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

/// Write a minimal CC fixture with `n_sessions` session JSONL files.
///
/// Each session file sits at `cc_dir/projects/proj-NNNN/sess-NNNN.jsonl`.
fn build_sessions_fixture(cc_dir: &Path, n_sessions: usize) {
    for i in 0..n_sessions {
        let proj_dir = cc_dir
            .join("projects")
            .join(format!("proj-{i:04}"));
        std::fs::create_dir_all(&proj_dir).expect("mkdir project dir");

        let session_id = format!("sess-{i:04}");
        let session_path = proj_dir.join(format!("{session_id}.jsonl"));

        let ts_base = 1_700_000_000u64 + (i as u64 * 3600);
        let mut lines = Vec::new();
        for turn in 0..3 {
            let ts = ts_base + (turn as u64 * 60);
            lines.push(format!(
                r#"{{"type":"human","sessionId":"{session_id}","timestamp":{ts},"message":{{"role":"user","content":"Fix the thing #{turn}"}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"assistant","sessionId":"{session_id}","timestamp":{ts},"model":"claude-haiku-4-5","message":{{"role":"assistant","content":"Done — fixed #{turn}."}}}}"#
            ));
        }
        std::fs::write(&session_path, lines.join("\n")).expect("write session jsonl");
    }
}

/// Build a CC fixture with one oversized session (> 50 MB) plus `n_normal`
/// normal sessions.
fn build_oversized_fixture(cc_dir: &Path, n_normal: usize) {
    build_sessions_fixture(cc_dir, n_normal);

    // Oversized session.
    let proj_dir = cc_dir.join("projects").join("proj-oversized");
    std::fs::create_dir_all(&proj_dir).expect("mkdir oversized proj");
    let big: Vec<u8> = vec![b'x'; (50 * 1024 * 1024) + 1];
    std::fs::write(proj_dir.join("big-session.jsonl"), &big).expect("write big file");
}

// ── Test: --include-sessions=false leaves sessions untouched ─────────────────

#[test]
#[serial(anvil_config_home)]
fn no_include_sessions_leaves_sessions_untouched() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc");
    set_anvil_home(&anvil_home);

    build_sessions_fixture(&cc_dir, 3);

    let source = runtime::ImportSource::ClaudeCode {
        profile_dir: cc_dir.clone(),
    };
    let result = commands::handlers::run_import_pipeline_headless(&source, false, false);
    clear_anvil_home();

    let summary = result.expect("pipeline must succeed");

    // Sessions line should show SKIPPED message.
    assert!(
        summary.contains("SKIPPED"),
        "summary must say sessions SKIPPED when --include-sessions not set;\ngot: {summary}"
    );

    // No daily records should exist.
    let daily_dir = anvil_home.join("daily");
    let daily_files = if daily_dir.exists() {
        std::fs::read_dir(&daily_dir)
            .map(|d| d.count())
            .unwrap_or(0)
    } else {
        0
    };
    assert_eq!(
        daily_files, 0,
        "no daily files should exist when --include-sessions not set"
    );
}

// ── Test: oversized sessions are reported but not summarized ──────────────────

#[test]
#[serial(anvil_config_home)]
fn oversized_sessions_reported_in_summary() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc");
    set_anvil_home(&anvil_home);

    // 2 normal + 1 oversized.
    build_oversized_fixture(&cc_dir, 2);

    // Use the sessions module directly with MockSummarizer to avoid provider calls.
    let _staging_dir_path = anvil_home.join(".import-staging");
    let staging = runtime::StagingDir::create_clean().expect("staging");

    let mut opts = runtime::import::sessions::SessionImportOpts {
        include_sessions: true,
        summarizer: Box::new(runtime::import::sessions::MockSummarizer),
    };

    let records = runtime::import::sessions::run_sessions_import(&cc_dir, &staging, &mut opts);
    clear_anvil_home();

    // Should find 3 sessions (2 normal + 1 oversized).
    assert_eq!(records.len(), 3, "should find 3 sessions total");

    let oversized: Vec<_> = records
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::Skipped
            && r.reason.as_deref() == Some("oversized"))
        .collect();
    assert_eq!(oversized.len(), 1, "exactly 1 session must be oversized-skipped");

    let summarized: Vec<_> = records
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::Summarized)
        .collect();
    assert_eq!(summarized.len(), 2, "exactly 2 normal sessions must be summarized");
}

// ── Test: idempotency — re-run produces no new records ────────────────────────

#[test]
#[serial(anvil_config_home)]
fn idempotent_rerun_produces_no_new_artifacts() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc");
    set_anvil_home(&anvil_home);

    build_sessions_fixture(&cc_dir, 2);

    let staging = runtime::StagingDir::create_clean().expect("staging");

    let mut opts1 = runtime::import::sessions::SessionImportOpts {
        include_sessions: true,
        summarizer: Box::new(runtime::import::sessions::MockSummarizer),
    };
    let first = runtime::import::sessions::run_sessions_import(&cc_dir, &staging, &mut opts1);
    let first_summarized = first
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::Summarized)
        .count();
    assert_eq!(first_summarized, 2, "first run must summarize 2 sessions");

    // Second run with the same staging directory — progress.json is preserved.
    let mut opts2 = runtime::import::sessions::SessionImportOpts {
        include_sessions: true,
        summarizer: Box::new(runtime::import::sessions::MockSummarizer),
    };
    let second = runtime::import::sessions::run_sessions_import(&cc_dir, &staging, &mut opts2);
    clear_anvil_home();

    let already: Vec<_> = second
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::AlreadyProcessed)
        .collect();
    assert_eq!(
        already.len(),
        2,
        "re-run must report both sessions as AlreadyProcessed; got: {:?}",
        second.iter().map(|r| &r.status).collect::<Vec<_>>()
    );
}

// ── Test: resumable cancellation + restart ────────────────────────────────────

#[test]
#[serial(anvil_config_home)]
fn resumable_after_simulated_cancellation() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc");
    set_anvil_home(&anvil_home);

    // 4 sessions — simulate cancellation after processing 2.
    build_sessions_fixture(&cc_dir, 4);

    let staging = runtime::StagingDir::create_clean().expect("staging");

    // Pre-seed progress.json with the hashes of the first 2 sessions.
    let projects_dir = cc_dir.join("projects");
    let mut processed_hashes: Vec<String> = Vec::new();
    for i in 0..2 {
        let proj_dir = projects_dir.join(format!("proj-{i:04}"));
        let session_path = proj_dir.join(format!("sess-{i:04}.jsonl"));
        if let Some(hash) = runtime::sha256_file(&session_path) {
            processed_hashes.push(hash);
        }
    }

    let progress = serde_json::json!({
        "processed": processed_hashes,
        "skipped_oversized": [],
        "started_at": "2026-05-15T00:00:00Z",
        "last_session_path": null
    });
    let progress_path = staging.path("sessions/progress.json");
    if let Some(parent) = progress_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir sessions");
    }
    std::fs::write(&progress_path, serde_json::to_string_pretty(&progress).expect("serialize"))
        .expect("write progress");

    let mut opts = runtime::import::sessions::SessionImportOpts {
        include_sessions: true,
        summarizer: Box::new(runtime::import::sessions::MockSummarizer),
    };
    let records = runtime::import::sessions::run_sessions_import(&cc_dir, &staging, &mut opts);
    clear_anvil_home();

    let already = records
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::AlreadyProcessed)
        .count();
    let new_summarized = records
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::Summarized)
        .count();

    assert_eq!(
        already, 2,
        "2 sessions from pre-seeded progress must be AlreadyProcessed"
    );
    assert_eq!(
        new_summarized, 2,
        "2 remaining sessions must be freshly summarized on resume"
    );
}

// ── Test: lesson nominations have correct import stamps ───────────────────────

#[test]
#[serial(anvil_config_home)]
fn lesson_nominations_have_import_stamps() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    let cc_dir = dir.path().join("cc");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    std::fs::create_dir_all(&cc_dir).expect("mkdir cc");
    set_anvil_home(&anvil_home);

    build_sessions_fixture(&cc_dir, 1);

    let staging = runtime::StagingDir::create_clean().expect("staging");
    let mut opts = runtime::import::sessions::SessionImportOpts {
        include_sessions: true,
        summarizer: Box::new(runtime::import::sessions::MockSummarizer),
    };
    let records = runtime::import::sessions::run_sessions_import(&cc_dir, &staging, &mut opts);
    clear_anvil_home();

    let summarized: Vec<_> = records
        .iter()
        .filter(|r| r.status == runtime::import::sessions::SessionImportStatus::Summarized)
        .collect();
    assert_eq!(summarized.len(), 1, "one session should be summarized");

    let sess_id = &summarized[0].session_id;
    let noms_written = summarized[0].nominations_written;
    assert!(noms_written > 0, "at least one nomination must be written");

    // Read the first nomination file.
    let nom_path = staging.path(&format!("nominations/sess-{sess_id}-0000.json"));
    assert!(nom_path.exists(), "nomination file must exist at {nom_path:?}");

    let content = std::fs::read_to_string(&nom_path).expect("read nomination");
    let v: serde_json::Value = serde_json::from_str(&content).expect("parse nomination json");

    assert_eq!(
        v.get("imported_from").and_then(|v| v.as_str()),
        Some("claude_code"),
        "imported_from must be claude_code"
    );
    assert!(
        v.get("imported_at").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        "imported_at must be set"
    );
    assert!(
        v.get("source_path").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        "source_path must be set"
    );
    assert!(
        v.get("content").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        "content must not be empty"
    );
    assert_eq!(
        v.get("status").and_then(|v| v.as_str()),
        Some("pending"),
        "nomination status must be pending"
    );
}

// ── Test: chunked summarization produces valid output ────────────────────────

#[test]
fn chunked_summarization_produces_valid_summary() {
    let s = runtime::import::sessions::MockSummarizer;
    // Build a large transcript that should trigger chunking (> 20_000 chars).
    let turn = "User: Fix something\nAssistant: Done — fixed.\n";
    let big = turn.repeat(600); // ~42_000 chars

    let result = runtime::import::sessions::summarize_chunked_pub(&big, &s);
    assert!(result.is_ok(), "chunked summarization must succeed: {:?}", result.err());
    let summary = result.unwrap();
    assert!(
        !summary.summary.is_empty(),
        "summary text must not be empty"
    );
}

// ── Test: commit_sessions_from_staging merges correctly ──────────────────────

#[test]
#[serial(anvil_config_home)]
fn commit_sessions_merges_daily_records() {
    let dir = TempDir::new().expect("tmpdir");
    let anvil_home = dir.path().join("anvil");
    std::fs::create_dir_all(&anvil_home).expect("mkdir anvil");
    set_anvil_home(&anvil_home);

    // Pre-populate live daily with one session.
    let live_daily = anvil_home.join("daily");
    std::fs::create_dir_all(&live_daily).expect("mkdir daily");
    let existing = serde_json::json!({
        "date": "2024-01-15",
        "sessions": [{
            "session_id": "existing-sess",
            "model": "claude-sonnet-4-6",
            "duration_secs": 100,
            "tokens_used": 500,
            "messages_count": 2,
            "tasks_completed": [],
            "tasks_open": [],
            "files_modified": [],
            "nominations_generated": 0,
            "credentials_auto_vaulted": 0
        }],
        "open_items": [],
        "total_tokens": 500,
        "total_cost_usd": 0.0
    });
    std::fs::write(
        live_daily.join("2024-01-15.json"),
        serde_json::to_string_pretty(&existing).expect("serialize existing"),
    )
    .expect("write existing daily");

    // Create staging with an additional session.
    let staging = runtime::StagingDir::create_clean().expect("staging");
    let staged_daily = staging.path("sessions/daily");
    std::fs::create_dir_all(&staged_daily).expect("mkdir staged daily");

    let new_sess = serde_json::json!({
        "date": "2024-01-15",
        "sessions": [{
            "session_id": "imported-sess",
            "model": "claude-haiku-4-5",
            "duration_secs": 300,
            "tokens_used": 1000,
            "messages_count": 4,
            "tasks_completed": ["imported task"],
            "tasks_open": [],
            "files_modified": ["main.rs"],
            "nominations_generated": 1,
            "credentials_auto_vaulted": 0
        }],
        "open_items": [],
        "total_tokens": 1000,
        "total_cost_usd": 0.0
    });
    std::fs::write(
        staged_daily.join("2024-01-15.json"),
        serde_json::to_string_pretty(&new_sess).expect("serialize new"),
    )
    .expect("write staged daily");

    let committed = runtime::import::sessions::commit_sessions_from_staging(&staging)
        .expect("commit must succeed");
    clear_anvil_home();

    assert_eq!(committed, 1, "one daily record should be committed");

    let merged = std::fs::read_to_string(live_daily.join("2024-01-15.json"))
        .expect("read merged daily");
    let merged_v: serde_json::Value = serde_json::from_str(&merged).expect("parse merged");
    let sessions = merged_v["sessions"].as_array().expect("sessions array");
    assert_eq!(
        sessions.len(),
        2,
        "merged record must have 2 sessions (1 existing + 1 imported)"
    );
}
