// Allow `unsafe` only in test code (env::set_var).
#![cfg_attr(test, allow(unsafe_code))]

/// Instructions import — Phase 6.1b.
///
/// Concrete `Discoverer`, `Triager`, `Translator`, and `Stager` implementations
/// for CLAUDE.md instruction files:
///
///   `~/.claude/CLAUDE.md`           →  `~/.anvil/ANVIL.md` (global, append-on-conflict)
///   `<project>/CLAUDE.md`            →  `<project>/ANVIL.md` (per-project, skip-on-conflict)
///
/// # Discovery roots
///
/// - Global: `<profile_dir>/CLAUDE.md` — always in scope.
/// - Per-project: filesystem walk anchored at `~/projects/`, depth ≤ 5.
///   A CLAUDE.md is in scope iff its parent directory contains a `.git/` dir.
///   Paths containing `node_modules` anywhere in the components are excluded.
///
/// # Conflict handling
///
/// - Global: if `~/.anvil/ANVIL.md` already exists at commit time, the content
///   is APPENDed under `## Imported from Claude Code (YYYY-MM-DD)`. This is
///   done in the `Stager` — the `Translator` always produces the comment-stamped
///   content and the `Stager` decides whether to write or append.
///
/// - Per-project: if `<project>/ANVIL.md` already exists, stage as
///   `<staging>/instructions/<proj-hash>/ANVIL.imported.md` and flag the
///   manifest entry with `status: NeedsReview` and a `skip_reason`.
///
/// # Read-only on `~/.claude/`
///
/// The import pipeline NEVER modifies the source CC installation.

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

/// Maximum directory depth for the per-project CLAUDE.md walk.
const MAX_WALK_DEPTH: usize = 5;

// ── InstructionsDiscoverer ──────────────────────────────────────────────────

/// Discovers CLAUDE.md files — global (from the CC profile dir) and per-project
/// (by walking `~/projects/` and known git repos).
pub struct InstructionsDiscoverer {
    /// Pre-loaded manifest for idempotency gating.
    manifest: ImportManifest,
    /// Optional override for the projects walk root (used in tests).
    projects_root_override: Option<PathBuf>,
}

impl InstructionsDiscoverer {
    /// Construct with the default manifest path and `~/projects/` walk root.
    #[must_use]
    pub fn new() -> Self {
        let path = ImportManifest::default_path();
        let manifest = ImportManifest::load_or_new(&path, env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| ImportManifest::new(env!("CARGO_PKG_VERSION")));
        Self {
            manifest,
            projects_root_override: None,
        }
    }

    /// Construct with a pre-built manifest and optional walk root override.
    #[must_use]
    pub fn with_manifest_and_root(
        manifest: ImportManifest,
        projects_root: Option<PathBuf>,
    ) -> Self {
        Self {
            manifest,
            projects_root_override: projects_root,
        }
    }

    /// Return the walk root for per-project discovery.
    fn projects_root(&self) -> PathBuf {
        if let Some(ref r) = self.projects_root_override {
            return r.clone();
        }
        dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("projects")
    }

    /// Walk `root` up to `MAX_WALK_DEPTH` levels, collecting CLAUDE.md files
    /// whose parent directory contains a `.git/` subdirectory.
    /// Excludes any path component equal to `node_modules`.
    fn walk_for_project_claude_mds(&self, root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        self.walk_dir(root, 0, &mut found);
        found
    }

    fn walk_dir(&self, dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if depth > MAX_WALK_DEPTH {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip node_modules at any depth.
            if path.file_name().map_or(false, |n| n == "node_modules") {
                continue;
            }

            // Skip hidden directories (except .git checks done explicitly below).
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with('.'))
            {
                continue;
            }

            if path.is_dir() {
                // Check for CLAUDE.md directly inside this directory,
                // but only if the directory is a git root.
                let claude_md = path.join("CLAUDE.md");
                if claude_md.is_file() && path.join(".git").exists() {
                    found.push(claude_md);
                }
                // Recurse (depth + 1).
                self.walk_dir(&path, depth + 1, found);
            }
        }
    }
}

impl Default for InstructionsDiscoverer {
    fn default() -> Self {
        Self::new()
    }
}

