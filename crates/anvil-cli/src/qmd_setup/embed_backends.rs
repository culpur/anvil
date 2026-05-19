//! Embedding-backend detector (Deliverable 3 of Agent A4).
//!
//! QMD needs a vector-embedding backend to generate the index used for
//! semantic search. The wizard's "embedding backend" sub-step picks
//! the best one available; A5 (healer) uses the same detector to spot
//! a broken backend.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedBackend {
    OllamaLocal { model: String, url: String },
    OllamaLocalNeedsPull { url: String },
    OllamaCloud,
    OpenAi,
    None,
}

impl EmbedBackend {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::OllamaLocal { model, url } => format!("Ollama local ({model} at {url})"),
            Self::OllamaLocalNeedsPull { url } => {
                format!("Ollama local — pull nomic-embed-text (~280MB) at {url}")
            }
            Self::OllamaCloud => "Ollama Cloud (nomic-embed-text, free tier)".to_string(),
            Self::OpenAi => "OpenAI text-embedding-3-small (uses your API quota)".to_string(),
            Self::None => "None (text search only)".to_string(),
        }
    }

    #[must_use]
    pub fn config_value(&self) -> String {
        match self {
            Self::OllamaLocal { model, .. } => format!("ollama:{model}"),
            Self::OllamaLocalNeedsPull { .. } => "ollama:nomic-embed-text".to_string(),
            Self::OllamaCloud => "ollama-cloud".to_string(),
            Self::OpenAi => "openai:text-embedding-3-small".to_string(),
            Self::None => "none".to_string(),
        }
    }

    #[must_use]
    pub const fn requires_setup(&self) -> bool {
        matches!(self, Self::OllamaLocalNeedsPull { .. })
    }
}

/// A3-shape view of the live Ollama state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaStateView {
    pub running: bool,
    pub url: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderConfigView {
    pub openai_configured: bool,
    pub ollama_cloud_configured: bool,
}

#[must_use]
pub fn detect_available(
    ollama_state: Option<&OllamaStateView>,
    providers: Option<&ProviderConfigView>,
) -> Vec<EmbedBackend> {
    let ollama: OllamaStateView = match ollama_state {
        Some(v) => v.clone(),
        None => probe_local_ollama(),
    };
    let providers: ProviderConfigView = match providers {
        Some(v) => v.clone(),
        None => probe_provider_config(),
    };

    let mut out: Vec<EmbedBackend> = Vec::new();

    if ollama.running {
        if let Some(model) = find_nomic_embed(&ollama.models) {
            out.push(EmbedBackend::OllamaLocal {
                model,
                url: ollama.url.clone(),
            });
        } else {
            out.push(EmbedBackend::OllamaLocalNeedsPull {
                url: ollama.url.clone(),
            });
        }
    }
    if providers.ollama_cloud_configured {
        out.push(EmbedBackend::OllamaCloud);
    }
    if providers.openai_configured {
        out.push(EmbedBackend::OpenAi);
    }
    out
}

fn find_nomic_embed(models: &[String]) -> Option<String> {
    models
        .iter()
        .find(|m| m.starts_with("nomic-embed-text"))
        .cloned()
}

fn probe_local_ollama() -> OllamaStateView {
    let url = std::env::var("OLLAMA_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "1", &format!("{url}/api/tags")])
        .output();
    let body = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            return OllamaStateView {
                running: false,
                url,
                models: Vec::new(),
            };
        }
    };
    let val = serde_json::from_str::<serde_json::Value>(&body).unwrap_or(serde_json::Value::Null);
    let models = val
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    OllamaStateView {
        running: true,
        url,
        models,
    }
}

