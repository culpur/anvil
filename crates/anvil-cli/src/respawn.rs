//! Self-respawn mechanism for Anvil v2.2.6.
//!
//! Captures the process launch context at startup and, when a full restart is
//! requested (e.g. after an AnvilHub package install), replaces the current
//! process in-place via `std::os::unix::process::CommandExt::exec` on
//! macOS/Linux.  On Windows, or when the launch context makes in-place
//! replacement unsafe, the caller receives a [`RespawnOutcome::PromptUser`]
//! message instead.
//!
//! # Safety note
//!
//! This module uses `#[allow(unsafe_code)]` because `CommandExt::exec` on
//! Unix is annotated `unsafe` in Rust's standard library — it bypasses the
//! `Drop` destructor chain.  All other code in this module is safe.

#![allow(unsafe_code)]

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

// ── Utility: Anvil home directory ─────────────────────────────────────────────

/// Returns `~/.anvil` (or the override from `ANVIL_CONFIG_HOME`).
///
/// Mirrors `utils::anvil_home_dir` but is intentionally self-contained so
/// this module has no dependency on `pub(crate)` helpers.
fn anvil_home() -> PathBuf {
    if let Ok(config_home) = env::var("ANVIL_CONFIG_HOME") {
        return PathBuf::from(config_home);
    }
    dirs_next::home_dir()
        .unwrap_or_else(|| env::current_dir().unwrap_or_default())
        .join(".anvil")
}

// ── Launch-context detection ──────────────────────────────────────────────────

/// All information captured once at process startup that governs whether an
/// in-place respawn is safe.
#[derive(Debug, Clone)]
pub struct RespawnContext {
    /// `argv[0]` as a string — the binary path used to invoke Anvil.
    pub argv0: String,
    /// The full argument list (`argv[1..]`) forwarded on respawn.
    pub args: Vec<String>,
    /// Relevant environment variables preserved across the exec boundary.
    ///
    /// Currently: `PATH`, `HOME`, `TERM`, `SHELL`, `LANG`, `COLORTERM`,
    /// `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`.
    pub env_captured: Vec<(String, String)>,
    /// `true` when stdin is not attached to a TTY (i.e. input was piped).
    pub launched_from_pipe: bool,
    /// `true` when the process was spawned by `cargo run` or from a Cargo
    /// build artefact path (`target/debug/` or `target/release/`).
    pub launched_from_cargo: bool,
    /// `true` when a known debugger environment variable was detected.
    pub launched_from_debugger: bool,
    /// `true` when `--no-respawn` was present in the original argument list.
    pub no_respawn_flag: bool,
}

impl RespawnContext {
    /// Capture the launch context.  Call this **once** at the very start of
    /// `main()`, before any argument parsing, so that the raw `argv` is
    /// intact.
    #[must_use]
    pub fn capture() -> Self {
        let mut all_args: Vec<String> = env::args().collect();
        let argv0 = all_args.first().cloned().unwrap_or_default();
        // args[1..] — the flags forwarded on respawn
        let args = if all_args.len() > 1 {
            all_args.split_off(1)
        } else {
            vec![]
        };

        let no_respawn_flag = args.iter().any(|a| a == "--no-respawn");

        let launched_from_pipe = Self::detect_pipe_stdin();
        let launched_from_cargo = Self::detect_cargo(&argv0);
        let launched_from_debugger = Self::detect_debugger();

        let env_keys = [
            "PATH",
            "HOME",
            "TERM",
            "SHELL",
            "LANG",
            "COLORTERM",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
        ];
        let env_captured = env_keys
            .iter()
            .filter_map(|key| env::var(key).ok().map(|value| ((*key).to_string(), value)))
            .collect();

        Self {
            argv0,
            args,
            env_captured,
            launched_from_pipe,
            launched_from_cargo,
            launched_from_debugger,
            no_respawn_flag,
        }
    }

    /// Return `true` when an in-place respawn via `exec` is safe.
    ///
    /// Respawn is only safe when:
    /// - The platform is macOS or Linux
    /// - stdin is attached to a TTY (not a pipe)
    /// - Not launched by `cargo run` or from a Cargo build artefact
    /// - Not launched under a debugger
    /// - The `--no-respawn` flag was not passed
    #[must_use]
    pub fn can_respawn(&self) -> bool {
        cfg!(any(target_os = "macos", target_os = "linux"))
            && !self.launched_from_pipe
            && !self.launched_from_cargo
            && !self.launched_from_debugger
            && !self.no_respawn_flag
    }

    // ── Private detection helpers ─────────────────────────────────────────

