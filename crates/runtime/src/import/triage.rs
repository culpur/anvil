/// Triage trait — categorise a discovered artifact into keep/skip/needs-review.
///
/// Phase 6.0: trait definition only.  Concrete implementations are added
/// per artifact type in Bucket 1+.
///
/// # Design
///
/// Triage is a pure function: given an artifact and its metadata, it returns
/// a `TriageDecision` without touching the filesystem.  The staging step
/// (which writes to disk) runs only for `Keep` decisions.

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta};

/// Decision produced by `Triager::triage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageDecision {
    /// Bring this artifact — proceed to Translate then Stage.
    Keep,
    /// Skip this artifact with the given reason (appears in the report).
    Skip {
        /// Human-readable explanation, e.g.
        /// "CC credentials are not portable; run `anvil login`".
        reason: String,
    },
    /// Stage the artifact but flag it for user review before Commit.
    NeedsReview {
        /// Explanation of why review is needed.
        reason: String,
    },
}

impl TriageDecision {
    /// Convenience: is this a Keep?
    #[must_use]
    pub fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }

    /// Convenience: is this a Skip?
    #[must_use]
    pub fn is_skip(&self) -> bool {
        matches!(self, Self::Skip { .. })
    }
}

/// Trait for classifying a single discovered artifact.
pub trait Triager: Send + Sync {
    /// Classify the artifact.
    fn triage(&self, artifact: &ImportArtifact, meta: &ImportArtifactMeta) -> TriageDecision;

    /// Human-readable name for this triager.
    fn name(&self) -> &'static str;
}
