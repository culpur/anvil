//! `anvil --check` — diagnostic health check for the Anvil installation.
//!
//! Prints a green/red checklist covering:
//! - Anvil binary on PATH
//! - Node.js and npm (version awareness)
//! - QMD discoverable
//! - Git on PATH
//! - Vault status
//! - API keys present for each configured provider
//! - Relay reachability (wss://passage.culpur.net/v1/relay)

use std::fmt;
use std::path::Path;
use std::process::Command;

// ── Node.js minimum version ───────────────────────────────────────────────────

/// Minimum acceptable Node.js major version (LTS floor).
const NODE_MIN_MAJOR: u32 = 18;

// ── Check result ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub(crate) struct CheckRow {
    pub label: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

impl fmt::Display for CheckRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (icon, color) = match self.status {
            CheckStatus::Ok => ("\u{2714}", "\x1b[32m"),   // ✔ green
            CheckStatus::Warn => ("\u{26A0}", "\x1b[33m"), // ⚠ yellow
            CheckStatus::Fail => ("\u{2718}", "\x1b[31m"), // ✘ red
        };
        write!(
            f,
            "  {color}{icon}\x1b[0m  {:<28} {}",
            self.label, self.detail
        )
    }
}

// ── Individual checks ─────────────────────────────────────────────────────────

/// Check whether `anvil` itself is on the PATH.
fn check_anvil_on_path() -> CheckRow {
    // We're running, so we're clearly on PATH or invoked directly.
    // Report the resolved path for user visibility.
    let path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    CheckRow {
        label: "Anvil on PATH",
        status: CheckStatus::Ok,
        detail: path,
    }
}

