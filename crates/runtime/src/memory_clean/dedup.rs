/// memory_clean::dedup — Phase 6.5d
///
/// Detect duplicate-meaning entries among imported memory files.
///
/// # Algorithm
///
/// 1. Quick path — Jaccard similarity on word-sets.  Pairs with
///    similarity ≥ 0.7 are flagged as candidates.
/// 2. Slow path — LLM judge.  For each flagged pair the rewriter is asked:
///    "Are these two entries describing the same memory?  Answer yes/no."
///    The LLM judge only runs on pairs that passed the Jaccard threshold.
///
/// Dedup **never auto-merges**.  It produces a list of `DedupCandidate`
/// structs that the caller presents to the user for review.
///
/// # Memory cap
///
/// Pairwise comparison is O(n²).  For large memory dirs the Jaccard phase
/// is fast (word-set operations); the LLM phase only runs on a bounded
/// subset of pairs (those that pass the threshold).

use std::collections::HashSet;
use std::path::PathBuf;

use crate::memory_clean::rewriter::MemoryRewriter;
use crate::memory_clean::scan::ScannedEntry;

// ── DedupCandidate ────────────────────────────────────────────────────────────

/// A pair of entries detected as potential duplicates.
///
/// The user must review each candidate and decide whether to merge.
/// Anvil never auto-merges.
#[derive(Debug, Clone)]
pub struct DedupCandidate {
    /// Paths of the two candidate entries.
    pub entries: (PathBuf, PathBuf),
    /// Similarity score in [0.0, 1.0].
    pub confidence: f32,
    /// Human-readable reason, e.g. "Jaccard 0.82" or "LLM judge: yes (high)".
    pub reason: String,
}

/// Options for dedup detection.
#[derive(Debug, Clone)]
pub struct DedupOpts {
    /// Jaccard similarity threshold.  Pairs above this are flagged.
    pub jaccard_threshold: f32,
    /// Whether to run the LLM judge on Jaccard-flagged pairs.
    pub use_llm_judge: bool,
}

impl Default for DedupOpts {
    fn default() -> Self {
        Self {
            jaccard_threshold: 0.7,
            use_llm_judge: false, // off by default; enabled with --dedup flag
        }
    }
}

// ── detect_duplicates ─────────────────────────────────────────────────────────

/// Detect duplicate-meaning entries in `entries`.
///
/// When `rewriter` is `None`, the LLM judge step is skipped regardless of
/// `opts.use_llm_judge`.
pub fn detect_duplicates(
    entries: &[ScannedEntry],
    opts: &DedupOpts,
    rewriter: Option<&dyn MemoryRewriter>,
) -> Vec<DedupCandidate> {
    let mut candidates: Vec<DedupCandidate> = Vec::new();

    if entries.len() < 2 {
        return candidates;
    }

    // Build word sets once per entry.
    let word_sets: Vec<HashSet<String>> = entries
        .iter()
        .map(|e| word_set(&e.body))
        .collect();

    // O(n²) Jaccard pass.
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let sim = jaccard_similarity(&word_sets[i], &word_sets[j]);
            if sim >= opts.jaccard_threshold {
                candidates.push(DedupCandidate {
                    entries: (entries[i].path.clone(), entries[j].path.clone()),
                    confidence: sim,
                    reason: format!("Jaccard similarity {sim:.2}"),
                });
            }
        }
    }

    // LLM judge pass (only on Jaccard-flagged pairs).
    if opts.use_llm_judge {
        if let Some(rw) = rewriter {
            candidates = candidates
                .into_iter()
                .map(|candidate| {
                    // Find bodies for this pair.
                    let body_a = entries
                        .iter()
                        .find(|e| e.path == candidate.entries.0)
                        .map(|e| e.body.as_str())
                        .unwrap_or("");
                    let body_b = entries
                        .iter()
                        .find(|e| e.path == candidate.entries.1)
                        .map(|e| e.body.as_str())
                        .unwrap_or("");

                    match llm_judge_duplicate(rw, body_a, body_b) {
                        Ok((is_dup, judge_confidence, reason)) => {
                            if is_dup {
                                DedupCandidate {
                                    confidence: judge_confidence,
                                    reason: format!("LLM judge: {reason}"),
                                    ..candidate
                                }
                            } else {
                                // LLM judge says not a dup — drop confidence below
                                // threshold so caller can filter.
                                DedupCandidate {
                                    confidence: 0.0,
                                    reason: format!("LLM judge: not duplicate ({reason})"),
                                    ..candidate
                                }
                            }
                        }
                        Err(_) => {
                            // On LLM failure, keep the Jaccard result.
                            candidate
                        }
                    }
                })
                .filter(|c| c.confidence > 0.0)
                .collect();
        }
    }

    candidates
}

// ── Jaccard similarity ────────────────────────────────────────────────────────

/// Compute Jaccard similarity between two word sets.
///
/// Returns a value in [0.0, 1.0].  Empty-set pair returns 0.0.
pub fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// Build a word set from text (lowercased, punctuation stripped).
pub fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
        .filter(|w| w.len() > 2) // skip very short tokens
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

// ── LLM judge ─────────────────────────────────────────────────────────────────