impl Discoverer for InstructionsDiscoverer {
    fn name(&self) -> &'static str {
        "instructions"
    }

    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact> {
        let profile_dir = match source {
            ImportSource::ClaudeCode { profile_dir } => profile_dir,
            _ => return Vec::new(),
        };

        let mut results = Vec::new();

        // ── 1. Global CLAUDE.md ──────────────────────────────────────────────
        let global_path = profile_dir.join("CLAUDE.md");
        if global_path.is_file() {
            if let Ok(bytes) = std::fs::read(&global_path) {
                let content_hash = sha256_hex(&bytes);
                if !self.manifest.is_already_committed(&global_path, &content_hash) {
                    let artifact = ImportArtifact::Instructions {
                        path: global_path.clone(),
                        is_global: true,
                        project_path: None,
                    };
                    let meta = ImportArtifactMeta {
                        source: source.clone(),
                        source_path: global_path,
                        content_hash,
                        discovered_at: SystemTime::now(),
                    };
                    results.push(DiscoveredArtifact { artifact, meta });
                }
            }
        }

        // ── 2. Per-project CLAUDE.md files ───────────────────────────────────
        let projects_root = self.projects_root();
        if projects_root.is_dir() {
            let per_project = self.walk_for_project_claude_mds(&projects_root);
            for path in per_project {
                let project_path = path.parent().map(PathBuf::from);
                if let Ok(bytes) = std::fs::read(&path) {
                    let content_hash = sha256_hex(&bytes);
                    if self.manifest.is_already_committed(&path, &content_hash) {
                        continue;
                    }
                    let artifact = ImportArtifact::Instructions {
                        path: path.clone(),
                        is_global: false,
                        project_path: project_path.clone(),
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
        }

        results
    }
}

// ── InstructionsTriager ──────────────────────────────────────────────────────

/// Triages instruction files.
///
/// - Global CLAUDE.md: always Keep.
/// - Per-project CLAUDE.md: Keep if the parent has a `.git/` directory.
///   Skip if empty or non-UTF8.
pub struct InstructionsTriager;

impl Triager for InstructionsTriager {
    fn name(&self) -> &'static str {
        "instructions"
    }

    fn triage(&self, artifact: &ImportArtifact, _meta: &ImportArtifactMeta) -> TriageDecision {
        let (path, is_global) = match artifact {
            ImportArtifact::Instructions {
                path, is_global, ..
            } => (path, *is_global),
            _ => {
                return TriageDecision::Skip {
                    reason: "InstructionsTriager received non-instructions artifact".to_string(),
                }
            }
        };

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

        if !is_global {
            // Per-project: require .git/ in the parent directory.
            if let Some(parent) = path.parent() {
                if !parent.join(".git").exists() {
                    return TriageDecision::Skip {
                        reason: "parent directory is not a git repository".to_string(),
                    };
                }
            }
        }

        TriageDecision::Keep
    }
}

// ── InstructionsTranslator ───────────────────────────────────────────────────

/// Translates CLAUDE.md → ANVIL.md content.
///
/// Prepends four HTML comment lines at the very top of the content:
///
/// ```markdown
/// <!-- imported_from: claude_code -->
/// <!-- imported_at: <timestamp> -->
/// <!-- source_path: <absolute_path> -->
/// <!-- content_hash: <sha256> -->
/// ```
///
/// The body is copied verbatim — no content rewrite.
/// (Day-2 cleanup is `anvil memory clean`, per the spec's §10 contract.)
pub struct InstructionsTranslator {
    /// Timestamp injected at construction; shared across all calls in a run.
    pub imported_at: String,
}

impl InstructionsTranslator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            imported_at: crate::import::now_rfc3339(),
        }
    }

    #[must_use]
    pub fn with_timestamp(ts: impl Into<String>) -> Self {
        Self {
            imported_at: ts.into(),
        }
    }
}

impl Default for InstructionsTranslator {
    fn default() -> Self {
        Self::new()
    }
}

