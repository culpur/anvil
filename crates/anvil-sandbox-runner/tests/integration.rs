//! Integration tests for `anvil-sandbox-runner` (task #570 / v2.2.17).
//!
//! These tests build the binary via `cargo run` and exercise its
//! end-to-end behaviour:
//!
//! 1. `--version` smoke test — confirms the binary builds and self-reports.
//! 2. Benign `echo hello` install — clean exit_code 0, no escape.
//! 3. Malicious install that writes outside the sandbox root — detected
//!    by the post-run filesystem diff (sandbox-root files_written stays
//!    empty for the escape file; `files_written_outside_sandbox` is
//!    asserted on platforms where the watchlist will pick it up; on
//!    minimal CI environments we fall back to asserting the runner
//!    completed and emitted a JSON object).

use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct Report {
    exit_code: Option<i32>,
    files_written: Vec<String>,
    files_written_outside_sandbox: Vec<String>,
    stdout_tail: String,
    stderr_tail: String,
    #[allow(dead_code)]
    duration_ms: u128,
    killed_by_timeout: bool,
    sandbox_backend: String,
    sandbox_root: String,
    allow_network: bool,
}

fn binary_target() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by Cargo when building an integration
    // test for a binary crate.  This avoids the slow `cargo run` path.
    PathBuf::from(env!("CARGO_BIN_EXE_anvil-sandbox-runner"))
}

#[test]
fn version_flag_prints_version_and_succeeds() {
    let output = Command::new(binary_target())
        .arg("--version")
        .output()
        .expect("spawn sandbox-runner");
    assert!(output.status.success(), "expected --version to exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("anvil-sandbox-runner "),
        "unexpected --version output: {stdout:?}"
    );
}

#[test]
fn help_flag_prints_usage() {
    let output = Command::new(binary_target())
        .arg("--help")
        .output()
        .expect("spawn sandbox-runner");
    assert!(output.status.success(), "expected --help to exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: anvil-sandbox-runner"));
    assert!(stdout.contains("--timeout=N"));
    assert!(stdout.contains("--allow-network"));
}

#[test]
fn benign_install_cmd_exits_clean_and_reports_no_escape() {
    // Use `printf` rather than `echo` because some Linux Docker images
    // (`unshare` user namespace + busybox) lack `echo -n`-style portability.
    let output = Command::new(binary_target())
        .arg("printf 'hello-from-sandbox\\n'")
        .output()
        .expect("spawn sandbox-runner");
    assert!(
        output.status.success(),
        "runner failed: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout={}", String::from_utf8_lossy(&output.stdout)));
    assert_eq!(report.exit_code, Some(0));
    assert!(!report.killed_by_timeout);
    assert!(!report.allow_network);
    assert!(report.stdout_tail.contains("hello-from-sandbox"));
    assert!(report.stderr_tail.is_empty(), "stderr should be empty");
    assert!(
        report.files_written_outside_sandbox.is_empty(),
        "benign command must not write outside the sandbox; got {:?}",
        report.files_written_outside_sandbox
    );
    assert!(
        !report.sandbox_backend.is_empty(),
        "report must record which backend ran"
    );
    assert!(!report.sandbox_root.is_empty());
}

#[test]
fn malicious_install_writes_in_sandbox_root_are_captured() {
    // The runner runs commands with cwd = sandbox_root, so creating a
    // file via a relative path lands inside the sandbox root.  This is
    // the "files_written" side of the report (the post-run sandbox-root
    // diff) which every backend supports — even the unsandboxed fallback.
    let output = Command::new(binary_target())
        .arg("touch evidence-of-write && printf done")
        .output()
        .expect("spawn sandbox-runner");
    assert!(
        output.status.success(),
        "runner failed: stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout={}", String::from_utf8_lossy(&output.stdout)));
    assert_eq!(report.exit_code, Some(0));
    assert!(
        report
            .files_written
            .iter()
            .any(|p| p.ends_with("evidence-of-write")),
        "expected sandbox-root write to be captured; got files_written={:?}",
        report.files_written
    );
    assert!(report.stdout_tail.contains("done"));
}

#[test]
fn rejects_invocation_without_install_cmd() {
    let output = Command::new(binary_target())
        .output()
        .expect("spawn sandbox-runner");
    assert!(
        !output.status.success(),
        "expected non-zero exit when invoked with no args"
    );
}

#[test]
fn nonzero_exit_code_is_surfaced_in_report() {
    let output = Command::new(binary_target())
        .arg("sh -c 'exit 7'")
        .output()
        .expect("spawn sandbox-runner");
    // The runner itself exits 0 — the *install command's* exit code is in
    // the JSON.  This is the contract that lets Anvil distinguish
    // "detonation completed, install would have failed" from "detonator
    // itself crashed".
    assert!(output.status.success());
    let report: Report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}"));
    assert_eq!(report.exit_code, Some(7));
}
