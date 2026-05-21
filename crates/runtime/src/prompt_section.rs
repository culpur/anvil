//! Structured representation of system-prompt sections.
//!
//! Before this module existed, the system prompt was a `Vec<String>` and
//! sections were identified by:
//!   - position (e.g. "the first element is always the intro"),
//!   - inline markers like `CUSTOM_STYLE_MARKER` baked into the string body,
//!   - `Vec::insert(0, ...)` to prepend, with no way to deduplicate or
//!     replace an existing section.
//!
//! That worked for one process but breaks for everything v2.2.14+ needs:
//!   - The daemon (v2.2.14 routines arc, then v2.3 reconnect) serializes
//!     session state to disk and reloads it on resume. A `Vec<String>` cannot
//!     be diffed, patched, or queried — the daemon would have to re-parse
//!     markers from the body to know what each section is.
//!   - Agents (`/agent`, `TeamDelegate`) inherit a snapshot of the parent
//!     prompt and must layer their own sections on top. Doing that on
//!     positional strings is fragile.
//!   - The seven-layer memory work (L1 Working) requires a typed
//!     `WorkingMemorySnapshot` for compaction and promotion. Compaction
//!     decides which sections to drop based on kind, not body.
//!
//! Each [`PromptSection`] carries a [`PromptSectionKind`] tag. Code that
//! used to splice strings by position or marker now upserts by kind.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Stable identifier for a system-prompt section.
///
/// Kinds drive deduplication, ordering, and serialization. A `Vec<PromptSection>`
/// MAY contain duplicates of [`Self::Custom`] and [`Self::Skill`] (skills
/// stack), but every other kind is expected to appear at most once. The
/// [`PromptSectionsExt`] helpers enforce that for typed kinds via `upsert_by_kind`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum PromptSectionKind {
    // ─── Static intro/system framing (top of prompt) ───────────────────────
    /// Identity line ("You are Anvil...").
    Intro,
    /// `# Output Style: <name>` block (from `/output-style` or config).
    OutputStyle,
    /// Built-in concise/terse "condensed" output-style fragment, opt-in via
    /// `/output-style condensed`. Replaces the previous in-body
    /// `TERSE_SKILL_BODY` insert-at-zero hack.
    OutputStyleCondensed,
    /// User-supplied custom output-style fragment via `/output-style <text>`.
    /// Replaces the previous `CUSTOM_STYLE_MARKER`-prefixed string hack.
    OutputStyleCustom,
    /// "Be concise and direct." prefix toggled by `/toggle-fast-mode`.
    /// Replaces the previous `FAST_PREFIX` insert-at-zero hack.
    FastMode,
    /// "# System" section.
    System,
    /// "# Doing tasks" section.
    DoingTasks,
    /// "# Executing actions with care" section.
    Actions,
    /// `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` marker — separates the static intro
    /// from the dynamic per-turn context below.
    DynamicBoundary,
    // ─── Dynamic per-turn context (below the boundary) ─────────────────────
    /// `# Environment context` block (version, model, cwd, date, headline).
    Environment,
    /// `# Retrieval order` block (Bucket 0).
    RetrievalOrder,
    /// `# Project context` block (cwd metadata).
    ProjectContext,
    /// `# Project instructions` rendered from ANVIL.md files.
    InstructionFiles,
    /// `# Persistent memory` block from `MEMORY.md`.
    PersistentMemory,
    /// `# Workspace knowledge (QMD)` injection.
    Qmd,
    /// `# Configuration` block.
    Config,
    /// `<known-files>` W11 file-fingerprint cache block.
    KnownFiles,
    /// L2 episodic: recent `DailySummary` entries from `~/.anvil/daily/`,
    /// injected when `ANVIL_DAILY_INJECT=1`. Capped to the most-recent N days
    /// (default 3) to keep prompt size bounded.
    DailySummary,
    // ─── User-injected prepends (formerly `insert(0, ...)`) ────────────────
    /// Active goal fragment, from `/goal`.
    Goal,
    /// A loaded skill body. May appear multiple times — one per loaded skill.
    /// The optional name disambiguates and lets `/skill unload` remove the
    /// right one.
    Skill,
    /// Arbitrary custom append (e.g. `with_append_sections` from external
    /// callers that don't have a typed kind). Free-form, may appear many
    /// times. Body is opaque.
    Custom,
}

