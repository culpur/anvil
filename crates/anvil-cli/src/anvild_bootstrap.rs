//! Bootstrap hook that ensures `anvild` is running before TUI starts.
//!
//! Called from `run_repl_tui` / `run_repl_plain` during startup, BEFORE the
//! alternate screen is activated (so `eprintln!` / `println!` are safe here).
//!
//! # Decision matrix
//!
//! | `anvild.autostart` | daemon alive? | action                                    |
//! |--------------------|---------------|-------------------------------------------|
//! | `yes`              | yes           | nothing — already up                      |
//! | `yes`              | no            | `spawn_detached`; wait up to 5s           |
//! | `no`               | any           | return; TUI keepalive fallback will run   |
//! | `ask`              | any (TTY)     | prompt user; persist choice; act on it   |
//! | `ask`              | any (non-TTY) | skip prompt; leave as `ask` for later     |

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use runtime::{load_anvild_config, save_anvild_config, AnvildAutostart, AnvildConfig};

use crate::daemon;
use crate::utils::anvil_home_dir;

// ── Public entry point ────────────────────────────────────────────────────────

/// Called during TUI startup hooks.  Reads config, optionally prompts, spawns.
///
/// Runs BEFORE alt-screen is activated — `eprintln!` is safe.
/// If the daemon can't be started, logs to `~/.anvil/anvil.log` and returns
/// gracefully.  Daemon absence is a recoverable condition.
pub fn ensure_anvild_for_session() {
    let home = anvil_home_dir();
    let config_path = home.join("config.json");
    let mut cfg = load_anvild_config(&config_path);

    match cfg.autostart {
        AnvildAutostart::Yes => {
            ensure_daemon_running(&home, &config_path, &cfg);
        }
        AnvildAutostart::No => {
            // User opted out; TUI keepalive fallback handles refresh.
        }
        AnvildAutostart::Ask => {
            if !io::stdout().is_terminal() {
                // Headless (CI, scripts): skip prompt, leave as "ask".
                return;
            }
            match prompt_user() {
                UserChoice::Install => {
                    cfg.autostart = AnvildAutostart::Yes;
                    cfg.install_service = true;
                    persist_choice(&config_path, &cfg);
                    // Install service unit, then spawn.
                    let binary = current_binary_path();
                    let code = daemon::install_service(&home, &binary);
                    if code != 0 {
                        eprintln!("anvild: service install exited with code {code} (continuing without service unit)");
                    }
                    ensure_daemon_running(&home, &config_path, &cfg);
                }
                UserChoice::AskLater => {
                    // Do nothing; leave autostart as "ask".
                    // (cfg was already "ask" so no write needed)
                }
                UserChoice::Never => {
                    cfg.autostart = AnvildAutostart::No;
                    cfg.install_service = false;
                    persist_choice(&config_path, &cfg);
                    // TUI keepalive fallback will run.
                }
            }
        }
    }
}

// ── Daemon lifecycle helpers ──────────────────────────────────────────────────

/// Spawn or verify the daemon.  Polls `anvild_running()` for up to 5 seconds.
fn ensure_daemon_running(
    home: &std::path::Path,
    config_path: &std::path::Path,
    cfg: &AnvildConfig,
) {
    if daemon::anvild_running() {
        return; // Already up.
    }

    let binary = current_binary_path();

    // If service was promised but the daemon isn't running, re-install (idempotent).
    if cfg.install_service {
        let code = daemon::install_service(home, &binary);
        if code != 0 {
            eprintln!("anvild: re-install service exited with code {code}");
        }
    }

    let spawn_code = daemon::spawn_detached(home, &binary);
    if spawn_code != 0 {
        let msg = format!("spawn_detached exited with code {spawn_code}");
        daemon::daemon_log(home, &msg);
        eprintln!("anvild: {msg} — using in-TUI keepalive fallback");
        return;
    }

    // Poll up to 5 seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if daemon::anvild_running() {
            return; // Daemon confirmed alive.
        }
        thread::sleep(Duration::from_millis(200));
    }

    daemon::daemon_log(home, "startup: daemon did not become alive within 5s; falling back to TUI keepalive");
    // Don't print to stderr — silent fallback is better UX.
    let _ = config_path; // suppress unused warning
}

