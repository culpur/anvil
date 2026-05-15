/// ImportArtifact — typed representation of a single artifact that can be
/// migrated from a CC installation into Anvil.
///
/// Each variant carries the minimum metadata needed to:
///   - identify the artifact uniquely (source path + content hash)
///   - route it to the correct destination in subsequent buckets
///   - report skip reasons accurately
///
/// Phase 6.0: only the enum, its metadata companion, and `ImportSource` are
/// defined here.  Discover / Translate / Commit logic lives in later buckets.

use std::path::PathBuf;
use std::time::SystemTime;

// ── ImportSource ────────────────────────────────────────────────────────────

/// Identifies the origin system of an import operation.
///
/// Only `ClaudeCode` is wired in Phase 6.0.  `File` and `Url` are
/// future-proofing stubs; they compile but have no implementations until
/// later arcs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportSource {
    /// A local CC installation at the given profile directory.
    /// Default: `~/.claude/` (read-only).
    ClaudeCode { profile_dir: PathBuf },
    /// Future: import from an arbitrary on-disk directory.
    #[allow(dead_code)]
    File { path: PathBuf },
    /// Future: import from a URL (e.g. a shared Anvil config gist).
    #[allow(dead_code)]
    Url { url: String },
}

impl ImportSource {
    /// Return a short human-readable label for progress reporting.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::ClaudeCode { profile_dir } => {
                format!("cc:{}", profile_dir.display())
            }
            Self::File { path } => format!("file:{}", path.display()),
            Self::Url { url } => format!("url:{url}"),
        }
    }

    /// Return the default CC source using `~/.claude/`.
    #[must_use]
    pub fn default_cc() -> Self {
        let profile_dir = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude");
        Self::ClaudeCode { profile_dir }
    }
}

// ── Settings scope ──────────────────────────────────────────────────────────

/// Whether a settings file is user-level or project-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    /// `~/.claude/settings.json` — user-level global settings.
    User,
    /// `<project>/.claude/settings.json` — project-local settings.
    Local,
}

// ── ImportArtifact ──────────────────────────────────────────────────────────

/// A single artifact that can be imported from a CC profile.
///
/// Variants map 1-to-1 with the artifact types in §4 of
/// `MIGRATION-CLAUDE-CODE.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportArtifact {
    /// A memory markdown file from `~/.claude/projects/<id>/memory/*.md`.
    Memory {
        /// Absolute path to the source file.
        path: PathBuf,
        /// `true` if the file begins with YAML frontmatter (`---`).
        has_frontmatter: bool,
    },
    /// A `CLAUDE.md` instruction file — global or per-project.
    Instructions {
        /// Absolute path to the source file.
        path: PathBuf,
        /// `true` for `~/.claude/CLAUDE.md`; `false` for project-level.
        is_global: bool,
        /// For project-level files: the project root directory.
        project_path: Option<PathBuf>,
    },
    /// A CC `settings.json` file.
    Settings {
        /// Absolute path to the source file.
        path: PathBuf,
        /// User-level or project-local.
        scope: SettingsScope,
    },
    /// A skill file from `~/.claude/skills/`.
    Skill {
        /// Absolute path to the source file.
        path: PathBuf,
    },
    /// A plugin entry from CC's plugin registry.
    Plugin {
        /// Path to the plugin manifest or installed_plugins.json entry.
        manifest_path: PathBuf,
        /// Marketplace identifier, if the plugin came from a marketplace.
        marketplace: Option<String>,
    },
    /// An agent definition file from `~/.claude/agents/`.
    Agent {
        /// Absolute path to the source file.
        path: PathBuf,
    },
    /// A session transcript JSONL file.
    ///
    /// Session import is gated by `--include-sessions` and is expensive;
    /// it is handled by Bucket 3 (session-summarization arc).
    Session {
        /// Absolute path to the `.jsonl` transcript.
        transcript_path: PathBuf,
        /// CC project identifier (directory hash or UUID).
        project_id: String,
        /// CC session identifier.
        session_id: String,
        /// File size in bytes (used for cost estimation and chunking strategy).
        size_bytes: u64,
    },
}