impl PromptSectionKind {
    /// Whether this kind can appear more than once in a prompt.
    ///
    /// `Skill` and `Custom` are repeatable; everything else is "at most one"
    /// and the upsert helpers replace the existing entry.
    #[must_use]
    pub const fn is_repeatable(&self) -> bool {
        matches!(self, Self::Skill | Self::Custom)
    }

    /// Stable string tag for logs, OTel, and the wire format.
    #[must_use]
    pub const fn as_tag(&self) -> &'static str {
        match self {
            Self::Intro => "intro",
            Self::OutputStyle => "output_style",
            Self::OutputStyleCondensed => "output_style_condensed",
            Self::OutputStyleCustom => "output_style_custom",
            Self::FastMode => "fast_mode",
            Self::System => "system",
            Self::DoingTasks => "doing_tasks",
            Self::Actions => "actions",
            Self::DynamicBoundary => "dynamic_boundary",
            Self::Environment => "environment",
            Self::RetrievalOrder => "retrieval_order",
            Self::ProjectContext => "project_context",
            Self::InstructionFiles => "instruction_files",
            Self::PersistentMemory => "persistent_memory",
            Self::Qmd => "qmd",
            Self::Config => "config",
            Self::KnownFiles => "known_files",
            Self::DailySummary => "daily_summary",
            Self::Goal => "goal",
            Self::Skill => "skill",
            Self::Custom => "custom",
        }
    }
}

/// A typed system-prompt section.
///
/// `body` is the rendered text exactly as it should appear in the assembled
/// prompt (no markers, no leading/trailing separator — the renderer
/// joins sections with `\n\n`).
///
/// `label` is optional human-friendly metadata that disambiguates repeatable
/// kinds (e.g. the skill name for [`PromptSectionKind::Skill`]). It is NOT
/// included in the rendered prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSection {
    pub kind: PromptSectionKind,
    pub body: String,
    /// Optional sub-identifier for repeatable kinds. `Skill("backend")` and
    /// `Skill("qa")` can coexist; `unload(Skill, "backend")` removes only
    /// one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PromptSection {
    /// Construct a section with no label.
    #[must_use]
    pub fn new(kind: PromptSectionKind, body: impl Into<String>) -> Self {
        Self {
            kind,
            body: body.into(),
            label: None,
        }
    }

    /// Construct a labeled section (used for repeatable kinds).
    #[must_use]
    pub fn labeled(
        kind: PromptSectionKind,
        body: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            body: body.into(),
            label: Some(label.into()),
        }
    }
}

/// A point-in-time snapshot of the working-memory layer (L1) — the
/// fully-assembled prompt sections plus a generation timestamp.
///
/// This is the unit the v2.2.14 daemon persists between session resumes.
/// Future compaction lives on this struct as a method (`compact()`) that
/// drops or promotes sections by kind rather than truncating raw text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingMemorySnapshot {
    pub sections: Vec<PromptSection>,
    /// Unix-seconds when this snapshot was generated.
    pub generated_at: u64,
}

impl WorkingMemorySnapshot {
    /// Construct from sections at "now".
    #[must_use]
    pub fn new(sections: Vec<PromptSection>) -> Self {
        Self {
            sections,
            generated_at: now_secs(),
        }
    }

    /// Render to the legacy `Vec<String>` shape consumed by the API-client
    /// boundary in `crates/anvil-cli/src/providers.rs`.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        self.sections.to_strings()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convenience operations on `Vec<PromptSection>`.
///
/// These replace the index-based `insert(0, ...)` / position-aware splicing
/// the old `Vec<String>` carried. Use kind-based upsert/remove from now on.
pub trait PromptSectionsExt {
    /// Project to the legacy `Vec<String>` shape for the API boundary.
    fn to_strings(&self) -> Vec<String>;

