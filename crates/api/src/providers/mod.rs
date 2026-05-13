use std::future::Future;
use std::pin::Pin;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse};

pub mod anvil_provider;
pub mod common;
pub mod ollama;
pub mod ollama_manage;
pub mod ollama_registry;
pub mod ollama_show;
pub mod ollama_tool_parser;
pub mod openai_compat;

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

pub trait Provider {
    type Stream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse>;

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    AnvilApi,
    Xai,
    OpenAi,
    Gemini,
    Ollama,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub provider: ProviderKind,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

const ANTHROPIC_META: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::AnvilApi,
    auth_env: "ANTHROPIC_API_KEY",
    base_url_env: "ANTHROPIC_BASE_URL",
    default_base_url: anvil_provider::DEFAULT_BASE_URL,
};

const XAI_META: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::Xai,
    auth_env: "XAI_API_KEY",
    base_url_env: "XAI_BASE_URL",
    default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
};

const OPENAI_META: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::OpenAi,
    auth_env: "OPENAI_API_KEY",
    base_url_env: "OPENAI_BASE_URL",
    default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
};

const GEMINI_META: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::Gemini,
    auth_env: "GEMINI_API_KEY",
    base_url_env: "GEMINI_BASE_URL",
    default_base_url: openai_compat::DEFAULT_GEMINI_BASE_URL,
};

const OLLAMA_META: ProviderMetadata = ProviderMetadata {
    provider: ProviderKind::Ollama,
    auth_env: "",
    base_url_env: "OLLAMA_HOST",
    default_base_url: ollama::DEFAULT_OLLAMA_BASE_URL,
};

const MODEL_REGISTRY: &[(&str, ProviderMetadata)] = &[
    // Anthropic aliases
    ("opus", ANTHROPIC_META),
    ("sonnet", ANTHROPIC_META),
    ("haiku", ANTHROPIC_META),
    ("claude-opus-4-6", ANTHROPIC_META),
    ("claude-sonnet-4-6", ANTHROPIC_META),
    ("claude-haiku-4-5-20251213", ANTHROPIC_META),
    // xAI / Grok
    ("grok", XAI_META),
    ("grok-3", XAI_META),
    ("grok-mini", XAI_META),
    ("grok-3-mini", XAI_META),
    ("grok-2", XAI_META),
    // OpenAI — GPT-5 frontier
    ("gpt-5.4", OPENAI_META),
    ("gpt-5.4-pro", OPENAI_META),
    ("gpt-5.4-mini", OPENAI_META),
    ("gpt-5.4-nano", OPENAI_META),
    ("gpt-5", OPENAI_META),
    ("gpt-5-mini", OPENAI_META),
    ("gpt-5-nano", OPENAI_META),
    // OpenAI — coding
    ("gpt-5-codex", OPENAI_META),
    ("gpt-5.3-codex", OPENAI_META),
    // OpenAI — image generation
    ("gpt-image-1.5", OPENAI_META),
    ("gpt-image-1", OPENAI_META),
    ("gpt-image-1-mini", OPENAI_META),
    // OpenAI — reasoning
    ("o3", OPENAI_META),
    ("o3-pro", OPENAI_META),
    ("o3-mini", OPENAI_META),
    ("o4-mini", OPENAI_META),
    ("o3-deep-research", OPENAI_META),
    ("o4-mini-deep-research", OPENAI_META),
    // OpenAI — previous gen
    ("gpt-4.1", OPENAI_META),
    ("gpt-4.1-mini", OPENAI_META),
    ("gpt-4o", OPENAI_META),
    ("gpt-4o-mini", OPENAI_META),
    ("gpt-4-turbo", OPENAI_META),
    ("gpt-4", OPENAI_META),
    ("gpt-3.5-turbo", OPENAI_META),
    ("o1", OPENAI_META),
    ("o1-mini", OPENAI_META),
    ("o1-preview", OPENAI_META),
    // Google Gemini
    ("gemini-2.5-pro", GEMINI_META),
    ("gemini-2.5-flash", GEMINI_META),
    ("gemini-2.0-flash", GEMINI_META),
    ("gemini-1.5-pro", GEMINI_META),
    ("gemini-1.5-flash", GEMINI_META),
    ("gemini-pro", GEMINI_META),
    ("gemini", GEMINI_META),
    // Ollama — well-known local model names
    ("llama3.2", OLLAMA_META),
    ("llama3.1", OLLAMA_META),
    ("llama3", OLLAMA_META),
    ("llama2", OLLAMA_META),
    ("mistral", OLLAMA_META),
    ("mixtral", OLLAMA_META),
    ("qwen2.5", OLLAMA_META),
    ("qwen2", OLLAMA_META),
    ("gemma3", OLLAMA_META),
    ("gemma2", OLLAMA_META),
    ("gemma", OLLAMA_META),
    ("phi4", OLLAMA_META),
    ("phi3", OLLAMA_META),
    ("deepseek-r1", OLLAMA_META),
    ("deepseek-v3", OLLAMA_META),
    ("deepseek-coder", OLLAMA_META),
    ("codellama", OLLAMA_META),
    ("vicuna", OLLAMA_META),
    ("orca-mini", OLLAMA_META),
    ("falcon", OLLAMA_META),
    ("solar", OLLAMA_META),
    ("starcoder2", OLLAMA_META),
    ("nomic-embed-text", OLLAMA_META),
    ("mxbai-embed-large", OLLAMA_META),
];

