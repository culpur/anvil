// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME / CC_PROFILE_DIR).
// Matches the pattern in staging.rs (Rust 2024 requires unsafe for set_var).
#![cfg_attr(test, allow(unsafe_code))]

/// Memory entry import — Phase 6.1a.
///
/// Concrete `Discoverer`, `Triager`, `Translator`, and `Stager` implementations
/// for CC memory markdown files:
///
///   `~/.claude/projects/<id>/memory/*.md`  →  `~/.anvil/memory/<filename>`
///
/// # Pipeline contract
///
/// 1. **Discover** — walk every `<profile_dir>/projects/<id>/memory/` directory,
///    collect all `.md` files, compute SHA-256, gate on the idempotency manifest.
/// 2. **Triage** — always Keep unless: empty file, non-UTF8 content, or already
///    committed with the same hash (no-op).
/// 3. **Translate** — preserve the original body verbatim.  Prepend (or merge
///    into) YAML frontmatter the fields `imported_from`, `imported_at`,
///    `source_path`, `content_hash`.  DO NOT modify existing frontmatter fields.
/// 4. **Stage** — write to `<staging>/memory/<dest_filename>` where
///    `dest_filename` is disambiguated: `<6-char project hash>-<original filename>`
///    when two project dirs produce the same filename, plain `<original filename>`
///    otherwise.
///
/// # Idempotency
///
/// Before staging, load `~/.anvil/.import-manifest.json`.  If an entry exists
/// with matching `source_path` AND `content_hash` at status `Committed`, skip.
///
/// # Read-only on `~/.claude/`
///
/// No write to the CC profile directory at any point.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::import::{
    artifact::{ImportArtifact, ImportArtifactMeta, ImportSource},
    discover::{DiscoveredArtifact, Discoverer},
    manifest::{ImportEntry, ImportEntryStatus, ImportManifest},
    sha256_hex,
    stage::{StageAction, Stager},
    staging::{anvil_config_home, StagingDir},
    triage::{TriageDecision, Triager},
    translate::{TranslationResult, Translator},
};

// ── MemoryDiscoverer ─────────────────────────────────────────────────────────

/// Discovers CC memory entries under `<profile_dir>/projects/<id>/memory/`.
///
/// Loads the committed manifest once at construction to enable idempotency
/// gating at discovery time (entries already committed are skipped early,
/// rather than staged again).
pub struct MemoryDiscoverer {
    /// Pre-loaded manifest for idempotency gating.
    manifest: ImportManifest,
}

impl MemoryDiscoverer {
    /// Construct a discoverer that loads the manifest from the default path.
    ///
    /// If the manifest does not exist or cannot be read, a fresh empty manifest
    /// is used (safe: the idempotency gate just returns false for all queries).
    #[must_use]
    pub fn new() -> Self {
        let path = ImportManifest::default_path();
        let manifest = ImportManifest::load_or_new(&path, env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| ImportManifest::new(env!("CARGO_PKG_VERSION")));
        Self { manifest }
    }

    /// Construct with a pre-built manifest (used in tests).
    #[must_use]
    pub fn with_manifest(manifest: ImportManifest) -> Self {
        Self { manifest }
    }
}

impl Default for MemoryDiscoverer {
    fn default() -> Self {
        Self::new()
    }
}

impl Discoverer for MemoryDiscoverer {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact> {
        let profile_dir = match source {
            ImportSource::ClaudeCode { profile_dir } => profile_dir,
            _ => return Vec::new(),
        };

        let projects_dir = profile_dir.join("projects");
        if !projects_dir.exists() {
            return Vec::new();
        }

        // Enumerate project directories. Each entry in projects/ is a project dir
        // identified by a hash/UUID. We collect all *.md files under their
        // `memory/` subdirectory.
        let mut results = Vec::new();