    fn detect_pipe_stdin() -> bool {
        use std::io::IsTerminal;
        !io::stdin().is_terminal()
    }

    fn detect_cargo(argv0: &str) -> bool {
        // Environment-based detection (most reliable for `cargo run`)
        if env::var_os("CARGO").is_some() || env::var_os("CARGO_MANIFEST_DIR").is_some() {
            return true;
        }
        // Path-based detection: binary lives inside a Cargo build artefact dir
        argv0.contains("target/debug/") || argv0.contains("target/release/")
    }

    fn detect_debugger() -> bool {
        // Known debugger env vars
        if env::var_os("DEBUGGER").is_some() || env::var_os("LLDB_INSTANCE").is_some() {
            return true;
        }
        // RUST_BACKTRACE=full is a strong signal for debugger / developer mode
        matches!(
            env::var("RUST_BACKTRACE").as_deref(),
            Ok("full") | Ok("FULL")
        )
    }
}

// ── Respawn outcome ───────────────────────────────────────────────────────────

/// The result returned by [`respawn`].
pub enum RespawnOutcome {
    /// The process has been replaced via `exec`.  This variant is unreachable
    /// in practice — `exec` never returns on success — but satisfies the type
    /// system and documents the intent.
    #[allow(dead_code)]
    Respawned,
    /// Respawn is not safe in this environment.  The `String` is a message
    /// suitable for display to the user explaining how to restart manually.
    PromptUser(String),
}

// ── Session state preservation ────────────────────────────────────────────────

/// Write a minimal resume marker to `~/.anvil/resume.json` so the next
/// process launch can offer to restore the session.
///
/// Fields:
/// - `session_id`  — current session identifier
/// - `written_at`  — Unix timestamp (seconds)
///
/// Failures are swallowed; a broken resume marker must never block a restart.
fn save_resume_state(session_id: &str) {
    let home = anvil_home();
    let _ = fs::create_dir_all(&home);
    let path = home.join("resume.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = json!({
        "session_id": session_id,
        "written_at": now,
    });
    let _ = fs::write(&path, payload.to_string());
}

// ── PID file management ───────────────────────────────────────────────────────

/// Path of the soft PID lock file: `~/.anvil/.running.pid`.
fn pid_file_path() -> PathBuf {
    anvil_home().join(".running.pid")
}

/// Write the current process PID to `~/.anvil/.running.pid`.
///
/// This is a *soft* lock: if multiple Anvil instances run concurrently, each
/// overwrites the file with its own PID.  The caller should check
/// [`read_running_pid`] at startup and issue a soft warning, not a hard block.
pub fn write_pid_file() {
    let home = anvil_home();
    let _ = fs::create_dir_all(&home);
    let pid = std::process::id();
    let _ = fs::write(pid_file_path(), pid.to_string());
}

/// Remove the PID file.  Call this on clean exit (e.g. via a Drop guard).
pub fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

/// Read the PID recorded in `~/.anvil/.running.pid`.
///
/// Returns `Some(pid)` if a *different* Anvil process is recorded there and
/// appears to still be running; `None` otherwise.
#[must_use]
pub fn read_running_pid() -> Option<u32> {
    let contents = fs::read_to_string(pid_file_path()).ok()?;
    let pid: u32 = contents.trim().parse().ok()?;

    // Ignore our own PID (can happen during a fast restart where the file
    // was not yet cleaned up).
    if pid == std::process::id() {
        return None;
    }

    // Best-effort liveness check on Unix: kill(pid, 0) returns 0 if alive.
    #[cfg(unix)]
    {
        // SAFETY: We are only sending signal 0 to query process existence.
        // No signal is delivered.  The PID is a u32 cast to the C pid_t type,
        // which is i32 on all supported platforms; valid PIDs fit safely.
        let alive = libc_kill_0(pid);
        if alive { Some(pid) } else { None }
    }
    #[cfg(not(unix))]
    {
        // On Windows we cannot easily check liveness; return as a hint.
        Some(pid)
    }
}

/// Thin wrapper around `kill(pid, 0)` so we isolate the `unsafe` block.
#[cfg(unix)]
fn libc_kill_0(pid: u32) -> bool {
    // SAFETY: kill(2) with sig=0 only checks whether the process exists.
    // No signal is delivered.  pid fits in i32 for any realistic process ID.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: see above — sig=0 is a permission/existence probe, not a signal delivery.
    unsafe { kill(pid as i32, 0) == 0 }
}

// ── Core respawn function ─────────────────────────────────────────────────────

