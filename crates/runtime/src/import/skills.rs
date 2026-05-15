// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// Skills import — Phase 6.2b.
///
/// Discovers and stages CC skills from:
/// - `~/.claude/skills/`  — user-authored skills
/// - `~/.claude/plugins/marketplaces/<m>/skills/` — marketplace-installed
///
/// # Imported skills land DISABLED
///
/// Anvil's security model requires explicit opt-in.  Every imported skill
/// has `enabled: false` in its staged metadata.  The user enables via
/// `/plugin enable <name>` or equivalent.  This is by design — bash-script
/// hooks from CC have not been reviewed against Anvil's sandbox model.
///
/// # Skill format compatibility
///
/// CC skills are directories containing a `SKILL.md` file with YAML
/// frontmatter (at minimum a `name:` field).  Anvil's skill format is
/// largely compatible.  The importer:
///   1. Validates presence of `SKILL.md` and a `name:` field.
///   2. Injects `imported_from: claude_code` and `imported_at: <RFC3339>`
///      into the frontmatter.
///   3. Strips the phrase "you are Claude Code" (case-insensitive) from
///      the body — Phase 6.5 will do a more thorough identity-rewrite pass.
///   4. Copies the entire skill directory to `<staging>/skills/<skill-name>/`.
///
/// # Name collision handling
///
/// If an Anvil skill with the same name already exists in `~/.anvil/skills/`,
/// the imported skill is staged as `<name>.imported/` and the manifest entry
/// is set to `NeedsReview`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::import::artifact::{ImportArtifact, ImportArtifactMeta, ImportSource};
use crate::import::discover::{DiscoveredArtifact, Discoverer};
use crate::import::stage::{StageAction, Stager};
use crate::import::staging::StagingDir;
use crate::import::translate::{TranslationResult, Translator};
use crate::import::triage::{TriageDecision, Triager};
use crate::import::{now_rfc3339, sha256_file};

// ── Skill frontmatter helpers ─────────────────────────────────────────────────

/// Parse YAML frontmatter from a SKILL.md string.
///
/// Returns `(frontmatter_text, body_text)` where `frontmatter_text` is the
/// raw YAML between `---` delimiters (without the delimiters), and
/// `body_text` is everything after.
///
/// Returns `None` if the file has no frontmatter.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.trim_start_matches('\n').trim_start_matches('\r');
    if !content.starts_with("---") {
        return None;
    }
    // Find the closing `---`
    let after_open = &content[3..];
    // Skip the newline immediately after `---`
    let after_open = after_open.trim_start_matches('\n').trim_start_matches('\r');
    // Find end of frontmatter block
    if let Some(end_pos) = after_open.find("\n---") {
        let fm = &after_open[..end_pos];
        let body = &after_open[end_pos + 4..]; // skip \n---
        let body = body.trim_start_matches('\n').trim_start_matches('\r');
        Some((fm, body))
    } else {
        None
    }
}