        let project_entries = match std::fs::read_dir(&projects_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        for project_entry in project_entries.flatten() {
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }

            let memory_dir = project_dir.join("memory");
            if !memory_dir.is_dir() {
                continue;
            }

            // Collect all .md files in this project's memory dir.
            let memory_entries = match std::fs::read_dir(&memory_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for mem_entry in memory_entries.flatten() {
                let path = mem_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }

                // Read bytes to compute hash; silently skip unreadable files.
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };

                let content_hash = sha256_hex(&bytes);

                // Idempotency gate: skip files already committed with the same hash.
                if self.manifest.is_already_committed(&path, &content_hash) {
                    continue;
                }

                // Detect frontmatter.
                let has_frontmatter = bytes.starts_with(b"---");

                let artifact = ImportArtifact::Memory {
                    path: path.clone(),
                    has_frontmatter,
                };

                let meta = ImportArtifactMeta {
                    source: source.clone(),
                    source_path: path,
                    content_hash,
                    discovered_at: SystemTime::now(),
                };

                results.push(DiscoveredArtifact { artifact, meta });
            }
        }

        results
    }
}

// ── MemoryTriager ────────────────────────────────────────────────────────────

/// Triages CC memory entries.
///
/// Keep rule: always Keep unless the file is empty or non-UTF8.
pub struct MemoryTriager;

impl Triager for MemoryTriager {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn triage(&self, artifact: &ImportArtifact, _meta: &ImportArtifactMeta) -> TriageDecision {
        let path = match artifact {
            ImportArtifact::Memory { path, .. } => path,
            _ => {
                return TriageDecision::Skip {
                    reason: "MemoryTriager received non-memory artifact".to_string(),
                }
            }
        };

        // Read the file to check emptiness and UTF-8 validity.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                return TriageDecision::Skip {
                    reason: format!("cannot read file: {e}"),
                }
            }
        };

        if bytes.is_empty() {
            return TriageDecision::Skip {
                reason: "empty file".to_string(),
            };
        }

        if std::str::from_utf8(&bytes).is_err() {
            return TriageDecision::Skip {
                reason: "non-UTF8 content".to_string(),
            };
        }

        TriageDecision::Keep
    }
}

// ── MemoryTranslator ─────────────────────────────────────────────────────────

/// Translates CC memory entries by stamping import metadata into frontmatter.
///
/// # Translation rules
///
/// - If the file already has YAML frontmatter (`---` ... `---`): the four stamp
///   fields (`imported_from`, `imported_at`, `source_path`, `content_hash`) are
///   INSERTED after the opening `---`, before any existing fields.  Existing
///   fields are preserved verbatim.
/// - If the file has no frontmatter: a complete frontmatter block is prepended.
/// - Body content is NEVER modified.
/// - `content_hash` is the SHA-256 of the ORIGINAL bytes (pre-stamp).
pub struct MemoryTranslator {
    /// Timestamp to use for all `imported_at` stamps in this run.
    /// Injected so tests can assert on a known value.
    pub imported_at: String,
}

impl MemoryTranslator {
    /// Construct with the current time as the `imported_at` timestamp.
    #[must_use]
    pub fn new() -> Self {
        Self {
            imported_at: crate::import::now_rfc3339(),
        }
    }

    /// Construct with a fixed timestamp (used in tests).
    #[must_use]
    pub fn with_timestamp(ts: impl Into<String>) -> Self {
        Self {
            imported_at: ts.into(),
        }
    }
}

impl Default for MemoryTranslator {
    fn default() -> Self {
        Self::new()
    }
}

impl Translator for MemoryTranslator {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn translate(
        &self,
        artifact: &ImportArtifact,
        meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String> {
        let (path, has_frontmatter) = match artifact {
            ImportArtifact::Memory {
                path,
                has_frontmatter,
            } => (path, *has_frontmatter),
            _ => return Err("MemoryTranslator received non-memory artifact".to_string()),
        };

        let source_text = std::str::from_utf8(source_bytes)
            .map_err(|e| format!("non-UTF8 source: {e}"))?;

        let source_path_str = meta.source_path.display().to_string();
        let content_hash = &meta.content_hash;
        let imported_at = &self.imported_at;

        let stamp_lines = format!(
            "imported_from: claude_code\nimported_at: {imported_at}\nsource_path: {source_path_str}\ncontent_hash: {content_hash}\n"
        );

        let translated = if has_frontmatter {
            // Insert stamp fields right after the opening `---`.
            // The existing frontmatter lines follow, then the closing `---`, then the body.
            let after_opener = source_text
                .strip_prefix("---\n")
                .or_else(|| source_text.strip_prefix("---\r\n"))
                .unwrap_or(source_text);
            format!("---\n{stamp_lines}{after_opener}")
        } else {
            // No frontmatter: prepend a complete block.
            format!("---\n{stamp_lines}---\n{source_text}")
        };

        // Compute destination filename (disambiguation happens in the Stager).
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "memory.md".to_string());

        let destination = anvil_config_home().join("memory").join(&filename);

        Ok(TranslationResult {
            bytes: translated.into_bytes(),
            suggested_name: filename,
            destination,
            warning: None,
        })
    }
}

