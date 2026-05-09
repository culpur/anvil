//! Trigger-keyword matching for Anvil's skill suggestion system.
//!
//! # Design rationale: suggested, not auto-injected
//!
//! Miessler's Personal_AI_Infrastructure and SuperClaude both auto-inject skill
//! content whenever a trigger keyword appears in user input.  That pattern is
//! safe in single-user, trusted-context tools, but Anvil ships to a general
//! audience.  Auto-injection creates a prompt-injection attack vector: a
//! carefully crafted user prompt (or any text the user pastes in) could contain
//! a trigger word and silently alter Anvil's behaviour — without the user being
//! aware that a skill has been loaded.
//!
//! This implementation exposes trigger matching as a pure engine only.
//! The caller (the TUI or REPL layer) is responsible for surfacing the match to
//! the user and asking for explicit confirmation before loading the skill.
//! This keeps the signal/noise ratio low and the user in control.
//!
//! # Matching rules
//!
//! - **Whole-word only** — "audit" matches in "please audit my code" but NOT
//!   in "contest", "testing", or "latest".  Substring matching is too noisy.
//! - **Case-insensitive** — triggers and prompt are both lowercased before
//!   comparison.
//! - **No regex** — word boundaries are detected by checking that the character
//!   before the match (if any) and the character after it (if any) are
//!   non-alphanumeric, non-hyphen, non-underscore.  Simple and footgun-free.
//! - **No async** — pure synchronous function; zero allocations beyond the
//!   result `Vec`.

use crate::agents::SkillSummary;

/// A skill whose trigger keyword matched the user's prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerMatch {
    /// The skill's canonical name (e.g. `"security-audit"`).
    pub skill_name: String,
    /// The exact trigger keyword that matched (lowercased).
    pub matched_trigger: String,
}

/// Scan `prompt` for whole-word, case-insensitive matches against each skill's
/// declared trigger keywords.
///
/// Returns one [`TriggerMatch`] per matching skill, deduplicated by
/// `skill_name` (the first matching trigger for each skill is reported).
/// Skills with an empty `triggers` list are silently skipped.
///
/// # Arguments
///
/// * `prompt` — the raw text the user typed (not yet sent to the API).
/// * `skills` — the slice of loaded [`SkillSummary`] values to test.
///
/// # Examples
///
/// ```rust
/// use commands::skill_triggers::{match_triggers, TriggerMatch};
/// use commands::agents::{SkillSummary, DefinitionSource, SkillOrigin};
///
/// let skill = SkillSummary {
///     name: "security-audit".to_string(),
///     description: None,
///     triggers: vec!["audit".to_string()],
///     chains_to: vec![],
///     body_bytes: None,
///     source: DefinitionSource::Bundled,
///     shadowed_by: None,
///     origin: SkillOrigin::SkillsDir,
/// };
/// let matches = match_triggers("please audit my code", &[&skill]);
/// assert_eq!(matches.len(), 1);
/// assert_eq!(matches[0].skill_name, "security-audit");
/// assert_eq!(matches[0].matched_trigger, "audit");
/// ```
pub fn match_triggers<'a>(prompt: &str, skills: &[&'a SkillSummary]) -> Vec<TriggerMatch> {
    if prompt.is_empty() {
        return vec![];
    }

    let prompt_lower = prompt.to_ascii_lowercase();
    let mut results: Vec<TriggerMatch> = Vec::new();

    'skill: for skill in skills {
        if skill.triggers.is_empty() {
            continue;
        }
        for trigger in &skill.triggers {
            if trigger.is_empty() {
                continue;
            }
            if whole_word_match(&prompt_lower, trigger) {
                // One match per skill — report the first hit and move on.
                results.push(TriggerMatch {
                    skill_name: skill.name.clone(),
                    matched_trigger: trigger.clone(),
                });
                continue 'skill;
            }
        }
    }

    results
}

/// Returns `true` when `needle` appears in `haystack` as a whole word.
///
/// Both arguments are assumed to be already lowercased.
///
/// A "word boundary" here means that the characters immediately before and
/// after the match (if they exist) are not word-constituent characters.
/// Word-constituent: ASCII alphanumeric, `-`, `_`.
///
/// This keeps multi-word trigger phrases like "code review" working: the space
/// in the trigger is not a word constituent, so the boundary check only applies
/// to the first character before the phrase and the first character after it.

/// Public wrapper exposing `whole_word_match` to other crates.
pub fn whole_word_match_pub(haystack: &str, needle: &str) -> bool {
    whole_word_match(haystack, needle)
}