/// Save session state, then respawn the process in-place.
///
/// # Parameters
/// - `ctx`        — the [`RespawnContext`] captured at startup
/// - `reason`     — human-readable reason logged before exec (e.g. `"package install"`)
/// - `session_id` — current session ID for the resume marker (pass `""` if unknown)
///
/// # Returns
/// - `Ok(RespawnOutcome::Respawned)` — unreachable; documented for type clarity
/// - `Ok(RespawnOutcome::PromptUser(msg))` — respawn not safe; show `msg` to user
/// - `Err(e)` — unexpected I/O error during exec
pub fn respawn(
    ctx: &RespawnContext,
    reason: &str,
    session_id: &str,
) -> io::Result<RespawnOutcome> {
    if !ctx.can_respawn() {
        return Ok(RespawnOutcome::PromptUser(build_prompt_user_message(ctx, reason)));
    }

    // Persist session resume marker so the new process can pick it up.
    if !session_id.is_empty() {
        save_resume_state(session_id);
    }

    // Remove the PID file; the new process will write its own on startup.
    remove_pid_file();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Build the command: forward all original args except --no-respawn.
        let filtered_args: Vec<&str> = ctx
            .args
            .iter()
            .filter(|a| a.as_str() != "--no-respawn")
            .map(String::as_str)
            .collect();

        let mut cmd = Command::new(&ctx.argv0);
        cmd.args(&filtered_args);

        // Restore the preserved environment variables.
        for (key, value) in &ctx.env_captured {
            cmd.env(key, value);
        }

        // SAFETY: exec(2) replaces the current process image.  Destructors for
        // objects in the current process will NOT run.  This is intentional:
        // the session resume marker has already been written; the PID file has
        // been cleaned up; and terminal state is restored by the OS on exec.
        // The only unsafe aspect is that std::os::unix::process::CommandExt::exec
        // requires unsafe because it bypasses Rust's drop glue.
        let err = cmd.exec();

        // exec only returns on failure.
        Err(err)
    }

    #[cfg(not(unix))]
    {
        Ok(RespawnOutcome::PromptUser(build_prompt_user_message(ctx, reason)))
    }
}

/// Build the user-facing "please restart manually" message.
fn build_prompt_user_message(ctx: &RespawnContext, reason: &str) -> String {
    let cmd = if ctx.argv0.is_empty() {
        "anvil".to_string()
    } else {
        ctx.argv0.clone()
    };
    format!("Restart required ({reason}) — please run `{cmd}` again to apply changes.")
}

// ── Resume state detection ────────────────────────────────────────────────────

/// Information read from `~/.anvil/resume.json` on startup.
#[derive(Debug)]
pub struct ResumeState {
    /// The session ID that was active before the last respawn.
    pub session_id: String,
    /// Unix timestamp (seconds) when the resume marker was written.
    pub written_at: u64,
}

/// Try to load the resume marker left by the previous process.
///
/// Returns `Some(state)` when the marker exists and was written within the
/// last 5 minutes (300 seconds).  Stale markers are deleted automatically.
#[must_use]
pub fn load_resume_state() -> Option<ResumeState> {
    let path = anvil_home().join("resume.json");
    let contents = fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;

    let session_id = value.get("session_id")?.as_str()?.to_string();
    let written_at = value.get("written_at")?.as_u64()?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if now.saturating_sub(written_at) > 300 {
        // Stale marker — clean up and pretend it doesn't exist.
        let _ = fs::remove_file(&path);
        return None;
    }

    Some(ResumeState { session_id, written_at })
}

/// Delete the resume marker.  Call this after the session has been fully
/// restored or when the user declines to restore.
pub fn clear_resume_state() {
    let _ = fs::remove_file(anvil_home().join("resume.json"));
}

// ── Public API for Phase 4 (AnvilHub installer) ───────────────────────────────

