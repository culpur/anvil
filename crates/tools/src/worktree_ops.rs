use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::json;

use crate::to_pretty_json;

/// Persistent state for an active worktree session.
#[derive(Debug, Clone)]
struct WorktreeState {
    /// Absolute path to the worktree directory.
    worktree_path: PathBuf,
    /// Branch name created for this worktree.
    branch: String,
    /// The working directory that was active before `EnterWorktree` was called.
    original_dir: PathBuf,
}

fn worktree_state() -> &'static Mutex<Option<WorktreeState>> {
    static INSTANCE: OnceLock<Mutex<Option<WorktreeState>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Deserialize)]
pub(crate) struct EnterWorktreeInput {
    pub(crate) branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExitWorktreeInput {
    pub(crate) cleanup: Option<bool>,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_enter_worktree(input: EnterWorktreeInput) -> Result<String, String> {
    let mut state = worktree_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if state.is_some() {
        return Err(
            "Already inside a worktree. Call ExitWorktree before entering another.".to_string(),
        );
    }

    // Locate the git repository root from the current working directory.
    let current = std::env::current_dir().map_err(|e| format!("cannot read cwd: {e}"))?;

    let repo_root_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&current)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if !repo_root_output.status.success() {
        return Err(format!(
            "Not inside a git repository ({})",
            String::from_utf8_lossy(&repo_root_output.stderr).trim()
        ));
    }

    let repo_root = PathBuf::from(
        String::from_utf8_lossy(&repo_root_output.stdout)
            .trim()
            .to_string(),
    );

    // Generate a unique id from wall-clock sub-second time.
    let id = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    );
    let branch_name = input
        .branch
        .unwrap_or_else(|| format!("worktree-{id}"));

    let worktree_path = repo_root.join(".anvil").join("worktrees").join(&id);

    // Ensure parent directory exists.
    std::fs::create_dir_all(worktree_path.parent().unwrap_or(&worktree_path))
        .map_err(|e| format!("cannot create worktrees directory: {e}"))?;

    // `git worktree add <path> -b <branch>`
    let add_output = Command::new("git")
        .args([
            "worktree",
            "add",
            worktree_path.to_str().ok_or("worktree path is not valid UTF-8")?,
            "-b",
            &branch_name,
        ])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git worktree add: {e}"))?;

    if !add_output.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&add_output.stderr).trim()
        ));
    }

    // Move the process working directory into the new worktree.
    std::env::set_current_dir(&worktree_path)
        .map_err(|e| format!("cannot cd into worktree: {e}"))?;

    *state = Some(WorktreeState {
        worktree_path: worktree_path.clone(),
        branch: branch_name.clone(),
        original_dir: current,
    });

    to_pretty_json(json!({
        "status": "entered_worktree",
        "worktree_path": worktree_path,
        "branch": branch_name,
        "message": format!(
            "Working directory is now the isolated worktree at {}. \
             Changes here will not affect the main tree.",
            worktree_path.display()
        )
    }))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_exit_worktree(input: ExitWorktreeInput) -> Result<String, String> {
    let mut state = worktree_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let ws = state.take().ok_or_else(|| {
        "Not currently inside a worktree. Call EnterWorktree first.".to_string()
    })?;

    let cleanup = input.cleanup.unwrap_or(true);

    // Restore the original working directory first so subsequent git commands
    // run in the main repo context.
    std::env::set_current_dir(&ws.original_dir)
        .map_err(|e| format!("cannot restore original directory: {e}"))?;

    let mut cleaned_up = false;
    let cleanup_message;

    if cleanup {
        // Check for uncommitted changes inside the worktree.
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&ws.worktree_path)
            .output();

        let has_changes = match status_output {
            Ok(out) => !out.stdout.is_empty(),
            Err(_) => true, // conservative: assume dirty when we cannot tell
        };

        if has_changes {
            cleanup_message = format!(
                "Worktree at {} has uncommitted changes and was NOT removed. \
                 Branch `{}` is still available.",
                ws.worktree_path.display(),
                ws.branch
            );
        } else {
            let remove_output = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    ws.worktree_path.to_str().unwrap_or(""),
                ])
                .current_dir(&ws.original_dir)
                .output();

            match remove_output {
                Ok(out) if out.status.success() => {
                    // Best-effort branch deletion.
                    let _ = Command::new("git")
                        .args(["branch", "-d", &ws.branch])
                        .current_dir(&ws.original_dir)
                        .output();
                    cleaned_up = true;
                    cleanup_message = format!(
                        "Worktree at {} and branch `{}` have been removed.",
                        ws.worktree_path.display(),
                        ws.branch
                    );
                }
                Ok(out) => {
                    cleanup_message = format!(
                        "git worktree remove failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Err(e) => {
                    cleanup_message = format!("could not run git worktree remove: {e}");
                }
            }
        }
    } else {
        cleanup_message = format!(
            "Cleanup skipped. Worktree at {} and branch `{}` remain.",
            ws.worktree_path.display(),
            ws.branch
        );
    }

    to_pretty_json(json!({
        "status": "exited_worktree",
        "worktree_path": ws.worktree_path,
        "branch": ws.branch,
        "cleaned_up": cleaned_up,
        "original_dir": ws.original_dir,
        "message": cleanup_message
    }))
}
