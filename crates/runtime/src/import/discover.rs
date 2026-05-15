/// Discover trait — enumerate artifacts from an import source.
///
/// Phase 6.0: trait definition only.  Concrete implementations are added
/// per artifact type in Bucket 1+ (memory, instructions, settings, etc.).
///
/// # Contract
///
/// Implementors MUST:
///   - Be read-only with respect to the source (`~/.claude/`).
///   - Return all artifacts of the relevant type, including those that will
///     later be triaged as skippable.
///   - Not panic on missing directories — return an empty vec instead.

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta, ImportSource};

/// Discovered artifact plus its metadata.
///
/// Returned in bulk by `Discoverer::discover`.
pub struct DiscoveredArtifact {
    /// The artifact itself (typed).
    pub artifact: ImportArtifact,
    /// Metadata attached at discovery time.
    pub meta: ImportArtifactMeta,
}

/// Trait for enumerating a specific class of CC artifacts.
///
/// Each implementor handles one variant of `ImportArtifact` (e.g. memory,
/// instructions, skills).  The pipeline calls `discover` on every registered
/// discoverer at the start of an import run.
pub trait Discoverer: Send + Sync {
    /// Return all artifacts of this type found under `source`.
    ///
    /// Errors are silently absorbed into a `skip_reason` — a discoverer
    /// that hits a permissions error on one file should continue with the
    /// rest, not abort the entire import.
    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact>;

    /// Human-readable name for this discoverer (used in progress output).
    fn name(&self) -> &'static str;
}
