//! `anvil project purge [path]` — wipe per-workspace Anvil state.
//!
//! CC parity FEAT-39 (CC v2.1.126). Removes everything Anvil has stored about
//! a given project: per-workspace session transcripts, daily summaries,
//! private project memory, and any cached file-history. Useful when archiving
//! a project, or before publishing a previously-private repo.
//!
//! Modes:
//!   - `--dry-run` / `-n`  — print what would be deleted, don't delete.
//!   - `-y` / `--yes`      — skip the confirmation prompt.
//!   - `-i` / `--interactive` — prompt for each file (deferred to a future
//!     iteration; for now `--interactive` implies `--dry-run` + visible list).
//!   - `--all`             — purge state for ALL projects under `~/.anvil/`.
//!     With `--all`, the path argument is ignored.
//!
//! What gets removed (for one project at `<path>`):
//!   - `~/.anvil/sessions/<workspace-hash>/`  (per-workspace transcripts)
//!   - `~/.anvil/daily/<workspace-hash>/`     (daily summaries scoped here)
//!   - `~/.anvil/private/<workspace-hash>.enc` (private project memory)
//!   - `~/.anvil/file-history/<workspace-hash>/` (if present)
//!
//! What is NEVER touched:
//!   - The vault (`~/.anvil/vault.*`)
//!   - Settings (`~/.anvil/settings.json`, `config.json`)
//!   - Theme, keybindings, OAuth credentials
//!   - The project's own working tree or `.git/`

use runtime::private_memory_project_hash;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeOptions {
    pub path: Option<PathBuf>,
    pub all: bool,
    pub dry_run: bool,
    pub assume_yes: bool,
    pub interactive: bool,
}

impl Default for PurgeOptions {
    fn default() -> Self {
        Self {
            path: None,
            all: false,
            dry_run: false,
            assume_yes: false,
            interactive: false,
        }
    }
}

/// Parse `project purge [path] [flags...]` from the slice that follows
/// `anvil project purge`. Returns `Err` on unknown flags or conflicting
/// combinations.
pub fn parse_purge_args(rest: &[String]) -> Result<PurgeOptions, String> {
    let mut opts = PurgeOptions::default();
    let mut idx = 0;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--all" => {
                opts.all = true;
                idx += 1;
            }
            "--dry-run" | "-n" => {
                opts.dry_run = true;
                idx += 1;
            }
            "-y" | "--yes" => {
                opts.assume_yes = true;
                idx += 1;
            }
            "-i" | "--interactive" => {
                opts.interactive = true;
                // For now, --interactive implies --dry-run because we don't
                // yet have a per-file prompt loop. Listing without action.
                opts.dry_run = true;
                idx += 1;
            }
            "--help" | "-h" => {
                return Err(format!("USAGE: anvil project purge [path] [--dry-run|-n] [--yes|-y] [--interactive|-i] [--all]"));
            }
            other if other.starts_with("--") || other.starts_with('-') => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if opts.path.is_some() {
                    return Err(format!("unexpected extra argument: {other}"));
                }
                opts.path = Some(PathBuf::from(other));
                idx += 1;
            }
        }
    }

    if opts.all && opts.path.is_some() {
        return Err("cannot combine --all with an explicit path".to_string());
    }
    if !opts.all && opts.path.is_none() {
        opts.path = Some(std::env::current_dir().map_err(|e| {
            format!("could not detect current directory: {e}; pass an explicit path or --all")
        })?);
    }

    Ok(opts)
}

/// Locate the on-disk targets that `purge` would remove for a given workspace
/// path. Returns the four canonical locations, regardless of whether they
/// currently exist on disk — the caller filters non-existent entries.
fn purge_targets_for(anvil_home: &Path, workspace: &Path) -> Vec<PathBuf> {
    let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let hash = private_memory_project_hash(&canonical);
    vec![
        anvil_home.join("sessions").join(&hash),
        anvil_home.join("daily").join(&hash),
        anvil_home.join("private").join(format!("{hash}.enc")),
        anvil_home.join("file-history").join(&hash),
    ]
}