impl Translator for InstructionsTranslator {
    fn name(&self) -> &'static str {
        "instructions"
    }

    fn translate(
        &self,
        artifact: &ImportArtifact,
        meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String> {
        let (path, is_global, project_path) = match artifact {
            ImportArtifact::Instructions {
                path,
                is_global,
                project_path,
            } => (path, *is_global, project_path),
            _ => {
                return Err(
                    "InstructionsTranslator received non-instructions artifact".to_string(),
                )
            }
        };

        let source_text = std::str::from_utf8(source_bytes)
            .map_err(|e| format!("non-UTF8 content: {e}"))?;

        let source_path_str = meta.source_path.display().to_string();
        let content_hash = &meta.content_hash;
        let imported_at = &self.imported_at;

        let header = format!(
            "<!-- imported_from: claude_code -->\n\
             <!-- imported_at: {imported_at} -->\n\
             <!-- source_path: {source_path_str} -->\n\
             <!-- content_hash: {content_hash} -->\n\n"
        );

        let translated = format!("{header}{source_text}");

        // Compute the staged destination path and final destination.
        let (_suggested_name, staging_subdir, destination) = if is_global {
            // Global: stages to instructions/global/ANVIL.md
            let dest = anvil_config_home().join("ANVIL.md");
            ("ANVIL.md".to_string(), "instructions/global".to_string(), dest)
        } else {
            // Per-project: stages to instructions/<proj-hash>/ANVIL.md
            let proj_hash = project_path
                .as_ref()
                .map(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let h = sha256_hex(name.as_bytes());
                    h[..8.min(h.len())].to_string()
                })
                .unwrap_or_else(|| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let h = sha256_hex(name.as_bytes());
                    h[..8.min(h.len())].to_string()
                });
            let dest = project_path
                .as_ref()
                .map(|p| p.join("ANVIL.md"))
                .unwrap_or_else(|| anvil_config_home().join("ANVIL.md"));
            (
                "ANVIL.md".to_string(),
                format!("instructions/{proj_hash}"),
                dest,
            )
        };

        // The staging_subdir is encoded in the suggested_name with a slash prefix
        // so the Stager knows where to write.  We embed the subdir in the
        // `suggested_name` field as `<subdir>/ANVIL.md`.
        let staging_relative = format!("{staging_subdir}/ANVIL.md");

        Ok(TranslationResult {
            bytes: translated.into_bytes(),
            suggested_name: staging_relative,
            destination,
            warning: None,
        })
    }
}

// ── InstructionsStager ───────────────────────────────────────────────────────

/// Stages translated CLAUDE.md content.
///
/// # Global conflict
///
/// If `~/.anvil/ANVIL.md` already exists at COMMIT time, the commit step
/// should append. The Stager writes the translated content to
/// `<staging>/instructions/global/ANVIL.md` unconditionally; the commit
/// orchestrator in `handle_import_command` handles append vs. clobber.
///
/// For the staging phase specifically: the stager ALWAYS writes to staging
/// (never to the live `~/.anvil/`). The append logic fires at commit time.
///
/// # Per-project conflict
///
/// If `<project>/ANVIL.md` already exists, the stager writes to
/// `<staging>/instructions/<proj-hash>/ANVIL.imported.md` (not `ANVIL.md`).
/// The caller marks the manifest entry `NeedsReview`.
pub struct InstructionsStager;

impl Stager for InstructionsStager {
    fn name(&self) -> &'static str {
        "instructions"
    }

    fn stage(
        &self,
        artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String> {
        let (is_global, _project_path) = match artifact {
            ImportArtifact::Instructions {
                is_global,
                project_path,
                ..
            } => (*is_global, project_path),
            _ => {
                return Err(
                    "InstructionsStager received non-instructions artifact".to_string(),
                )
            }
        };

        // `translation.suggested_name` encodes the staging relative path
        // (e.g. "instructions/global/ANVIL.md" or "instructions/<hash>/ANVIL.md").
        let staging_relative = &translation.suggested_name;

        if is_global {
            // Always write to staging. Append vs. clobber decision is deferred
            // to commit time by the orchestrator.
            let staged_path = staging
                .stage_bytes(staging_relative, &translation.bytes)
                .map_err(|e| e.to_string())?;

            return Ok(StageAction {
                staged_path,
                destination: translation.destination.clone(),
            });
        }

        // Per-project: check for existing ANVIL.md at the final destination.
        let dest = &translation.destination;
        if dest.exists() {
            // Conflict: stage as ANVIL.imported.md.
            let imported_relative = staging_relative.replace("ANVIL.md", "ANVIL.imported.md");
            let staged_path = staging
                .stage_bytes(&imported_relative, &translation.bytes)
                .map_err(|e| e.to_string())?;

            // Destination is also renamed.
            let imported_dest = dest.with_file_name("ANVIL.imported.md");

            return Ok(StageAction {
                staged_path,
                destination: imported_dest,
            });
        }

        // No conflict: stage normally.
        let staged_path = staging
            .stage_bytes(staging_relative, &translation.bytes)
            .map_err(|e| e.to_string())?;

        Ok(StageAction {
            staged_path,
            destination: dest.clone(),
        })
    }
}

