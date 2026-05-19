// Edition 2024: env::set_var/remove_var require unsafe; we don't use those here.
#![deny(clippy::print_stdout, clippy::print_stderr)]

//! Local-only `/release` slash command (task #656 F7).
//!
//! This is intentionally a thin shim over `scripts/release.sh` — it is for
//! the operator's own workflow (Maverick / soulofall) and is NOT a feature
//! for end users. Public surfaces (README, AnvilHub /about, culpur.net,
//! release notes) must not mention `/release`.
//!
//! The handler returns a `String` to the TUI scrollback rather than
//! spawning an alt-screen-corrupting subprocess (see
//! feedback-tui-stdout-anti-pattern). For a real release, the user is
//! expected to run `scripts/release.sh` from a shell — this command
//! exists to show status, pre-flight checks, and document the next
//! invocation.

use std::path::{Path, PathBuf};

/// Operator-only subcommands. Add new variants here as the workflow grows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReleaseSub {
    Status,
    Preflight,
    DryRun,
    Help,
    Unknown(String),
}

impl ReleaseSub {
    pub(crate) fn parse(args: Option<&str>) -> Self {
        match args.map(str::trim).unwrap_or("") {
            "" | "help" | "-h" | "--help" => Self::Help,
            "status" => Self::Status,
            "preflight" | "pre-flight" => Self::Preflight,
            "dry-run" | "dryrun" => Self::DryRun,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// Top-level entry. Returns a status string for the chat log.
/// Operator-only — see module doc.
pub(crate) fn run_release_command(args: Option<&str>) -> String {
    match ReleaseSub::parse(args) {
        ReleaseSub::Help => help_text(),
        ReleaseSub::Status => status_report(repo_root()),
        ReleaseSub::Preflight => preflight_report(repo_root()),
        ReleaseSub::DryRun => dry_run_report(repo_root()),
        ReleaseSub::Unknown(s) => format!(
            "/release: unknown subcommand {s:?}\nTry /release help."
        ),
    }
}

/// Where the anvil-dev checkout lives. Falls back to cwd. We keep this
/// pure-ish (env lookup only) so tests can drive it via a fixture.
fn repo_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANVIL_REPO_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn help_text() -> String {
    "\
/release \u{2014} operator-only release helpers (local skill, not public).

USAGE
  /release status      Show current branch + tag + workspace version.
  /release preflight   Run release-script preflight (read-only).
  /release dry-run     Same as preflight + describe what release.sh
                       would do without running it.
  /release help        This message.

NOTES
  Cuts a real release with `scripts/release.sh` from a shell. This
  command does NOT invoke release.sh \u{2014} that needs per-release
  authorization and stable terminal state (alt-screen would corrupt
  cargo / cross output)."
        .to_string()
}

/// `/release status` — current branch, tag, workspace version.
/// Pure-data: reads git + Cargo.toml; no side effects.
pub(crate) fn status_report(root: PathBuf) -> String {
    let mut out = String::new();
    out.push_str("Release status\n");

    let branch = git_current_branch(&root).unwrap_or_else(|| "(unknown)".to_string());
    out.push_str(&format!("  branch:    {branch}\n"));

    let head = git_head_short(&root).unwrap_or_else(|| "(unknown)".to_string());
    out.push_str(&format!("  head:      {head}\n"));

    let latest_tag = git_latest_tag(&root).unwrap_or_else(|| "(no tags)".to_string());
    out.push_str(&format!("  last tag:  {latest_tag}\n"));

    let version = workspace_version(&root).unwrap_or_else(|| "(unreadable)".to_string());
    out.push_str(&format!("  Cargo:     v{version}\n"));

    let dirty = git_is_dirty(&root);
    out.push_str(&format!(
        "  worktree:  {}\n",
        if dirty { "DIRTY" } else { "clean" }
    ));

    out
}

/// `/release preflight` — read-only checks: tag matches Cargo, no
/// uncommitted, no unpushed.
pub(crate) fn preflight_report(root: PathBuf) -> String {
    let mut out = String::from("Release preflight\n");
    let mut issues = 0usize;

    if git_is_dirty(&root) {
        out.push_str("  ✗ working tree has uncommitted changes\n");
        issues += 1;
    } else {
        out.push_str("  ✓ working tree clean\n");
    }

    let upstream_ahead = git_unpushed_count(&root);
    match upstream_ahead {
        Some(0) => out.push_str("  ✓ branch is up to date with upstream\n"),
        Some(n) => {
            out.push_str(&format!("  ✗ branch has {n} unpushed commit(s)\n"));
            issues += 1;
        }
        None => out.push_str("  ? upstream unknown (no tracking branch)\n"),
    }

    let cargo_v = workspace_version(&root);
    let tag_v = git_latest_tag(&root).map(|t| t.trim_start_matches('v').to_string());
    match (cargo_v.as_deref(), tag_v.as_deref()) {
        (Some(c), Some(t)) if c == t => {
            out.push_str(&format!(
                "  ✓ Cargo version v{c} matches latest tag v{t}\n"
            ));
        }
        (Some(c), Some(t)) => {
            out.push_str(&format!(
                "  ! Cargo v{c} differs from latest tag v{t} (expected if cutting a new release)\n"
            ));
        }
        _ => {
            out.push_str("  ? could not compare Cargo vs latest tag\n");
        }
    }

    let release_sh = root.join("scripts").join("release.sh");
    if release_sh.is_file() {
        out.push_str(&format!("  ✓ found {}\n", release_sh.display()));
    } else {
        out.push_str(&format!("  ✗ missing {}\n", release_sh.display()));
        issues += 1;
    }

    out.push_str(&format!(
        "\n{} issue(s).\n",
        if issues == 0 { "0".to_string() } else { issues.to_string() }
    ));
    out
}

fn dry_run_report(root: PathBuf) -> String {
    let mut out = preflight_report(root.clone());
    out.push_str("\nWould run (operator must execute from shell):\n");
    out.push_str(&format!("  cd {}\n", root.display()));
    out.push_str("  scripts/release.sh\n");
    out.push_str("\n(Per CLAUDE.md, /release never invokes release.sh directly —\n");
    out.push_str(" needs explicit per-release authorization + a stable terminal.)\n");
    out
}

// ─── git helpers ─────────────────────────────────────────────────────────────
//
// All shell out to `git -C <root>` and trim. None of them panic — every
// failure mode reports as None / false so the status report stays useful
// in degraded environments.

fn git_current_branch(root: &Path) -> Option<String> {
    git_output(root, &["rev-parse", "--abbrev-ref", "HEAD"])
}

fn git_head_short(root: &Path) -> Option<String> {
    git_output(root, &["rev-parse", "--short", "HEAD"])
}

fn git_latest_tag(root: &Path) -> Option<String> {
    git_output(root, &["describe", "--tags", "--abbrev=0"])
}

fn git_is_dirty(root: &Path) -> bool {
    git_output(root, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

fn git_unpushed_count(root: &Path) -> Option<usize> {
    let raw = git_output(root, &["rev-list", "--count", "@{u}..HEAD"])?;
    raw.parse::<usize>().ok()
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

// ─── Cargo.toml helper ───────────────────────────────────────────────────────

fn workspace_version(root: &Path) -> Option<String> {
    let cargo_toml = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&cargo_toml).ok()?;
    // Look for top-level `version = "X.Y.Z"`. The repo's root Cargo.toml
    // is a [workspace] manifest with a single workspace.package version.
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim().trim_matches('"').trim_matches('\'');
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_help_variants() {
        assert_eq!(ReleaseSub::parse(None), ReleaseSub::Help);
        assert_eq!(ReleaseSub::parse(Some("")), ReleaseSub::Help);
        assert_eq!(ReleaseSub::parse(Some("help")), ReleaseSub::Help);
        assert_eq!(ReleaseSub::parse(Some("-h")), ReleaseSub::Help);
        assert_eq!(ReleaseSub::parse(Some("--help")), ReleaseSub::Help);
    }

    #[test]
    fn parse_subcommands() {
        assert_eq!(ReleaseSub::parse(Some("status")), ReleaseSub::Status);
        assert_eq!(ReleaseSub::parse(Some("preflight")), ReleaseSub::Preflight);
        assert_eq!(ReleaseSub::parse(Some("pre-flight")), ReleaseSub::Preflight);
        assert_eq!(ReleaseSub::parse(Some("dry-run")), ReleaseSub::DryRun);
        assert_eq!(ReleaseSub::parse(Some("dryrun")), ReleaseSub::DryRun);
    }

    #[test]
    fn parse_unknown_preserves_input() {
        match ReleaseSub::parse(Some("ship-it")) {
            ReleaseSub::Unknown(s) => assert_eq!(s, "ship-it"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn help_text_mentions_release_sh() {
        let h = help_text();
        assert!(h.contains("scripts/release.sh"));
        assert!(h.contains("operator-only"));
    }

    #[test]
    fn dry_run_includes_release_sh_command() {
        let tmp = tempfile::tempdir().unwrap();
        let report = dry_run_report(tmp.path().to_path_buf());
        assert!(report.contains("scripts/release.sh"));
        assert!(report.contains("cd"));
    }

    #[test]
    fn workspace_version_parses_simple_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace.package]\nversion = \"2.2.18\"\nedition = \"2024\"\n",
        )
        .unwrap();
        assert_eq!(workspace_version(tmp.path()), Some("2.2.18".to_string()));
    }

    #[test]
    fn workspace_version_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        // No Cargo.toml at all.
        assert_eq!(workspace_version(tmp.path()), None);
    }

    #[test]
    fn status_report_handles_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let report = status_report(tmp.path().to_path_buf());
        assert!(report.contains("Release status"));
        // No git → all unknowns, but the report still renders cleanly.
        assert!(report.contains("branch:"));
        assert!(report.contains("Cargo:"));
    }

    #[test]
    fn preflight_report_flags_missing_release_sh() {
        let tmp = tempfile::tempdir().unwrap();
        let report = preflight_report(tmp.path().to_path_buf());
        assert!(report.contains("missing"));
        assert!(report.contains("release.sh"));
    }

    #[test]
    fn run_release_command_default_is_help() {
        let out = run_release_command(None);
        assert!(out.contains("/release"));
        assert!(out.contains("operator-only"));
    }
}