/// Ask the rewriter whether two entries describe the same memory.
///
/// Returns `(is_duplicate, confidence, reason)`.
fn llm_judge_duplicate(
    rewriter: &dyn MemoryRewriter,
    body_a: &str,
    body_b: &str,
) -> Result<(bool, f32, String), String> {
    let judge_prompt = format!(
        "You are comparing two memory entries to determine if they describe \
         the same piece of knowledge and could be merged.\n\n\
         ENTRY A:\n{body_a}\n\n\
         ENTRY B:\n{body_b}\n\n\
         Are these entries describing the same memory or knowledge?\n\
         Respond with a JSON object: \
         {{\"duplicate\": true|false, \"confidence\": 0.0-1.0, \"reason\": \"brief explanation\"}}\n\
         Return ONLY the JSON, no code fences."
    );

    // We use the rewriter's interface (it can call Ollama / Anthropic / OpenAI).
    // The MockRewriter doesn't speak judge-JSON so we parse its output
    // as best we can.  In production the ProviderRewriter will return proper JSON.
    let result = rewriter.rewrite(&judge_prompt)?;

    // Try to parse result.rewritten as judge JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(result.rewritten.trim()) {
        let is_dup = v
            .get("duplicate")
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        let confidence = v
            .get("confidence")
            .and_then(|c| c.as_f64())
            .map(|c| c as f32)
            .unwrap_or(0.5);
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("unspecified")
            .to_string();
        return Ok((is_dup, confidence, reason));
    }

    // Fallback: interpret any rewritten text containing "yes" as duplicate.
    let lower = result.rewritten.to_ascii_lowercase();
    let is_dup = lower.contains("yes") || lower.contains("duplicate");
    Ok((is_dup, 0.5, "heuristic parse".to_string()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_clean::scan::ScannedEntry;
    use std::path::PathBuf;

    fn make_entry(name: &str, body: &str) -> ScannedEntry {
        ScannedEntry {
            path: PathBuf::from(format!("/home/.anvil/memory/{name}")),
            content_hash: name.to_string(),
            imported_from: "claude_code".to_string(),
            imported_at: None,
            source_path: None,
            body: body.to_string(),
            raw_frontmatter: "---\nimported_from: claude_code\n---".to_string(),
            scanned_at: "2026-05-15T00:00:00Z".to_string(),
        }
    }

    // ── jaccard_similarity ────────────────────────────────────────────────────

    #[test]
    fn jaccard_identical_sets() {
        let a: HashSet<String> = ["foo", "bar", "baz"].iter().map(|s| s.to_string()).collect();
        let b = a.clone();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn jaccard_disjoint_sets() {
        let a: HashSet<String> = ["foo", "bar"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["baz", "qux"].iter().map(|s| s.to_string()).collect();
        assert!((jaccard_similarity(&a, &b)).abs() < 0.001);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a: HashSet<String> = ["foo", "bar", "baz"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["foo", "bar", "qux"].iter().map(|s| s.to_string()).collect();
        let sim = jaccard_similarity(&a, &b);
        // Intersection = {foo, bar} = 2; union = {foo, bar, baz, qux} = 4; sim = 0.5
        assert!((sim - 0.5).abs() < 0.01);
    }

    // ── detect_duplicates with high Jaccard similarity ────────────────────────

    #[test]
    fn detect_finds_near_duplicate_pair() {
        let a = make_entry("rule-a.md", "Always commit after finishing work on any task.");
        let b = make_entry("rule-b.md", "Always commit after finishing work on any task and push.");
        let opts = DedupOpts { jaccard_threshold: 0.6, use_llm_judge: false };
        let candidates = detect_duplicates(&[a, b], &opts, None);
        assert!(!candidates.is_empty(), "near-duplicate pair must be flagged");
        assert!(candidates[0].confidence >= 0.6);
    }

    // ── detect_duplicates: distinct entries are not flagged ───────────────────

    #[test]
    fn detect_no_duplicates_for_distinct_entries() {
        let a = make_entry("rule-a.md", "Always commit after finishing work.");
        let b = make_entry("rule-b.md", "Never use rsync to deploy. Only git pull.");
        let opts = DedupOpts::default();
        let candidates = detect_duplicates(&[a, b], &opts, None);
        assert!(
            candidates.is_empty(),
            "distinct entries must not be flagged; got: {candidates:?}"
        );
    }

    // ── detect_duplicates: empty list returns empty ───────────────────────────

    #[test]
    fn detect_empty_returns_empty() {
        let opts = DedupOpts::default();
        let candidates = detect_duplicates(&[], &opts, None);
        assert!(candidates.is_empty());
    }

    // ── word_set strips short tokens ──────────────────────────────────────────

    #[test]
    fn word_set_filters_short_tokens() {
        let set = word_set("a bb ccc dddd");
        // "a" (len 1) and "bb" (len 2) are filtered; "ccc" and "dddd" remain.
        assert!(!set.contains("a"));
        assert!(!set.contains("bb"));
        assert!(set.contains("ccc"));
        assert!(set.contains("dddd"));
    }

    // ── DedupCandidate reason contains Jaccard score ──────────────────────────

    #[test]
    fn candidate_reason_contains_jaccard() {
        let body = "always commit after work on any coding project task";
        let a = make_entry("rule-a.md", body);
        let b = make_entry("rule-b.md", body); // identical bodies
        let opts = DedupOpts { jaccard_threshold: 0.5, use_llm_judge: false };
        let candidates = detect_duplicates(&[a, b], &opts, None);
        assert!(!candidates.is_empty());
        assert!(
            candidates[0].reason.contains("Jaccard"),
            "reason should mention Jaccard"
        );
    }
}
