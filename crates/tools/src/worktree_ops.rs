use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::json;
use runtime::{ConfigLoader, CwdChangedPayload, HookRunner};

use crate::to_pretty_json;

/// Fire `CwdChanged` hooks after a successful `set_current_dir`.
///
/// # Limitation
/// The `HookRunner` is freshly constructed from the on-disk config at the new
/// cwd.  If the session's in-memory runner carries runtime-only extras (e.g.
/// MCP-tool hooks) they will not fire here.  A full solution would require
/// threading the session's `HookRunner` down through the tool executor; that
/// refactor is deferred to a future stream.
fn fire_cwd_changed(old_cwd: &std::path::Path, new_cwd: &std::path::Path) {
    let runner = match ConfigLoader::default_for(new_cwd).load() {
        Ok(cfg) => HookRunner::from_feature_config(cfg.feature_config()),
        Err(_) => HookRunner::default(),
    };
    let _ = runner.run_cwd_changed(&CwdChangedPayload {
        old_cwd: old_cwd.display().to_string(),
        new_cwd: new_cwd.display().to_string(),
    });
}

/// Persistent state for an active worktree session.
#[derive(Debug, Clone)]
struct WorktreeState {
    /// Absolute path to the worktree directory.
    worktree_path: PathBuf,
    /// Branch name created for this worktree.
    branch: String,
    /// The working directory that was active before `EnterWorktree` was called.
    original_dir: PathBuf,
    /// CC parity (v2.1.144-B10, task #724): the absolute session-storage
    /// directory (`<original_dir>/.anvil/sessions/`) captured at enter time
    /// so `/branch` (and any other command that wants to load conversation
    /// history written by the parent session) can recover it after the
    /// process CWD has been switched into the worktree.
    original_sessions_dir: PathBuf,
}

fn worktree_state() -> &'static Mutex<Option<WorktreeState>> {
    static INSTANCE: OnceLock<Mutex<Option<WorktreeState>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(None))
}

/// CC parity (v2.1.144-B10, task #724): return the absolute session-storage
/// directory captured at `EnterWorktree` time, or `None` if no worktree is
/// currently active.
///
/// `/branch` and any other command that loads conversation history written
/// by the parent session should consult this before falling back to
/// `<cwd>/.anvil/sessions/` — otherwise post-worktree-switch lookups will
/// resolve against the (empty) worktree's sessions directory and surface
/// "No conversation to branch".
#[must_use]
pub fn original_sessions_dir() -> Option<PathBuf> {
    let state = worktree_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.as_ref().map(|ws| ws.original_sessions_dir.clone())
}

/// CC parity (v2.1.144-B10, task #724): return the absolute working
/// directory active before `EnterWorktree` was called, or `None` if no
/// worktree is currently active. This is the parent-tree CWD; callers can
/// resolve other relative paths (hook configs, MCP files, .anvil/*) against
/// it during worktree-scoped work.
#[must_use]
pub fn original_dir() -> Option<PathBuf> {
    let state = worktree_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.as_ref().map(|ws| ws.original_dir.clone())
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

    // CC-133-F1: when `worktree.baseRef` is set in settings.json, pass it
    // as the start-point argument so the new branch is created off that
    // ref instead of HEAD.  Absent → original behaviour (HEAD).
    let base_ref = ConfigLoader::default_for(&repo_root)
        .load()
        .ok()
        .and_then(|cfg| cfg.worktree().base_ref().map(str::to_string));

    let worktree_path_str = worktree_path.to_str().ok_or("worktree path is not valid UTF-8")?;
    let mut git_args: Vec<&str> = vec!["worktree", "add", worktree_path_str, "-b", &branch_name];
    if let Some(ref base) = base_ref {
        git_args.push(base);
    }

    let add_output = Command::new("git")
        .args(&git_args)
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
    fire_cwd_changed(&current, &worktree_path);

    // CC parity (v2.1.144-B10, task #724): capture the original session
    // storage directory BEFORE the cwd switch so `/branch` and any other
    // post-enter conversation-history lookup can recover the path it was
    // resolved against at session start. The current code already swaps
    // CWD via `set_current_dir` above, so any subsequent
    // `<cwd>/.anvil/sessions/` resolve would point inside the worktree.
    let original_sessions_dir = current.join(".anvil").join("sessions");

    *state = Some(WorktreeState {
        worktree_path: worktree_path.clone(),
        branch: branch_name.clone(),
        original_dir: current,
        original_sessions_dir,
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
    fire_cwd_changed(&ws.worktree_path, &ws.original_dir);

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