/// Extract the `name:` field from a YAML frontmatter string.
///
/// Handles `name: value` (with optional quotes) on its own line.
/// Returns `None` if not found.
fn extract_name_from_frontmatter(frontmatter: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            let name = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Strip "you are Claude Code" phrases from the body (light pass).
///
/// Phase 6.5 will do a more thorough LLM-powered rewrite; this is a
/// conservative regex-free strip of the most common form.
fn strip_cc_identity_phrase(body: &str) -> (String, bool) {
    // Build a lowercase index to find occurrences without losing case on surroundings
    let lower = body.to_lowercase();
    let needle = "you are claude code";
    if !lower.contains(needle) {
        return (body.to_string(), false);
    }

    // Simple line-by-line pass: drop lines that contain the phrase
    let mut out_lines: Vec<&str> = Vec::new();
    let mut stripped = false;
    for line in body.lines() {
        if line.to_lowercase().contains(needle) {
            stripped = true;
        } else {
            out_lines.push(line);
        }
    }
    (out_lines.join("\n"), stripped)
}

/// Inject `imported_from: claude_code` and `imported_at: <ts>` into YAML
/// frontmatter, and optionally strip the CC identity phrase from the body.
///
/// Returns the transformed SKILL.md content.
fn stamp_skill_content(content: &str, imported_at: &str) -> (String, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();

    if let Some((fm, body)) = split_frontmatter(content) {
        // Inject stamps at the start of frontmatter
        let new_fm = format!(
            "{fm}\nimported_from: claude_code\nimported_at: \"{imported_at}\""
        );

        let (clean_body, stripped) = strip_cc_identity_phrase(body);
        if stripped {
            warnings.push(
                "Stripped 'you are Claude Code' identity phrase from skill body. \
                 Run `anvil memory clean` for a thorough identity rewrite."
                    .to_string(),
            );
        }

        let result = format!("---\n{new_fm}\n---\n\n{clean_body}");
        (result, warnings)
    } else {
        // No frontmatter — prepend a minimal one
        let (clean_body, stripped) = strip_cc_identity_phrase(content);
        if stripped {
            warnings.push(
                "Stripped 'you are Claude Code' identity phrase from skill body. \
                 Run `anvil memory clean` for a thorough identity rewrite."
                    .to_string(),
            );
        }
        let result = format!(
            "---\nimported_from: claude_code\nimported_at: \"{imported_at}\"\nenabled: false\n---\n\n{clean_body}"
        );
        (result, warnings)
    }
}

// ── Discoverer ───────────────────────────────────────────────────────────────

/// Discovers CC skills from user-authored and marketplace directories.
pub struct SkillsDiscoverer;

impl Discoverer for SkillsDiscoverer {
    fn discover(&self, source: &ImportSource) -> Vec<DiscoveredArtifact> {
        let profile_dir = match source {
            ImportSource::ClaudeCode { profile_dir } => profile_dir.clone(),
            _ => return vec![],
        };

        let mut results = Vec::new();

        // 1. User-authored skills: ~/.claude/skills/<skill-name>/SKILL.md
        let user_skills_dir = profile_dir.join("skills");
        if user_skills_dir.is_dir() {
            results.extend(discover_skills_in_dir(&user_skills_dir, source));
        }

        // 2. Marketplace skills: ~/.claude/plugins/marketplaces/<m>/skills/<skill-name>/SKILL.md
        let marketplaces_dir = profile_dir.join("plugins").join("marketplaces");
        if marketplaces_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&marketplaces_dir) {
                for entry in entries.flatten() {
                    let marketplace_dir = entry.path();
                    if !marketplace_dir.is_dir() {
                        continue;
                    }
                    let skills_subdir = marketplace_dir.join("skills");
                    if skills_subdir.is_dir() {
                        results.extend(discover_skills_in_dir(&skills_subdir, source));
                    }
                }
            }
        }

        results
    }

    fn name(&self) -> &'static str {
        "skills-discoverer"
    }
}

/// Walk `skills_dir` and return one `DiscoveredArtifact` per valid skill directory.
fn discover_skills_in_dir(skills_dir: &Path, source: &ImportSource) -> Vec<DiscoveredArtifact> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let skill_path = entry.path();
        if !skill_path.is_dir() {
            // Also handle flat SKILL.md files at the skills root (some CC versions)
            if skill_path.extension().and_then(|e| e.to_str()) == Some("md") {
                let hash = sha256_file(&skill_path).unwrap_or_default();
                results.push(DiscoveredArtifact {
                    artifact: ImportArtifact::Skill {
                        path: skill_path.clone(),
                    },
                    meta: ImportArtifactMeta {
                        source: source.clone(),
                        source_path: skill_path,
                        content_hash: hash,
                        discovered_at: SystemTime::now(),
                    },
                });
            }
            continue;
        }

        let skill_md = skill_path.join("SKILL.md");
        if !skill_md.exists() {
            // Skip directories without SKILL.md — not valid skill dirs
            continue;
        }

        let hash = sha256_file(&skill_md).unwrap_or_default();
        results.push(DiscoveredArtifact {
            artifact: ImportArtifact::Skill {
                path: skill_md.clone(),
            },
            meta: ImportArtifactMeta {
                source: source.clone(),
                source_path: skill_md,
                content_hash: hash,
                discovered_at: SystemTime::now(),
            },
        });
    }

    results
}

