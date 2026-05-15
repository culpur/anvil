// Allow `unsafe` only in test code (env::set_var for ANVIL_CONFIG_HOME).
#![cfg_attr(test, allow(unsafe_code))]

/// `MemoryRewriter` trait + implementations — Phase 6.5.
///
/// This is the single seam between the memory-clean pipeline and the LLM
/// backend.  Tests inject a [`MockRewriter`]; production code uses
/// [`ProviderRewriter`].
///
/// # Design notes
///
/// - Same provider-detection order as `import::sessions::ProviderSummarizer`:
///   Ollama localhost first, then Anthropic, then OpenAI.
/// - Fails loud when no provider is configured — no silent MockRewriter
///   fallback in production (per `feedback-no-silent-deferral.md`).
/// - The LLM returns JSON `{"rewritten": "...", "changes": [...]}`.

use serde::{Deserialize, Serialize};

// ── RewriteResult ─────────────────────────────────────────────────────────────

/// Result from a single memory-entry rewrite call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RewriteResult {
    /// Rewritten body content.  Frontmatter is handled by the caller, not
    /// the rewriter — this contains only the post-frontmatter body text.
    pub rewritten: String,
    /// Human-readable list of changes applied, e.g.
    /// `["normalized 'CC' (3 occurrences)", "stripped identity preamble"]`.
    pub changes: Vec<String>,
}

// ── MemoryRewriter trait ──────────────────────────────────────────────────────

/// Rewrite a single memory entry body.
///
/// `original` is the full body text (frontmatter already stripped).
///
/// # Errors
///
/// Returns `Err(reason)` when the provider call fails or the LLM returns
/// unparseable output.
pub trait MemoryRewriter: Send + Sync {
    fn rewrite(&self, original: &str) -> Result<RewriteResult, String>;
}

// ── MockRewriter ──────────────────────────────────────────────────────────────

/// Deterministic rewriter for tests.
///
/// Applies a set of textual substitutions so tests can assert on
/// transformation correctness without a live LLM.  The changes list is
/// deterministic and independent of network state.
pub struct MockRewriter;

impl MemoryRewriter for MockRewriter {
    fn rewrite(&self, original: &str) -> Result<RewriteResult, String> {
        let mut rewritten = original.to_string();
        let mut changes: Vec<String> = Vec::new();

        // Simulate "CC" normalization.
        let count = rewritten.matches("Claude Code").count();
        if count > 0 {
            rewritten = rewritten.replace("Claude Code", "CC");
            changes.push(format!("normalized 'CC' ({count} occurrence(s))"));
        }

        // Simulate .claude/ → .anvil/ where not a citation.
        let dot_claude_count = rewritten.matches(".claude/").count();
        if dot_claude_count > 0 {
            // In the mock we replace all occurrences — production rewriter is
            // more context-aware.
            rewritten = rewritten.replace(".claude/", ".anvil/");
            changes.push(format!("replaced .claude/ → .anvil/ ({dot_claude_count} occurrence(s))"));
        }

        // Simulate identity preamble strip.
        let preambles = [
            "As an AI assistant",
            "I am Claude",
            "As Claude",
        ];
        for p in &preambles {
            if rewritten.contains(p) {
                // Strip the sentence that starts with the preamble.
                rewritten = strip_sentence_starting_with(&rewritten, p);
                changes.push(format!("stripped identity preamble: '{p}'"));
            }
        }

        if changes.is_empty() {
            changes.push("no changes required".to_string());
        }

        Ok(RewriteResult { rewritten, changes })
    }
}

/// Remove the first sentence in `text` that starts with `prefix`.
///
/// A sentence is delimited by `.`, `!`, or `\n`.  If no match is found,
/// returns `text` unchanged.
fn strip_sentence_starting_with(text: &str, prefix: &str) -> String {
    let Some(start) = text.find(prefix) else {
        return text.to_string();
    };

    // Find the end of the sentence (`.`, `!`, or newline after `start`).
    let end_offset = text[start..]
        .find(['.', '!', '\n'])
        .map(|i| i + 1)
        .unwrap_or(text[start..].len());

    let mut result = text.to_string();
    result.drain(start..start + end_offset);
    result
}

