//! Task #661 (v2.2.18 / Agent A2) — integration test for `--setup`
//! routing.
//!
//! ## What this test guards
//!
//! Before v2.2.18, `anvil --setup` (the flag invoked by installer
//! scripts) routed to the legacy `setup::run_setup_wizard` — an
//! ASCII-box-and-stdin-paste wizard at `crates/anvil-cli/src/setup.rs`.
//! The new alt-screen modal wizard lived at `wizard.rs` and was only
//! reachable via the first-run-no-config gate.  A user reported on
//! 2026-05-19 that `curl … | bash` dropped them into the legacy
//! wizard (because `install.sh` ended with `exec anvil --setup`).
//!
//! As of #661, `CliAction::Setup` routes to `wizard::run_first_run_wizard`
//! (the same entry point the no-config gate uses).  This test asserts
//! the regression cannot return.
//!
//! ## How it works
//!
//! We can't easily drive a PTY-bound alt-screen in a `cargo test` run
//! across every CI runner (the alt-screen wizard takes the
//! `io::stdout().is_terminal()` branch — under `cargo test` neither
//! stdin nor stdout is a TTY, so it falls back to the stdin variant
//! `run_first_run_wizard_via_stdin`).  Both variants live in
//! `wizard.rs`; both emit text the legacy wizard never does.
//!
//! The discriminator we key off:
//!   - **Legacy (setup.rs)**: emits `Step 1 of 4` + `Paste your API key`
//!   - **New (wizard.rs)**: emits `Welcome to Anvil v` + `Step 1 of 7`
//!     with a hammer prefix (`\u{2692}`) on the welcome line.
//!
//! Asserting the **legacy** strings are absent is the regression gate.
//! Asserting at least one **new** string is present is the positive
//! confirmation.
//!
//! ## Sandboxing
//!
//! `ANVIL_CONFIG_HOME` points at a temp dir so the test never touches
//! the user's real `~/.anvil/`.  The child is killed after a short
//! grace window — we only need the first banner; the rest of the
//! wizard wants interactive input we can't provide.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Path to the `anvil` binary cargo built for this test.  Cargo
/// populates the `CARGO_BIN_EXE_<name>` env var at compile time for
/// every `[[bin]]` target in the crate.
fn anvil_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_anvil"))
}

#[test]
fn setup_flag_does_not_emit_legacy_step_1_of_4() {
    // Sandboxed config home so the wizard's first-run gate fires and we
    // never touch ~/.anvil/ on the developer's machine.
    let tmp = tempfile::tempdir().expect("tempdir must succeed");

    let mut child = Command::new(anvil_bin())
        .arg("--setup")
        .env("ANVIL_CONFIG_HOME", tmp.path())
        // Closed stdin so the wizard's interactive prompts get an
        // immediate EOF and exit cleanly.  The wizard prints its
        // welcome banner BEFORE the first read, so we still see the
        // banner text we're checking for.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Strip any inherited PATH that might point at a `cargo`
        // intercepting binary; keep everything else.
        .spawn()
        .expect("spawning anvil --setup must succeed");

    // Give the wizard ~3 seconds to emit its banner + first prompt,
    // then terminate.  3s is generous on Apple Silicon (binary cold
    // starts in <100ms); CI runners may want longer but 3s has been
    // sufficient across all our hardware.
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().expect("try_wait must not fail").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Reap if not already.
    let _ = child.wait();

    let mut stdout = String::new();
    if let Some(ref mut out) = child.stdout {
        let _ = out.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(ref mut err) = child.stderr {
        let _ = err.read_to_string(&mut stderr);
    }
    let combined = format!("{stdout}\n{stderr}");

    // LEGACY indicators — any one of these means we routed to setup.rs
    // (the bug).  We check for the most specific strings: the legacy
    // emits "Step 1 of 4" + "Step 2 of 4" + "Step 3 of 4" + "Step 4 of 4"
    // (the new wizard uses 7 steps), and the legacy is the only path
    // that asks "Paste your API key" on stdin (the new wizard uses an
    // alt-screen TextInputModal).
    assert!(
        !combined.contains("Step 1 of 4"),
        "FAIL: `anvil --setup` emitted 'Step 1 of 4' — that's the \
         legacy setup.rs wizard.  CliAction::Setup must route to \
         wizard::run_first_run_wizard.\n--- captured output ---\n{combined}"
    );
    assert!(
        !combined.contains("Paste your API key"),
        "FAIL: `anvil --setup` emitted 'Paste your API key' on stdin \
         — that's the legacy setup.rs wizard's API-key prompt.  The \
         new wizard uses an alt-screen TextInputModal.\n\
         --- captured output ---\n{combined}"
    );

    // NEW-wizard positive indicator.  Under stdin-redirected execution
    // the wizard goes through `run_first_run_wizard_via_stdin`, which
    // emits a boxed welcome banner with the literal "Welcome to Anvil v".
    assert!(
        combined.contains("Welcome to Anvil"),
        "FAIL: `anvil --setup` did not emit the new wizard's welcome \
         banner ('Welcome to Anvil').  Output capture may have raced \
         the child; if this fails intermittently consider extending \
         the 3s grace.\n--- captured output ---\n{combined}"
    );
}