// ── Pipeline result types ─────────────────────────────────────────────────────

/// Whether an instruction file requires review (conflict) or is clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionConflict {
    /// No conflict — will land at `destination`.
    Clean,
    /// Conflict — staged as `ANVIL.imported.md`; user must merge manually.
    NeedsReview { reason: String },
}

/// A staged instruction entry.
#[derive(Debug)]
pub struct StagedInstruction {
    /// Manifest entry (Staged or NeedsReview).
    pub entry: ImportEntry,
    /// Absolute path within the staging directory.
    pub staged_path: PathBuf,
    /// Conflict status.
    pub conflict: InstructionConflict,
}

/// A skipped instruction entry.
#[derive(Debug)]
pub struct SkippedInstruction {
    pub source_path: PathBuf,
    pub reason: String,
}

/// Output of `run_instructions_pipeline`.
#[derive(Debug, Default)]
pub struct InstructionsPipelineResult {
    pub staged: Vec<StagedInstruction>,
    pub skipped: Vec<SkippedInstruction>,
}

// ── run_instructions_pipeline ─────────────────────────────────────────────────

/// Run the full instructions import pipeline (discover → triage → translate → stage).
///
/// Does NOT commit or write the manifest.
pub fn run_instructions_pipeline(
    source: &ImportSource,
    staging: &StagingDir,
    manifest: &ImportManifest,
    projects_root: Option<PathBuf>,
) -> InstructionsPipelineResult {
    let discoverer =
        InstructionsDiscoverer::with_manifest_and_root(manifest.clone(), projects_root);
    let triager = InstructionsTriager;
    let translator = InstructionsTranslator::new();
    let stager = InstructionsStager;

    let discovered = discoverer.discover(source);
    let mut result = InstructionsPipelineResult::default();

    for da in discovered {
        let decision = triager.triage(&da.artifact, &da.meta);

        match decision {
            TriageDecision::Skip { reason } => {
                result.skipped.push(SkippedInstruction {
                    source_path: da.meta.source_path.clone(),
                    reason,
                });
                continue;
            }
            TriageDecision::NeedsReview { .. } | TriageDecision::Keep => {}
        }

        let bytes = match std::fs::read(&da.meta.source_path) {
            Ok(b) => b,
            Err(e) => {
                result.skipped.push(SkippedInstruction {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("read error: {e}"),
                });
                continue;
            }
        };

        let translation = match translator.translate(&da.artifact, &da.meta, &bytes) {
            Ok(t) => t,
            Err(e) => {
                result.skipped.push(SkippedInstruction {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("translate error: {e}"),
                });
                continue;
            }
        };

        let action = match stager.stage(&da.artifact, &translation, staging) {
            Ok(a) => a,
            Err(e) => {
                result.skipped.push(SkippedInstruction {
                    source_path: da.meta.source_path.clone(),
                    reason: format!("stage error: {e}"),
                });
                continue;
            }
        };

        // Detect conflict from the staged filename.
        let conflict = if action
            .staged_path
            .file_name()
            .map_or(false, |n| n.to_string_lossy().contains("ANVIL.imported"))
        {
            let proj_display = action.destination.parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            InstructionConflict::NeedsReview {
                reason: format!("ANVIL.md already exists at {proj_display}"),
            }
        } else {
            InstructionConflict::Clean
        };

        let status = match &conflict {
            InstructionConflict::NeedsReview { .. } => ImportEntryStatus::Staged,
            InstructionConflict::Clean => ImportEntryStatus::Staged,
        };

        let skip_reason = match &conflict {
            InstructionConflict::NeedsReview { reason } => Some(reason.clone()),
            InstructionConflict::Clean => None,
        };

        let entry = ImportEntry {
            artifact: "instructions".to_string(),
            source_path: da.meta.source_path.clone(),
            destination_path: action.destination.clone(),
            content_hash: da.meta.content_hash.clone(),
            imported_at: translator.imported_at.clone(),
            status,
            skip_reason,
            error: None,
        };

        result.staged.push(StagedInstruction {
            entry,
            staged_path: action.staged_path,
            conflict,
        });
    }

    result
}

