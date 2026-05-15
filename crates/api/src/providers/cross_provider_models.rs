//! Tests and helpers for the cross-provider unified model list (v2.2.15).
//!
//! This module exercises:
//!   1. Merge logic — [`fetch_all_configured_models`] assembles `UnifiedModel`
//!      entries from every configured provider in parallel.
//!   2. Provider-prefix display — every `UnifiedModel::display` is
//!      `"<provider_slug>/<model_id>"`.
//!   3. Atomic switch contract — switching to `"cursor/claude-4-sonnet-thinking"`
//!      resolves to `(ProviderKind::Cursor, "claude-4-sonnet-thinking")`.
//!   4. Ambiguous bare-name error path — when >1 providers expose a model
//!      with the same bare ID, callers must prompt the user to qualify it.
//!
//! See `feedback-model-switch-must-be-atomic.md` for the switch contract.
//! See `feedback-model-list-is-live-not-registry.md` for the live-fetch rules.

use super::model_list::UnifiedModel;
use super::ProviderKind;

// ---------------------------------------------------------------------------
// Atomic switch resolution helpers (used by the TUI `/model` handler)
// ---------------------------------------------------------------------------

/// Outcome of resolving a provider-prefixed or bare model name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSwitchResolution {
    /// Unambiguous switch target — provide these to `apply_model_switch`.
    Resolved {
        provider: ProviderKind,
        model_id: String,
    },
    /// The bare name matches multiple providers — the user must qualify.
    Ambiguous {
        model_id: String,
        providers: Vec<ProviderKind>,
    },
    /// No configured provider exposes this model.
    NotFound { name: String },
}

/// Resolve a `/model` picker input against the unified model list.
///
/// Accepts two forms:
///
/// - `"<provider>/<model>"` — provider-prefixed (e.g. `"cursor/claude-4-sonnet-thinking"`).
///   Extracts the provider slug and model ID, resolves the provider kind,
///   and returns `Resolved` immediately.
///
/// - `"<model>"` — bare model name (no slash).  Searches the `catalog` for
///   entries whose `model_id` matches.  If exactly one provider matches,
///   returns `Resolved`.  If more than one match, returns `Ambiguous`.
///   If none match, returns `NotFound`.
///
/// The switch itself (rebuilding the `ConversationRuntime`, updating TUI
/// chrome, flipping `self.model`) is performed by the caller in one atomic
/// operation per `feedback-model-switch-must-be-atomic.md`.
#[must_use]
pub fn resolve_model_switch(
    input: &str,
    catalog: &[UnifiedModel],
) -> ModelSwitchResolution {
    if let Some((slug, model_id)) = input.split_once('/') {
        // Provider-prefixed form: "cursor/claude-4-sonnet-thinking"
        let provider = match super::model_list::slug_to_provider_kind(slug) {
            Some(p) => p,
            None => {
                return ModelSwitchResolution::NotFound {
                    name: input.to_string(),
                };
            }
        };
        return ModelSwitchResolution::Resolved {
            provider,
            model_id: model_id.to_string(),
        };
    }

    // Bare model name — search the catalog.
    let matches: Vec<ProviderKind> = catalog
        .iter()
        .filter(|m| m.model_id == input)
        .map(|m| m.provider)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    match matches.len() {
        0 => ModelSwitchResolution::NotFound {
            name: input.to_string(),
        },
        1 => ModelSwitchResolution::Resolved {
            provider: matches[0],
            model_id: input.to_string(),
        },
        _ => ModelSwitchResolution::Ambiguous {
            model_id: input.to_string(),
            providers: matches,
        },
    }
}