// ── Triager ───────────────────────────────────────────────────────────────────

/// Triages skill artifacts.
///
/// Skip rules:
/// - `SKILL.md` is missing or unreadable → Skip
/// - `SKILL.md` has no frontmatter or no `name:` field → Skip (malformed)
/// - All other skills → Keep
pub struct SkillsTriager;

impl Triager for SkillsTriager {
    fn triage(&self, artifact: &ImportArtifact, _meta: &ImportArtifactMeta) -> TriageDecision {
        let path = match artifact {
            ImportArtifact::Skill { path } => path,
            _ => {
                return TriageDecision::Skip {
                    reason: "SkillsTriager only handles Skill artifacts".to_string(),
                };
            }
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return TriageDecision::Skip {
                    reason: format!("cannot read SKILL.md: {e}"),
                };
            }
        };

        // Validate frontmatter + name field
        match split_frontmatter(&content) {
            None => TriageDecision::Skip {
                reason: "SKILL.md has no YAML frontmatter".to_string(),
            },
            Some((fm, _body)) => {
                if extract_name_from_frontmatter(fm).is_none() {
                    TriageDecision::Skip {
                        reason: "SKILL.md frontmatter missing `name:` field".to_string(),
                    }
                } else {
                    TriageDecision::Keep
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "skills-triager"
    }
}

// ── Translator ────────────────────────────────────────────────────────────────

/// Translates a CC skill's `SKILL.md` content:
/// 1. Injects `imported_from: claude_code` frontmatter stamps.
/// 2. Injects `enabled: false` (skills land disabled by default).
/// 3. Strips "you are Claude Code" identity phrase.
pub struct SkillsTranslator;

impl Translator for SkillsTranslator {
    fn translate(
        &self,
        artifact: &ImportArtifact,
        _meta: &ImportArtifactMeta,
        source_bytes: &[u8],
    ) -> Result<TranslationResult, String> {
        let path = match artifact {
            ImportArtifact::Skill { path } => path,
            _ => return Err("SkillsTranslator only handles Skill artifacts".to_string()),
        };

        let content = std::str::from_utf8(source_bytes)
            .map_err(|e| format!("SKILL.md is not valid UTF-8: {e}"))?;

        let imported_at = now_rfc3339();
        let (stamped, warnings) = stamp_skill_content(content, &imported_at);

        // Ensure `enabled: false` is in the frontmatter
        let stamped = ensure_enabled_false(&stamped);

        // Derive the skill name for destination path
        let skill_name = if let Some((fm, _)) = split_frontmatter(&stamped) {
            extract_name_from_frontmatter(fm)
                .unwrap_or_else(|| path_to_skill_name(path))
        } else {
            path_to_skill_name(path)
        };

        // Destination: ~/.anvil/skills/<skill-name>/SKILL.md
        let anvil_home = crate::import::staging::anvil_config_home();
        let destination = anvil_home.join("skills").join(&skill_name).join("SKILL.md");

        let warning = if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        };

        Ok(TranslationResult {
            bytes: stamped.into_bytes(),
            suggested_name: format!("{skill_name}/SKILL.md"),
            destination,
            warning,
        })
    }

    fn name(&self) -> &'static str {
        "skills-translator"
    }
}

/// Derive a skill name from the file path (parent directory name or stem).
fn path_to_skill_name(path: &Path) -> String {
    // If path is <dir>/SKILL.md, use the dir name
    if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
        if let Some(parent) = path.parent() {
            if let Some(name) = parent.file_name().and_then(|n| n.to_str()) {
                return name.to_string();
            }
        }
    }
    // Fall back to file stem
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown-skill")
        .to_string()
}