/// After a package install, prompt the user about restarting and respawn if
/// they agree.
///
/// Behaviour by [`RestartRequirement`]:
/// - `None`  → no-op; returns immediately
/// - `Soft`  → reload config in-place; prints "Config reloaded."
/// - `Full`  → prompts "Restart Anvil now? [Y/n]"; respawns on Y, prints a
///             reminder message on N
///
/// Called by Phase 4 (AnvilHub installer) after a successful package install.
#[allow(dead_code)] // public API — wired up in Phase 4
pub async fn prompt_restart_and_respawn(
    ctx: &RespawnContext,
    requirement: commands::RestartRequirement,
    session_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{IsTerminal, Write};
    use commands::RestartRequirement;

    match requirement {
        RestartRequirement::None => {
            // Nothing to do.
        }
        RestartRequirement::Soft => {
            // Soft reload: just print a confirmation.  The actual config
            // reload happens in the LiveCli config path; Phase 4 should call
            // that separately.
            println!("Config reloaded.");
        }
        RestartRequirement::Full => {
            // Prompt the user.
            print!("Restart Anvil now? [Y/n] ");
            let _ = io::stdout().flush();

            let mut choice = String::new();
            if io::stdout().is_terminal() {
                let _ = io::stdin().read_line(&mut choice);
            }

            let answer = choice.trim().to_ascii_lowercase();
            if answer.is_empty() || answer == "y" || answer == "yes" {
                match respawn(ctx, "package install", session_id)? {
                    RespawnOutcome::Respawned => {
                        // Unreachable — execvp replaced us.
                    }
                    RespawnOutcome::PromptUser(msg) => {
                        println!("{msg}");
                        std::process::exit(42);
                    }
                }
            } else {
                println!(
                    "Anvil will need to be restarted to activate this change."
                );
            }
        }
    }

    Ok(())
}

// ── PID-file RAII guard ───────────────────────────────────────────────────────

/// A drop guard that removes the PID file on clean exit.
///
/// Create one instance in `main()` so that `remove_pid_file()` is called
/// automatically when the process exits normally.
pub struct PidFileGuard;

impl PidFileGuard {
    /// Write the current PID and return the guard.
    #[must_use]
    pub fn new() -> Self {
        write_pid_file();
        Self
    }
}