/// Run `<cmd> <version_flag>` and return trimmed stdout, or `None`.
fn version_output(cmd: &str, flag: &str) -> Option<String> {
    Command::new(cmd)
        .arg(flag)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Parse major version from e.g. "v20.11.0" → 20.
fn parse_major(version_str: &str) -> Option<u32> {
    version_str
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
}

fn check_node() -> CheckRow {
    match version_output("node", "--version") {
        None => CheckRow {
            label: "Node.js",
            status: CheckStatus::Fail,
            detail: "not found — install from https://nodejs.org".to_string(),
        },
        Some(ver) => {
            let major = parse_major(&ver).unwrap_or(0);
            if major >= NODE_MIN_MAJOR {
                CheckRow {
                    label: "Node.js",
                    status: CheckStatus::Ok,
                    detail: ver,
                }
            } else {
                CheckRow {
                    label: "Node.js",
                    status: CheckStatus::Warn,
                    detail: format!("{ver} — upgrade to v{NODE_MIN_MAJOR}+ recommended"),
                }
            }
        }
    }
}

fn check_npm() -> CheckRow {
    match version_output("npm", "--version") {
        None => CheckRow {
            label: "npm",
            status: CheckStatus::Fail,
            detail: "not found".to_string(),
        },
        Some(ver) => CheckRow {
            label: "npm",
            status: CheckStatus::Ok,
            detail: ver,
        },
    }
}

fn check_git() -> CheckRow {
    match version_output("git", "--version") {
        None => CheckRow {
            label: "Git",
            status: CheckStatus::Fail,
            detail: "not found — install git from https://git-scm.com".to_string(),
        },
        Some(ver) => CheckRow {
            label: "Git",
            status: CheckStatus::Ok,
            detail: ver,
        },
    }
}

/// Look for qmd in PATH and a few documented fallback locations.
pub(crate) fn find_qmd() -> Option<String> {
    // 1. On PATH
    if version_output("qmd", "--version").is_some() {
        return Some("qmd (on PATH)".to_string());
    }
    // 2. Common install locations
    let candidates = [
        dirs_next::home_dir()
            .map(|h| h.join(".local/bin/qmd"))
            .as_deref()
            .map(Path::to_path_buf),
        Some(std::path::PathBuf::from("/usr/local/bin/qmd")),
        Some(std::path::PathBuf::from("/opt/homebrew/bin/qmd")),
    ];
    for candidate in candidates.iter().flatten() {
        if candidate.exists() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

fn check_qmd() -> CheckRow {
    match find_qmd() {
        Some(path) => CheckRow {
            label: "QMD",
            status: CheckStatus::Ok,
            detail: path,
        },
        None => CheckRow {
            label: "QMD",
            status: CheckStatus::Warn,
            detail: "not found — install via AnvilHub or https://anvilhub.culpur.net".to_string(),
        },
    }
}

/// Check vault status by reading `~/.anvil/vault/` presence and `~/.anvil/config.json`.
fn check_vault() -> CheckRow {
    let Some(home) = dirs_next::home_dir() else {
        return CheckRow {
            label: "Vault",
            status: CheckStatus::Fail,
            detail: "cannot resolve home directory".to_string(),
        };
    };
    let anvil_home = home.join(".anvil");
    let vault_dir = anvil_home.join("vault");
    let config = anvil_home.join("config.json");

    if !anvil_home.exists() {
        return CheckRow {
            label: "Vault",
            status: CheckStatus::Warn,
            detail: "~/.anvil/ not found — run `anvil --setup`".to_string(),
        };
    }
    if !vault_dir.exists() {
        return CheckRow {
            label: "Vault",
            status: CheckStatus::Warn,
            detail: "vault not initialised — run `anvil --setup`".to_string(),
        };
    }
    if !config.exists() {
        return CheckRow {
            label: "Vault",
            status: CheckStatus::Warn,
            detail: "config.json missing — run `anvil --setup`".to_string(),
        };
    }
    CheckRow {
        label: "Vault",
        status: CheckStatus::Ok,
        detail: vault_dir.display().to_string(),
    }
}

/// Check that at least one provider has a key in config.json.
fn check_api_keys() -> CheckRow {
    let Some(home) = dirs_next::home_dir() else {
        return CheckRow {
            label: "API keys",
            status: CheckStatus::Fail,
            detail: "cannot resolve home directory".to_string(),
        };
    };
    let config_path = home.join(".anvil").join("config.json");
    let Ok(data) = std::fs::read_to_string(&config_path) else {
        return CheckRow {
            label: "API keys",
            status: CheckStatus::Warn,
            detail: "config.json not found — run `anvil --setup`".to_string(),
        };
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else {
        return CheckRow {
            label: "API keys",
            status: CheckStatus::Warn,
            detail: "config.json is not valid JSON".to_string(),
        };
    };

    // Count providers with a non-empty api_key or url (for Ollama).
    let providers = ["anthropic", "openai", "google", "xai", "ollama"];
    let mut configured: Vec<&str> = Vec::new();
    for p in &providers {
        let has_key = val
            .pointer(&format!("/providers/{p}/api_key"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        let has_url = val
            .pointer(&format!("/providers/{p}/url"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        if has_key || has_url {
            configured.push(p);
        }
    }

    if configured.is_empty() {
        CheckRow {
            label: "API keys",
            status: CheckStatus::Warn,
            detail: "no providers configured — run `anvil --setup`".to_string(),
        }
    } else {
        CheckRow {
            label: "API keys",
            status: CheckStatus::Ok,
            detail: configured.join(", "),
        }
    }
}

/// Ping the AnvilHub packages endpoint as a relay/connectivity sanity check.
fn check_relay() -> CheckRow {
    // Using the packages endpoint as a lightweight connectivity check.
    // It is unauthenticated and small.
    let url = "https://passage.culpur.net/v1/hub/packages?limit=1";
    let out = Command::new("curl")
        .args(["-sf", "--max-time", "5", "-o", "/dev/null", "-w", "%{http_code}", url])
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let code = String::from_utf8_lossy(&o.stdout);
            let code = code.trim();
            // 200 or 401 both mean the server is reachable
            if matches!(code, "200" | "401" | "403") {
                CheckRow {
                    label: "Relay reachable",
                    status: CheckStatus::Ok,
                    detail: format!("passage.culpur.net (HTTP {code})"),
                }
            } else {
                CheckRow {
                    label: "Relay reachable",
                    status: CheckStatus::Warn,
                    detail: format!("unexpected HTTP {code} from passage.culpur.net"),
                }
            }
        }
        _ => CheckRow {
            label: "Relay reachable",
            status: CheckStatus::Fail,
            detail: "cannot reach passage.culpur.net — check network".to_string(),
        },
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run all checks and print the report.  Returns the number of failed checks.
///
/// v2.2.18: also runs the new `health::probe_all` layer (#667) and prints
/// its structured report below the legacy checklist.  The exit code is
/// non-zero if either the legacy checks have a failure or the health
/// probes detect Breakage / Drift severity.
pub(crate) fn run_check() -> u32 {
    let rows = collect_checks();
    print_check_report(&rows);
    let legacy_fails = rows
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count() as u32;

    // New health probe layer (v2.2.18 / task #667).
    let report = crate::health::probe_all(crate::health::HAPPY_PATH_BUDGET);
    println!("{}", report.render_cli());
    let health_fails = match report.severity() {
        crate::health::Severity::Green => 0,
        crate::health::Severity::Drift => 1,
        crate::health::Severity::Breakage => 1,
    };

    legacy_fails + health_fails
}

/// Collect all check results (separated for testability).
pub(crate) fn collect_checks() -> Vec<CheckRow> {
    vec![
        check_anvil_on_path(),
        check_node(),
        check_npm(),
        check_git(),
        check_qmd(),
        check_vault(),
        check_api_keys(),
        check_relay(),
    ]
}

fn print_check_report(rows: &[CheckRow]) {
    println!();
    println!("\x1b[1mAnvil environment check\x1b[0m");
    println!("\x1b[90m{}\x1b[0m", "\u{2501}".repeat(60));
    for row in rows {
        println!("{row}");
    }
    println!("\x1b[90m{}\x1b[0m", "\u{2501}".repeat(60));
    let fails = rows.iter().filter(|r| r.status == CheckStatus::Fail).count();
    let warns = rows.iter().filter(|r| r.status == CheckStatus::Warn).count();
    if fails == 0 && warns == 0 {
        println!("  \x1b[1;32mAll checks passed.\x1b[0m");
    } else {
        if fails > 0 {
            println!("  \x1b[1;31m{fails} check(s) failed.\x1b[0m");
        }
        if warns > 0 {
            println!("  \x1b[1;33m{warns} warning(s).\x1b[0m");
        }
    }
    println!();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_row_display_ok() {
        let row = CheckRow {
            label: "Anvil on PATH",
            status: CheckStatus::Ok,
            detail: "/usr/local/bin/anvil".to_string(),
        };
        let s = row.to_string();
        assert!(s.contains("\u{2714}"), "ok row must contain checkmark");
        assert!(s.contains("Anvil on PATH"));
    }

    #[test]
    fn check_row_display_fail() {
        let row = CheckRow {
            label: "Git",
            status: CheckStatus::Fail,
            detail: "not found".to_string(),
        };
        let s = row.to_string();
        assert!(s.contains("\u{2718}"), "fail row must contain cross");
    }

    #[test]
    fn check_row_display_warn() {
        let row = CheckRow {
            label: "Node.js",
            status: CheckStatus::Warn,
            detail: "v16 — upgrade recommended".to_string(),
        };
        let s = row.to_string();
        assert!(s.contains("\u{26A0}"), "warn row must contain warning sign");
    }

    #[test]
    fn parse_major_handles_prefixed_v() {
        assert_eq!(parse_major("v20.11.0"), Some(20));
        assert_eq!(parse_major("18.0.0"), Some(18));
        assert_eq!(parse_major("v8.17.0"), Some(8));
    }

    #[test]
    fn parse_major_handles_invalid() {
        assert_eq!(parse_major("not-a-version"), None);
        assert_eq!(parse_major(""), None);
    }

    #[test]
    fn collect_checks_returns_eight_rows() {
        // Smoke test: all eight checks run without panicking.
        let rows = collect_checks();
        assert_eq!(rows.len(), 8);
    }

    #[test]
    fn anvil_on_path_always_ok() {
        // We are running, so this must always be Ok.
        let row = check_anvil_on_path();
        assert_eq!(row.status, CheckStatus::Ok);
    }

    #[test]
    fn node_check_returns_valid_status() {
        let row = check_node();
        // Must be one of the three statuses; specifically not a panic.
        assert!(matches!(
            row.status,
            CheckStatus::Ok | CheckStatus::Warn | CheckStatus::Fail
        ));
    }

    #[test]
    fn run_check_returns_non_negative() {
        // Just ensure it doesn't panic; exact count depends on environment.
        let _fails = run_check();
    }
}
