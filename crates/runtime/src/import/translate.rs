/// Translate trait — transform artifact bytes before staging.
///
/// Phase 6.0: trait definition only.  Concrete implementations are added
/// per artifact type in Bucket 1+.
///
/// # Design
///
/// Translation is a pure transformation of bytes: given the source artifact's
/// raw content, return the transformed bytes ready to be written to staging.
///
/// Examples:
///   - Memory files: prepend YAML frontmatter fields `imported_from`,
///     `imported_at`, `source_path`, `content_hash`.
///   - Settings: remap CC key names to Anvil equivalents per §5 of the spec.
///   - Instructions (CLAUDE.md → ANVIL.md): no content change; just copy.

use std::path::PathBuf;

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta};

/// Result of a translation pass.
#[derive(Debug, Clone)]
pub struct TranslationResult {
    /// Transformed bytes to write to staging.
    pub bytes: Vec<u8>,
    /// Suggested filename within the staging artifact subdirectory.
    /// Bucket implementations may override this.
    pub suggested_name: String,
    /// Destination path under `~/.anvil/` where this artifact should land
    /// after commit.
    pub destination: PathBuf,
    /// Optional warning to include in the import report (e.g. "bash hook
    /// imported disabled — enable explicitly").
    pub warning: Option<String>,
}

/// Trait for transforming a discovered artifact's bytes.
pub trait Translator: Send + Sync {
    /// Transform `source_bytes` (read from `artifact.source_path()`).
    ///
    /// Return `Ok(None)` to skip translation (pass bytes through verbatim).
    ///
    /// # Errors
    ///
    /// Return `Err(reason)` if the translation fails in a way that should
    /// mark the artifact as `Failed` in the manifest.
    fn translate(
        &self,
        artifact: &ImportArtifact,
        meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String>;

    /// Human-readable name for this translator.
    fn name(&self) -> &'static str;
}
