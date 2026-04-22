// skill_evals.rs — Three-arm skill evaluation harness for AnvilHub skills.
//
// ATTRIBUTION: The three-arm evaluation pattern (baseline / terse / skill),
// the honest-caveats requirement, and the "skill only wins if it beats __terse__"
// framing are adapted from the caveman eval design by Julius Brussee (MIT, 2026).
// No code was copied verbatim. Anvil is MIT.
//
// TOKEN ESTIMATOR WARNING
// ───────────────────────
// This module does NOT use tiktoken or any real BPE tokenizer.  It uses a
// lightweight heuristic: (byte_len / 4).  Benchmarks against cl100k_base
// suggest ~±25% accuracy on typical English prose and code.  Numbers are
// directional only — do NOT use them for billing, quota enforcement, or
// precise comparison across very short responses.  This limitation is surfaced
// in every EvalSummary::honest_caveats field automatically.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─── Public types ────────────────────────────────────────────────────────────

/// Configuration for a single skill evaluation run.
#[derive(Debug, Clone)]
pub struct EvalConfig {
    /// Path to the SKILL.md file to evaluate.
    pub skill_path: PathBuf,
    /// Input prompts to test against all three arms.
    pub prompts: Vec<String>,
    /// Model identifier, e.g. "claude-sonnet-4-6" or "qwen3:8b".
    pub model: String,
    /// Provider string: "anthropic" | "openai" | "ollama" | "xai" | "google".
    pub provider: String,
    /// Directory where JSON snapshots will be written.
    pub snapshot_dir: PathBuf,
}

/// The result of running a single prompt through a single arm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArmResult {
    /// "__baseline__" | "__terse__" | "<skill_name>"
    pub arm: String,
    pub prompt: String,
    pub response: String,
    /// Estimated token count (heuristic — see module doc).
    pub estimated_tokens: usize,
    /// Raw UTF-8 byte length of the response.
    pub byte_len: usize,
}

/// Full report for one evaluation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub model: String,
    pub skill_name: String,
    /// One `[ArmResult; 3]` per prompt, ordered [baseline, terse, skill].
    pub results_per_prompt: Vec<[ArmResult; 3]>,
    pub summary: EvalSummary,
}

/// Aggregate statistics across all prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSummary {
    pub baseline_avg_tokens: f64,
    pub terse_avg_tokens: f64,
    pub skill_avg_tokens: f64,
    /// `(skill_avg - terse_avg) / terse_avg * 100`.
    /// Negative = skill saved tokens beyond what terseness alone achieves.
    /// Near-zero or positive = skill content adds no value.
    /// Set to 0.0 when terse_avg == 0 to avoid division by zero.
    pub skill_vs_terse_delta_pct: f64,
    /// Mandatory caveats included in every report — never empty.
    pub honest_caveats: Vec<String>,
}

/// Errors that can arise during evaluation.
#[derive(Debug)]
pub enum EvalError {
    /// Could not read the skill file.
    SkillRead(std::io::Error),
    /// Could not create the snapshot directory or write a snapshot.
    SnapshotIo(std::io::Error),
    /// Snapshot serialisation failed.
    SnapshotSerialise(serde_json::Error),
    /// Provider returned an error for this arm.
    ProviderError {
        arm: String,
        prompt: String,
        message: String,
    },
    /// No prompts were provided.
    NoPrompts,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SkillRead(e) => write!(f, "failed to read skill file: {e}"),
            Self::SnapshotIo(e) => write!(f, "snapshot I/O error: {e}"),
            Self::SnapshotSerialise(e) => write!(f, "snapshot serialisation error: {e}"),
            Self::ProviderError { arm, prompt, message } => {
                write!(f, "provider error on arm '{arm}' for prompt '{prompt}': {message}")
            }
            Self::NoPrompts => write!(f, "EvalConfig.prompts must not be empty"),
        }
    }
}

impl std::error::Error for EvalError {}

// ─── Token estimator ─────────────────────────────────────────────────────────