#[must_use]
pub fn resolve_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    MODEL_REGISTRY
        .iter()
        .find_map(|(alias, metadata)| {
            (*alias == lower).then_some(match metadata.provider {
                ProviderKind::AnvilApi => match *alias {
                    "opus" => "claude-opus-4-6",
                    "sonnet" => "claude-sonnet-4-6",
                    "haiku" => "claude-haiku-4-5-20251213",
                    _ => trimmed,
                },
                ProviderKind::Xai => match *alias {
                    "grok" | "grok-3" => "grok-3",
                    "grok-mini" | "grok-3-mini" => "grok-3-mini",
                    "grok-2" => "grok-2",
                    _ => trimmed,
                },
                ProviderKind::OpenAi | ProviderKind::Gemini | ProviderKind::Ollama => trimmed,
            })
        })
        .map_or_else(|| trimmed.to_string(), ToOwned::to_owned)
}

#[must_use]
pub fn metadata_for_model(model: &str) -> Option<ProviderMetadata> {
    let canonical = resolve_model_alias(model);
    let lower = canonical.to_ascii_lowercase();
    if let Some((_, metadata)) = MODEL_REGISTRY.iter().find(|(alias, _)| *alias == lower) {
        return Some(*metadata);
    }
    // Dynamic prefix matching for model families not enumerated in the registry.
    if lower.starts_with("grok") {
        return Some(XAI_META);
    }
    if lower.starts_with("gpt-")
        || lower.starts_with("gpt-image")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        return Some(OPENAI_META);
    }
    if lower.starts_with("llama")
        || lower.starts_with("mistral")
        || lower.starts_with("mixtral")
        || lower.starts_with("qwen")
        || lower.starts_with("gemma")
        || lower.starts_with("phi")
        || lower.starts_with("deepseek")
        || lower.starts_with("codellama")
        || lower.starts_with("vicuna")
        || lower.starts_with("falcon")
        || lower.starts_with("solar")
        || lower.starts_with("starcoder")
        || lower.contains('/')
    {
        return Some(OLLAMA_META);
    }
    None
}

/// Returns every model alias known to the static registry, paired with the
/// provider that serves it.
///
/// Drives `/model <TAB>` completion alongside any locally-discovered Ollama
/// models cached at startup.  The returned slice is alphabetically stable
/// (same order as `MODEL_REGISTRY`) so completion popups don't reshuffle
/// between keystrokes.
#[must_use]
pub fn known_models() -> Vec<(&'static str, ProviderKind)> {
    MODEL_REGISTRY
        .iter()
        .map(|(alias, meta)| (*alias, meta.provider))
        .collect()
}

#[must_use]
pub fn detect_provider_kind(model: &str) -> ProviderKind {
    if let Some(metadata) = metadata_for_model(model) {
        return metadata.provider;
    }
    // Ollama models typically contain ':' (e.g., qwen3:8b, llama3.2:latest, glm-5:cloud)
    if model.contains(':') {
        return ProviderKind::Ollama;
    }
    // Unknown model — fall back by checking available credentials.
    if anvil_provider::has_auth_from_env_or_saved().unwrap_or(false) {
        return ProviderKind::AnvilApi;
    }
    if openai_compat::has_api_key("OPENAI_API_KEY") {
        return ProviderKind::OpenAi;
    }
    if openai_compat::has_api_key("XAI_API_KEY") {
        return ProviderKind::Xai;
    }
    if openai_compat::has_api_key("GEMINI_API_KEY") || openai_compat::has_api_key("GOOGLE_API_KEY") {
        return ProviderKind::Gemini;
    }
    ProviderKind::AnvilApi
}

/// Return the display name shown in the `/model` report for a provider kind.
#[must_use]
pub const fn provider_display_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::AnvilApi => "Anthropic (Anvil)",
        ProviderKind::Xai => "xAI",
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::Gemini => "Google Gemini",
        ProviderKind::Ollama => "Ollama (local)",
    }
}

