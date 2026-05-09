//! Skill-chaining engine for Anvil v2.3.0.
//!
//! # Design: suggest-not-auto
//!
//! Chains are NEVER auto-injected.  The evaluator returns a list of
//! [`ChainCandidate`] values that the TUI or REPL layer presents to the user
//! with a `[chain via <skill>]` annotation.  The user still runs
//! `/skill load <name>` explicitly — same as for top-level trigger matches.
//!
//! # Frontmatter extension
//!
//! A skill's SKILL.md can declare chained skills:
//!
//! ```yaml
//! chains_to:
//!   - skill: token-economy
//!     when: always
//!   - skill: file-fingerprint
//!     when: "if-keyword: cat"
//!   - skill: security-audit
//!     when: "if-skill-loaded: code-review"
//! ```
//!
//! String shorthand (assumes `when: always`):
//!
//! ```yaml
//! chains_to: [skill-a, skill-b]
//! ```
//!
//! # Limits
//!
//! - Depth ≤ 3 (configurable via [`ChainEvaluator::max_depth`]).
//! - Accumulated SKILL.md byte count ≤ 25 000 (configurable).
//! - Each skill may chain to at most 5 others (anti-spam, configurable).
//! - Cycle detection: if A → B → A, the second visit to A is skipped.
//! - Skills already loaded are not returned as candidates.
//! - Broken chain references (skill not in `all_skills`) are logged and skipped.

use std::collections::{HashMap, HashSet};

use crate::agents::SkillSummary;

// ─── ChainWhen ───────────────────────────────────────────────────────────────

/// Condition under which a `chains_to` entry is activated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainWhen {
    /// Always suggest the chained skill.
    Always,
    /// Suggest only when the original prompt contains `keyword`
    /// (whole-word, case-insensitive).
    IfKeyword(String),
    /// Suggest only when skill `name` is currently loaded.
    IfSkillLoaded(String),
}

impl ChainWhen {
    /// Parse the `when:` value string from YAML frontmatter.
    ///
    /// Recognises:
    /// - `"always"` → [`ChainWhen::Always`]
    /// - `"if-keyword: foo"` → [`ChainWhen::IfKeyword("foo")`]
    /// - `"if-skill-loaded: bar"` → [`ChainWhen::IfSkillLoaded("bar")`]
    ///
    /// Unknown patterns fall back to [`ChainWhen::Always`] for forward
    /// compatibility (the field is silently tolerated, not a fatal error).
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let v = value.trim();
        if v.eq_ignore_ascii_case("always") {
            return Self::Always;
        }
        if let Some(rest) = v.strip_prefix("if-keyword:") {
            return Self::IfKeyword(rest.trim().to_ascii_lowercase());
        }
        if let Some(rest) = v.strip_prefix("if-skill-loaded:") {
            return Self::IfSkillLoaded(rest.trim().to_string());
        }
        // Unknown clause — treat as always for forward compatibility.
        Self::Always
    }

    /// Evaluate whether this condition fires given the current context.
    #[must_use]
    pub fn matches(&self, prompt: &str, loaded_skill_names: &HashSet<String>) -> bool {
        match self {
            Self::Always => true,
            Self::IfKeyword(kw) => {
                crate::skill_triggers::whole_word_match_pub(&prompt.to_ascii_lowercase(), kw)
            }
            Self::IfSkillLoaded(name) => {
                loaded_skill_names.contains(&name.to_ascii_lowercase())
            }
        }
    }
}

// ─── ChainEntry ──────────────────────────────────────────────────────────────

/// A single entry in a skill's `chains_to:` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainEntry {
    /// Target skill name.
    pub skill: String,
    /// Condition for suggestion.
    pub when: ChainWhen,
}

// ─── ChainCandidate ───────────────────────────────────────────────────────────

/// A skill recommended as a chain candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCandidate {
    /// The skill that should be suggested.
    pub skill_name: String,
    /// The skill that declared the chain to `skill_name`.
    pub triggered_by: String,
    /// Human-readable description of why this fires.
    pub reason: String,
    /// Chain depth: 1 = direct chain from a loaded skill, 2 = chain-of-chain, etc.
    pub depth: usize,
}

// ─── ChainEvaluator ──────────────────────────────────────────────────────────

