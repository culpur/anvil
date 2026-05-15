/// Import pipeline — Phase 6 of the v2.2.14 arc.
///
/// Provides the types, traits, and staging machinery for migrating a CC
/// installation into Anvil.  The pipeline has seven stages:
///
/// ```text
/// 1. Discover    — enumerate ~/.claude/ artifact types
/// 2. Triage      — categorize each into Anvil tier + skip list
/// 3. Translate   — frontmatter stamps; light path/identity rewrites
/// 4. Stage       — write to ~/.anvil/.import-staging/
/// 5. Diff        — show user what will land where
/// 6. Confirm     — user accepts or cancels
/// 7. Commit      — atomic move from staging to final destinations
/// 8. Report      — manifest + skipped-with-reasons + needs-review list
/// ```
///
/// # Phase 6.0 scope
///
/// This module contains only the foundation:
/// - `ImportArtifact` enum and `ImportSource` (artifact.rs)
/// - `ImportManifest` / `ImportEntry` schema (manifest.rs)
/// - Staging directory lifecycle: create, validate, commit, rollback (staging.rs)
/// - Trait stubs: `Discoverer`, `Triager`, `Translator`, `Stager` (one each)
/// - Commit orchestrator (commit.rs)
/// - Report generator (report.rs)
///
/// Bucket 1+ fills in concrete Discoverer/Translator/Stager implementations
/// for each artifact type.
///
/// # Idempotency guarantee
///
/// Re-running `anvil import claude-code` on unchanged source is a no-op.
/// The manifest (`~/.anvil/.import-manifest.json`) records every committed
/// artifact's `(source_path, content_hash)` pair.  An artifact is skipped
/// if its hash and path already appear in a `Committed` entry.
///
/// # Read-only on ~/.claude/
///
/// The import pipeline NEVER modifies the source CC installation.

pub mod artifact;
pub mod commit;
pub mod discover;
pub mod instructions;
pub mod manifest;
pub mod memory;
pub mod plugins;
pub mod report;
pub mod settings;
pub mod skills;
pub mod stage;
pub mod staging;
pub mod triage;
pub mod translate;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use artifact::{ImportArtifact, ImportArtifactMeta, ImportSource, SettingsScope};
pub use commit::{run_commit, CommitResult};
pub use discover::{DiscoveredArtifact, Discoverer};
pub use manifest::{ImportEntry, ImportEntryStatus, ImportManifest, ManifestError};
pub use report::{generate_report, generate_full_report, write_report, write_full_report, ReportOptions};
pub use stage::{StageAction, Stager};
pub use staging::{
    anvil_config_home, staging_dir, CommitReport, StagingDir, StagingError,
    STAGING_SUBDIRS,
};
pub use triage::{TriageDecision, Triager};
pub use translate::{TranslationResult, Translator};

// ── Content hashing ──────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of `bytes`.
///
/// Used as the idempotency key: stored in `ImportEntry::content_hash` and
/// compared on re-runs to detect unchanged artifacts.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Compute the SHA-256 hex digest of the file at `path`.
///
/// Returns `None` if the file cannot be read; the caller should record a
/// skip-with-reason rather than failing the whole import.
#[must_use]
pub fn sha256_file(path: &std::path::Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| sha256_hex(&b))
}

// ── RFC 3339 timestamp helper ─────────────────────────────────────────────────

/// Return the current time formatted as an RFC 3339 string.
///
/// Uses UTC with second precision.  Always valid; never panics.
#[must_use]
pub fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal RFC 3339 without chrono dependency — good enough for the manifest.
    let (y, mo, d, h, mi, s) = secs_to_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Decompose a UNIX timestamp (seconds since epoch) to UTC components.