/// Filter targets to only those that actually exist on disk.
fn existing(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| p.exists()).cloned().collect()
}

/// Run the purge with the supplied options against `anvil_home` (typically
/// `default_config_home()`). Returns `Ok(removed_paths)` on success.
pub fn run_purge<W: Write>(
    anvil_home: &Path,
    opts: &PurgeOptions,
    stdout: &mut W,
) -> io::Result<Vec<PathBuf>> {
    let workspaces: Vec<PathBuf> = if opts.all {
        list_all_workspace_paths_unknowable(anvil_home)
    } else {
        vec![opts.path.clone().expect("parse_purge_args ensures path is set when !all")]
    };

    let mut all_targets: Vec<PathBuf> = Vec::new();
    if opts.all {
        // For --all we can't reverse-map hashes back to paths, so we just
        // list every per-workspace dir under sessions/, daily/, etc.
        for sub in ["sessions", "daily", "private", "file-history"] {
            let dir = anvil_home.join(sub);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    all_targets.push(entry.path());
                }
            }
        }
    } else {
        for ws in &workspaces {
            all_targets.extend(existing(&purge_targets_for(anvil_home, ws)));
        }
    }

    if all_targets.is_empty() {
        writeln!(stdout, "anvil project purge: no per-workspace state to remove.")?;
        return Ok(Vec::new());
    }

    writeln!(stdout, "The following Anvil state will be removed:")?;
    for t in &all_targets {
        writeln!(stdout, "  {}", t.display())?;
    }

    if opts.dry_run {
        writeln!(stdout, "(dry-run; nothing was deleted)")?;
        return Ok(Vec::new());
    }

    if !opts.assume_yes {
        write!(stdout, "Proceed? [y/N] ")?;
        stdout.flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        if !matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            writeln!(stdout, "Aborted.")?;
            return Ok(Vec::new());
        }
    }

    let mut removed = Vec::new();
    for t in &all_targets {
        let result = if t.is_dir() {
            fs::remove_dir_all(t)
        } else {
            fs::remove_file(t)
        };
        match result {
            Ok(()) => removed.push(t.clone()),
            Err(e) => {
                writeln!(stdout, "  warning: failed to remove {}: {e}", t.display())?;
            }
        }
    }
    writeln!(stdout, "Removed {} item(s).", removed.len())?;
    Ok(removed)
}

