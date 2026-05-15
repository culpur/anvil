/// Commit module — atomic move from staging to final destinations.
///
/// Phase 6.0: types and orchestration logic.  The actual move machinery
/// lives in `StagingDir::commit_to_final`; this module wraps it with
/// pre/post-commit hooks for future bucket use.
///
/// # Safety contract
///
/// - All moves are atomic rename(2) calls on POSIX systems.
/// - If the source and destination are on different filesystems,
///   `rename` fails and the entry is marked `Failed` (never partial).
/// - The final manifest is always written, even when individual entries fail.

use crate::import::manifest::ImportManifest;
use crate::import::staging::{CommitReport, StagingDir, StagingError};

/// Result of a full commit phase.
#[derive(Debug, Clone, Default)]
pub struct CommitResult {
    /// Number of artifacts successfully committed to final destinations.
    pub committed: usize,
    /// Number of artifacts that failed during commit.
    pub failed: usize,
    /// Errors encountered, if any.
    pub errors: Vec<String>,
}

impl CommitResult {
    /// Return `true` if every staged artifact was committed successfully.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed == 0
    }
}

/// Run the commit phase: move all `Staged` entries to final destinations.
///
/// Updates entry statuses in `manifest` in-place.  Caller is responsible
/// for persisting the manifest after this call.
///
/// # Errors
///
/// Returns `StagingError` if the staging directory itself cannot be accessed.
/// Individual entry errors are captured in `CommitResult::errors`.
pub fn run_commit(
    manifest: &mut ImportManifest,
    staging: &StagingDir,
) -> Result<CommitResult, StagingError> {
    let report: CommitReport = staging.commit_to_final(manifest)?;

    let errors: Vec<String> = manifest
        .entries
        .iter()
        .filter_map(|e| e.error.as_ref())
        .cloned()
        .collect();

    Ok(CommitResult {
        committed: report.committed,
        failed: report.failed,
        errors,
    })
}