/// Evaluates chain candidates for a given set of loaded skills.
#[derive(Debug, Clone)]
pub struct ChainEvaluator {
    /// Maximum recursion depth.  Default: 3.
    pub max_depth: usize,
    /// Maximum accumulated SKILL.md byte count before stopping emission.
    /// Default: 25 000.
    pub max_total_bytes: usize,
    /// Maximum number of `chains_to` entries honored per skill (anti-spam).
    /// Default: 5.
    pub max_chain_per_skill: usize,
}

impl Default for ChainEvaluator {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_total_bytes: 25_000,
            max_chain_per_skill: 5,
        }
    }
}

impl ChainEvaluator {
    /// Create an evaluator with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate chain candidates.
    ///
    /// # Arguments
    ///
    /// * `loaded_skills` — skills currently active in the session.
    /// * `all_skills`    — all discovered skills (name → `SkillSummary`).
    /// * `prompt`        — the user's current prompt text.
    ///
    /// # Returns
    ///
    /// A deduplicated, depth-ordered `Vec<ChainCandidate>`.  Skills already
    /// in `loaded_skills` are excluded.  The list is empty when no chains fire.
    #[must_use]
    pub fn evaluate(
        &self,
        loaded_skills: &[SkillSummary],
        all_skills: &HashMap<String, SkillSummary>,
        prompt: &str,
    ) -> Vec<ChainCandidate> {
        // Build a set of currently loaded skill names (lower-cased).
        let loaded_names: HashSet<String> = loaded_skills
            .iter()
            .map(|s| s.name.to_ascii_lowercase())
            .collect();

        let mut candidates: Vec<ChainCandidate> = Vec::new();
        // Track accumulated byte count of referenced skills.
        let mut accumulated_bytes: usize = 0;
        // Track which skills have already been emitted as candidates (dedup + cycle).
        let mut emitted: HashSet<String> = HashSet::new();

        for loaded in loaded_skills {
            // Visit the chain starting at each loaded skill.
            self.walk(
                loaded,
                1,
                all_skills,
                &loaded_names,
                prompt,
                &mut accumulated_bytes,
                &mut emitted,
                &mut candidates,
            );
        }

        candidates
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        skill: &SkillSummary,
        depth: usize,
        all_skills: &HashMap<String, SkillSummary>,
        loaded_names: &HashSet<String>,
        prompt: &str,
        accumulated_bytes: &mut usize,
        emitted: &mut HashSet<String>,
        candidates: &mut Vec<ChainCandidate>,
    ) {
        if depth > self.max_depth {
            return;
        }
        if *accumulated_bytes >= self.max_total_bytes {
            return;
        }

        // Build the loaded-name set for `if-skill-loaded:` checks.
        // We include both the original loaded names AND any already emitted
        // candidates (they might become loaded).
        let loaded_set: HashSet<String> = loaded_names
            .iter()
            .cloned()
            .chain(emitted.iter().cloned())
            .collect();

        let chains = &skill.chains_to;
        let capped = &chains[..chains.len().min(self.max_chain_per_skill)];

        for entry in capped {
            let target_key = entry.skill.to_ascii_lowercase();

            // Skip already-loaded skills.
            if loaded_names.contains(&target_key) {
                continue;
            }

            // Evaluate `when` clause.
            if !entry.when.matches(prompt, &loaded_set) {
                continue;
            }

            // Broken chain reference?
            let Some(target_skill) = all_skills.get(&target_key) else {
                // Log to stderr (non-blocking — we don't want to crash the TUI).
                eprintln!(
                    "[anvil skill-chaining] warning: skill '{}' chains_to '{}' but that skill \
                     was not found in the discovery results — skipping.",
                    skill.name, entry.skill
                );
                continue;
            };

            // Cycle / duplicate guard.
            if emitted.contains(&target_key) {
                continue;
            }

            // Byte budget: approximate cost by the target's body size.
            let body_bytes = target_skill
                .body_bytes
                .unwrap_or(0);
            if *accumulated_bytes + body_bytes > self.max_total_bytes && *accumulated_bytes > 0 {
                // Budget exhausted — stop emitting further candidates.
                return;
            }
            *accumulated_bytes += body_bytes;

            let reason = match &entry.when {
                ChainWhen::Always => "always".to_string(),
                ChainWhen::IfKeyword(kw) => format!("keyword: {kw}"),
                ChainWhen::IfSkillLoaded(name) => format!("if-skill-loaded: {name}"),
            };

            emitted.insert(target_key.clone());
            candidates.push(ChainCandidate {
                skill_name: target_skill.name.clone(),
                triggered_by: skill.name.clone(),
                reason,
                depth,
            });

            // Recurse.
            self.walk(
                target_skill,
                depth + 1,
                all_skills,
                loaded_names,
                prompt,
                accumulated_bytes,
                emitted,
                candidates,
            );
        }
    }
}