fn probe_provider_config() -> ProviderConfigView {
    let dir = runtime::default_config_home();
    let path = dir.join("config.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return ProviderConfigView::default(),
    };
    let val: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let providers = val.get("providers");
    let openai_configured = providers
        .and_then(|p| p.get("openai"))
        .and_then(|o| o.get("api_key"))
        .and_then(|k| k.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let ollama_cloud_configured = val
        .get("ollama_cloud")
        .and_then(|c| c.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    ProviderConfigView {
        openai_configured,
        ollama_cloud_configured,
    }
}

pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_state_yields_empty_list_when_nothing_running() {
        let ollama = OllamaStateView {
            running: false,
            url: "http://localhost:11434".to_string(),
            models: Vec::new(),
        };
        let prov = ProviderConfigView::default();
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(out.is_empty());
    }

    #[test]
    fn local_ollama_with_nomic_yields_ollama_local() {
        let ollama = OllamaStateView {
            running: true,
            url: "http://localhost:11434".to_string(),
            models: vec!["nomic-embed-text:latest".to_string()],
        };
        let prov = ProviderConfigView::default();
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(matches!(out[0], EmbedBackend::OllamaLocal { .. }));
        if let EmbedBackend::OllamaLocal { model, .. } = &out[0] {
            assert_eq!(model, "nomic-embed-text:latest");
        }
    }

    #[test]
    fn local_ollama_without_nomic_yields_needs_pull() {
        let ollama = OllamaStateView {
            running: true,
            url: "http://localhost:11434".to_string(),
            models: vec!["qwen3:8b".to_string()],
        };
        let prov = ProviderConfigView::default();
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(matches!(out[0], EmbedBackend::OllamaLocalNeedsPull { .. }));
        assert!(out[0].requires_setup());
    }

    #[test]
    fn ollama_cloud_appears_when_provider_configured() {
        let ollama = OllamaStateView {
            running: false,
            url: "http://localhost:11434".to_string(),
            models: Vec::new(),
        };
        let prov = ProviderConfigView {
            openai_configured: false,
            ollama_cloud_configured: true,
        };
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(out.contains(&EmbedBackend::OllamaCloud));
    }

    #[test]
    fn openai_appears_when_provider_configured() {
        let ollama = OllamaStateView {
            running: false,
            url: "http://localhost:11434".to_string(),
            models: Vec::new(),
        };
        let prov = ProviderConfigView {
            openai_configured: true,
            ollama_cloud_configured: false,
        };
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(out.contains(&EmbedBackend::OpenAi));
    }

    #[test]
    fn local_ollama_with_nomic_is_preferred_over_others() {
        let ollama = OllamaStateView {
            running: true,
            url: "http://localhost:11434".to_string(),
            models: vec!["nomic-embed-text".to_string()],
        };
        let prov = ProviderConfigView {
            openai_configured: true,
            ollama_cloud_configured: true,
        };
        let out = detect_available(Some(&ollama), Some(&prov));
        assert!(matches!(out[0], EmbedBackend::OllamaLocal { .. }));
    }

    #[test]
    fn label_renders_for_each_variant() {
        for v in &[
            EmbedBackend::OllamaLocal {
                model: "nomic-embed-text".into(),
                url: "http://localhost:11434".into(),
            },
            EmbedBackend::OllamaLocalNeedsPull {
                url: "http://localhost:11434".into(),
            },
            EmbedBackend::OllamaCloud,
            EmbedBackend::OpenAi,
            EmbedBackend::None,
        ] {
            let l = v.label();
            assert!(!l.is_empty());
        }
    }

    #[test]
    fn config_value_is_machine_readable() {
        assert_eq!(
            EmbedBackend::OllamaLocal {
                model: "nomic-embed-text:latest".into(),
                url: "http://localhost:11434".into(),
            }
            .config_value(),
            "ollama:nomic-embed-text:latest"
        );
        assert_eq!(EmbedBackend::OllamaCloud.config_value(), "ollama-cloud");
        assert_eq!(
            EmbedBackend::OpenAi.config_value(),
            "openai:text-embedding-3-small"
        );
        assert_eq!(EmbedBackend::None.config_value(), "none");
    }

    #[test]
    fn find_nomic_embed_matches_with_and_without_suffix() {
        assert_eq!(
            find_nomic_embed(&["nomic-embed-text".to_string()]),
            Some("nomic-embed-text".to_string())
        );
        assert_eq!(
            find_nomic_embed(&["nomic-embed-text:latest".to_string()]),
            Some("nomic-embed-text:latest".to_string())
        );
        assert_eq!(find_nomic_embed(&["llama3:8b".to_string()]), None);
    }
}