fn whole_word_match(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let needle_len = n.len();
    if needle_len == 0 || haystack.len() < needle_len {
        return false;
    }

    let limit = haystack.len() - needle_len;
    let mut i = 0;
    while i <= limit {
        if h[i..i + needle_len] == *n {
            let before_ok = i == 0 || !is_word_char(h[i - 1]);
            let after_ok = i + needle_len >= h.len() || !is_word_char(h[i + needle_len]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Returns `true` for ASCII characters that are considered part of a word for
/// boundary detection: alphanumeric, hyphen, underscore.
#[inline]
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Filter `skills` by a user-supplied query string for the type-to-filter
/// search box on the `/skill suggest` picker.
///
/// Matching is **case-insensitive substring** against either:
///   1. The skill's `name`, or
///   2. Any of the skill's declared `triggers`.
///
/// An empty / whitespace-only `query` is a no-op: every skill is returned in
/// the original order.  Otherwise, the input order of `skills` is preserved
/// among the survivors.
///
/// This deliberately does NOT use the whole-word matcher used by
/// [`match_triggers`].  The picker filter is a search affordance (the user is
/// typing a partial token to narrow a long list), not a behavioural decision
/// about whether to load a skill, so substring is the correct UX.
///
/// # Examples
///
/// ```rust
/// use commands::skill_triggers::filter_skills;
/// use commands::agents::{SkillSummary, DefinitionSource, SkillOrigin};
///
/// fn s(name: &str) -> SkillSummary {
///     SkillSummary {
///         name: name.to_string(),
///         description: None,
///         triggers: vec![],
///         chains_to: vec![],
///         body_bytes: None,
///         source: DefinitionSource::Bundled,
///         shadowed_by: None,
///         origin: SkillOrigin::SkillsDir,
///     }
/// }
/// let all = [s("file"), s("git"), s("filesystem")];
/// let refs: Vec<&SkillSummary> = all.iter().collect();
/// let hits = filter_skills("fil", &refs);
/// assert_eq!(hits.len(), 2);
/// ```
#[must_use]
pub fn filter_skills<'a>(query: &str, skills: &[&'a SkillSummary]) -> Vec<&'a SkillSummary> {
    let q = query.trim();
    if q.is_empty() {
        return skills.to_vec();
    }
    let q_lower = q.to_ascii_lowercase();

    skills
        .iter()
        .copied()
        .filter(|skill| {
            if skill.name.to_ascii_lowercase().contains(&q_lower) {
                return true;
            }
            skill
                .triggers
                .iter()
                .any(|t| t.to_ascii_lowercase().contains(&q_lower))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{DefinitionSource, SkillOrigin, SkillSummary};

    fn make_skill(name: &str, triggers: &[&str]) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            description: None,
            triggers: triggers.iter().map(|s| s.to_ascii_lowercase()).collect(),
            chains_to: vec![],
            body_bytes: None,
            source: DefinitionSource::Bundled,
            shadowed_by: None,
            origin: SkillOrigin::SkillsDir,
        }
    }

    // --- whole-word positive cases ---

    #[test]
    fn matches_whole_word_at_start() {
        let s = make_skill("security-audit", &["audit"]);
        let r = match_triggers("audit my code", &[&s]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].matched_trigger, "audit");
    }

    #[test]
    fn matches_whole_word_in_middle() {
        let s = make_skill("security-audit", &["audit"]);
        let r = match_triggers("please audit my code", &[&s]);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn matches_whole_word_at_end() {
        let s = make_skill("security-audit", &["audit"]);
        let r = match_triggers("please do an audit", &[&s]);
        assert_eq!(r.len(), 1);
    }

    // --- whole-word negative cases (the key correctness requirement) ---

    #[test]
    fn does_not_match_substring_contest() {
        let s = make_skill("tester", &["test"]);
        let r = match_triggers("this is a contest of wills", &[&s]);
        assert!(r.is_empty(), "substring 'contest' must not match trigger 'test'");
    }

    #[test]
    fn does_not_match_substring_testing() {
        let s = make_skill("tester", &["test"]);
        let r = match_triggers("we are testing the system", &[&s]);
        assert!(r.is_empty(), "substring 'testing' must not match trigger 'test'");
    }

    #[test]
    fn does_not_match_substring_latest() {
        let s = make_skill("tester", &["test"]);
        let r = match_triggers("get the latest version", &[&s]);
        assert!(r.is_empty(), "substring 'latest' must not match trigger 'test'");
    }

    // --- case-insensitive ---

    #[test]
    fn matches_case_insensitive_uppercase_in_prompt() {
        let s = make_skill("security-audit", &["audit"]);
        let r = match_triggers("Please AUDIT the codebase", &[&s]);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn matches_case_insensitive_mixed_trigger() {
        // Trigger stored as "Audit" in source; lowercased at parse time.
        let s = make_skill("security-audit", &["Audit"]);
        let r = match_triggers("audit the code", &[&s]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].matched_trigger, "audit");
    }

    // --- multi-skill ---

    #[test]
    fn returns_both_skills_when_prompt_triggers_two() {
        let sa = make_skill("security-audit", &["audit"]);
        let cr = make_skill("code-review", &["review"]);
        let r = match_triggers("audit and review this PR", &[&sa, &cr]);
        let names: Vec<&str> = r.iter().map(|m| m.skill_name.as_str()).collect();
        assert!(names.contains(&"security-audit"), "expected security-audit in {names:?}");
        assert!(names.contains(&"code-review"), "expected code-review in {names:?}");
        assert_eq!(r.len(), 2);
    }

    // --- deduplication ---

    #[test]
    fn deduplicates_skill_with_multiple_matching_triggers() {
        // Both "audit" and "security" appear in the prompt, but we should get
        // exactly one TriggerMatch for this skill.
        let s = make_skill("security-audit", &["audit", "security"]);
        let r = match_triggers("please audit the security posture", &[&s]);
        assert_eq!(r.len(), 1, "expected exactly 1 match, got: {r:?}");
    }

    // --- empty triggers ---

    #[test]
    fn skill_with_no_triggers_never_matches() {
        let s = make_skill("implicit-skill", &[]);
        let r = match_triggers("audit review security vulnerability owasp pentest", &[&s]);
        assert!(r.is_empty());
    }

    // --- empty prompt ---

    #[test]
    fn empty_prompt_never_matches() {
        let s = make_skill("security-audit", &["audit"]);
        let r = match_triggers("", &[&s]);
        assert!(r.is_empty());
    }

    // --- front-matter parsing of bundled security-audit.md ---

    #[test]
    fn parses_bundled_security_audit_frontmatter() {
        // The bundled file is embedded at compile time so this test always
        // reflects the current on-disk state.
        let contents = include_str!("../bundled/skills/security-audit/SKILL.md");
        let fm = crate::agents::parse_skill_frontmatter(contents);
        assert_eq!(fm.name.as_deref(), Some("security-audit"));
        assert!(
            fm.triggers.contains(&"audit".to_string()),
            "expected 'audit' in triggers: {:?}",
            fm.triggers
        );
        assert!(
            fm.triggers.contains(&"owasp".to_string()),
            "expected 'owasp' in triggers: {:?}",
            fm.triggers
        );
    }

    // --- multi-word trigger phrase ---

    #[test]
    fn matches_multi_word_trigger_phrase() {
        let s = make_skill("code-review", &["code review"]);
        let r = match_triggers("can you do a code review of this PR", &[&s]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].matched_trigger, "code review");
    }

    #[test]
    fn does_not_match_partial_phrase() {
        // "code" alone must not fire the "code review" trigger
        let s = make_skill("code-review", &["code review"]);
        let r = match_triggers("look at this code please", &[&s]);
        assert!(r.is_empty());
    }

    // ── filter_skills (picker type-to-filter) ────────────────────────────────

    fn skill_named(name: &str) -> SkillSummary {
        make_skill(name, &[])
    }

    #[test]
    fn filter_skills_substring_returns_two_of_three() {
        // Spec: filter_skills("fil", &[file, git, filesystem]) == 2
        let file = skill_named("file");
        let git = skill_named("git");
        let filesystem = skill_named("filesystem");
        let all = [&file, &git, &filesystem];
        let hits = filter_skills("fil", &all);
        assert_eq!(hits.len(), 2, "expected 2 hits, got: {:?}", hits.iter().map(|s| &s.name).collect::<Vec<_>>());
        let names: Vec<&str> = hits.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"file"));
        assert!(names.contains(&"filesystem"));
        assert!(!names.contains(&"git"));
    }

    #[test]
    fn filter_skills_empty_query_returns_all() {
        let a = skill_named("alpha");
        let b = skill_named("beta");
        let all = [&a, &b];
        assert_eq!(filter_skills("", &all).len(), 2);
        assert_eq!(filter_skills("   ", &all).len(), 2);
    }

    #[test]
    fn filter_skills_is_case_insensitive() {
        let a = skill_named("FileSystem");
        let all = [&a];
        assert_eq!(filter_skills("file", &all).len(), 1);
        assert_eq!(filter_skills("SYS", &all).len(), 1);
    }

    #[test]
    fn filter_skills_matches_trigger_keywords() {
        // Skill "security-audit" has trigger "owasp" — searching "owa" should hit
        // the skill even though its name doesn't contain "owa".
        let s = make_skill("security-audit", &["audit", "owasp"]);
        let all = [&s];
        let hits = filter_skills("owa", &all);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "security-audit");
    }

    #[test]
    fn filter_skills_no_matches_returns_empty() {
        let a = skill_named("alpha");
        let b = skill_named("beta");
        let all = [&a, &b];
        assert!(filter_skills("zzz", &all).is_empty());
    }

    #[test]
    fn filter_skills_preserves_input_order() {
        let z = skill_named("zfile");
        let a = skill_named("afile");
        let m = skill_named("mfile");
        let all = [&z, &a, &m];
        let hits = filter_skills("file", &all);
        let names: Vec<&str> = hits.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["zfile", "afile", "mfile"]);
    }
}