fn secs_to_utc(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    const SECS_PER_MIN: u64 = 60;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_DAY: u64 = 86400;

    let s = secs % SECS_PER_MIN;
    let mi = (secs / SECS_PER_MIN) % 60;
    let h = (secs / SECS_PER_HOUR) % 24;

    // Days since epoch
    let mut days = secs / SECS_PER_DAY;
    let mut year: u64 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: [u64; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u64 = 1;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1, h, mi, s)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ── OTel trace events ────────────────────────────────────────────────────────

/// OTel event constants for the import pipeline.
///
/// Phase 6.0 declared the skeleton; Phase 6.4 adds the full set wired in the
/// orchestrator.
pub mod events {
    pub const DISCOVERED: &str = "import.discovered";
    pub const TRANSLATED: &str = "import.translated";
    pub const STAGED: &str = "import.staged";
    pub const COMMITTED: &str = "import.committed";
    pub const SKIPPED: &str = "import.skipped";
    pub const INVOKED: &str = "import.invoked";
    // Phase 6.4 additions
    pub const CONFLICT_DETECTED: &str = "import.conflict_detected";
    pub const COMPLETED: &str = "import.completed";
}

// ── OTel emit helpers ────────────────────────────────────────────────────────

/// Emit `import.invoked` at command entry.
pub fn otel_import_invoked(source: &str, scope: &str, dry_run: bool, include_sessions: bool) {
    crate::otel::emit_event(
        events::INVOKED,
        &[
            ("source", source),
            ("scope", scope),
            ("dry_run", if dry_run { "true" } else { "false" }),
            ("include_sessions", if include_sessions { "true" } else { "false" }),
        ],
    );
}

/// Emit `import.discovered` once per artifact type.
pub fn otel_import_discovered(kind: &str, count: usize) {
    let count_s = count.to_string();
    crate::otel::emit_event(
        events::DISCOVERED,
        &[("kind", kind), ("count", &count_s)],
    );
}

/// Emit `import.staged` once per artifact type.
pub fn otel_import_staged(kind: &str, count: usize) {
    let count_s = count.to_string();
    crate::otel::emit_event(
        events::STAGED,
        &[("kind", kind), ("count", &count_s)],
    );
}

/// Emit `import.conflict_detected` when conflicts surface during triage.
pub fn otel_import_conflict_detected(kind: &str, count: usize) {
    let count_s = count.to_string();
    crate::otel::emit_event(
        events::CONFLICT_DETECTED,
        &[("kind", kind), ("count", &count_s)],
    );
}

/// Emit `import.committed` once per artifact type after commit.
pub fn otel_import_committed(kind: &str, count: usize) {
    let count_s = count.to_string();
    crate::otel::emit_event(
        events::COMMITTED,
        &[("kind", kind), ("count", &count_s)],
    );
}

/// Emit `import.skipped` per skip category.
pub fn otel_import_skipped(kind: &str, count: usize, reason: &str) {
    let count_s = count.to_string();
    crate::otel::emit_event(
        events::SKIPPED,
        &[("kind", kind), ("count", &count_s), ("reason", reason)],
    );
}

/// Emit `import.completed` — the final pipeline event.
pub fn otel_import_completed(
    total_committed: usize,
    total_skipped: usize,
    total_needs_review: usize,
    duration_ms: u64,
) {
    let committed_s = total_committed.to_string();
    let skipped_s = total_skipped.to_string();
    let needs_review_s = total_needs_review.to_string();
    let duration_s = duration_ms.to_string();
    crate::otel::emit_event(
        events::COMPLETED,
        &[
            ("total_committed", &committed_s),
            ("total_skipped", &skipped_s),
            ("total_needs_review", &needs_review_s),
            ("duration_ms", &duration_s),
        ],
    );
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_deterministic() {
        let a = sha256_hex(b"hello world");
        let b = sha256_hex(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn sha256_hex_is_sensitive_to_content() {
        let a = sha256_hex(b"hello world");
        let b = sha256_hex(b"hello world!");
        assert_ne!(a, b);
    }

    #[test]
    fn sha256_hex_known_value() {
        // printf 'abc' | shasum -a 256
        // ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let h = sha256_hex(b"abc");
        assert_eq!(h, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn now_rfc3339_looks_like_iso8601() {
        let ts = now_rfc3339();
        // Minimal sanity check: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "timestamp should be 20 chars: {ts}");
        assert!(ts.ends_with('Z'), "should end with Z: {ts}");
        assert_eq!(&ts[4..5], "-", "should have dash after year");
        assert_eq!(&ts[7..8], "-", "should have dash after month");
        assert_eq!(&ts[10..11], "T", "should have T separator");
    }

    #[test]
    fn events_constants_are_prefixed() {
        assert!(events::DISCOVERED.starts_with("import."));
        assert!(events::COMMITTED.starts_with("import."));
        assert!(events::INVOKED.starts_with("import."));
    }
}