// ── ProviderRewriter ──────────────────────────────────────────────────────────

/// System prompt for the rewrite LLM call.
///
/// Intentionally does not mention external research project names.
const REWRITE_SYSTEM_PROMPT: &str = "\
You are normalizing memory entries imported from a previous AI assistant \
installation into Anvil.

Apply these transformations:
1. Replace \"Claude Code\" → \"CC\" (per project convention)
2. Replace \".claude/\" → \".anvil/\" ONLY when the path refers to Anvil's \
   own config directory — NOT when it is a citation of where data was sourced \
   from (i.e., keep source_path references intact)
3. Strip identity preambles like \"As an AI assistant...\" or \"I am Claude...\" \
   — these do not apply in Anvil
4. Preserve all factual content, hostnames, decisions, timestamps, file paths, \
   and code snippets EXACTLY
5. Preserve all internal links like [[memory-name]]
6. Do NOT add information that was not in the source
7. Do NOT rewrite citations or provenance notes that reference where data \
   was originally stored

Output a JSON object with exactly these two keys:
  \"rewritten\": the full rewritten body content (string)
  \"changes\": array of human-readable strings describing what was changed

Return ONLY the JSON object with no markdown code fences or preamble.";

/// Production rewriter.  Auto-detects the cheapest configured provider:
/// Ollama localhost first, then Anthropic, then OpenAI.
///
/// Provider detection follows the same order as `ProviderSummarizer::detect()`
/// in `crates/runtime/src/import/sessions.rs`.
#[derive(Debug)]
pub struct ProviderRewriter {
    provider: RewriteProvider,
}

#[derive(Debug, Clone)]
enum RewriteProvider {
    Ollama { base_url: String, model: String },
    Anthropic { api_key: String, model: String },
    OpenAi { api_key: String, model: String },
}

impl ProviderRewriter {
    /// Auto-detect the cheapest available provider.
    ///
    /// # Errors
    ///
    /// Returns `Err` with an actionable message when no provider is configured
    /// or reachable.  Callers must NOT fall back to `MockRewriter` in
    /// production — surface the error to the user.
    pub fn detect() -> Result<Self, String> {
        // 1. Ollama localhost — probe /api/tags
        let ollama_base = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        if ollama_is_live(&ollama_base) {
            let model = pick_ollama_model(&ollama_base)
                .unwrap_or_else(|| "llama3.2:3b".to_string());
            return Ok(Self {
                provider: RewriteProvider::Ollama { base_url: ollama_base, model },
            });
        }

        // 2. Anthropic API key
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            if !key.is_empty() {
                return Ok(Self {
                    provider: RewriteProvider::Anthropic {
                        api_key: key,
                        model: "claude-haiku-4-5".to_string(),
                    },
                });
            }
        }