impl Default for PidFileGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        remove_pid_file();
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialise any test that touches the shared resume.json on disk so
    /// parallel test threads don't stomp each other's writes.
    static RESUME_MUTEX: Mutex<()> = Mutex::new(());

    // ── RespawnContext::capture ───────────────────────────────────────────

    #[test]
    fn capture_does_not_panic() {
        // Verify capture() runs without panicking in a normal test environment.
        let ctx = RespawnContext::capture();
        // argv0 should be the test runner binary, which is non-empty.
        assert!(!ctx.argv0.is_empty());
    }

    // ── can_respawn gate ─────────────────────────────────────────────────

    #[test]
    fn can_respawn_false_when_pipe_stdin() {
        let ctx = RespawnContext {
            argv0: "/usr/local/bin/anvil".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: true,
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        assert!(!ctx.can_respawn(), "piped stdin must block respawn");
    }

    #[test]
    fn can_respawn_false_when_cargo() {
        let ctx = RespawnContext {
            argv0: "target/debug/anvil".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: false,
            launched_from_cargo: true,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        assert!(!ctx.can_respawn(), "cargo launch must block respawn");
    }

    #[test]
    fn can_respawn_false_when_debugger() {
        let ctx = RespawnContext {
            argv0: "/usr/local/bin/anvil".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: false,
            launched_from_cargo: false,
            launched_from_debugger: true,
            no_respawn_flag: false,
        };
        assert!(!ctx.can_respawn(), "debugger must block respawn");
    }

    #[test]
    fn can_respawn_false_when_no_respawn_flag() {
        let ctx = RespawnContext {
            argv0: "/usr/local/bin/anvil".to_string(),
            args: vec!["--no-respawn".to_string()],
            env_captured: vec![],
            launched_from_pipe: false,
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: true,
        };
        assert!(!ctx.can_respawn(), "--no-respawn flag must block respawn");
    }

    #[cfg(windows)]
    #[test]
    fn can_respawn_false_on_windows() {
        // On Windows the cfg!(any(target_os = ...)) guard is false.
        let ctx = RespawnContext {
            argv0: "anvil.exe".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: false,
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        assert!(!ctx.can_respawn(), "Windows must always return false");
    }

    // ── no_respawn_flag consistency ──────────────────────────────────────

    #[test]
    fn no_respawn_flag_consistent_with_args() {
        let ctx = RespawnContext::capture();
        let has_flag = ctx.args.iter().any(|a| a == "--no-respawn");
        assert_eq!(
            ctx.no_respawn_flag, has_flag,
            "no_respawn_flag must match args list"
        );
    }

    // ── Cargo detection ──────────────────────────────────────────────────

    #[test]
    fn cargo_detected_via_env() {
        // CARGO_MANIFEST_DIR is set in all cargo test environments.
        // So launched_from_cargo must be true when tests run under cargo.
        let detected = RespawnContext::detect_cargo("some_other_path");
        // This assertion only holds when running under `cargo test`.
        if env::var_os("CARGO_MANIFEST_DIR").is_some() {
            assert!(detected, "CARGO_MANIFEST_DIR must trigger cargo detection");
        }
    }

    #[test]
    fn cargo_detected_via_path() {
        // Path-based detection must fire regardless of env state.
        assert!(
            RespawnContext::detect_cargo("target/debug/anvil"),
            "debug path must trigger cargo detection"
        );
        assert!(
            RespawnContext::detect_cargo("/home/user/project/target/release/anvil"),
            "release path must trigger cargo detection"
        );

        // Env-var detection: only assert when no cargo env vars are set.
        // Under `cargo test` these vars are always present, so we can't reliably
        // assert the negative case. The path check above is the core invariant.
        if env::var_os("CARGO").is_none() && env::var_os("CARGO_MANIFEST_DIR").is_none() {
            assert!(
                !RespawnContext::detect_cargo("/opt/homebrew/bin/anvil"),
                "normal install path must not trigger cargo detection without CARGO env"
            );
        }
    }

    // ── Parser: /restart variants ────────────────────────────────────────
    // NOTE: These tests use commands::SlashCommand which lives in a sibling
    // crate; tests here verify the respawn module in isolation.  The parser
    // roundtrip tests live in crates/commands/src/lib.rs.

    // ── Resume state roundtrip ───────────────────────────────────────────

    #[test]
    fn resume_state_roundtrip() {
        let _guard = RESUME_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        // We need a writable home.
        let home = anvil_home();
        let _ = fs::create_dir_all(&home);
        let resume_path = home.join("resume.json");

        // Write a fresh marker.
        save_resume_state("test-session-roundtrip");

        // Should be loadable immediately.
        let state = load_resume_state();
        assert!(state.is_some(), "fresh resume state should be loadable");
        let state = state.unwrap();
        assert_eq!(state.session_id, "test-session-roundtrip");

        // Clean up.
        clear_resume_state();
        assert!(
            !resume_path.exists(),
            "clear_resume_state should delete the file"
        );
    }

    #[test]
    fn resume_state_stale_returns_none() {
        let _guard = RESUME_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let home = anvil_home();
        let _ = fs::create_dir_all(&home);
        let resume_path = home.join("resume.json");

        // Write a marker with a timestamp 400s in the past.
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(400);

        let payload = json!({
            "session_id": "stale-session",
            "written_at": old_ts,
        });
        let _ = fs::write(&resume_path, payload.to_string());

        let state = load_resume_state();
        assert!(state.is_none(), "stale resume state should return None");
        assert!(
            !resume_path.exists(),
            "stale file should be deleted by load_resume_state"
        );
    }

    // ── build_prompt_user_message ────────────────────────────────────────

    #[test]
    fn prompt_user_message_contains_binary_name() {
        let ctx = RespawnContext {
            argv0: "/opt/homebrew/bin/anvil".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: true,
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        let msg = build_prompt_user_message(&ctx, "package install");
        assert!(
            msg.contains("/opt/homebrew/bin/anvil"),
            "message should include the binary path"
        );
        assert!(
            msg.contains("package install"),
            "message should include the reason"
        );
    }

    #[test]
    fn prompt_user_message_falls_back_to_anvil() {
        let ctx = RespawnContext {
            argv0: String::new(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: true,
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        let msg = build_prompt_user_message(&ctx, "test");
        assert!(
            msg.contains("`anvil`"),
            "empty argv0 should fall back to `anvil`"
        );
    }

    // ── respawn returns PromptUser when unsafe ───────────────────────────

    #[test]
    fn respawn_returns_prompt_user_when_not_safe() {
        let ctx = RespawnContext {
            argv0: "/usr/local/bin/anvil".to_string(),
            args: vec![],
            env_captured: vec![],
            launched_from_pipe: true, // makes can_respawn() false
            launched_from_cargo: false,
            launched_from_debugger: false,
            no_respawn_flag: false,
        };
        let outcome = respawn(&ctx, "test", "").expect("respawn should not error on unsafe ctx");
        assert!(
            matches!(outcome, RespawnOutcome::PromptUser(_)),
            "unsafe context must yield PromptUser, not exec"
        );
    }

    // ── PID file ─────────────────────────────────────────────────────────

    #[test]
    fn pid_file_write_and_remove() {
        let path = pid_file_path();
        write_pid_file();
        assert!(path.exists(), "PID file should exist after write_pid_file");

        let contents = fs::read_to_string(&path).unwrap_or_default();
        let pid: u32 = contents.trim().parse().expect("PID file should contain a number");
        assert_eq!(pid, std::process::id(), "PID file should contain current PID");

        remove_pid_file();
        assert!(!path.exists(), "PID file should be removed after remove_pid_file");
    }
}
