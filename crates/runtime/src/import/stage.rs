/// Stage module — write translated artifact bytes to the staging directory.
///
/// Phase 6.0: stage action types and the `Stager` trait.  Concrete
/// implementations are added per artifact type in Bucket 1+.
///
/// # Role in the pipeline
///
/// After Triage (Keep) and Translate:
///   1. `Stager::stage` writes the translated bytes to
///      `~/.anvil/.import-staging/<artifact_kind>/<filename>`.
///   2. The manifest entry is updated to `Staged`.
///   3. `Commit` later moves staged files to their final destinations
///      atomically.

use std::path::PathBuf;

use crate::import::staging::StagingDir;
use crate::import::translate::TranslationResult;
use crate::import::artifact::ImportArtifact;

/// Action record for a single stage operation.
#[derive(Debug, Clone)]
pub struct StageAction {
    /// Absolute path within the staging directory where bytes were written.
    pub staged_path: PathBuf,
    /// Final destination path (under `~/.anvil/`) for the commit phase.
    pub destination: PathBuf,
}

/// Trait for writing a translated artifact into the staging directory.
pub trait Stager: Send + Sync {
    /// Write `translation` into `staging`.
    ///
    /// Returns the `StageAction` describing where the file landed in staging
    /// and where it should be committed to.
    ///
    /// # Errors
    ///
    /// Return `Err(reason)` on I/O failure.
    fn stage(
        &self,
        artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String>;

    /// Human-readable name for this stager.
    fn name(&self) -> &'static str;
}