impl ImportArtifact {
    /// Return a short, stable string tag for this variant.
    /// Used as the `artifact` discriminator in `ImportEntry`.
    #[must_use]
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::Memory { .. } => "memory",
            Self::Instructions { .. } => "instructions",
            Self::Settings { .. } => "settings",
            Self::Skill { .. } => "skill",
            Self::Plugin { .. } => "plugin",
            Self::Agent { .. } => "agent",
            Self::Session { .. } => "session",
        }
    }

    /// Return the primary source path for this artifact.
    #[must_use]
    pub fn source_path(&self) -> &PathBuf {
        match self {
            Self::Memory { path, .. }
            | Self::Instructions { path, .. }
            | Self::Settings { path, .. }
            | Self::Skill { path }
            | Self::Agent { path } => path,
            Self::Plugin { manifest_path, .. } => manifest_path,
            Self::Session { transcript_path, .. } => transcript_path,
        }
    }
}

// ── ImportArtifactMeta ──────────────────────────────────────────────────────

/// Per-artifact metadata attached at discovery time.
///
/// Kept separate from `ImportArtifact` so the enum stays clean and cloneable
/// without dragging in `SystemTime` everywhere.
#[derive(Debug, Clone)]
pub struct ImportArtifactMeta {
    /// The import source that produced this artifact.
    pub source: ImportSource,
    /// Original path on disk (absolute, under `~/.claude/`).
    pub source_path: PathBuf,
    /// SHA-256 hex digest of the file bytes at discovery time.
    /// Used by the idempotency gate to detect no-op re-runs.
    pub content_hash: String,
    /// When the artifact was enumerated.
    pub discovered_at: SystemTime,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_tag_round_trips_all_variants() {
        let cases = vec![
            ImportArtifact::Memory {
                path: PathBuf::from("/tmp/a.md"),
                has_frontmatter: false,
            },
            ImportArtifact::Instructions {
                path: PathBuf::from("/tmp/CLAUDE.md"),
                is_global: true,
                project_path: None,
            },
            ImportArtifact::Settings {
                path: PathBuf::from("/tmp/settings.json"),
                scope: SettingsScope::User,
            },
            ImportArtifact::Skill {
                path: PathBuf::from("/tmp/skill.md"),
            },
            ImportArtifact::Plugin {
                manifest_path: PathBuf::from("/tmp/plugin.json"),
                marketplace: None,
            },
            ImportArtifact::Agent {
                path: PathBuf::from("/tmp/agent.json"),
            },
            ImportArtifact::Session {
                transcript_path: PathBuf::from("/tmp/session.jsonl"),
                project_id: "proj-1".into(),
                session_id: "sess-1".into(),
                size_bytes: 1024,
            },
        ];

        let expected_tags = [
            "memory", "instructions", "settings", "skill", "plugin", "agent", "session",
        ];

        for (artifact, expected) in cases.iter().zip(expected_tags.iter()) {
            assert_eq!(artifact.kind_tag(), *expected);
        }
    }

    #[test]
    fn import_source_label_contains_identifier() {
        let cc = ImportSource::ClaudeCode {
            profile_dir: PathBuf::from("/home/user/.claude"),
        };
        assert!(cc.label().contains("cc:"));
        assert!(cc.label().contains(".claude"));

        let file = ImportSource::File {
            path: PathBuf::from("/tmp/backup"),
        };
        assert!(file.label().starts_with("file:"));

        let url = ImportSource::Url {
            url: "https://example.com/config".into(),
        };
        assert!(url.label().starts_with("url:"));
    }

    #[test]
    fn source_path_returns_primary_path() {
        let p = PathBuf::from("/tmp/test.md");
        let artifact = ImportArtifact::Memory {
            path: p.clone(),
            has_frontmatter: false,
        };
        assert_eq!(artifact.source_path(), &p);
    }
}