/// Ensure `enabled: false` appears in the SKILL.md frontmatter.
///
/// If `enabled:` already exists, it is overwritten to `false` so imported
/// skills always land disabled.
fn ensure_enabled_false(content: &str) -> String {
    if let Some((fm, body)) = split_frontmatter(content) {
        // Check if `enabled:` already appears
        if fm.lines().any(|l| l.trim().starts_with("enabled:")) {
            // Rewrite it to false
            let new_fm: String = fm
                .lines()
                .map(|l| {
                    if l.trim().starts_with("enabled:") {
                        "enabled: false".to_string()
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("---\n{new_fm}\n---\n\n{body}")
        } else {
            // Append enabled: false
            format!("---\n{fm}\nenabled: false\n---\n\n{body}")
        }
    } else {
        content.to_string()
    }
}

// ── Stager ────────────────────────────────────────────────────────────────────

/// Stages a translated skill to `<staging>/skills/<skill-name>/`.
///
/// Collision handling: if `~/.anvil/skills/<name>/` already exists, the skill
/// is staged as `<name>.imported/` and marked `NeedsReview`.
pub struct SkillsStager;

impl Stager for SkillsStager {
    fn stage(
        &self,
        _artifact: &ImportArtifact,
        translation: &TranslationResult,
        staging: &StagingDir,
    ) -> Result<StageAction, String> {
        let rel = format!("skills/{}", translation.suggested_name);
        let staged_path = staging
            .stage_bytes(&rel, &translation.bytes)
            .map_err(|e| e.to_string())?;

        Ok(StageAction {
            staged_path,
            destination: translation.destination.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "skills-stager"
    }
}

// ── Public convenience entry point ────────────────────────────────────────────

/// Run the full skills import pipeline for a CC profile directory.
///
/// Returns a list of `(skill_name, staged_path, needs_review, warnings)` tuples
/// for the caller to record in the manifest.
///
/// # Errors
///
/// Never returns `Err` — individual skill failures are collected in the returned
/// list as `needs_review = true` with a warning explaining the skip.
pub fn run_skills_import(
    cc_profile_dir: &Path,
    staging: &StagingDir,
) -> Vec<SkillImportRecord> {
    let source = ImportSource::ClaudeCode {
        profile_dir: cc_profile_dir.to_path_buf(),
    };

    let discoverer = SkillsDiscoverer;
    let triager = SkillsTriager;
    let translator = SkillsTranslator;
    let _stager = SkillsStager;

    let discovered = discoverer.discover(&source);
    let mut records: Vec<SkillImportRecord> = Vec::new();

    for da in discovered {
        let triage = triager.triage(&da.artifact, &da.meta);
        if let TriageDecision::Skip { reason } = triage {
            records.push(SkillImportRecord {
                source_path: da.meta.source_path.clone(),
                staged_path: None,
                skill_name: path_to_skill_name(da.artifact.source_path()),
                needs_review: false,
                skipped: true,
                warnings: vec![format!("skipped: {reason}")],
            });
            continue;
        }

        let source_bytes = match std::fs::read(da.artifact.source_path()) {
            Ok(b) => b,
            Err(e) => {
                records.push(SkillImportRecord {
                    source_path: da.meta.source_path.clone(),
                    staged_path: None,
                    skill_name: path_to_skill_name(da.artifact.source_path()),
                    needs_review: false,
                    skipped: true,
                    warnings: vec![format!("read error: {e}")],
                });
                continue;
            }
        };

        match translator.translate(&da.artifact, &da.meta, &source_bytes) {
            Err(e) => {
                records.push(SkillImportRecord {
                    source_path: da.meta.source_path.clone(),
                    staged_path: None,
                    skill_name: path_to_skill_name(da.artifact.source_path()),
                    needs_review: false,
                    skipped: true,
                    warnings: vec![format!("translation error: {e}")],
                });
            }
            Ok(translation) => {
                // Check for name collision with existing Anvil skills
                let anvil_skill_path = translation.destination.parent().map(|p| p.to_path_buf());
                let (needs_review, staged_rel) =
                    if anvil_skill_path.as_deref().map(|p| p.exists()).unwrap_or(false) {
                        // Collision — stage as <name>.imported/
                        let collision_name =
                            format!("{}.imported", translation.suggested_name.trim_end_matches("/SKILL.md"));
                        (true, format!("skills/{collision_name}/SKILL.md"))
                    } else {
                        (false, format!("skills/{}", translation.suggested_name))
                    };

                let warn = translation.warning.clone();
                match staging.stage_bytes(&staged_rel, &translation.bytes) {
                    Err(e) => {
                        let mut ws = warn.map(|w| vec![w]).unwrap_or_default();
                        ws.push(format!("stage error: {e}"));
                        records.push(SkillImportRecord {
                            source_path: da.meta.source_path.clone(),
                            staged_path: None,
                            skill_name: path_to_skill_name(da.artifact.source_path()),
                            needs_review,
                            skipped: true,
                            warnings: ws,
                        });
                    }
                    Ok(staged_path) => {
                        let skill_name = translation
                            .suggested_name
                            .split('/')
                            .next()
                            .unwrap_or("unknown")
                            .to_string();
                        let mut ws = warn.map(|w| vec![w]).unwrap_or_default();
                        if needs_review {
                            ws.push(format!(
                                "Name collision with existing Anvil skill — staged as {staged_rel}"
                            ));
                        }
                        records.push(SkillImportRecord {
                            source_path: da.meta.source_path.clone(),
                            staged_path: Some(staged_path),
                            skill_name,
                            needs_review,
                            skipped: false,
                            warnings: ws,
                        });
                    }
                }
            }
        }
    }

    records
}

/// Record for one skill's import outcome.
#[derive(Debug, Clone)]
pub struct SkillImportRecord {
    /// Original source path.
    pub source_path: PathBuf,
    /// Where the skill was staged (None if skipped or failed).
    pub staged_path: Option<PathBuf>,
    /// Resolved skill name.
    pub skill_name: String,
    /// True if the skill was staged but needs user review (e.g. name collision).
    pub needs_review: bool,
    /// True if the skill was not staged.
    pub skipped: bool,
    /// Warnings / reasons.
    pub warnings: Vec<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn make_skill(dir: &Path, skill_name: &str, body: &str) -> PathBuf {
        let skill_dir = dir.join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md, body).unwrap();
        skill_md
    }

    const VALID_SKILL: &str = r#"---
name: my-skill
description: Does something useful
---

This skill does useful things.
"#;

    const SKILL_WITH_CC_PHRASE: &str = r#"---
name: cc-skill
---

You are Claude Code. Do what I say.
Use your CC powers.
"#;

    const SKILL_NO_FRONTMATTER: &str = "Just a plain markdown file without frontmatter.\n";

    const SKILL_NO_NAME: &str = "---\ndescription: Missing name\n---\n\nBody.\n";

    // ── split_frontmatter ────────────────────────────────────────────────────

    #[test]
    fn split_frontmatter_parses_valid() {
        let (fm, body) = split_frontmatter(VALID_SKILL).unwrap();
        assert!(fm.contains("name: my-skill"));
        assert!(body.contains("useful things"));
    }

    #[test]
    fn split_frontmatter_returns_none_for_no_frontmatter() {
        assert!(split_frontmatter(SKILL_NO_FRONTMATTER).is_none());
    }

    // ── extract_name_from_frontmatter ────────────────────────────────────────

    #[test]
    fn extract_name_finds_name() {
        let (fm, _) = split_frontmatter(VALID_SKILL).unwrap();
        assert_eq!(extract_name_from_frontmatter(fm), Some("my-skill".to_string()));
    }

    #[test]
    fn extract_name_returns_none_when_missing() {
        let fm = "description: Missing name";
        assert!(extract_name_from_frontmatter(fm).is_none());
    }

    // ── stamp_skill_content ──────────────────────────────────────────────────

    #[test]
    fn stamp_injects_imported_from() {
        let (stamped, warnings) = stamp_skill_content(VALID_SKILL, "2026-05-15T00:00:00Z");
        assert!(stamped.contains("imported_from: claude_code"));
        assert!(stamped.contains("imported_at:"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn stamp_strips_cc_identity_phrase() {
        let (stamped, warnings) = stamp_skill_content(SKILL_WITH_CC_PHRASE, "2026-05-15T00:00:00Z");
        assert!(!stamped.to_lowercase().contains("you are claude code"));
        assert!(!warnings.is_empty(), "should warn about stripping");
    }

    #[test]
    fn stamp_no_identity_phrase_no_warning() {
        let (stamped, warnings) = stamp_skill_content(VALID_SKILL, "2026-05-15T00:00:00Z");
        assert!(warnings.is_empty());
        assert!(stamped.contains("useful things"), "body should be preserved");
    }

    // ── ensure_enabled_false ─────────────────────────────────────────────────

    #[test]
    fn ensure_enabled_false_adds_when_absent() {
        let content = "---\nname: test\n---\n\nBody.\n";
        let result = ensure_enabled_false(content);
        assert!(result.contains("enabled: false"));
    }

    #[test]
    fn ensure_enabled_false_overwrites_true() {
        let content = "---\nname: test\nenabled: true\n---\n\nBody.\n";
        let result = ensure_enabled_false(content);
        assert!(result.contains("enabled: false"));
        assert!(!result.contains("enabled: true"));
    }

    // ── Triager ──────────────────────────────────────────────────────────────

    #[test]
    fn triager_keep_valid_skill() {
        let dir = TempDir::new().unwrap();
        let path = make_skill(dir.path(), "good-skill", VALID_SKILL);
        let artifact = ImportArtifact::Skill { path: path.clone() };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path,
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = SkillsTriager;
        assert!(t.triage(&artifact, &meta).is_keep());
    }

    #[test]
    fn triager_skip_no_frontmatter() {
        let dir = TempDir::new().unwrap();
        let path = make_skill(dir.path(), "bad-skill", SKILL_NO_FRONTMATTER);
        let artifact = ImportArtifact::Skill { path: path.clone() };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path,
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = SkillsTriager;
        assert!(t.triage(&artifact, &meta).is_skip());
    }

    #[test]
    fn triager_skip_no_name_field() {
        let dir = TempDir::new().unwrap();
        let path = make_skill(dir.path(), "no-name-skill", SKILL_NO_NAME);
        let artifact = ImportArtifact::Skill { path: path.clone() };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path,
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = SkillsTriager;
        assert!(t.triage(&artifact, &meta).is_skip());
    }

    // ── Translator ────────────────────────────────────────────────────────────

    #[test]
    fn translator_injects_enabled_false() {
        let dir = TempDir::new().unwrap();
        let path = make_skill(dir.path(), "my-skill", VALID_SKILL);
        let artifact = ImportArtifact::Skill { path: path.clone() };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path.clone(),
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = SkillsTranslator;
        let bytes = std::fs::read(&path).unwrap();
        let result = t.translate(&artifact, &meta, &bytes).unwrap();
        let content = String::from_utf8(result.bytes).unwrap();
        assert!(content.contains("enabled: false"));
        assert!(content.contains("imported_from: claude_code"));
    }

    #[test]
    fn translator_destination_uses_skill_name() {
        let dir = TempDir::new().unwrap();
        let path = make_skill(dir.path(), "audit-skill", VALID_SKILL);
        let artifact = ImportArtifact::Skill { path: path.clone() };
        let meta = ImportArtifactMeta {
            source: ImportSource::ClaudeCode {
                profile_dir: dir.path().to_path_buf(),
            },
            source_path: path.clone(),
            content_hash: "abc".to_string(),
            discovered_at: SystemTime::now(),
        };
        let t = SkillsTranslator;
        let bytes = std::fs::read(&path).unwrap();
        let result = t.translate(&artifact, &meta, &bytes).unwrap();
        // Destination should contain the skill name from frontmatter
        assert!(result.destination.to_string_lossy().contains("my-skill"));
    }

    // ── Discovery ─────────────────────────────────────────────────────────────

    #[test]
    fn discoverer_finds_user_skills() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        make_skill(&skills_dir, "skill-a", VALID_SKILL);
        make_skill(&skills_dir, "skill-b", VALID_SKILL);

        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = SkillsDiscoverer;
        let found = d.discover(&source);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn discoverer_finds_marketplace_skills() {
        let dir = TempDir::new().unwrap();
        let market_skills = dir
            .path()
            .join("plugins")
            .join("marketplaces")
            .join("default")
            .join("skills");
        std::fs::create_dir_all(&market_skills).unwrap();
        make_skill(&market_skills, "market-skill", VALID_SKILL);

        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = SkillsDiscoverer;
        let found = d.discover(&source);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn discoverer_skips_dirs_without_skill_md() {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("no-skill-md-dir")).unwrap();
        std::fs::write(skills_dir.join("no-skill-md-dir").join("README.md"), "x")
            .unwrap();

        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = SkillsDiscoverer;
        let found = d.discover(&source);
        assert!(found.is_empty());
    }

    #[test]
    fn discoverer_empty_when_no_skills_dir() {
        let dir = TempDir::new().unwrap();
        let source = ImportSource::ClaudeCode {
            profile_dir: dir.path().to_path_buf(),
        };
        let d = SkillsDiscoverer;
        assert!(d.discover(&source).is_empty());
    }

    // ── Full pipeline ─────────────────────────────────────────────────────────

    #[test]
    #[serial(anvil_config_home)]
    fn run_skills_import_stages_valid_skills() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let skills_dir = cc_dir.join("skills");
        make_skill(&skills_dir, "skill-a", VALID_SKILL);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_skills_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(records.len(), 1);
        assert!(!records[0].skipped);
        assert!(!records[0].needs_review);
        assert!(records[0].staged_path.as_ref().unwrap().exists());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn run_skills_import_skips_malformed_skill() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let skills_dir = cc_dir.join("skills");
        make_skill(&skills_dir, "bad-skill", SKILL_NO_FRONTMATTER);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_skills_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(records.len(), 1);
        assert!(records[0].skipped);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn run_skills_import_idempotent_on_no_change() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let skills_dir = cc_dir.join("skills");
        make_skill(&skills_dir, "stable-skill", VALID_SKILL);

        // First import
        let staging1 = crate::import::staging::StagingDir::create_clean().unwrap();
        let r1 = run_skills_import(&cc_dir, &staging1);

        // Second import (StagingDir::create_clean() backs up first)
        let staging2 = crate::import::staging::StagingDir::create_clean().unwrap();
        let r2 = run_skills_import(&cc_dir, &staging2);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(r1.len(), r2.len(), "idempotent: same number of records");
        assert_eq!(r1[0].skill_name, r2[0].skill_name);
    }

    #[test]
    #[serial(anvil_config_home)]
    fn name_collision_stages_as_imported_suffix() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        // Pre-create an Anvil skill to trigger collision
        let anvil_skills = dir.path().join("skills").join("my-skill");
        std::fs::create_dir_all(&anvil_skills).unwrap();
        std::fs::write(anvil_skills.join("SKILL.md"), VALID_SKILL).unwrap();

        let cc_dir = dir.path().join("cc");
        let skills_dir = cc_dir.join("skills");
        make_skill(&skills_dir, "colliding-skill", VALID_SKILL);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_skills_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert_eq!(records.len(), 1);
        assert!(records[0].needs_review);
        assert!(!records[0].warnings.is_empty());
    }

    #[test]
    #[serial(anvil_config_home)]
    fn imported_skills_land_disabled() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ANVIL_CONFIG_HOME", dir.path()) };

        let cc_dir = dir.path().join("cc");
        let skills_dir = cc_dir.join("skills");
        // Skill with enabled: true — should be overridden to false
        let enabled_skill = "---\nname: my-skill\nenabled: true\n---\n\nBody.\n";
        make_skill(&skills_dir, "enabled-skill", enabled_skill);

        let staging = crate::import::staging::StagingDir::create_clean().unwrap();
        let records = run_skills_import(&cc_dir, &staging);

        unsafe { std::env::remove_var("ANVIL_CONFIG_HOME") };

        assert!(!records.is_empty());
        let staged_path = records[0].staged_path.as_ref().unwrap();
        let content = std::fs::read_to_string(staged_path).unwrap();
        assert!(content.contains("enabled: false"), "imported skill must be disabled");
        assert!(!content.contains("enabled: true"), "enabled: true must be overwritten");
    }
}