        // 3. OpenAI API key
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.is_empty() {
                return Ok(Self {
                    provider: RewriteProvider::OpenAi {
                        api_key: key,
                        model: "gpt-4o-mini".to_string(),
                    },
                });
            }
        }

        Err(
            "No rewrite provider available for `anvil memory clean`. \
             Start Ollama (`ollama serve`) or set ANTHROPIC_API_KEY / \
             OPENAI_API_KEY."
                .to_string(),
        )
    }

    /// Build the user prompt for a single entry.
    fn build_prompt(original: &str) -> String {
        format!(
            "Normalize the following memory entry per the instructions above.\n\n\
             ENTRY:\n{original}"
        )
    }

    fn call_ollama(base_url: &str, model: &str, prompt: &str) -> Result<RewriteResult, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "system": REWRITE_SYSTEM_PROMPT,
            "prompt": prompt,
            "stream": false
        });

        let resp = client
            .post(format!("{base_url}/api/generate"))
            .json(&body)
            .send()
            .map_err(|e| format!("ollama request: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("ollama returned HTTP {}", resp.status()));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse ollama response: {e}"))?;
        let text = json
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or("ollama response missing 'response' field")?;

        parse_rewrite_json(text)
    }

    fn call_anthropic(api_key: &str, model: &str, prompt: &str) -> Result<RewriteResult, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 2048,
            "system": REWRITE_SYSTEM_PROMPT,
            "messages": [{ "role": "user", "content": prompt }]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| format!("anthropic request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("anthropic returned HTTP {status}: {body}"));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse anthropic response: {e}"))?;
        let text = json
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .ok_or("anthropic response missing content[0].text")?;

        parse_rewrite_json(text)
    }

    fn call_openai(api_key: &str, model: &str, prompt: &str) -> Result<RewriteResult, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 2048,
            "messages": [
                { "role": "system", "content": REWRITE_SYSTEM_PROMPT },
                { "role": "user", "content": prompt }
            ]
        });

        let resp = client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .map_err(|e| format!("openai request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(format!("openai returned HTTP {status}: {body}"));
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| format!("parse openai response: {e}"))?;
        let text = json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .ok_or("openai response missing choices[0].message.content")?;

        parse_rewrite_json(text)
    }
}

impl MemoryRewriter for ProviderRewriter {
    fn rewrite(&self, original: &str) -> Result<RewriteResult, String> {
        let prompt = Self::build_prompt(original);
        match &self.provider {
            RewriteProvider::Ollama { base_url, model } => {
                Self::call_ollama(base_url, model, &prompt)
            }
            RewriteProvider::Anthropic { api_key, model } => {
                Self::call_anthropic(api_key, model, &prompt)
            }
            RewriteProvider::OpenAi { api_key, model } => {
                Self::call_openai(api_key, model, &prompt)
            }
        }
    }
}

// ── Provider helpers ──────────────────────────────────────────────────────────

fn ollama_is_live(base_url: &str) -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{base_url}/api/tags"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn pick_ollama_model(base_url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client
        .get(format!("{base_url}/api/tags"))
        .send()
        .ok()?;
    let json: serde_json::Value = resp.json().ok()?;
    let models = json.get("models")?.as_array()?;
    let names: Vec<String> = models
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    for keyword in &["3b", "mini", "small", "7b", "phi", "gemma", "qwen", "llama3"] {
        if let Some(n) = names.iter().find(|n| n.to_ascii_lowercase().contains(keyword)) {
            return Some(n.clone());
        }
    }
    names.into_iter().next()
}