// ── Append helper (used at commit time for global ANVIL.md) ───────────────────

/// Append `new_content` to an existing ANVIL.md at `dest_path` under a heading.
///
/// Called by the commit orchestrator when `~/.anvil/ANVIL.md` already exists.
/// Writes atomically via a temp file.
///
/// # Errors
///
/// Returns an error string on I/O failure.
pub fn append_to_existing_anvil_md(dest_path: &Path, new_content: &str, date: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(dest_path)
        .map_err(|e| format!("read existing ANVIL.md: {e}"))?;

    let heading = format!("\n\n## Imported from Claude Code ({date})\n\n");
    let appended = format!("{existing}{heading}{new_content}");

    let tmp = dest_path.with_extension("tmp");
    std::fs::write(&tmp, appended.as_bytes())
        .map_err(|e| format!("write tmp ANVIL.md: {e}"))?;
    std::fs::rename(&tmp, dest_path)
        .map_err(|e| format!("rename tmp to ANVIL.md: {e}"))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn set_anvil_home(path: &Path) {
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", path) };
    }

    fn clear_anvil_home() {
        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };
    }

    // ── instructions_global_appends_to_existing_anvil_md ─────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn instructions_global_appends_to_existing_anvil_md() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // Pre-existing ANVIL.md.
        let anvil_home = dir.path().to_path_buf();
        std::fs::create_dir_all(&anvil_home).expect("mkdir anvil home");
        let existing_anvil = anvil_home.join("ANVIL.md");
        std::fs::write(&existing_anvil, b"# Existing Anvil Instructions\n\nSome rules.\n")
            .expect("write existing ANVIL.md");

        // Run append.
        let new_content = "## From CC\n\nImported content.\n";
        append_to_existing_anvil_md(&existing_anvil, new_content, "2026-05-15")
            .expect("append");

        let result = std::fs::read_to_string(&existing_anvil).expect("read back");
        clear_anvil_home();

        assert!(
            result.contains("# Existing Anvil Instructions"),
            "existing content must be preserved"
        );
        assert!(
            result.contains("## Imported from Claude Code (2026-05-15)"),
            "import heading must be appended"
        );
        assert!(
            result.contains("Imported content."),
            "new content must be appended"
        );
        // Original content must come before appended content.
        let orig_pos = result.find("Existing Anvil").expect("find original");
        let new_pos = result.find("Imported content.").expect("find new");
        assert!(orig_pos < new_pos, "original must precede appended content");
    }

    // ── instructions_per_project_with_existing_anvil_md_stages_as_imported_md ─

    #[test]
    #[serial(anvil_config_home)]
    fn instructions_per_project_with_existing_anvil_md_stages_as_imported_md() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // Create a project with both CLAUDE.md and an existing ANVIL.md.
        let project_dir = dir.path().join("my-project");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");

        // Make it a git root.
        std::fs::create_dir_all(project_dir.join(".git")).expect("mkdir .git");

        // Write CLAUDE.md.
        let claude_md = project_dir.join("CLAUDE.md");
        std::fs::write(&claude_md, b"# Project Instructions\n\nDo things.\n")
            .expect("write CLAUDE.md");

        // Write existing ANVIL.md to simulate a conflict.
        let anvil_md = project_dir.join("ANVIL.md");
        std::fs::write(&anvil_md, b"# Existing ANVIL\n").expect("write existing ANVIL.md");

        // Build CC profile pointing to a fake global CLAUDE.md only.
        let profile_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&profile_dir).expect("mkdir profile");

        let source = ImportSource::ClaudeCode {
            profile_dir: profile_dir.clone(),
        };

        let staging = StagingDir::create_clean().expect("staging");
        let manifest = ImportManifest::new("test");

        // Use projects_root pointing to the dir containing our project.
        let projects_root = dir.path().to_path_buf();
        let result =
            run_instructions_pipeline(&source, &staging, &manifest, Some(projects_root));

        clear_anvil_home();

        // The per-project file should be staged.
        assert_eq!(
            result.staged.len(),
            1,
            "expected 1 staged instruction (CLAUDE.md from project)"
        );

        let staged = &result.staged[0];

        // It should be in NeedsReview conflict state.
        assert!(
            matches!(staged.conflict, InstructionConflict::NeedsReview { .. }),
            "expected NeedsReview conflict, got {:?}",
            staged.conflict
        );

        // The staged file should be ANVIL.imported.md.
        let staged_name = staged
            .staged_path
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert_eq!(
            staged_name, "ANVIL.imported.md",
            "conflicting file must be staged as ANVIL.imported.md"
        );

        // The manifest entry should have a skip_reason.
        assert!(
            staged.entry.skip_reason.is_some(),
            "manifest entry must have skip_reason for NeedsReview"
        );
    }

    // ── instructions_per_project_skips_node_modules ───────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn instructions_per_project_skips_node_modules() {
        let dir = TempDir::new().expect("tmpdir");
        set_anvil_home(dir.path());

        // A real project.
        let real_project = dir.path().join("real-project");
        std::fs::create_dir_all(&real_project).expect("mkdir real project");
        std::fs::create_dir_all(real_project.join(".git")).expect("mkdir .git");
        std::fs::write(
            real_project.join("CLAUDE.md"),
            b"# Real project instructions\n",
        )
        .expect("write CLAUDE.md");

        // A CLAUDE.md inside node_modules (must be excluded).
        let node_mod_pkg = dir.path().join("real-project").join("node_modules").join("some-pkg");
        std::fs::create_dir_all(&node_mod_pkg).expect("mkdir node_modules/some-pkg");
        std::fs::write(
            node_mod_pkg.join("CLAUDE.md"),
            b"# Should be excluded\n",
        )
        .expect("write node_modules CLAUDE.md");

        let profile_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&profile_dir).expect("mkdir profile");

        let source = ImportSource::ClaudeCode {
            profile_dir: profile_dir.clone(),
        };

        let staging = StagingDir::create_clean().expect("staging");
        let manifest = ImportManifest::new("test");

        // Walk from the dir that contains both the real project and the node_modules path.
        let projects_root = dir.path().to_path_buf();
        let result =
            run_instructions_pipeline(&source, &staging, &manifest, Some(projects_root));

        clear_anvil_home();

        // Only the real project's CLAUDE.md should be staged.
        assert_eq!(
            result.staged.len(),
            1,
            "only 1 CLAUDE.md should be staged (node_modules excluded), got {}",
            result.staged.len()
        );

        // Verify the staged file is not the node_modules one.
        let staged_source = &result.staged[0].entry.source_path;
        assert!(
            !staged_source
                .components()
                .any(|c| c.as_os_str() == "node_modules"),
            "staged file must not be from node_modules: {staged_source:?}"
        );
    }

    // ── HTML comment stamps are present ──────────────────────────────────────

    #[test]
    fn instructions_translator_adds_comment_stamps() {
        let source_text = "# My CLAUDE.md\n\nSome instructions.\n";
        let source_bytes = source_text.as_bytes();
        let content_hash = sha256_hex(source_bytes);

        let tmp = TempDir::new().expect("tmpdir");
        let path = tmp.path().join("CLAUDE.md");
        std::fs::write(&path, source_bytes).expect("write");

        let artifact = ImportArtifact::Instructions {
            path: path.clone(),
            is_global: true,
            project_path: None,
        };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: PathBuf::from("/fake/.claude"),
            },
            source_path: path.clone(),
            content_hash: content_hash.clone(),
            discovered_at: SystemTime::now(),
        };

        let translator = InstructionsTranslator::with_timestamp("2026-05-15T00:00:00Z");
        let result = translator
            .translate(&artifact, &meta, source_bytes)
            .expect("translate");

        let output = String::from_utf8(result.bytes).expect("valid utf8");

        assert!(output.contains("<!-- imported_from: claude_code -->"), "imported_from comment missing");
        assert!(output.contains("<!-- imported_at: 2026-05-15T00:00:00Z -->"), "imported_at comment missing");
        assert!(output.contains(&format!("<!-- content_hash: {content_hash} -->")), "content_hash comment missing");
        assert!(output.contains("<!-- source_path:"), "source_path comment missing");
        assert!(output.contains("# My CLAUDE.md"), "original body must be present");
    }
}