/// Heuristic token estimator.
///
/// Implementation: `byte_len / 4`.
///
/// This approximates the cl100k_base BPE average token size for English/code
/// text (~4 bytes per token).  Accuracy is ±25% on typical inputs.  It is
/// intentionally crude — the point of this harness is *relative* comparison
/// across arms, not absolute token counts.
///
/// DO NOT use this value for billing, quota enforcement, or exact cross-model
/// comparisons.  The caveats in every `EvalSummary` document this.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    // We use byte length rather than char count so that multi-byte UTF-8
    // sequences (emojis, CJK) aren't systematically under-counted.
    (text.len() + 3) / 4 // integer ceiling division
}

// ─── Prompt slug ─────────────────────────────────────────────────────────────

/// Derive a deterministic, filesystem-safe snapshot key from a prompt.
///
/// Strategy: take the first 32 bytes of the prompt (ASCII-folded, spaces
/// replaced with underscores, non-alphanumeric stripped), append a 16-bit
/// truncation of the FNV-like `DefaultHasher` over the full prompt bytes.
/// This keeps slugs human-readable while ensuring two prompts that share a
/// 32-byte prefix are still distinct.
#[must_use]
pub fn prompt_slug(prompt: &str) -> String {
    let prefix: String = prompt
        .chars()
        .take(32)
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '_' || c == '-' {
                '_'
            } else {
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect();

    let mut hasher = DefaultHasher::new();
    prompt.hash(&mut hasher);
    let hash = hasher.finish() & 0xFFFF; // keep 16 bits — short but sufficient for disambiguation

    format!("{prefix}_{hash:04x}")
}

// ─── Honest caveats ──────────────────────────────────────────────────────────

/// Return the mandatory set of honest caveats included in every report.
/// Never returns an empty Vec.
#[must_use]
pub fn honest_caveats() -> Vec<String> {
    vec![
        "Token counts use a byte-length heuristic (byte_len / 4). \
         This is NOT a real tokenizer. \
         Accuracy is approximately ±25% on typical English/code text. \
         Numbers are directional, not exact. \
         Do not use them for billing, quota enforcement, or precise cross-model comparison."
            .to_string(),
        "This harness measures estimated token count only. \
         It does NOT measure: response fidelity, latency, dollar cost, \
         cross-model consistency, or user-perceived quality."
            .to_string(),
        "A negative `skill_vs_terse_delta_pct` shows the skill content provides value beyond \
         just 'be terse.' A near-zero or positive delta means the skill is doing nothing useful \
         — consider whether the skill earns its prompt-budget cost."
            .to_string(),
    ]
}

// ─── Summary math ────────────────────────────────────────────────────────────

/// Compute `EvalSummary` from a completed `results_per_prompt`.
/// Handles zero `terse_avg` safely (delta returns 0.0).
#[must_use]
pub fn compute_summary(results_per_prompt: &[[ArmResult; 3]]) -> EvalSummary {
    let n = results_per_prompt.len();

    if n == 0 {
        return EvalSummary {
            baseline_avg_tokens: 0.0,
            terse_avg_tokens: 0.0,
            skill_avg_tokens: 0.0,
            skill_vs_terse_delta_pct: 0.0,
            honest_caveats: honest_caveats(),
        };
    }

    let sum_baseline: usize = results_per_prompt.iter().map(|[b, _, _]| b.estimated_tokens).sum();
    let sum_terse: usize = results_per_prompt.iter().map(|[_, t, _]| t.estimated_tokens).sum();
    let sum_skill: usize = results_per_prompt.iter().map(|[_, _, s]| s.estimated_tokens).sum();

    let baseline_avg = sum_baseline as f64 / n as f64;
    let terse_avg = sum_terse as f64 / n as f64;
    let skill_avg = sum_skill as f64 / n as f64;

    let delta_pct = if terse_avg == 0.0 {
        0.0
    } else {
        (skill_avg - terse_avg) / terse_avg * 100.0
    };

    EvalSummary {
        baseline_avg_tokens: baseline_avg,
        terse_avg_tokens: terse_avg,
        skill_avg_tokens: skill_avg,
        skill_vs_terse_delta_pct: delta_pct,
        honest_caveats: honest_caveats(),
    }
}

// ─── Snapshot I/O ────────────────────────────────────────────────────────────

/// Snapshot format written to disk.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct PromptSnapshot {
    pub prompt_slug: String,
    pub prompt: String,
    pub arms: [ArmResult; 3],
}