// ─── Render helper ────────────────────────────────────────────────────────────

/// Render a `/skill chains` graph listing for the current skill discovery set.
///
/// Each skill with a non-empty `chains_to` list is shown with its chain
/// entries and their `when` clauses.
#[must_use]
pub fn render_chains_graph(all_skills: &HashMap<String, SkillSummary>) -> String {
    let mut skills_with_chains: Vec<&SkillSummary> = all_skills
        .values()
        .filter(|s| !s.chains_to.is_empty())
        .collect();
    skills_with_chains.sort_by(|a, b| a.name.cmp(&b.name));

    if skills_with_chains.is_empty() {
        return "No skills declare chains_to entries.".to_string();
    }

    let mut lines = vec!["Skill chain graph:".to_string(), String::new()];

    for skill in skills_with_chains {
        lines.push(format!("  {}:", skill.name));
        for entry in &skill.chains_to {
            let when_str = match &entry.when {
                ChainWhen::Always => "always".to_string(),
                ChainWhen::IfKeyword(kw) => format!("if-keyword: {kw}"),
                ChainWhen::IfSkillLoaded(name) => format!("if-skill-loaded: {name}"),
            };
            lines.push(format!("    → {} ({})", entry.skill, when_str));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Format chain candidates for display in `/skill suggest` output.
///
/// Returns `None` when `candidates` is empty (nothing to append).
#[must_use]
pub fn format_chain_candidates(candidates: &[ChainCandidate]) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }

    let max_name = candidates.iter().map(|c| c.skill_name.len()).max().unwrap_or(0);
    let mut lines = vec![String::new(), "Chain suggestions:".to_string()];

    for c in candidates {
        let padded = format!("{:<width$}", c.skill_name, width = max_name);
        lines.push(format!(
            "  {padded}    [chain via {}] ({}) — /skill load {}",
            c.triggered_by, c.reason, c.skill_name
        ));
    }

    Some(lines.join("\n"))
}

/// Build a TUI-format auto-hint for chain candidates at turn-start.
///
/// Returns `None` when no candidates exist.
#[must_use]
pub fn format_chain_hint(loaded_skill_name: &str, candidates: &[ChainCandidate]) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }

    let parts: Vec<String> = candidates
        .iter()
        .map(|c| format!("{} ({})", c.skill_name, c.reason))
        .collect();

    Some(format!(
        "\u{1f4a1} You loaded `{}`; it chains to {}. `/skill load <name>` to add.",
        loaded_skill_name,
        parts.join(" and ")
    ))
}

// ─── Frontmatter parsing ─────────────────────────────────────────────────────