// ── MemoryStager ─────────────────────────────────────────────────────────────

/// Stages translated memory entries.
///
/// Handles filename disambiguation: when two CC project directories produce the
/// same filename (e.g. `feedback-foo.md` exists in project A and project B),
/// the destination filename is prefixed with the first 6 chars of the SHA-256
/// of the source_path's parent directory name: `<proj-hash>-<filename>`.
///
/// Collision detection is performed across all artifacts staged in a single run
/// via the `seen_names` map.
pub struct MemoryStager {
    /// Tracks filenames seen so far in this run.
    /// Key: plain filename.  Value: source_path of the first artifact that
    /// claimed that name (used to disambiguate collisions).
    seen_names: std::sync::Mutex<HashMap<String, PathBuf>>,
}

impl MemoryStager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen_names: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MemoryStager {
    fn default() -> Self {
        Self::new()
    }
}

impl Stager for MemoryStager {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn stage(
        &self,
        artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String> {
        let source_path = artifact.source_path();
        let base_name = &translation.suggested_name;

        // Check for filename collision.
        let dest_name = {
            let mut seen = self.seen_names.lock().unwrap();
            if let Some(prior_path) = seen.get(base_name.as_str()) {
                if prior_path != source_path {
                    // Collision: disambiguate both this entry and the prior.
                    // For this entry, prefix with the first 6 chars of the
                    // SHA-256 of the project dir name.
                    let project_hash = project_id_prefix(source_path);
                    format!("{project_hash}-{base_name}")
                } else {
                    // Same source path re-staged — idempotent, use plain name.
                    base_name.clone()
                }
            } else {
                seen.insert(base_name.clone(), source_path.clone());
                base_name.clone()
            }
        };

        let relative = format!("memory/{dest_name}");
        let staged_path = staging
            .stage_bytes(&relative, &translation.bytes)
            .map_err(|e| e.to_string())?;

        // Final destination: ~/.anvil/memory/<dest_name>
        let destination = anvil_config_home().join("memory").join(&dest_name);

        Ok(StageAction {
            staged_path,
            destination,
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Derive a 6-character prefix from the project directory name.
///
/// Takes the parent-of-parent of `path` (the project dir id), computes
/// SHA-256 of its name as a string, and returns the first 6 hex chars.
fn project_id_prefix(path: &Path) -> String {
    // path = <profile>/projects/<id>/memory/<file>
    // parent(0) = <profile>/projects/<id>/memory
    // parent(1) = <profile>/projects/<id>
    let project_id = path
        .parent()           // memory/
        .and_then(|p| p.parent())  // <id>/
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let hash = sha256_hex(project_id.as_bytes());
    hash[..6.min(hash.len())].to_string()
}

/// Run the full memory import pipeline (discover → triage → translate → stage)
/// for a CC source.
///
/// Returns:
/// - `staged`: entries that were successfully staged (with their `ImportEntry`).
/// - `skipped`: entries that were skipped (with source path and reason).
///
/// Does NOT commit or write the manifest.  The caller owns the manifest write.
///
/// # Errors
///
/// Never returns `Err` — individual entry failures are captured in `skipped`.
pub fn run_memory_pipeline(
    source: &ImportSource,
    staging: &StagingDir,
    manifest: &ImportManifest,
) -> MemoryPipelineResult {
    let discoverer = MemoryDiscoverer::with_manifest(manifest.clone());
    let triager = MemoryTriager;
    let translator = MemoryTranslator::new();
    let stager = MemoryStager::new();

    let discovered = discoverer.discover(source);
    let mut result = MemoryPipelineResult::default();

    for da in discovered {
        let decision = triager.triage(&da.artifact, &da.meta);

        match decision {
            TriageDecision::Skip { reason } => {
                result.skipped.push(SkippedEntry {
                    source_path: da.meta.source_path.clone(),
                    reason,
                });
                continue;
            }
            TriageDecision::NeedsReview { reason } => {
                result.needs_review.push(NeedsReviewEntry {
                    source_path: da.meta.source_path.clone(),
                    reason,
                });
                // Still stage it.
            }
            TriageDecision::Keep => {}
        }

        // Read source bytes for translation.
        let bytes = match std::fs::read(&da.meta.source_path) {
            Ok(b) => b,
            Err(e) => {
                result.skipped.push(SkippedEntry {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("read error: {e}"),
                });
                continue;
            }
        };

        let translation = match translator.translate(&da.artifact, &da.meta, &bytes) {
            Ok(t) => t,
            Err(e) => {
                result.skipped.push(SkippedEntry {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("translate error: {e}"),
                });
                continue;
            }
        };

        let action = match stager.stage(&da.artifact, &translation, staging) {
            Ok(a) => a,
            Err(e) => {
                result.skipped.push(SkippedEntry {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("stage error: {e}"),
                });
                continue;
            }
        };

        let entry = ImportEntry {
            artifact: "memory".to_string(),
            source_path: da.meta.source_path.clone(),
            destination_path: action.destination.clone(),
            content_hash: da.meta.content_hash.clone(),
            imported_at: translator.imported_at.clone(),
            status: ImportEntryStatus::Staged,
            skip_reason: None,
            error: None,
        };

        result.staged.push(StagedEntry {
            entry,
            staged_path: action.staged_path,
        });
    }

    result
}

// ── Result types ─────────────────────────────────────────────────────────────

/// A successfully staged artifact.
#[derive(Debug)]
pub struct StagedEntry {
    /// Manifest entry (status: Staged).
    pub entry: ImportEntry,
    /// Absolute path within the staging directory.
    pub staged_path: PathBuf,
}

/// A skipped artifact.
#[derive(Debug)]
pub struct SkippedEntry {
    /// Source path of the skipped artifact.
    pub source_path: PathBuf,
    /// Human-readable reason.
    pub reason: String,
}

/// An artifact flagged for user review.
#[derive(Debug)]
pub struct NeedsReviewEntry {
    /// Source path of the flagged artifact.
    pub source_path: PathBuf,
    /// Human-readable reason.
    pub reason: String,
}

/// Output of `run_memory_pipeline`.
#[derive(Debug, Default)]
pub struct MemoryPipelineResult {
    /// Successfully staged entries.
    pub staged: Vec<StagedEntry>,
    /// Skipped entries with reasons.
    pub skipped: Vec<SkippedEntry>,
    /// Entries staged but flagged for review.
    pub needs_review: Vec<NeedsReviewEntry>,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Set ANVIL_CONFIG_HOME to `path` for the duration of the test.
    fn set_anvil_home(path: &Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
    }

    fn clear_anvil_home() {
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
    }

    /// Build a fake CC profile directory tree with `n_projects` projects, each
    /// containing `files_per_project` memory entries.
    fn build_cc_fixture(
        root: &Path,
        n_projects: usize,
        files_per_project: usize,
    ) -> PathBuf {
        let profile_dir = root.join(".claude");
        for i in 0..n_projects {
            let project_id = format!("project-{i:04}");
            let memory_dir = profile_dir
                .join("projects")
                .join(&project_id)
                .join("memory");
            std::fs::create_dir_all(&memory_dir).expect("create memory dir");

            for j in 0..files_per_project {
                let fname = format!("entry-{j:03}.md");
                let content = format!(
                    "---\nname: entry {j}\ntype: rule\n---\n# Rule {j}\n\nContent for rule {j} in project {i}.\n"
                );
                std::fs::write(memory_dir.join(&fname), content.as_bytes())
                    .expect("write memory file");
            }
        }
        profile_dir
    }

    // ── memory_discovery_finds_all_md_in_project_memory_dirs ─────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn memory_discovery_finds_all_md_in_project_memory_dirs() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // 3 projects × 5 files each = 15 memory entries expected.
        let profile_dir = build_cc_fixture(dir.path(), 3, 5);
        let source = ImportSource::ClaudeCode { profile_dir };

        let discoverer = MemoryDiscoverer::with_manifest(ImportManifest::new("test"));
        let discovered = discoverer.discover(&source);

        clear_anvil_home();
        assert_eq!(
            discovered.len(),
            15,
            "Expected 15 memory entries (3 projects × 5 files), found {}",
            discovered.len()
        );
    }

    // ── memory_translate_preserves_original_frontmatter_and_adds_stamps ──────

    #[test]
    fn memory_translate_preserves_original_frontmatter_and_adds_stamps() {
        let source_text = "---\nname: foo\ntype: rule\n---\n# Body\n\nSome content.\n";
        let source_bytes = source_text.as_bytes();
        let content_hash = sha256_hex(source_bytes);

        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("foo.md");
        std::fs::write(&path, source_bytes).expect("write fixture");

        let artifact = ImportArtifact::Memory {
            path: path.clone(),
            has_frontmatter: true,
        };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: PathBuf::from("/fake/.claude"),
            },
            source_path: path.clone(),
            content_hash: content_hash.clone(),
            discovered_at: SystemTime::now(),
        };

        let translator = MemoryTranslator::with_timestamp("2026-05-15T00:00:00Z");
        let result = translator
            .translate(&artifact, &meta, source_bytes)
            .expect("translate");

        let output = String::from_utf8(result.bytes).expect("valid utf8");

        // Original fields must be present.
        assert!(output.contains("name: foo"), "original 'name: foo' must be preserved");
        assert!(output.contains("type: rule"), "original 'type: rule' must be preserved");

        // Stamps must be present.
        assert!(output.contains("imported_from: claude_code"), "imported_from stamp missing");
        assert!(output.contains("imported_at: 2026-05-15T00:00:00Z"), "imported_at stamp missing");
        assert!(output.contains(&format!("content_hash: {content_hash}")), "content_hash stamp missing");
        assert!(output.contains("source_path:"), "source_path stamp missing");

        // Body must be present verbatim.
        assert!(output.contains("# Body\n\nSome content."), "body must be verbatim");

        // Stamps appear BEFORE the original fields in the frontmatter.
        let stamp_pos = output.find("imported_from:").expect("imported_from pos");
        let name_pos = output.find("name: foo").expect("name pos");
        assert!(
            stamp_pos < name_pos,
            "stamps should appear before original fields"
        );
    }

    #[test]
    fn memory_translate_no_frontmatter_prepends_full_block() {
        let source_text = "# Just a heading\n\nSome body text.\n";
        let source_bytes = source_text.as_bytes();
        let content_hash = sha256_hex(source_bytes);

        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("bare.md");
        std::fs::write(&path, source_bytes).expect("write fixture");

        let artifact = ImportArtifact::Memory {
            path: path.clone(),
            has_frontmatter: false,
        };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: PathBuf::from("/fake/.claude"),
            },
            source_path: path.clone(),
            content_hash: content_hash.clone(),
            discovered_at: SystemTime::now(),
        };

        let translator = MemoryTranslator::with_timestamp("2026-05-15T00:00:00Z");
        let result = translator
            .translate(&artifact, &meta, source_bytes)
            .expect("translate");

        let output = String::from_utf8(result.bytes).expect("valid utf8");

        assert!(output.starts_with("---\n"), "must start with frontmatter opener");
        assert!(output.contains("imported_from: claude_code"), "stamp missing");
        assert!(output.contains("---\n# Just a heading"), "body must follow frontmatter");
    }