#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    let canonical = resolve_model_alias(model);
    let lower = canonical.to_ascii_lowercase();
    if lower.contains("opus") {
        32_000
    } else if lower.starts_with("o1") || lower.starts_with("o3") || lower.starts_with("o4") {
        100_000
    } else if lower.starts_with("gpt-5") {
        // GPT-5 family — 128K context window
        128_000
    } else if lower.starts_with("gpt-image") {
        // Image generation models don't use token budgets in the same way;
        // return a nominal value to avoid surprises.
        4_096
    } else if lower.starts_with("gpt-4o") {
        16_384
    } else if lower.starts_with("gpt-4") {
        8_192
    } else if lower.starts_with("gpt-") {
        4_096
    } else if lower.starts_with("llama")
        || lower.starts_with("mistral")
        || lower.starts_with("mixtral")
        || lower.starts_with("qwen")
        || lower.starts_with("gemma")
        || lower.starts_with("phi")
        || lower.starts_with("deepseek")
        || lower.contains('/')
    {
        // Ollama / local models: conservative default; the model's actual
        // context window is configured in the Modelfile, not here.
        4_096
    } else {
        64_000
    }
}

/// Shared environment-variable mutex for tests across this crate.
///
/// All tests that read or write environment variables must hold this lock for
/// the duration of the test, including any `EnvRestore` RAII guards.  Using a
/// single crate-level mutex (instead of per-module `OnceLock<Mutex<()>>`)
/// prevents races when the test runner executes tests from different modules
/// in the same OS process, since env vars are process-wide state.
#[cfg(test)]
pub(super) fn crate_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{
        detect_provider_kind, known_models, max_tokens_for_model, resolve_model_alias,
        ProviderKind,
    };

    #[test]
    fn known_models_covers_every_provider() {
        let models = known_models();
        assert!(
            models.iter().any(|(_, k)| *k == ProviderKind::AnvilApi),
            "missing Anthropic entries"
        );
        assert!(
            models.iter().any(|(_, k)| *k == ProviderKind::OpenAi),
            "missing OpenAI entries"
        );
        assert!(
            models.iter().any(|(_, k)| *k == ProviderKind::Xai),
            "missing xAI entries"
        );
        assert!(
            models.iter().any(|(_, k)| *k == ProviderKind::Gemini),
            "missing Gemini entries"
        );
        assert!(
            models.iter().any(|(_, k)| *k == ProviderKind::Ollama),
            "missing Ollama entries"
        );
        let names: Vec<&str> = models.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"claude-sonnet-4-6"));
        assert!(names.contains(&"gpt-4o"));
        assert!(names.contains(&"grok-3"));
        assert!(names.contains(&"gemini-2.5-pro"));
        assert!(names.contains(&"llama3.2"));
    }

    #[test]
    fn resolves_grok_aliases() {
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
        assert_eq!(resolve_model_alias("grok-2"), "grok-2");
    }

    #[test]
    fn detects_provider_from_model_name_first() {
        assert_eq!(detect_provider_kind("grok"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::AnvilApi
        );
        assert_eq!(detect_provider_kind("gpt-4o"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("o3-mini"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("llama3.2"), ProviderKind::Ollama);
        assert_eq!(detect_provider_kind("mistral"), ProviderKind::Ollama);
        assert_eq!(detect_provider_kind("qwen2.5"), ProviderKind::Ollama);
        assert_eq!(detect_provider_kind("myorg/mymodel"), ProviderKind::Ollama);
        // GPT-5 and image models should route to OpenAI
        assert_eq!(detect_provider_kind("gpt-5.4-mini"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("gpt-5"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("gpt-image-1.5"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("gpt-5-codex"), ProviderKind::OpenAi);
        assert_eq!(detect_provider_kind("o3-pro"), ProviderKind::OpenAi);
    }

    #[test]
    fn max_tokens_covers_all_providers() {
        assert_eq!(max_tokens_for_model("opus"), 32_000);
        assert_eq!(max_tokens_for_model("grok-3"), 64_000);
        assert_eq!(max_tokens_for_model("gpt-4o"), 16_384);
        assert_eq!(max_tokens_for_model("gpt-4"), 8_192);
        assert_eq!(max_tokens_for_model("o1"), 100_000);
        assert_eq!(max_tokens_for_model("o3-mini"), 100_000);
        assert_eq!(max_tokens_for_model("o4-mini"), 100_000);
        assert_eq!(max_tokens_for_model("llama3.2"), 4_096);
        assert_eq!(max_tokens_for_model("mistral"), 4_096);
        // GPT-5 family should report 128K
        assert_eq!(max_tokens_for_model("gpt-5.4-mini"), 128_000);
        assert_eq!(max_tokens_for_model("gpt-5"), 128_000);
        assert_eq!(max_tokens_for_model("gpt-5-codex"), 128_000);
        // Image generation models return nominal value
        assert_eq!(max_tokens_for_model("gpt-image-1.5"), 4_096);
    }
}