/// Write a snapshot for one prompt to `<snapshot_dir>/<model>/<skill_name>/<slug>.json`.
pub fn write_snapshot(
    snapshot_dir: &std::path::Path,
    model: &str,
    skill_name: &str,
    slug: &str,
    snapshot: &PromptSnapshot,
) -> Result<(), EvalError> {
    // Sanitise path components to avoid directory traversal.
    let safe_model = model.replace(['/', '\\', ':'], "_");
    let safe_skill = skill_name.replace(['/', '\\', ':'], "_");

    let dir = snapshot_dir.join(&safe_model).join(&safe_skill);
    std::fs::create_dir_all(&dir).map_err(EvalError::SnapshotIo)?;

    let path = dir.join(format!("{slug}.json"));
    let json = serde_json::to_string_pretty(snapshot).map_err(EvalError::SnapshotSerialise)?;
    std::fs::write(path, json).map_err(EvalError::SnapshotIo)?;
    Ok(())
}

// ─── Provider abstraction for testability ────────────────────────────────────

/// Thin trait over the three-arm call so tests can inject a mock without
/// network access.  The real implementation (below) delegates to Anvil's
/// existing `api` provider clients.
#[async_trait::async_trait]
pub trait ArmCaller: Send + Sync {
    /// Call the model with `system_prompt` (None = no system prompt).
    async fn call(
        &self,
        model: &str,
        system_prompt: Option<&str>,
        user_prompt: &str,
    ) -> Result<String, String>;
}

// ─── Real provider caller ─────────────────────────────────────────────────────

/// Production `ArmCaller` that routes through Anvil's provider registry.
pub struct AnvilProviderCaller;

#[async_trait::async_trait]
impl ArmCaller for AnvilProviderCaller {
    async fn call(
        &self,
        model: &str,
        system_prompt: Option<&str>,
        user_prompt: &str,
    ) -> Result<String, String> {
        use api::{
            AnvilApiClient, InputMessage, MessageRequest, OllamaClient, OpenAiCompatClient,
            OpenAiCompatConfig, ProviderKind, detect_provider_kind,
        };

        let provider_kind = detect_provider_kind(model);
        let max_tokens = api::max_tokens_for_model(model);

        let request = MessageRequest {
            model: model.to_string(),
            max_tokens,
            messages: vec![InputMessage::user_text(user_prompt)],
            system: system_prompt.map(str::to_string),
            tools: None,
            tool_choice: None,
            stream: false,
        };

        let response_text = match provider_kind {
            ProviderKind::AnvilApi => {
                let client = AnvilApiClient::from_env()
                    .map_err(|e| format!("AnvilApiClient init: {e}"))?;
                let resp = client
                    .send_message(&request)
                    .await
                    .map_err(|e| format!("AnvilApi error: {e}"))?;
                extract_text_from_response(&resp)
            }
            ProviderKind::OpenAi => {
                let client = OpenAiCompatClient::from_env(OpenAiCompatConfig::openai())
                    .map_err(|e| format!("OpenAI client init: {e}"))?;
                let resp = client
                    .send_message(&request)
                    .await
                    .map_err(|e| format!("OpenAI error: {e}"))?;
                extract_text_from_response(&resp)
            }
            ProviderKind::Xai => {
                let client = OpenAiCompatClient::from_env(OpenAiCompatConfig::xai())
                    .map_err(|e| format!("xAI client init: {e}"))?;
                let resp = client
                    .send_message(&request)
                    .await
                    .map_err(|e| format!("xAI error: {e}"))?;
                extract_text_from_response(&resp)
            }
            ProviderKind::Gemini => {
                let client = OpenAiCompatClient::from_env(OpenAiCompatConfig::gemini())
                    .map_err(|e| format!("Gemini client init: {e}"))?;
                let resp = client
                    .send_message(&request)
                    .await
                    .map_err(|e| format!("Gemini error: {e}"))?;
                extract_text_from_response(&resp)
            }
            ProviderKind::Ollama => {
                // OllamaClient::from_env is infallible — no API key required.
                let client = OllamaClient::from_env();
                let resp = client
                    .send_message(&request)
                    .await
                    .map_err(|e| format!("Ollama error: {e}"))?;
                extract_text_from_response(&resp)
            }
        };

        Ok(response_text)
    }
}