    // ── memory_re_import_is_noop_when_content_unchanged ───────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn memory_re_import_is_noop_when_content_unchanged() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let profile_dir = build_cc_fixture(dir.path(), 1, 3);
        let source = ImportSource::ClaudeCode {
            profile_dir: profile_dir.clone(),
        };

        // First run: collect all discovered artifacts.
        let first_discoverer = MemoryDiscoverer::with_manifest(ImportManifest::new("test"));
        let first_run = first_discoverer.discover(&source);
        assert_eq!(first_run.len(), 3, "first run should find 3 entries");

        // Build a committed manifest with all three entries.
        let mut manifest = ImportManifest::new("test");
        for da in &first_run {
            let mut entry = ImportEntry::pending(
                "memory",
                da.meta.source_path.clone(),
                dir.path().join("memory").join(
                    da.meta.source_path.file_name().unwrap()
                ),
                da.meta.content_hash.clone(),
                "2026-05-15T00:00:00Z",
            );
            entry.status = ImportEntryStatus::Committed;
            manifest.push(entry);
        }

        // Second run with committed manifest: should discover 0 (idempotency gate).
        let second_discoverer = MemoryDiscoverer::with_manifest(manifest);
        let second_run = second_discoverer.discover(&source);

        clear_anvil_home();
        assert_eq!(
            second_run.len(),
            0,
            "second run on unchanged source should find 0 entries (idempotency)"
        );
    }

    // ── memory_re_import_re_stages_when_content_changed ───────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn memory_re_import_re_stages_when_content_changed() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let profile_dir = build_cc_fixture(dir.path(), 1, 1);
        let source = ImportSource::ClaudeCode {
            profile_dir: profile_dir.clone(),
        };

        // Find the single memory file.
        let discoverer = MemoryDiscoverer::with_manifest(ImportManifest::new("test"));
        let first_run = discoverer.discover(&source);
        assert_eq!(first_run.len(), 1);

        let original_path = first_run[0].meta.source_path.clone();
        let original_hash = first_run[0].meta.content_hash.clone();

        // Build a committed manifest.
        let mut manifest = ImportManifest::new("test");
        let mut entry = ImportEntry::pending(
            "memory",
            original_path.clone(),
            dir.path().join("memory").join(original_path.file_name().unwrap()),
            original_hash.clone(),
            "2026-05-15T00:00:00Z",
        );
        entry.status = ImportEntryStatus::Committed;
        manifest.push(entry);

        // Modify the source file.
        std::fs::write(&original_path, b"---\nname: modified\n---\nNew content.\n")
            .expect("modify source");

        // Second run should find 1 entry (content changed → different hash).
        let second_discoverer = MemoryDiscoverer::with_manifest(manifest);
        let second_run = second_discoverer.discover(&source);

        clear_anvil_home();
        assert_eq!(
            second_run.len(),
            1,
            "changed source should produce 1 re-discovered entry"
        );
        assert_ne!(
            second_run[0].meta.content_hash,
            original_hash,
            "hash should differ after content change"
        );
    }

    // ── Full pipeline test: end-to-end stage writes ───────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn memory_pipeline_stages_all_files() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        let profile_dir = build_cc_fixture(dir.path(), 2, 3);
        let source = ImportSource::ClaudeCode { profile_dir };

        let staging = StagingDir::create_clean().expect("staging");
        let manifest = ImportManifest::new("test");

        let result = run_memory_pipeline(&source, &staging, &manifest);

        clear_anvil_home();
        assert_eq!(
            result.staged.len(),
            6,
            "expected 6 staged entries (2 projects × 3 files), got {}",
            result.staged.len()
        );
        assert!(result.skipped.is_empty(), "no skips expected");
    }

    // ── Filename disambiguation test ──────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn memory_pipeline_disambiguates_colliding_filenames() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // Two projects, both with a file named "rule.md".
        let profile = dir.path().join(".claude");
        for proj in ["proj-aaa", "proj-bbb"] {
            let mem_dir = profile.join("projects").join(proj).join("memory");
            std::fs::create_dir_all(&mem_dir).expect("mkdir");
            std::fs::write(
                mem_dir.join("rule.md"),
                format!("# Rule in {proj}\n").as_bytes(),
            )
            .expect("write");
        }

        let source = ImportSource::ClaudeCode { profile_dir: profile };
        let staging = StagingDir::create_clean().expect("staging");
        let manifest = ImportManifest::new("test");

        let result = run_memory_pipeline(&source, &staging, &manifest);

        clear_anvil_home();

        // Both files should be staged (no skips).
        assert_eq!(result.staged.len(), 2, "both files must be staged");

        // Their destination names must differ.
        let dest_names: Vec<String> = result
            .staged
            .iter()
            .map(|e| {
                e.entry
                    .destination_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_ne!(
            dest_names[0], dest_names[1],
            "colliding filenames must be disambiguated: {dest_names:?}"
        );
    }
}
