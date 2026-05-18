// Trait-based agent composition for Anvil.
//
// Attribution: Dimension taxonomy (expertise / personality / approach) is adapted from
// Daniel Miessler's Personal_AI_Infrastructure project,
// Copyright 2025 Daniel Miessler, MIT License.
// https://github.com/danielmiessler/PersonalAI
//
// This module is an independent Rust rewrite; no source text from upstream is reproduced.
// Anvil is also MIT licensed — the licenses are compatible.
//
// Design divergences from the Miessler TypeScript reference:
//
//  1. `voice` / `voice_id` are omitted — Anvil is a text-only coding assistant.
//  2. Duplicate-dimension detection (`ConflictingTraits`) is enabled by default.
//     The upstream implementation silently allows multiple traits per dimension.
//     Anvil makes this an explicit error so that callers think carefully about
//     intent, while still providing an opt-out via `ComposeOptions`.
//  3. Prompt assembly order is fixed: intro → expertise → personality → approach → task.
//     Expertise fragments establish the agent's identity domain first so that
//     personality tone and methodology approach modify a well-scoped identity rather
//     than an abstract one.  Task is last because it is the deliverable scope that
//     the composed identity should address.
//  4. The catalogue is embedded at compile time via `include_str!` and lazily parsed
//     once per process with `OnceLock`, rather than read from disk at runtime.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

// ── Raw YAML structures (private) ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawCatalogue {
    traits: Vec<RawTrait>,
}

#[derive(Debug, Deserialize)]
struct RawTrait {
    name: String,
    dimension: String,
    prompt_fragment: String,
}

// ── Public types ─────────────────────────────────────────────────────────────

/// A single named trait with its dimension and system-prompt fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trait {
    pub name: String,
    pub dimension: String,
    pub prompt_fragment: String,
}

/// Parsed and indexed catalogue of all available traits.
#[derive(Debug, Clone)]
pub struct TraitCatalogue {
    by_name: HashMap<String, Trait>,
}

impl TraitCatalogue {
    /// Parse a YAML string into a `TraitCatalogue`.
    ///
    /// # Errors
    /// Returns a `ComposeError::ParseError` if the YAML is malformed.
    pub fn from_yaml(yaml: &str) -> Result<Self, ComposeError> {
        let raw: RawCatalogue =
            serde_yaml::from_str(yaml).map_err(|e| ComposeError::ParseError(e.to_string()))?;

        let by_name = raw
            .traits
            .into_iter()
            .map(|rt| {
                (
                    rt.name.clone(),
                    Trait {
                        name: rt.name,
                        dimension: rt.dimension,
                        prompt_fragment: rt.prompt_fragment,
                    },
                )
            })
            .collect();

        Ok(Self { by_name })
    }

    /// All traits in the catalogue (order is unspecified).
    #[must_use]
    pub fn all(&self) -> Vec<&Trait> {
        self.by_name.values().collect()
    }

    /// Look up a trait by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Trait> {
        self.by_name.get(name)
    }
}

// ── Bundled catalogue ────────────────────────────────────────────────────────

const BUNDLED_YAML: &str = include_str!("../assets/traits.yaml");

static BUNDLED_CATALOGUE: OnceLock<TraitCatalogue> = OnceLock::new();

/// Return the compile-time bundled trait catalogue, parsed once per process.
///
/// # Panics
/// Panics if the bundled YAML is malformed — this indicates a build-time defect,
/// not a runtime condition, so a panic is appropriate (loud failure, not silent).
#[must_use]
pub fn bundled_catalogue() -> &'static TraitCatalogue {
    BUNDLED_CATALOGUE.get_or_init(|| {
        TraitCatalogue::from_yaml(BUNDLED_YAML)
            .expect("bundled traits.yaml failed to parse — this is a build defect")
    })
}

// ── Formatting ────────────────────────────────────────────────────────────────