    /// Convert a legacy `Vec<String>` to `Vec<PromptSection>` by wrapping
    /// each element as `PromptSectionKind::Custom`. Used for migration
    /// adapters where the caller has not yet been refactored.
    fn from_strings_lossy(strings: Vec<String>) -> Vec<PromptSection>;

    /// Find the first section with the given kind (and matching label, if
    /// the kind is repeatable and `label` is `Some`).
    fn find_by_kind(
        &self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<&PromptSection>;

    /// Position of the first matching section, or `None`.
    fn position_by_kind(
        &self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<usize>;

    /// Insert-or-replace a section by kind.
    ///
    /// For non-repeatable kinds, replaces the existing entry in place.
    /// For repeatable kinds (`Skill`, `Custom`), matches by `(kind, label)`
    /// and replaces in place, or appends if no match. The label on
    /// `section` is the lookup key for repeatable kinds.
    ///
    /// New entries are prepended (inserted at position 0) for kinds that
    /// historically used `insert(0, ...)`: `Goal`, `Skill`, `OutputStyleCondensed`,
    /// `OutputStyleCustom`, `FastMode`. All other new entries are appended.
    fn upsert_by_kind(&mut self, section: PromptSection);

    /// Remove the first section matching `(kind, label)`. Returns the
    /// removed section if any.
    fn remove_by_kind(
        &mut self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<PromptSection>;

    /// Remove ALL sections matching `kind` regardless of label. Returns the
    /// number removed. Used by `/skill clear` and reset operations.
    fn remove_all_by_kind(&mut self, kind: &PromptSectionKind) -> usize;

    /// Iterate `(kind, body)` pairs in injection order.
    ///
    /// Task #731 / Layer 1 — `/memory layer 1` walks this iterator to render
    /// the live working-memory inventory rather than the hand-written static
    /// text it used before. Yielding `(PromptSectionKind, &str)` keeps the
    /// caller free from re-walking labels separately; labels live on the
    /// underlying `PromptSection` when the consumer needs them.
    fn iter_by_kind(&self) -> Box<dyn Iterator<Item = (&PromptSectionKind, &str)> + '_>;
}

impl PromptSectionsExt for Vec<PromptSection> {
    fn to_strings(&self) -> Vec<String> {
        self.iter().map(|s| s.body.clone()).collect()
    }

    fn from_strings_lossy(strings: Vec<String>) -> Vec<PromptSection> {
        strings
            .into_iter()
            .map(|body| PromptSection::new(PromptSectionKind::Custom, body))
            .collect()
    }

    fn find_by_kind(
        &self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<&PromptSection> {
        self.iter().find(|s| section_matches(s, kind, label))
    }

    fn position_by_kind(
        &self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<usize> {
        self.iter().position(|s| section_matches(s, kind, label))
    }

    fn upsert_by_kind(&mut self, section: PromptSection) {
        let label = section.label.as_deref();
        if let Some(pos) = self.position_by_kind(&section.kind, label) {
            self[pos] = section;
            return;
        }
        if prepends_when_new(&section.kind) {
            self.insert(0, section);
        } else {
            self.push(section);
        }
    }

    fn remove_by_kind(
        &mut self,
        kind: &PromptSectionKind,
        label: Option<&str>,
    ) -> Option<PromptSection> {
        let pos = self.position_by_kind(kind, label)?;
        Some(self.remove(pos))
    }

    fn remove_all_by_kind(&mut self, kind: &PromptSectionKind) -> usize {
        let before = self.len();
        self.retain(|s| &s.kind != kind);
        before - self.len()
    }

    fn iter_by_kind(&self) -> Box<dyn Iterator<Item = (&PromptSectionKind, &str)> + '_> {
        Box::new(self.iter().map(|s| (&s.kind, s.body.as_str())))
    }
}

fn section_matches(
    section: &PromptSection,
    kind: &PromptSectionKind,
    label: Option<&str>,
) -> bool {
    if &section.kind != kind {
        return false;
    }
    match (label, section.label.as_deref()) {
        // No filter requested — match any label.
        (None, _) => true,
        // Filter requested but section has no label — only match if the
        // requested label is empty (rare; treat as "match unlabeled").
        (Some(req), None) => req.is_empty(),
        (Some(req), Some(sec_lbl)) => req == sec_lbl,
    }
}

/// Kinds that prepend at position 0 when first inserted, to match the
/// legacy `insert(0, ...)` behavior. All of these used to live above the
/// system intro because they need to bias the assistant before any other
/// instructions take effect.
fn prepends_when_new(kind: &PromptSectionKind) -> bool {
    matches!(
        kind,
        PromptSectionKind::Goal
            | PromptSectionKind::Skill
            | PromptSectionKind::OutputStyleCondensed
            | PromptSectionKind::OutputStyleCustom
            | PromptSectionKind::FastMode
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_strings_round_trips_bodies_in_order() {
        let sections = vec![
            PromptSection::new(PromptSectionKind::Intro, "intro body"),
            PromptSection::new(PromptSectionKind::System, "system body"),
        ];
        assert_eq!(sections.to_strings(), vec!["intro body", "system body"]);
    }

    #[test]
    fn from_strings_lossy_wraps_every_entry_as_custom() {
        let v = Vec::<PromptSection>::from_strings_lossy(vec![
            "a".to_string(),
            "b".to_string(),
        ]);
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|s| s.kind == PromptSectionKind::Custom));
    }

    #[test]
    fn upsert_replaces_existing_singleton_kind_in_place() {
        let mut v = vec![
            PromptSection::new(PromptSectionKind::Intro, "first intro"),
            PromptSection::new(PromptSectionKind::System, "system"),
        ];
        v.upsert_by_kind(PromptSection::new(PromptSectionKind::Intro, "new intro"));
        // Length unchanged; old intro replaced in place at position 0.
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].body, "new intro");
        assert_eq!(v[1].kind, PromptSectionKind::System);
    }

    #[test]
    fn upsert_goal_prepends_when_new() {
        let mut v = vec![
            PromptSection::new(PromptSectionKind::Intro, "intro"),
            PromptSection::new(PromptSectionKind::System, "system"),
        ];
        v.upsert_by_kind(PromptSection::new(PromptSectionKind::Goal, "goal body"));
        assert_eq!(v[0].kind, PromptSectionKind::Goal);
        assert_eq!(v[0].body, "goal body");
    }

    #[test]
    fn upsert_goal_replaces_in_place_on_second_call() {
        let mut v = vec![
            PromptSection::new(PromptSectionKind::Goal, "old goal"),
            PromptSection::new(PromptSectionKind::Intro, "intro"),
        ];
        v.upsert_by_kind(PromptSection::new(PromptSectionKind::Goal, "new goal"));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].body, "new goal");
        assert_eq!(v[1].kind, PromptSectionKind::Intro);
    }