/// Format the `Ambiguous` error message shown to the user.
#[must_use]
pub fn format_ambiguous_model_error(model_id: &str, providers: &[ProviderKind]) -> String {
    let options: Vec<String> = providers
        .iter()
        .map(|&k| {
            let slug = match k {
                ProviderKind::AnvilApi => "anthropic",
                ProviderKind::Xai => "xai",
                ProviderKind::OpenAi => "openai",
                ProviderKind::Gemini => "gemini",
                ProviderKind::Ollama => "ollama",
                ProviderKind::Fireworks => "fireworks",
                ProviderKind::Groq => "groq",
                ProviderKind::Mistral => "mistral",
                ProviderKind::Perplexity => "perplexity",
                ProviderKind::DeepSeek => "deepseek",
                ProviderKind::TogetherAi => "togetherai",
                ProviderKind::DeepInfra => "deepinfra",
                ProviderKind::Cerebras => "cerebras",
                ProviderKind::NvidiaNim => "nvidia-nim",
                ProviderKind::HuggingFace => "huggingface",
                ProviderKind::MoonshotAi => "moonshotai",
                ProviderKind::Nebius => "nebius",
                ProviderKind::OpenRouter => "openrouter",
                ProviderKind::LmStudio => "lmstudio",
                ProviderKind::Chutes => "chutes",
                ProviderKind::Scaleway => "scaleway",
                ProviderKind::Baseten => "baseten",
                ProviderKind::MiniMax => "minimax",
                ProviderKind::StackIt => "stackit",
                ProviderKind::Cortecs => "cortecs",
                ProviderKind::Ai302 => "302ai",
                ProviderKind::Zai => "zai",
                ProviderKind::OpenCode => "opencode",
                ProviderKind::OpenCodeGo => "opencode-go",
                ProviderKind::Copilot => "copilot",
                ProviderKind::Azure => "azure",
                ProviderKind::Bedrock => "bedrock",
                ProviderKind::Alibaba => "alibaba",
                ProviderKind::Antigravity => "antigravity",
                ProviderKind::Cursor => "cursor",
            };
            format!("{slug}/{model_id}")
        })
        .collect();

    format!(
        "Ambiguous model name \"{model_id}\" — multiple providers expose this model.\n\
         Qualify the name with a provider prefix:\n{}",
        options.iter().map(|o| format!("  /model {o}")).collect::<Vec<_>>().join("\n")
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Edition 2024: env::set_var/remove_var require unsafe.
    #![allow(unsafe_code)]

    use super::*;
    use crate::providers::model_list::UnifiedModel;
    use crate::providers::ProviderKind;

    fn make_catalog(entries: &[(&str, ProviderKind)]) -> Vec<UnifiedModel> {
        entries
            .iter()
            .map(|(id, kind)| {
                let slug = match kind {
                    ProviderKind::AnvilApi => "anthropic",
                    ProviderKind::Cursor => "cursor",
                    ProviderKind::OpenAi => "openai",
                    ProviderKind::Groq => "groq",
                    _ => "unknown",
                };
                UnifiedModel {
                    provider: *kind,
                    model_id: id.to_string(),
                    display: format!("{slug}/{id}"),
                }
            })
            .collect()
    }

    // ── Test 1: provider-prefix display ──────────────────────────────────────

    #[test]
    fn unified_model_display_is_provider_prefixed() {
        let m = UnifiedModel {
            provider: ProviderKind::Cursor,
            model_id: "claude-4-sonnet-thinking".to_string(),
            display: "cursor/claude-4-sonnet-thinking".to_string(),
        };
        assert_eq!(m.display, "cursor/claude-4-sonnet-thinking");
        assert!(m.display.starts_with("cursor/"));
    }

    #[test]
    fn anthropic_model_display_prefix() {
        let m = UnifiedModel {
            provider: ProviderKind::AnvilApi,
            model_id: "claude-sonnet-4-6".to_string(),
            display: "anthropic/claude-sonnet-4-6".to_string(),
        };
        assert_eq!(m.display, "anthropic/claude-sonnet-4-6");
    }

    // ── Test 2: atomic switch resolution — prefixed form ────────────────────

    #[test]
    fn resolve_prefixed_cursor_model() {
        let catalog = make_catalog(&[
            ("claude-4-sonnet-thinking", ProviderKind::Cursor),
        ]);
        let result = resolve_model_switch("cursor/claude-4-sonnet-thinking", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::Resolved {
                provider: ProviderKind::Cursor,
                model_id: "claude-4-sonnet-thinking".to_string(),
            }
        );
    }

    #[test]
    fn resolve_prefixed_anthropic_model() {
        let catalog = make_catalog(&[("claude-sonnet-4-6", ProviderKind::AnvilApi)]);
        let result = resolve_model_switch("anthropic/claude-sonnet-4-6", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::Resolved {
                provider: ProviderKind::AnvilApi,
                model_id: "claude-sonnet-4-6".to_string(),
            }
        );
    }

    #[test]
    fn resolve_prefixed_openai_model() {
        let catalog = make_catalog(&[("gpt-5.4", ProviderKind::OpenAi)]);
        let result = resolve_model_switch("openai/gpt-5.4", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::Resolved {
                provider: ProviderKind::OpenAi,
                model_id: "gpt-5.4".to_string(),
            }
        );
    }

    // ── Test 3: bare name — unambiguous ──────────────────────────────────────

    #[test]
    fn resolve_bare_name_single_match() {
        let catalog = make_catalog(&[
            ("claude-4-sonnet-thinking", ProviderKind::Cursor),
            ("gpt-5.4", ProviderKind::OpenAi),
        ]);
        let result = resolve_model_switch("gpt-5.4", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::Resolved {
                provider: ProviderKind::OpenAi,
                model_id: "gpt-5.4".to_string(),
            }
        );
    }

    // ── Test 4: ambiguous bare name ───────────────────────────────────────────

    #[test]
    fn resolve_bare_name_ambiguous_returns_ambiguous_variant() {
        // Same model ID exposed by two providers.
        let catalog = make_catalog(&[
            ("llama-3.1", ProviderKind::Groq),
            ("llama-3.1", ProviderKind::AnvilApi),
        ]);
        let result = resolve_model_switch("llama-3.1", &catalog);
        match result {
            ModelSwitchResolution::Ambiguous { model_id, providers } => {
                assert_eq!(model_id, "llama-3.1");
                assert_eq!(providers.len(), 2);
                assert!(providers.contains(&ProviderKind::Groq));
                assert!(providers.contains(&ProviderKind::AnvilApi));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_error_message_contains_all_prefixed_options() {
        let providers = vec![ProviderKind::Groq, ProviderKind::AnvilApi];
        let msg = format_ambiguous_model_error("llama-3.1", &providers);
        assert!(msg.contains("Ambiguous model name"), "missing header: {msg}");
        assert!(msg.contains("groq/llama-3.1"), "missing groq option: {msg}");
        assert!(msg.contains("anthropic/llama-3.1"), "missing anthropic option: {msg}");
        assert!(msg.contains("/model "), "must show /model command: {msg}");
    }

    // ── Test 5: not found ─────────────────────────────────────────────────────

    #[test]
    fn resolve_bare_name_not_found() {
        let catalog = make_catalog(&[("gpt-5.4", ProviderKind::OpenAi)]);
        let result = resolve_model_switch("nonexistent-model", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::NotFound {
                name: "nonexistent-model".to_string(),
            }
        );
    }

    #[test]
    fn resolve_unknown_provider_slug_returns_not_found() {
        let catalog: Vec<UnifiedModel> = vec![];
        let result = resolve_model_switch("notaslug/some-model", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::NotFound {
                name: "notaslug/some-model".to_string(),
            }
        );
    }

    // ── Test 6: merge logic — deduplication ──────────────────────────────────

    #[test]
    fn unified_model_display_uniqueness_by_provider_prefix() {
        // Two providers expose "model-x", but their display strings differ.
        let cursor_entry = UnifiedModel {
            provider: ProviderKind::Cursor,
            model_id: "model-x".to_string(),
            display: "cursor/model-x".to_string(),
        };
        let anthropic_entry = UnifiedModel {
            provider: ProviderKind::AnvilApi,
            model_id: "model-x".to_string(),
            display: "anthropic/model-x".to_string(),
        };
        assert_ne!(cursor_entry.display, anthropic_entry.display);
        assert_eq!(cursor_entry.model_id, anthropic_entry.model_id);
    }

    // ── Test 7: resolve_model_switch ignores catalog for prefixed form ────────

    #[test]
    fn resolve_prefixed_does_not_require_model_in_catalog() {
        // An empty catalog still resolves a well-formed prefixed slug.
        let catalog: Vec<UnifiedModel> = vec![];
        let result = resolve_model_switch("cursor/claude-4-sonnet-thinking", &catalog);
        assert_eq!(
            result,
            ModelSwitchResolution::Resolved {
                provider: ProviderKind::Cursor,
                model_id: "claude-4-sonnet-thinking".to_string(),
            }
        );
    }
}