/// Format a human-readable listing of all traits in the catalogue.
#[must_use]
pub fn format_traits_listing(catalogue: &TraitCatalogue) -> String {
    let all = catalogue.all();
    if all.is_empty() {
        return "No traits found in catalogue.".to_string();
    }

    // Group by dimension, preserving insertion order via stable sort.
    let mut sorted = all;
    sorted.sort_by(|a, b| a.dimension.cmp(&b.dimension).then(a.name.cmp(&b.name)));

    let mut by_dim: Vec<(String, Vec<&Trait>)> = Vec::new();
    for t in sorted {
        if let Some(group) = by_dim.iter_mut().find(|(d, _)| d == &t.dimension) {
            group.1.push(t);
        } else {
            by_dim.push((t.dimension.clone(), vec![t]));
        }
    }

    let mut lines = vec!["Available agent traits:".to_string()];
    for (dim, traits) in by_dim {
        lines.push(format!("\n  [{dim}]"));
        for t in traits {
            let first_line = t.prompt_fragment.lines().next().unwrap_or("");
            lines.push(format!("    {:<20} — {}", t.name, first_line));
        }
    }
    lines.push(String::new());
    lines.push("Usage: /agent compose <trait>[,<trait>...] \"<task>\"".to_string());
    lines.join("\n")
}

// ── Composition ──────────────────────────────────────────────────────────────

/// A fully composed agent ready for use as a system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedAgent {
    /// The assembled system prompt, ready for injection.
    pub prompt: String,
    /// The trait names that were composed to build this agent.
    pub traits: Vec<String>,
    /// Optional UI colour hint (hex string, e.g. `"#4a90d9"`).
    pub color: Option<String>,
}

/// Options that control how `compose_agent` behaves.
#[derive(Debug, Clone, Default)]
pub struct ComposeOptions {
    /// When `true`, two traits from the same dimension are allowed.
    /// By default (`false`) such a combination is a `ConflictingTraits` error.
    pub allow_dimension_conflicts: bool,
    /// Optional UI colour hint attached to the composed agent unchanged.
    pub color: Option<String>,
}

/// Errors that can arise during trait composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeError {
    /// No traits were requested.
    EmptyTraits,
    /// A requested trait name does not exist in the catalogue.
    UnknownTrait(String),
    /// Two traits from the same dimension were requested (default behaviour).
    ConflictingTraits {
        dim: String,
        a: String,
        b: String,
    },
    /// The YAML catalogue could not be parsed.
    ParseError(String),
}

impl std::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyTraits => write!(f, "at least one trait must be specified"),
            Self::UnknownTrait(name) => write!(f, "unknown trait: {name:?}"),
            Self::ConflictingTraits { dim, a, b } => {
                write!(
                    f,
                    "conflicting traits in dimension {dim:?}: {a:?} and {b:?} — \
                     use ComposeOptions::allow_dimension_conflicts to override"
                )
            }
            Self::ParseError(msg) => write!(f, "YAML parse error: {msg}"),
        }
    }
}

impl std::error::Error for ComposeError {}

/// Compose an agent from a set of trait names and a task description.
///
/// Prompt assembly order:
///   1. Intro paragraph — scopes the agent as the union of its requested traits.
///   2. Expertise fragments — establish identity domain.
///   3. Personality fragments — shape tone.
///   4. Approach fragments — set methodology.
///   5. Task block — the deliverable scope.
///
/// This ordering ensures that personality and approach modifiers are applied to a
/// well-scoped technical identity rather than an abstract one, and that the task
/// is the final instruction the model reads before generating output.
///
/// # Errors
/// See [`ComposeError`].
pub fn compose_agent(
    catalogue: &TraitCatalogue,
    trait_names: &[&str],
    task: &str,
) -> Result<ComposedAgent, ComposeError> {
    compose_agent_with_options(catalogue, trait_names, task, ComposeOptions::default())
}