    #[test]
    fn upsert_skill_with_distinct_labels_stacks() {
        let mut v = Vec::new();
        v.upsert_by_kind(PromptSection::labeled(
            PromptSectionKind::Skill,
            "skill A body",
            "alpha",
        ));
        v.upsert_by_kind(PromptSection::labeled(
            PromptSectionKind::Skill,
            "skill B body",
            "beta",
        ));
        assert_eq!(v.len(), 2);
        // Both prepended; the second is now at position 0.
        assert_eq!(v[0].label.as_deref(), Some("beta"));
        assert_eq!(v[1].label.as_deref(), Some("alpha"));
    }

    #[test]
    fn upsert_skill_same_label_replaces() {
        let mut v = Vec::new();
        v.upsert_by_kind(PromptSection::labeled(
            PromptSectionKind::Skill,
            "v1",
            "alpha",
        ));
        v.upsert_by_kind(PromptSection::labeled(
            PromptSectionKind::Skill,
            "v2",
            "alpha",
        ));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].body, "v2");
    }

    #[test]
    fn remove_by_kind_removes_matching() {
        let mut v = vec![
            PromptSection::new(PromptSectionKind::Intro, "a"),
            PromptSection::new(PromptSectionKind::FastMode, "fast"),
            PromptSection::new(PromptSectionKind::System, "b"),
        ];
        let removed = v.remove_by_kind(&PromptSectionKind::FastMode, None);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().body, "fast");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, PromptSectionKind::Intro);
        assert_eq!(v[1].kind, PromptSectionKind::System);
    }

    #[test]
    fn remove_all_by_kind_clears_repeatable() {
        let mut v = vec![
            PromptSection::labeled(PromptSectionKind::Skill, "a", "alpha"),
            PromptSection::new(PromptSectionKind::Intro, "intro"),
            PromptSection::labeled(PromptSectionKind::Skill, "b", "beta"),
        ];
        let count = v.remove_all_by_kind(&PromptSectionKind::Skill);
        assert_eq!(count, 2);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, PromptSectionKind::Intro);
    }

    #[test]
    fn working_memory_snapshot_serde_round_trips() {
        let snap = WorkingMemorySnapshot::new(vec![
            PromptSection::new(PromptSectionKind::Intro, "intro"),
            PromptSection::labeled(PromptSectionKind::Skill, "body", "alpha"),
        ]);
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: WorkingMemorySnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.sections, snap.sections);
        assert_eq!(back.generated_at, snap.generated_at);
    }

    #[test]
    fn kind_tags_are_stable() {
        // If any tag changes, on-disk snapshots from prior runs become
        // unreadable — that's a breaking change and the test should fail.
        assert_eq!(PromptSectionKind::Intro.as_tag(), "intro");
        assert_eq!(PromptSectionKind::Goal.as_tag(), "goal");
        assert_eq!(PromptSectionKind::Skill.as_tag(), "skill");
        assert_eq!(PromptSectionKind::DynamicBoundary.as_tag(), "dynamic_boundary");
        assert_eq!(PromptSectionKind::FastMode.as_tag(), "fast_mode");
        assert_eq!(PromptSectionKind::OutputStyleCustom.as_tag(), "output_style_custom");
        assert_eq!(PromptSectionKind::DailySummary.as_tag(), "daily_summary");
    }

    #[test]
    fn iter_by_kind_yields_pairs_in_injection_order() {
        // Task #731 / L1: the iterator backs `/memory layer 1`'s live
        // working-memory render. Order must match `to_strings()` so the
        // displayed inventory matches the order the API client sends.
        let v = vec![
            PromptSection::new(PromptSectionKind::Environment, "env body"),
            PromptSection::new(PromptSectionKind::RetrievalOrder, "retrieval body"),
            PromptSection::labeled(PromptSectionKind::Skill, "skill body", "alpha"),
        ];
        let collected: Vec<(PromptSectionKind, String)> = v
            .iter_by_kind()
            .map(|(k, body)| (k.clone(), body.to_string()))
            .collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].0, PromptSectionKind::Environment);
        assert_eq!(collected[0].1, "env body");
        assert_eq!(collected[1].0, PromptSectionKind::RetrievalOrder);
        assert_eq!(collected[2].0, PromptSectionKind::Skill);
        assert_eq!(collected[2].1, "skill body");
    }

    #[test]
    fn is_repeatable_matches_design() {
        assert!(!PromptSectionKind::Intro.is_repeatable());
        assert!(!PromptSectionKind::Goal.is_repeatable());
        assert!(!PromptSectionKind::FastMode.is_repeatable());
        assert!(PromptSectionKind::Skill.is_repeatable());
        assert!(PromptSectionKind::Custom.is_repeatable());
    }
}