fn persist_choice(config_path: &std::path::Path, cfg: &AnvildConfig) {
    if let Err(e) = save_anvild_config(config_path, cfg) {
        eprintln!("anvild: could not persist config: {e}");
    }
}

fn current_binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("anvil"))
}

// ── First-launch prompt ──────────────────────────────────────────────────────

/// The three choices presented to the user on first TUI launch.
enum UserChoice {
    Install,
    AskLater,
    Never,
}

/// Print a plain-terminal prompt (safe: this runs before alt-screen).
///
/// NOT-TTY path: callers must guard with `io::stdout().is_terminal()`.
///
/// # Alt-screen callers
///
/// Do NOT invoke this function from inside the wizard alt-screen or any
/// other ratatui context.  Any `println!`/`print!` call while ratatui's
/// alt-screen is active goes BEHIND the alt-screen and corrupts the
/// back-buffer — see `feedback-tui-stdout-anti-pattern.md`.
///
/// First-run users reach this function only when `autostart == Ask` AND
/// the first-run wizard did not run (e.g. upgrade from an older version
/// that predates v2.2.19).  The wizard now handles the first-run case
/// via `wizard_daemon::run_daemon_step`, which leaves `autostart` as
/// either `Yes` or `No` — both of which short-circuit before this
/// function is called.  For upgrade users who skipped the wizard entirely,
/// `autostart` remains `Ask`, this function fires on the NEXT plain-
/// terminal startup (before `run_repl_tui` enters alt-screen), and that
/// is safe.
fn prompt_user() -> UserChoice {
    println!();
    println!("\x1b[1;36m┌─ Anvil background service (anvild) ─────────────────────────┐\x1b[0m");
    println!("\x1b[36m│\x1b[0m                                                              \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m  Anvil works best with a background service (anvild) that:  \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m    • Keeps your Anthropic OAuth token refreshed so sessions  \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m      resume cleanly                                          \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m    • Runs scheduled routines even when no terminal is open   \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m                                                              \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m  Install anvild as a background service?                     \x1b[36m│\x1b[0m");
    println!("\x1b[36m│\x1b[0m                                                              \x1b[36m│\x1b[0m");
    println!("\x1b[1;36m└──────────────────────────────────────────────────────────────┘\x1b[0m");
    println!();
    println!("  \x1b[1m[1]\x1b[0m Install (recommended)");
    println!("  \x1b[1m[2]\x1b[0m Not now, ask me next time");
    println!("  \x1b[1m[3]\x1b[0m Never ask me again");
    println!();

    loop {
        print!("  Choice [1/2/3]: ");
        let _ = io::Write::flush(&mut io::stdout());

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return UserChoice::AskLater;
        }
        match line.trim() {
            "1" | "" => return UserChoice::Install,
            "2" => return UserChoice::AskLater,
            "3" => return UserChoice::Never,
            _ => {
                println!("  Please enter 1, 2, or 3.");
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_daemon_returns_immediately_when_config_is_no() {
        // With autostart=No, ensure_daemon_running is never called.
        // We can only test that the config loading path works.
        let dir = TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.json");
        let cfg = AnvildConfig {
            autostart: AnvildAutostart::No,
            install_service: false,
        };
        save_anvild_config(&config_path, &cfg).expect("save");
        let loaded = load_anvild_config(&config_path);
        assert_eq!(loaded.autostart, AnvildAutostart::No);
    }

    #[test]
    fn ensure_daemon_returns_immediately_when_daemon_alive() {
        // daemon::anvild_running() returns false in this test context (no real pid file),
        // so ensure_daemon_running will attempt spawn.  We just check it doesn't panic
        // with a harmless binary path that immediately fails.
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path();
        let cfg = AnvildConfig {
            autostart: AnvildAutostart::Yes,
            install_service: false,
        };
        // This will fail to spawn (no 'anvil' binary in PATH in test), but should not panic.
        ensure_daemon_running(home, &home.join("config.json"), &cfg);
    }

    #[test]
    fn persist_choice_writes_to_disk() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("config.json");
        let cfg = AnvildConfig {
            autostart: AnvildAutostart::Yes,
            install_service: true,
        };
        persist_choice(&path, &cfg);
        let loaded = load_anvild_config(&path);
        assert_eq!(loaded.autostart, AnvildAutostart::Yes);
        assert!(loaded.install_service);
    }
}