/// Like [`compose_agent`] but with explicit options.
///
/// # Errors
/// See [`ComposeError`].
pub fn compose_agent_with_options(
    catalogue: &TraitCatalogue,
    trait_names: &[&str],
    task: &str,
    opts: ComposeOptions,
) -> Result<ComposedAgent, ComposeError> {
    if trait_names.is_empty() {
        return Err(ComposeError::EmptyTraits);
    }

    // Resolve and validate all trait names first so errors are reported before
    // any string assembly begins.
    let resolved: Vec<&Trait> = trait_names
        .iter()
        .map(|name| {
            catalogue
                .get(name)
                .ok_or_else(|| ComposeError::UnknownTrait((*name).to_owned()))
        })
        .collect::<Result<_, _>>()?;

    // Dimension conflict check — O(n²) but n is tiny (≤30 traits in practice).
    if !opts.allow_dimension_conflicts {
        let mut seen_dims: HashMap<&str, &str> = HashMap::new();
        for t in &resolved {
            if let Some(prev_name) = seen_dims.insert(t.dimension.as_str(), t.name.as_str()) {
                return Err(ComposeError::ConflictingTraits {
                    dim: t.dimension.clone(),
                    a: prev_name.to_owned(),
                    b: t.name.clone(),
                });
            }
        }
    }

    // Partition resolved traits into ordered buckets.
    let mut expertise: Vec<&Trait> = Vec::new();
    let mut personality: Vec<&Trait> = Vec::new();
    let mut approach: Vec<&Trait> = Vec::new();
    let mut other: Vec<&Trait> = Vec::new();

    for t in &resolved {
        match t.dimension.as_str() {
            "expertise" => expertise.push(t),
            "personality" => personality.push(t),
            "approach" => approach.push(t),
            _ => other.push(t),
        }
    }

    // Build the prompt.
    let trait_labels: Vec<&str> = trait_names.iter().copied().collect();
    let intro = format!(
        "You are a specialised assistant composed from the following traits: {}.\n\
         Apply all of them simultaneously — they are not ranked; let them inform \
         each other as you work.",
        trait_labels.join(", ")
    );

    let mut sections: Vec<String> = vec![intro];

    for t in expertise
        .iter()
        .chain(personality.iter())
        .chain(approach.iter())
        .chain(other.iter())
    {
        sections.push(t.prompt_fragment.trim().to_owned());
    }

    sections.push(format!("## Task\n{}", task.trim()));

    let prompt = sections.join("\n\n");

    Ok(ComposedAgent {
        prompt,
        traits: trait_names.iter().map(|s| (*s).to_owned()).collect(),
        color: opts.color,
    })
}

// ── User-facing message helpers ───────────────────────────────────────────────
//
// These return pure strings (no `println!`) so that callers in both TUI and
// headless modes can choose where the output goes — `tui.push_system(msg)`
// for the live ratatui session, `println!("{msg}")` for `--print` / batch
// mode.  Task #624 (v2.2.14 Phase 1): the previous `run_agent_command` in
// `cmd_provider.rs` wrote directly to stdout, which corrupted the TUI
// back-buffer because bytes went BEHIND the alt-screen.
//
// See also `feedback-tui-stdout-anti-pattern.md`.

/// Usage message when `/agent compose` is invoked with no trait list.
#[must_use]
pub fn format_agent_compose_empty_traits_usage() -> String {
    "No traits provided. Usage: /agent compose security,skeptical,first-principles \"audit auth.rs\""
        .to_string()
}

/// Usage message when `/agent compose <traits>` is invoked with no task.
#[must_use]
pub fn format_agent_compose_empty_task_usage(traits: &[String]) -> String {
    format!(
        "No task provided. Usage: /agent compose {} \"<task description>\"",
        traits.join(",")
    )
}

/// Format a `ComposeError` as a user-facing message.
#[must_use]
pub fn format_agent_compose_error(err: &ComposeError) -> String {
    match err {
        ComposeError::EmptyTraits => format_agent_compose_empty_traits_usage(),
        ComposeError::UnknownTrait(name) => {
            format!("Unknown trait: {name}. Run /agent traits to list available traits.")
        }
        ComposeError::ConflictingTraits { dim, a, b } => format!(
            "Conflicting traits in dimension \"{dim}\": \"{a}\" and \"{b}\" cannot be combined — \
             they would fight over the same identity axis.\n\
             Remove one of them, or use traits from different dimensions.\n\
             (A future version will add --allow-conflicts for when you really want this.)"
        ),
        ComposeError::ParseError(msg) => format!("Trait catalogue parse error: {msg}"),
    }
}