/// Parse the LLM's JSON response into a [`RewriteResult`].
///
/// Strips common markdown code fence wrappers before parsing.
pub(crate) fn parse_rewrite_json(text: &str) -> Result<RewriteResult, String> {
    let stripped = text.trim();
    let stripped = stripped
        .strip_prefix("```json")
        .or_else(|| stripped.strip_prefix("```"))
        .unwrap_or(stripped)
        .trim_start();
    let stripped = stripped
        .strip_suffix("```")
        .unwrap_or(stripped)
        .trim_end();

    let v: serde_json::Value =
        serde_json::from_str(stripped).map_err(|e| format!("JSON parse: {e} — text: {stripped}"))?;

    let rewritten = v
        .get("rewritten")
        .and_then(|x| x.as_str())
        .ok_or("response missing 'rewritten' field")?
        .to_string();

    let changes = v
        .get("changes")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Ok(RewriteResult { rewritten, changes })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MockRewriter: CC normalization ────────────────────────────────────────

    #[test]
    fn mock_rewriter_normalizes_cc() {
        let mock = MockRewriter;
        let input = "When Claude Code is running, use Claude Code flags.";
        let result = mock.rewrite(input).expect("rewrite");
        assert!(
            result.rewritten.contains("CC"),
            "should contain CC"
        );
        assert!(
            !result.rewritten.contains("Claude Code"),
            "should not contain 'Claude Code'"
        );
        assert!(
            result.changes.iter().any(|c| c.contains("normalized")),
            "changes should mention normalization"
        );
    }

    // ── MockRewriter: .claude/ path replacement ───────────────────────────────

    #[test]
    fn mock_rewriter_replaces_dot_claude_paths() {
        let mock = MockRewriter;
        let input = "Config lives at ~/.claude/settings.json";
        let result = mock.rewrite(input).expect("rewrite");
        assert!(
            result.rewritten.contains(".anvil/"),
            "should replace .claude/ with .anvil/"
        );
    }

    // ── MockRewriter: no changes → reports no changes ─────────────────────────

    #[test]
    fn mock_rewriter_no_changes_reports_it() {
        let mock = MockRewriter;
        let input = "Prefer Rust over Python for performance.";
        let result = mock.rewrite(input).expect("rewrite");
        assert!(
            result.changes.iter().any(|c| c.contains("no changes")),
            "should report no changes when nothing to replace; got: {:?}",
            result.changes
        );
    }

    // ── MockRewriter is deterministic ─────────────────────────────────────────

    #[test]
    fn mock_rewriter_is_deterministic() {
        let mock = MockRewriter;
        let input = "Use Claude Code for all tasks in .claude/config.";
        let a = mock.rewrite(input).expect("first");
        let b = mock.rewrite(input).expect("second");
        assert_eq!(a, b, "mock rewriter must be deterministic");
    }

    // ── parse_rewrite_json: plain JSON ────────────────────────────────────────

    #[test]
    fn parse_rewrite_json_plain() {
        let json = r#"{"rewritten":"Prefer CC.","changes":["normalized 'CC' (1 occurrence)"]}"#;
        let result = parse_rewrite_json(json).expect("parse");
        assert_eq!(result.rewritten, "Prefer CC.");
        assert_eq!(result.changes.len(), 1);
    }

    // ── parse_rewrite_json: with code fences ─────────────────────────────────

    #[test]
    fn parse_rewrite_json_strips_code_fences() {
        let json = "```json\n{\"rewritten\":\"Done.\",\"changes\":[]}\n```";
        let result = parse_rewrite_json(json).expect("parse with fence");
        assert_eq!(result.rewritten, "Done.");
        assert!(result.changes.is_empty());
    }

    // ── parse_rewrite_json: missing rewritten field ───────────────────────────

    #[test]
    fn parse_rewrite_json_missing_field_errors() {
        let json = r#"{"changes":[]}"#;
        let err = parse_rewrite_json(json);
        assert!(err.is_err(), "should error on missing 'rewritten' field");
    }

    // ── ProviderRewriter::detect fails loud with no provider ──────────────────

    #[test]
    fn provider_rewriter_detect_fails_loud_without_provider() {
        // Unset env vars that could give a false positive.
        let _old_anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
        let _old_openai = std::env::var("OPENAI_API_KEY").ok();
        let _old_ollama = std::env::var("OLLAMA_BASE_URL").ok();

        // Force non-existent Ollama endpoint + no API keys.
        unsafe {
            std::env::set_var("OLLAMA_BASE_URL", "http://127.0.0.1:19999");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }

        let result = ProviderRewriter::detect();

        // Restore
        unsafe {
            std::env::remove_var("OLLAMA_BASE_URL");
            if let Some(v) = _old_anthropic {
                std::env::set_var("ANTHROPIC_API_KEY", v);
            }
            if let Some(v) = _old_openai {
                std::env::set_var("OPENAI_API_KEY", v);
            }
        }

        assert!(
            result.is_err(),
            "detect() must fail loud when no provider is configured"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("ANTHROPIC_API_KEY") || err.contains("ollama"),
            "error must name at least one fix path; got: {err}"
        );
    }
}