fn extract_text_from_response(resp: &api::MessageResponse) -> String {
    resp.content
        .iter()
        .filter_map(|block| {
            if let api::OutputContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

// ─── Core eval engine ─────────────────────────────────────────────────────────

/// Run the three-arm evaluation using the provided `caller`.
///
/// This is the pure engine function — it is separated from `run_evals` so that
/// tests can inject a mock `ArmCaller` without network access.
pub async fn run_evals_with_caller(
    cfg: &EvalConfig,
    caller: &dyn ArmCaller,
) -> Result<EvalReport, EvalError> {
    if cfg.prompts.is_empty() {
        return Err(EvalError::NoPrompts);
    }

    let skill_body =
        std::fs::read_to_string(&cfg.skill_path).map_err(EvalError::SkillRead)?;

    let skill_name = cfg
        .skill_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown_skill")
        .to_string();

    let terse_system = "Answer concisely.";
    let skill_system = format!("{terse_system}\n\n{skill_body}");

    let mut results_per_prompt: Vec<[ArmResult; 3]> = Vec::with_capacity(cfg.prompts.len());

    for prompt in &cfg.prompts {
        let slug = prompt_slug(prompt);

        // Arm 0: baseline (no system prompt)
        let baseline_response = caller
            .call(&cfg.model, None, prompt)
            .await
            .map_err(|message| EvalError::ProviderError {
                arm: "__baseline__".to_string(),
                prompt: prompt.clone(),
                message,
            })?;

        // Arm 1: terse
        let terse_response = caller
            .call(&cfg.model, Some(terse_system), prompt)
            .await
            .map_err(|message| EvalError::ProviderError {
                arm: "__terse__".to_string(),
                prompt: prompt.clone(),
                message,
            })?;

        // Arm 2: skill
        let skill_response = caller
            .call(&cfg.model, Some(&skill_system), prompt)
            .await
            .map_err(|message| EvalError::ProviderError {
                arm: skill_name.clone(),
                prompt: prompt.clone(),
                message,
            })?;

        let arms = [
            make_arm_result("__baseline__", prompt, baseline_response),
            make_arm_result("__terse__", prompt, terse_response),
            make_arm_result(&skill_name, prompt, skill_response),
        ];

        // Write snapshot before accumulating results so partial runs are visible.
        let snapshot = PromptSnapshot {
            prompt_slug: slug.clone(),
            prompt: prompt.clone(),
            arms: arms.clone(),
        };
        write_snapshot(&cfg.snapshot_dir, &cfg.model, &skill_name, &slug, &snapshot)?;

        results_per_prompt.push(arms);
    }

    let summary = compute_summary(&results_per_prompt);

    Ok(EvalReport {
        model: cfg.model.clone(),
        skill_name,
        results_per_prompt,
        summary,
    })
}

fn make_arm_result(arm: &str, prompt: &str, response: String) -> ArmResult {
    let byte_len = response.len();
    let estimated_tokens = estimate_tokens(&response);
    ArmResult {
        arm: arm.to_string(),
        prompt: prompt.to_string(),
        response,
        estimated_tokens,
        byte_len,
    }
}

/// Run a full three-arm evaluation using Anvil's real provider clients.
///
/// For unit tests, use `run_evals_with_caller` with a mock `ArmCaller`.
pub async fn run_evals(cfg: &EvalConfig) -> Result<EvalReport, EvalError> {
    run_evals_with_caller(cfg, &AnvilProviderCaller).await
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_arm(arm: &str, prompt: &str, tokens: usize) -> ArmResult {
        ArmResult {
            arm: arm.to_string(),
            prompt: prompt.to_string(),
            response: "x".repeat(tokens * 4), // 4 bytes/token via our heuristic
            estimated_tokens: tokens,
            byte_len: tokens * 4,
        }
    }

    fn hand_built_results() -> Vec<[ArmResult; 3]> {
        vec![
            [
                make_arm("__baseline__", "p1", 100),
                make_arm("__terse__", "p1", 80),
                make_arm("skill_foo", "p1", 60),
            ],
            [
                make_arm("__baseline__", "p2", 200),
                make_arm("__terse__", "p2", 160),
                make_arm("skill_foo", "p2", 140),
            ],
        ]
    }

    // ── summary math ─────────────────────────────────────────────────────────

    #[test]
    fn summary_math_is_correct_on_hand_built_results() {
        let results = hand_built_results();
        let summary = compute_summary(&results);

        // baseline avg = (100 + 200) / 2 = 150
        assert!((summary.baseline_avg_tokens - 150.0).abs() < f64::EPSILON);
        // terse avg = (80 + 160) / 2 = 120
        assert!((summary.terse_avg_tokens - 120.0).abs() < f64::EPSILON);
        // skill avg = (60 + 140) / 2 = 100
        assert!((summary.skill_avg_tokens - 100.0).abs() < f64::EPSILON);
        // delta = (100 - 120) / 120 * 100 = -16.666...
        let expected_delta = (100.0_f64 - 120.0) / 120.0 * 100.0;
        assert!((summary.skill_vs_terse_delta_pct - expected_delta).abs() < 1e-9);
        // negative delta = skill saved tokens vs terse
        assert!(summary.skill_vs_terse_delta_pct < 0.0);
    }

    #[test]
    fn summary_handles_zero_terse_avg_without_divide_by_zero() {
        let results = vec![[
            make_arm("__baseline__", "p1", 0),
            make_arm("__terse__", "p1", 0),
            make_arm("skill", "p1", 0),
        ]];
        let summary = compute_summary(&results);
        assert_eq!(summary.skill_vs_terse_delta_pct, 0.0);
    }

    #[test]
    fn summary_on_empty_results_returns_zeros() {
        let summary = compute_summary(&[]);
        assert_eq!(summary.baseline_avg_tokens, 0.0);
        assert_eq!(summary.terse_avg_tokens, 0.0);
        assert_eq!(summary.skill_avg_tokens, 0.0);
        assert_eq!(summary.skill_vs_terse_delta_pct, 0.0);
    }

    // ── honest caveats ───────────────────────────────────────────────────────

    #[test]
    fn honest_caveats_is_non_empty() {
        let caveats = honest_caveats();
        assert!(!caveats.is_empty(), "caveats must never be empty");
        assert!(caveats.len() >= 3, "expected at least 3 caveats");
    }

    #[test]
    fn summary_embeds_honest_caveats() {
        let summary = compute_summary(&hand_built_results());
        assert!(!summary.honest_caveats.is_empty());
    }

    // ── prompt slug ──────────────────────────────────────────────────────────

    #[test]
    fn prompt_slug_is_deterministic() {
        let prompt = "What is the capital of France?";
        let slug1 = prompt_slug(prompt);
        let slug2 = prompt_slug(prompt);
        assert_eq!(slug1, slug2);
    }

    #[test]
    fn prompt_slug_differs_for_distinct_prompts() {
        let a = prompt_slug("What is the capital of France?");
        let b = prompt_slug("What is the capital of Germany?");
        assert_ne!(a, b);
    }

    #[test]
    fn prompt_slug_distinguishes_same_prefix() {
        // Two prompts sharing the first 32 bytes must still produce distinct slugs.
        let base = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaXX";
        let a = prompt_slug(&format!("{base}A"));
        let b = prompt_slug(&format!("{base}B"));
        assert_ne!(a, b);
    }

    #[test]
    fn prompt_slug_is_filesystem_safe() {
        let slug = prompt_slug("Hello, world! /path\\to:file");
        assert!(
            slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "slug contained unsafe char: {slug}"
        );
    }

    // ── token estimator ──────────────────────────────────────────────────────

    #[test]
    fn estimate_tokens_is_nonzero_for_nonempty_text() {
        assert!(estimate_tokens("hello world") > 0);
    }

    #[test]
    fn estimate_tokens_is_zero_for_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_scales_with_length() {
        let short = estimate_tokens("hi");
        let long = estimate_tokens(&"hi".repeat(100));
        assert!(long > short);
    }

    // ── snapshot round-trip ──────────────────────────────────────────────────

    #[test]
    fn snapshot_serialises_and_round_trips() {
        let arms = [
            make_arm("__baseline__", "test prompt", 50),
            make_arm("__terse__", "test prompt", 40),
            make_arm("my_skill", "test prompt", 30),
        ];
        let snapshot = PromptSnapshot {
            prompt_slug: prompt_slug("test prompt"),
            prompt: "test prompt".to_string(),
            arms,
        };

        let json = serde_json::to_string_pretty(&snapshot).expect("serialise");
        let recovered: PromptSnapshot = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(snapshot, recovered);
    }

    #[test]
    fn snapshot_write_and_read_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let arms = [
            make_arm("__baseline__", "q", 10),
            make_arm("__terse__", "q", 8),
            make_arm("skill_x", "q", 6),
        ];
        let slug = prompt_slug("q");
        let snapshot = PromptSnapshot {
            prompt_slug: slug.clone(),
            prompt: "q".to_string(),
            arms,
        };

        write_snapshot(tmp.path(), "claude-sonnet-4-6", "skill_x", &slug, &snapshot)
            .expect("write");

        let path = tmp
            .path()
            .join("claude-sonnet-4-6")
            .join("skill_x")
            .join(format!("{slug}.json"));
        let content = std::fs::read_to_string(path).expect("read back");
        let recovered: PromptSnapshot = serde_json::from_str(&content).expect("parse");
        assert_eq!(snapshot, recovered);
    }

    // ── mock caller end-to-end ───────────────────────────────────────────────

    /// A mock `ArmCaller` that returns a response whose token count depends on
    /// whether a system prompt is present and its length.
    struct MockCaller {
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ArmCaller for MockCaller {
        async fn call(
            &self,
            _model: &str,
            system_prompt: Option<&str>,
            _user_prompt: &str,
        ) -> Result<String, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Simulate that a longer system prompt leads to a shorter response
            // (the model "knows" what to say without elaboration).
            let base = 400usize; // 100 tokens via the heuristic
            let reduction = system_prompt.map_or(0, |s| s.len() / 2);
            let response_bytes = base.saturating_sub(reduction);
            Ok("x".repeat(response_bytes))
        }
    }

    #[tokio::test]
    async fn run_evals_with_mock_caller_produces_valid_report() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Write a minimal SKILL.md
        let skill_path = tmp.path().join("SKILL.md");
        std::fs::write(&skill_path, "# Test Skill\nBe extremely terse and use bullet points.")
            .expect("write skill");

        let cfg = EvalConfig {
            skill_path,
            prompts: vec![
                "What is Rust?".to_string(),
                "Explain async/await.".to_string(),
            ],
            model: "claude-sonnet-4-6".to_string(),
            provider: "anthropic".to_string(),
            snapshot_dir: tmp.path().join("snapshots"),
        };

        let call_count = Arc::new(AtomicUsize::new(0));
        let caller = MockCaller { call_count: Arc::clone(&call_count) };

        let report = run_evals_with_caller(&cfg, &caller)
            .await
            .expect("eval should succeed");

        // 2 prompts × 3 arms = 6 calls
        assert_eq!(call_count.load(Ordering::SeqCst), 6);

        assert_eq!(report.results_per_prompt.len(), 2);
        assert_eq!(report.model, "claude-sonnet-4-6");
        assert_eq!(report.skill_name, "SKILL");

        // Summary caveats populated
        assert!(!report.summary.honest_caveats.is_empty());

        // Snapshots written
        let snap_dir = tmp.path().join("snapshots").join("claude-sonnet-4-6").join("SKILL");
        assert!(snap_dir.is_dir());
        let entries: Vec<_> = std::fs::read_dir(&snap_dir)
            .expect("read snap dir")
            .collect();
        assert_eq!(entries.len(), 2, "one snapshot per prompt");
    }

    #[tokio::test]
    async fn run_evals_with_mock_caller_errors_on_no_prompts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_path = tmp.path().join("SKILL.md");
        std::fs::write(&skill_path, "skill body").expect("write");

        let cfg = EvalConfig {
            skill_path,
            prompts: vec![],
            model: "m".to_string(),
            provider: "anthropic".to_string(),
            snapshot_dir: tmp.path().to_path_buf(),
        };

        let call_count = Arc::new(AtomicUsize::new(0));
        let caller = MockCaller { call_count };
        let err = run_evals_with_caller(&cfg, &caller).await.unwrap_err();
        assert!(matches!(err, EvalError::NoPrompts));
    }
}