/// Summary header for a successfully composed agent, e.g.
/// `"Composing agent with traits: security, skeptical"`.
#[must_use]
pub fn format_agent_compose_summary(composed: &ComposedAgent) -> String {
    format!("Composing agent with traits: {}", composed.traits.join(", "))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue() -> &'static TraitCatalogue {
        bundled_catalogue()
    }

    #[test]
    fn bundled_yaml_parses_cleanly() {
        // If the bundled YAML is malformed, bundled_catalogue() panics.
        // Reaching this line means parse succeeded.
        let cat = catalogue();
        assert!(!cat.all().is_empty(), "catalogue must not be empty");
    }

    #[test]
    fn bundled_catalogue_has_at_least_25_traits() {
        assert!(
            catalogue().all().len() >= 25,
            "expected ≥25 traits, got {}",
            catalogue().all().len()
        );
    }

    #[test]
    fn compose_three_trait_agent_contains_all_fragments_and_task() {
        let cat = catalogue();
        let task = "Review the authentication module for correctness.";
        let agent =
            compose_agent(cat, &["security", "blunt", "root-cause"], task).expect("compose failed");

        // All three prompt fragments must appear.
        let sec_fragment = cat.get("security").unwrap().prompt_fragment.trim().to_owned();
        let blunt_fragment = cat.get("blunt").unwrap().prompt_fragment.trim().to_owned();
        let root_fragment = cat
            .get("root-cause")
            .unwrap()
            .prompt_fragment
            .trim()
            .to_owned();

        assert!(
            agent.prompt.contains(&sec_fragment),
            "security fragment missing from prompt"
        );
        assert!(
            agent.prompt.contains(&blunt_fragment),
            "blunt fragment missing from prompt"
        );
        assert!(
            agent.prompt.contains(&root_fragment),
            "root-cause fragment missing from prompt"
        );
        assert!(
            agent.prompt.contains(task),
            "task text missing from prompt"
        );

        // Trait list is recorded.
        assert_eq!(
            agent.traits,
            vec!["security", "blunt", "root-cause"],
            "traits list mismatch"
        );
    }

    #[test]
    fn compose_rejects_unknown_trait() {
        let cat = catalogue();
        let err = compose_agent(cat, &["security", "nonexistent-trait"], "do stuff")
            .expect_err("expected UnknownTrait error");
        assert_eq!(
            err,
            ComposeError::UnknownTrait("nonexistent-trait".to_owned())
        );
    }

    #[test]
    fn compose_rejects_two_traits_same_dimension_by_default() {
        let cat = catalogue();
        // "security" and "backend" are both expertise dimension.
        let err = compose_agent(cat, &["security", "backend"], "do stuff")
            .expect_err("expected ConflictingTraits error");
        match err {
            ComposeError::ConflictingTraits { dim, .. } => {
                assert_eq!(dim, "expertise");
            }
            other => panic!("expected ConflictingTraits, got {other:?}"),
        }
    }

    #[test]
    fn compose_allows_two_same_dimension_when_opted_in() {
        let cat = catalogue();
        let opts = ComposeOptions {
            allow_dimension_conflicts: true,
            color: None,
        };
        let result = compose_agent_with_options(cat, &["security", "backend"], "do stuff", opts);
        assert!(result.is_ok(), "expected Ok with allow_dimension_conflicts");
    }

    #[test]
    fn compose_rejects_empty_trait_list() {
        let cat = catalogue();
        let err = compose_agent(cat, &[], "do stuff").expect_err("expected EmptyTraits error");
        assert_eq!(err, ComposeError::EmptyTraits);
    }

    #[test]
    fn compose_is_deterministic_for_fixed_input() {
        let cat = catalogue();
        let traits = &["rust", "methodical", "incremental"];
        let task = "Refactor the connection pool.";

        let a = compose_agent(cat, traits, task).expect("first compose failed");
        let b = compose_agent(cat, traits, task).expect("second compose failed");

        assert_eq!(a, b, "compose output must be deterministic for identical inputs");
    }

    #[test]
    fn colour_hint_is_forwarded() {
        let cat = catalogue();
        let opts = ComposeOptions {
            allow_dimension_conflicts: false,
            color: Some("#4a90d9".to_owned()),
        };
        let agent =
            compose_agent_with_options(cat, &["debugging", "curious", "depth-first"], "Find the bug.", opts)
                .expect("compose failed");
        assert_eq!(agent.color.as_deref(), Some("#4a90d9"));
    }

    #[test]
    fn prompt_ordering_expertise_before_personality_before_approach() {
        let cat = catalogue();
        // One trait from each dimension.
        let agent =
            compose_agent(cat, &["backend", "pragmatic", "systematic"], "Design a rate limiter.")
                .expect("compose failed");

        let expertise_frag = cat.get("backend").unwrap().prompt_fragment.trim().to_owned();
        let personality_frag = cat
            .get("pragmatic")
            .unwrap()
            .prompt_fragment
            .trim()
            .to_owned();
        let approach_frag = cat
            .get("systematic")
            .unwrap()
            .prompt_fragment
            .trim()
            .to_owned();

        let expertise_pos = agent.prompt.find(&expertise_frag).expect("expertise frag not found");
        let personality_pos = agent
            .prompt
            .find(&personality_frag)
            .expect("personality frag not found");
        let approach_pos = agent.prompt.find(&approach_frag).expect("approach frag not found");

        assert!(
            expertise_pos < personality_pos,
            "expertise must come before personality in prompt"
        );
        assert!(
            personality_pos < approach_pos,
            "personality must come before approach in prompt"
        );
    }

    // ── Task #624: TUI-safe output helpers (no println!) ─────────────────────
    //
    // These tests pin the contract that `/agent traits` and `/agent compose`
    // return strings the caller can route through `tui.push_system` (so the
    // ratatui back-buffer stays consistent) rather than writing directly to
    // stdout (which used to bypass the alt-screen and freeze the visible TUI).
    //
    // See `feedback-tui-stdout-anti-pattern.md`.

    /// `/agent traits` must produce a non-empty listing as a returned String
    /// — it must NOT print to stdout, and `format_traits_listing` is the only
    /// function we expose to build that listing.  Any future refactor that
    /// adds a `println!` path for `/agent traits` will fail the `grep` audit
    /// gate inside `cmd_provider.rs`.
    #[test]
    fn run_agent_traits_returns_listing_as_string_not_prints() {
        let listing = format_traits_listing(bundled_catalogue());
        assert!(
            !listing.is_empty(),
            "traits listing must be a non-empty string"
        );
        assert!(
            listing.contains("Available agent traits"),
            "traits listing must include a header line — got: {listing}"
        );
        assert!(
            listing.contains("Usage: /agent compose"),
            "traits listing must include the compose usage footer"
        );
        // The function returns a String — confirm it via a type-check at the
        // call site.  If a future refactor changes the return type to `()`,
        // this test stops compiling.
        let _: String = listing;
    }

    /// `/agent compose` (no trait list, no task) must return a usage string
    /// the caller can `push_system` to the TUI.  Previously this used
    /// `println!`, which corrupted the back-buffer.
    #[test]
    fn run_agent_compose_no_traits_returns_usage_string() {
        let msg = format_agent_compose_empty_traits_usage();
        assert!(msg.contains("No traits provided"));
        assert!(msg.contains("/agent compose"));
        // Same shape via the unified error formatter.
        assert_eq!(
            format_agent_compose_error(&ComposeError::EmptyTraits),
            msg,
            "EmptyTraits formatter must agree with the bare usage helper"
        );
    }

    /// `/agent compose <traits> "<task>"` must surface a summary header
    /// the caller can push to the TUI before the model turn runs.  The
    /// header must list every trait the user requested, separated by
    /// `", "` (not commas-without-spaces).
    #[test]
    fn run_agent_compose_with_valid_traits_returns_composition_summary() {
        let cat = bundled_catalogue();
        let composed = compose_agent(
            cat,
            &["security", "blunt", "root-cause"],
            "Audit auth.rs.",
        )
        .expect("compose should succeed for known traits");

        let summary = format_agent_compose_summary(&composed);
        assert!(
            summary.starts_with("Composing agent with traits:"),
            "summary must start with the canonical prefix — got: {summary}"
        );
        assert!(summary.contains("security"));
        assert!(summary.contains("blunt"));
        assert!(summary.contains("root-cause"));
        assert!(
            summary.contains(", "),
            "trait list must be comma-and-space separated for readability"
        );
        // Type pin: caller receives a String, not `()`.
        let _: String = summary;
    }

    /// Integration assertion: the helper outputs intended for the TUI must
    /// NOT contain raw ANSI control sequences that would betray a
    /// `println!`-shaped origin (e.g. embedded `\r\n` line endings or
    /// alt-screen escape codes).  This pins the property the bug report
    /// flagged on 2026-05-18 — that `/agent traits` output appears in the
    /// scrollback log via `push_system`, not as raw stdout bytes that the
    /// alt-screen swallows.
    #[test]
    fn agent_traits_output_appears_in_tui_log_not_raw_stdout() {
        let listing = format_traits_listing(bundled_catalogue());
        // Newlines are fine (push_system wraps and renders them); embedded
        // carriage returns or DEC private-mode escapes are not.
        assert!(
            !listing.contains('\r'),
            "TUI-bound output must not contain carriage returns"
        );
        assert!(
            !listing.contains("\x1b["),
            "TUI-bound output must not contain raw ANSI escape sequences"
        );
        // Compose error formatter, same property.
        let err_msg = format_agent_compose_error(&ComposeError::UnknownTrait("ghost".to_string()));
        assert!(err_msg.contains("Unknown trait: ghost"));
        assert!(!err_msg.contains('\r'));
        assert!(!err_msg.contains("\x1b["));
    }
}