/// For `--all`, we can't map hashes back to paths since hashing is one-way.
/// This stub is here for symmetry; the actual `--all` handling enumerates
/// directory entries directly in `run_purge`.
fn list_all_workspace_paths_unknowable(_anvil_home: &Path) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_bare_purge_uses_cwd() {
        let opts = parse_purge_args(&[]).expect("parse");
        assert!(opts.path.is_some());
        assert!(!opts.all);
        assert!(!opts.dry_run);
    }

    #[test]
    fn parses_explicit_path() {
        let args = vec!["/some/project".to_string()];
        let opts = parse_purge_args(&args).expect("parse");
        assert_eq!(opts.path.as_deref(), Some(Path::new("/some/project")));
    }

    #[test]
    fn parses_all_flag() {
        let opts = parse_purge_args(&["--all".to_string()]).expect("parse");
        assert!(opts.all);
        assert!(opts.path.is_none());
    }

    #[test]
    fn parses_dry_run_short_and_long() {
        for flag in ["--dry-run", "-n"] {
            let opts = parse_purge_args(&[flag.to_string(), "/p".to_string()]).expect("parse");
            assert!(opts.dry_run, "{flag} should set dry_run");
        }
    }

    #[test]
    fn rejects_all_with_path() {
        let args = vec!["/some/project".to_string(), "--all".to_string()];
        assert!(parse_purge_args(&args).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        let args = vec!["--frobnicate".to_string()];
        assert!(parse_purge_args(&args).is_err());
    }

    #[test]
    fn dry_run_lists_targets_but_deletes_nothing() {
        let home = TempDir::new().expect("tmp");
        let workspace = TempDir::new().expect("workspace");
        // run_purge canonicalizes the workspace path before hashing, so the
        // test must do the same when planting fixtures or the hashes won't
        // match (macOS resolves /var/folders/... → /private/var/folders/...).
        let canonical_workspace = workspace.path().canonicalize().unwrap();
        let hash = private_memory_project_hash(&canonical_workspace);

        // Plant fake state for this workspace.
        let session_dir = home.path().join("sessions").join(&hash);
        let daily_dir = home.path().join("daily").join(&hash);
        let private_file = home.path().join("private").join(format!("{hash}.enc"));
        fs::create_dir_all(&session_dir).unwrap();
        fs::create_dir_all(&daily_dir).unwrap();
        fs::create_dir_all(home.path().join("private")).unwrap();
        fs::write(&session_dir.join("a.json"), "{}").unwrap();
        fs::write(&private_file, b"enc").unwrap();

        let opts = PurgeOptions {
            path: Some(workspace.path().to_path_buf()),
            all: false,
            dry_run: true,
            assume_yes: true,
            interactive: false,
        };
        let mut output = Vec::new();
        let removed = run_purge(home.path(), &opts, &mut output).expect("purge");
        assert!(removed.is_empty(), "dry-run should remove nothing");
        assert!(session_dir.exists(), "session dir should still exist after dry-run");
        assert!(private_file.exists(), "private file should still exist after dry-run");

        let stdout = String::from_utf8_lossy(&output);
        assert!(stdout.contains(&hash), "output should mention workspace hash:\n{stdout}");
        assert!(stdout.contains("dry-run"), "output should declare dry-run mode:\n{stdout}");
    }

    #[test]
    fn live_purge_removes_known_targets() {
        let home = TempDir::new().expect("tmp");
        let workspace = TempDir::new().expect("workspace");
        // run_purge canonicalizes the workspace path before hashing, so the
        // test must do the same when planting fixtures or the hashes won't
        // match (macOS resolves /var/folders/... → /private/var/folders/...).
        let canonical_workspace = workspace.path().canonicalize().unwrap();
        let hash = private_memory_project_hash(&canonical_workspace);

        let session_dir = home.path().join("sessions").join(&hash);
        let private_file = home.path().join("private").join(format!("{hash}.enc"));
        fs::create_dir_all(&session_dir).unwrap();
        fs::create_dir_all(home.path().join("private")).unwrap();
        fs::write(&session_dir.join("a.json"), "{}").unwrap();
        fs::write(&private_file, b"enc").unwrap();

        let opts = PurgeOptions {
            path: Some(workspace.path().to_path_buf()),
            all: false,
            dry_run: false,
            assume_yes: true,
            interactive: false,
        };
        let mut output = Vec::new();
        let removed = run_purge(home.path(), &opts, &mut output).expect("purge");
        assert_eq!(removed.len(), 2, "should have removed sessions and private");
        assert!(!session_dir.exists());
        assert!(!private_file.exists());
    }

    #[test]
    fn live_purge_no_state_is_a_clean_no_op() {
        let home = TempDir::new().expect("tmp");
        let workspace = TempDir::new().expect("workspace");
        let opts = PurgeOptions {
            path: Some(workspace.path().to_path_buf()),
            all: false,
            dry_run: false,
            assume_yes: true,
            interactive: false,
        };
        let mut output = Vec::new();
        let removed = run_purge(home.path(), &opts, &mut output).expect("purge");
        assert!(removed.is_empty());
        let stdout = String::from_utf8_lossy(&output);
        assert!(stdout.contains("no per-workspace state"), "should print no-op message:\n{stdout}");
    }
}