/// Parse the `chains_to:` block from YAML frontmatter text.
///
/// Supports both:
/// - Full object form:
///   ```yaml
///   chains_to:
///     - skill: token-economy
///       when: always
///   ```
/// - String shorthand (assumes `when: always`):
///   ```yaml
///   chains_to: [skill-a, skill-b]
///   ```
///
/// Unknown / malformed entries are skipped with a debug message.
#[must_use]
pub fn parse_chains_to(frontmatter_block: &str) -> Vec<ChainEntry> {
    // Find the `chains_to:` key.
    let lines: Vec<&str> = frontmatter_block.lines().collect();
    let mut result: Vec<ChainEntry> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("chains_to:") {
            let rest = rest.trim();
            if rest.starts_with('[') {
                // Inline list form.
                result.extend(parse_inline_chains_to(rest));
            } else if rest.is_empty() {
                // Block list form — gather indented `- skill:` sub-blocks.
                i += 1;
                while i < lines.len() {
                    let sub = lines[i];
                    let sub_trimmed = sub.trim();
                    // End of block list.
                    if !sub.starts_with(' ') && !sub.starts_with('\t') && !sub_trimmed.is_empty() {
                        break;
                    }
                    if sub_trimmed == "---" {
                        break;
                    }
                    if let Some(rest2) = sub_trimmed.strip_prefix("- skill:") {
                        // Found a list item.  Collect sibling `when:` if present.
                        let skill_name = crate::agents::unquote_frontmatter_value_pub(rest2.trim()).to_string();
                        if skill_name.is_empty() {
                            i += 1;
                            continue;
                        }
                        let mut when_str = "always".to_string();
                        // Peek at next line(s) for `when:`.
                        let mut j = i + 1;
                        while j < lines.len() {
                            let peek = lines[j];
                            let peek_t = peek.trim();
                            if peek_t.starts_with("- ") || (!peek.starts_with(' ') && !peek.starts_with('\t') && !peek_t.is_empty()) {
                                break;
                            }
                            if let Some(w) = peek_t.strip_prefix("when:") {
                                when_str = crate::agents::unquote_frontmatter_value_pub(w.trim()).to_string();
                                j += 1;
                                break;
                            }
                            j += 1;
                        }
                        result.push(ChainEntry {
                            skill: skill_name,
                            when: ChainWhen::parse(&when_str),
                        });
                        i = j;
                        continue;
                    } else if sub_trimmed.starts_with("- ") {
                        // String shorthand inside block form: `- skill-name`
                        let name = crate::agents::unquote_frontmatter_value_pub(
                            sub_trimmed.trim_start_matches('-').trim()
                        ).to_string();
                        if !name.is_empty() && !name.contains(':') {
                            result.push(ChainEntry {
                                skill: name,
                                when: ChainWhen::Always,
                            });
                        } else if name.contains(':') {
                            // Could be a `skill:` key that wasn't matched above — skip
                            eprintln!(
                                "[anvil skill-chaining] warning: malformed chains_to entry '{}' — skipping",
                                name
                            );
                        }
                    }
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }

    result
}

fn parse_inline_chains_to(rest: &str) -> Vec<ChainEntry> {
    // rest is like `[skill-a, skill-b]` or `[{skill: foo, when: always}]`
    let inner = rest
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();

    if inner.is_empty() {
        return vec![];
    }

    // If no `{` present, it's a flat list of skill names.
    if !inner.contains('{') {
        return inner
            .split(',')
            .filter_map(|item| {
                let name = crate::agents::unquote_frontmatter_value_pub(item.trim()).to_string();
                if name.is_empty() {
                    None
                } else {
                    Some(ChainEntry {
                        skill: name,
                        when: ChainWhen::Always,
                    })
                }
            })
            .collect();
    }

    // Object form not supported in inline (YAML spec requires block for nested
    // objects).  Fall back to treating each element as a plain skill name, warn
    // if something unexpected appears.
    eprintln!(
        "[anvil skill-chaining] warning: inline chains_to with object form is not supported; \
         use block list form instead. Input: {rest}"
    );
    vec![]
}

// ─── Public re-export of whole_word_match for ChainWhen::IfKeyword ───────────
// (We add a pub wrapper in skill_triggers.rs — see below)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{DefinitionSource, SkillOrigin, SkillSummary};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_skill(name: &str, chains: Vec<ChainEntry>, body_bytes: usize) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            description: None,
            triggers: vec![],
            chains_to: chains,
            body_bytes: Some(body_bytes),
            source: DefinitionSource::Bundled,
            shadowed_by: None,
            origin: SkillOrigin::SkillsDir,
        }
    }

    fn skill_map(skills: Vec<SkillSummary>) -> HashMap<String, SkillSummary> {
        skills
            .into_iter()
            .map(|s| (s.name.to_ascii_lowercase(), s))
            .collect()
    }

    fn evaluator() -> ChainEvaluator {
        ChainEvaluator::default()
    }

    // ── Test 1: always clause ────────────────────────────────────────────────

    #[test]
    fn chain_with_always_clause_always_suggests() {
        let code_review = make_skill(
            "code-review",
            vec![ChainEntry { skill: "security-audit".into(), when: ChainWhen::Always }],
            1000,
        );
        let security_audit = make_skill("security-audit", vec![], 1200);

        let loaded = vec![code_review.clone()];
        let all = skill_map(vec![code_review, security_audit]);

        let candidates = evaluator().evaluate(&loaded, &all, "please review my code");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].skill_name, "security-audit");
        assert_eq!(candidates[0].triggered_by, "code-review");
        assert_eq!(candidates[0].reason, "always");
        assert_eq!(candidates[0].depth, 1);
    }

    // ── Test 2: if-keyword — fires ───────────────────────────────────────────

    #[test]
    fn chain_with_if_keyword_only_suggests_when_prompt_contains_keyword() {
        let loader = make_skill(
            "file-ops",
            vec![ChainEntry {
                skill: "fingerprint".into(),
                when: ChainWhen::IfKeyword("cat".into()),
            }],
            500,
        );
        let fingerprint = make_skill("fingerprint", vec![], 400);

        let loaded = vec![loader.clone()];
        let all = skill_map(vec![loader, fingerprint]);

        // Should NOT suggest when prompt doesn't contain "cat".
        let no_match = evaluator().evaluate(&loaded, &all, "show file contents");
        assert!(
            no_match.is_empty(),
            "expected no candidate when keyword absent: {no_match:?}"
        );

        // Should suggest when prompt contains whole-word "cat".
        let yes_match = evaluator().evaluate(&loaded, &all, "cat the config file");
        assert_eq!(yes_match.len(), 1);
        assert_eq!(yes_match[0].skill_name, "fingerprint");
        assert_eq!(yes_match[0].reason, "keyword: cat");
    }

    // ── Test 3: if-skill-loaded ───────────────────────────────────────────────

    #[test]
    fn chain_with_if_skill_loaded_only_suggests_when_other_skill_loaded() {
        let a = make_skill(
            "skill-a",
            vec![ChainEntry {
                skill: "skill-c".into(),
                when: ChainWhen::IfSkillLoaded("skill-b".into()),
            }],
            300,
        );
        let c = make_skill("skill-c", vec![], 300);

        // Without skill-b loaded: no candidate.
        let loaded_no_b = vec![a.clone()];
        let all = skill_map(vec![a.clone(), c.clone()]);
        let no_c = evaluator().evaluate(&loaded_no_b, &all, "any prompt");
        assert!(no_c.is_empty(), "expected no candidate: {no_c:?}");

        // With skill-b loaded.
        let b = make_skill("skill-b", vec![], 200);
        let loaded_with_b = vec![a, b.clone()];
        let all2 = skill_map(vec![
            make_skill("skill-a", vec![
                ChainEntry {
                    skill: "skill-c".into(),
                    when: ChainWhen::IfSkillLoaded("skill-b".into()),
                }
            ], 300),
            b,
            c,
        ]);
        let with_c = evaluator().evaluate(&loaded_with_b, &all2, "any prompt");
        // skill-a chains to skill-c when skill-b loaded.
        let names: Vec<&str> = with_c.iter().map(|c| c.skill_name.as_str()).collect();
        assert!(names.contains(&"skill-c"), "expected skill-c in {names:?}");
    }

    // ── Test 4: depth budget ──────────────────────────────────────────────────

    #[test]
    fn depth_budget_exceeded_stops_recursion() {
        // A → B → C → D; max_depth = 2 → only A→B and B→C should appear, not C→D.
        let a = make_skill(
            "a",
            vec![ChainEntry { skill: "b".into(), when: ChainWhen::Always }],
            100,
        );
        let b = make_skill(
            "b",
            vec![ChainEntry { skill: "c".into(), when: ChainWhen::Always }],
            100,
        );
        let c = make_skill(
            "c",
            vec![ChainEntry { skill: "d".into(), when: ChainWhen::Always }],
            100,
        );
        let d = make_skill("d", vec![], 100);

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a, b, c, d]);

        let ev = ChainEvaluator { max_depth: 2, ..Default::default() };
        let candidates = ev.evaluate(&loaded, &all, "go");
        let names: Vec<&str> = candidates.iter().map(|c| c.skill_name.as_str()).collect();
        assert!(names.contains(&"b"), "expected b in {names:?}");
        assert!(names.contains(&"c"), "expected c in {names:?}");
        assert!(!names.contains(&"d"), "d should be cut off at depth 3: {names:?}");
    }

    // ── Test 5: total bytes budget ────────────────────────────────────────────

    #[test]
    fn total_bytes_budget_stops_emission() {
        let a = make_skill(
            "a",
            vec![
                ChainEntry { skill: "big1".into(), when: ChainWhen::Always },
                ChainEntry { skill: "big2".into(), when: ChainWhen::Always },
            ],
            100,
        );
        let big1 = make_skill("big1", vec![], 20_000);
        let big2 = make_skill("big2", vec![], 20_000);

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a, big1, big2]);

        let ev = ChainEvaluator {
            max_total_bytes: 25_000,
            ..Default::default()
        };
        let candidates = ev.evaluate(&loaded, &all, "go");
        // big1 (20KB) fits; big2 (another 20KB) exceeds the 25KB limit.
        assert_eq!(candidates.len(), 1, "expected only big1, got: {candidates:?}");
        assert_eq!(candidates[0].skill_name, "big1");
    }

    // ── Test 6: cycle detection ───────────────────────────────────────────────

    #[test]
    fn cycle_detection_prevents_infinite_loop() {
        let a = make_skill(
            "a",
            vec![ChainEntry { skill: "b".into(), when: ChainWhen::Always }],
            200,
        );
        let b = make_skill(
            "b",
            vec![ChainEntry { skill: "a".into(), when: ChainWhen::Always }],
            200,
        );

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a, b]);

        let candidates = evaluator().evaluate(&loaded, &all, "go");
        // a is loaded → suggests b (depth 1); b chains back to a, but a is loaded → skipped.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].skill_name, "b");
    }

    // ── Test 7: broken reference ──────────────────────────────────────────────

    #[test]
    fn broken_reference_logs_warning_and_skips() {
        let a = make_skill(
            "a",
            vec![ChainEntry { skill: "ghost".into(), when: ChainWhen::Always }],
            200,
        );

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a]); // "ghost" is not in all_skills

        let candidates = evaluator().evaluate(&loaded, &all, "go");
        // ghost is missing → skipped, no panic.
        assert!(candidates.is_empty(), "expected empty, got {candidates:?}");
    }

    // ── Test 8: already-loaded skill not returned ─────────────────────────────

    #[test]
    fn already_loaded_skill_not_returned_as_candidate() {
        let a = make_skill(
            "a",
            vec![ChainEntry { skill: "b".into(), when: ChainWhen::Always }],
            200,
        );
        let b = make_skill("b", vec![], 200);

        let loaded = vec![a.clone(), b.clone()]; // b already loaded
        let all = skill_map(vec![a, b]);

        let candidates = evaluator().evaluate(&loaded, &all, "go");
        assert!(
            candidates.iter().all(|c| c.skill_name != "b"),
            "b should not be a candidate since it's already loaded: {candidates:?}"
        );
    }

    // ── Test 9: max_chain_per_skill anti-spam ─────────────────────────────────

    #[test]
    fn chains_to_capped_at_max_chain_per_skill() {
        let chains: Vec<ChainEntry> = (0..10u32)
            .map(|n| ChainEntry {
                skill: format!("skill-{n}"),
                when: ChainWhen::Always,
            })
            .collect();
        let parent = make_skill("parent", chains, 500);
        let targets: Vec<SkillSummary> = (0..10u32)
            .map(|n| make_skill(&format!("skill-{n}"), vec![], 100))
            .collect();

        let loaded = vec![parent.clone()];
        let mut all = skill_map(targets);
        all.insert("parent".to_string(), parent);

        let ev = ChainEvaluator {
            max_chain_per_skill: 5,
            ..Default::default()
        };
        let candidates = ev.evaluate(&loaded, &all, "go");
        assert!(
            candidates.len() <= 5,
            "expected ≤5 candidates, got {}: {candidates:?}",
            candidates.len()
        );
    }

    // ── Test 10: frontmatter full object form ─────────────────────────────────

    #[test]
    fn frontmatter_parses_chains_to_with_full_object_form() {
        let fm = "\
---
name: my-skill
chains_to:
  - skill: token-economy
    when: always
  - skill: file-fingerprint
    when: \"if-keyword: cat\"
---
";
        let entries = parse_chains_to(fm);
        assert_eq!(entries.len(), 2, "expected 2 entries: {entries:?}");
        assert_eq!(entries[0].skill, "token-economy");
        assert_eq!(entries[0].when, ChainWhen::Always);
        assert_eq!(entries[1].skill, "file-fingerprint");
        assert_eq!(entries[1].when, ChainWhen::IfKeyword("cat".into()));
    }

    // ── Test 11: frontmatter string shorthand ─────────────────────────────────

    #[test]
    fn frontmatter_parses_chains_to_with_string_shorthand() {
        let fm = "\
---
name: my-skill
chains_to: [skill-a, skill-b]
---
";
        let entries = parse_chains_to(fm);
        assert_eq!(entries.len(), 2, "expected 2 entries: {entries:?}");
        assert_eq!(entries[0].skill, "skill-a");
        assert_eq!(entries[0].when, ChainWhen::Always);
        assert_eq!(entries[1].skill, "skill-b");
        assert_eq!(entries[1].when, ChainWhen::Always);
    }

    // ── Test 12: missing chains_to ────────────────────────────────────────────

    #[test]
    fn frontmatter_missing_chains_to_returns_empty_list() {
        let fm = "\
---
name: my-skill
triggers: [review]
---
";
        let entries = parse_chains_to(fm);
        assert!(entries.is_empty(), "expected empty: {entries:?}");
    }

    // ── Test 13: malformed chains_to ──────────────────────────────────────────

    #[test]
    fn frontmatter_malformed_chains_to_logged_skipped_doesnt_crash() {
        let fm = "\
---
name: my-skill
chains_to: !!binary \"bad\"
---
";
        // Must not panic; returns empty.
        let entries = parse_chains_to(fm);
        // Either empty or best-effort, but no crash.
        let _ = entries; // result is ignored — the contract is "no crash"
    }

    // ── Test 14: two-level deep walk ──────────────────────────────────────────

    #[test]
    fn evaluator_walks_two_levels_deep() {
        // A → B → C; A is loaded, expect B at depth 1, C at depth 2.
        let a = make_skill(
            "a",
            vec![ChainEntry { skill: "b".into(), when: ChainWhen::Always }],
            100,
        );
        let b = make_skill(
            "b",
            vec![ChainEntry { skill: "c".into(), when: ChainWhen::Always }],
            100,
        );
        let c = make_skill("c", vec![], 100);

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a, b, c]);
        let candidates = evaluator().evaluate(&loaded, &all, "go");

        let b_c = candidates.iter().find(|c| c.skill_name == "b");
        let c_c = candidates.iter().find(|c| c.skill_name == "c");

        assert!(b_c.is_some(), "expected b in candidates: {candidates:?}");
        assert!(c_c.is_some(), "expected c in candidates: {candidates:?}");
        assert_eq!(b_c.unwrap().depth, 1);
        assert_eq!(c_c.unwrap().depth, 2);
    }

    // ── Test 15: three-level capped ───────────────────────────────────────────

    #[test]
    fn evaluator_walks_three_levels_deep_caps_at_three() {
        // A → B → C → D; default max_depth=3 should include D at depth 3 but
        // stop there (E at depth 4 must not appear).
        let a = make_skill("a", vec![ChainEntry { skill: "b".into(), when: ChainWhen::Always }], 100);
        let b = make_skill("b", vec![ChainEntry { skill: "c".into(), when: ChainWhen::Always }], 100);
        let c = make_skill("c", vec![ChainEntry { skill: "d".into(), when: ChainWhen::Always }], 100);
        let d = make_skill("d", vec![ChainEntry { skill: "e".into(), when: ChainWhen::Always }], 100);
        let e = make_skill("e", vec![], 100);

        let loaded = vec![a.clone()];
        let all = skill_map(vec![a, b, c, d, e]);
        let candidates = evaluator().evaluate(&loaded, &all, "go");

        let names: Vec<&str> = candidates.iter().map(|c| c.skill_name.as_str()).collect();
        assert!(names.contains(&"b"), "expected b: {names:?}");
        assert!(names.contains(&"c"), "expected c: {names:?}");
        assert!(names.contains(&"d"), "expected d at depth 3: {names:?}");
        assert!(!names.contains(&"e"), "e beyond depth 3 must be excluded: {names:?}");

        let d_entry = candidates.iter().find(|c| c.skill_name == "d").unwrap();
        assert_eq!(d_entry.depth, 3);
    }

    // ── Test 16: /skill chains renders graph ──────────────────────────────────

    #[test]
    fn slash_skill_chains_command_renders_graph() {
        let a = make_skill(
            "code-review",
            vec![ChainEntry { skill: "security-audit".into(), when: ChainWhen::Always }],
            500,
        );
        let b = make_skill("security-audit", vec![], 800);
        let all = skill_map(vec![a, b]);

        let output = render_chains_graph(&all);
        assert!(output.contains("code-review:"), "expected skill name: {output}");
        assert!(output.contains("security-audit"), "expected target: {output}");
        assert!(output.contains("always"), "expected when clause: {output}");
    }
}
