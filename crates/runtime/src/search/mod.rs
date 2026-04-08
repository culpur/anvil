use std::time::Duration;

pub mod cache;
pub mod providers;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchProvider {
    DuckDuckGo,
    Tavily,
    Exa,
    SearXNG,
    Brave,
    Google,
    Perplexity,
    Bing,
}

impl SearchProvider {
    #[allow(dead_code)]
    fn name(&self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Tavily => "tavily",
            Self::Exa => "exa",
            Self::SearXNG => "searxng",
            Self::Brave => "brave",
            Self::Google => "google",
            Self::Perplexity => "perplexity",
            Self::Bing => "bing",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some(Self::DuckDuckGo),
            "tavily" => Some(Self::Tavily),
            "exa" => Some(Self::Exa),
            "searxng" | "searx" => Some(Self::SearXNG),
            "brave" => Some(Self::Brave),
            "google" => Some(Self::Google),
            "perplexity" => Some(Self::Perplexity),
            "bing" => Some(Self::Bing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub provider_type: SearchProvider,
    pub api_key: Option<String>,
    /// Base URL override (used for `SearXNG` custom instances).
    pub base_url: Option<String>,
    /// Google Custom Search Engine ID.
    pub cx: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub providers: Vec<ProviderConfig>,
    pub default_provider: String,
    pub cache_ttl_secs: u64,
    pub max_results: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            providers: vec![ProviderConfig {
                name: "duckduckgo".to_string(),
                provider_type: SearchProvider::DuckDuckGo,
                api_key: None,
                base_url: None,
                cx: None,
                enabled: true,
            }],
            default_provider: "duckduckgo".to_string(),
            cache_ttl_secs: 300,
            max_results: 8,
        }
    }
}

// ---------------------------------------------------------------------------
// SearchEngine facade
// ---------------------------------------------------------------------------

pub struct SearchEngine {
    config: SearchConfig,
    cache: cache::SearchResultCache,
}

impl SearchEngine {
    #[must_use]
    pub fn new(config: SearchConfig) -> Self {
        Self {
            config,
            cache: cache::SearchResultCache::new(),
        }
    }

    /// Load a `SearchEngine` from `~/.anvil/search.json` and environment
    /// variables.  Falls back to a `DuckDuckGo`-only default if the file is
    /// absent or unparseable.
    #[must_use]
    pub fn from_env_and_config() -> Self {
        let mut config = load_search_config_file().unwrap_or_default();
        inject_env_api_keys(&mut config);
        Self::new(config)
    }

    // ------------------------------------------------------------------
    // Querying
    // ------------------------------------------------------------------

    /// Search using the default provider (or `provider_override` if given).
    pub fn search(
        &mut self,
        query: &str,
        provider_override: Option<&str>,
    ) -> Result<Vec<SearchResult>, String> {
        let provider_name = provider_override
            .unwrap_or(&self.config.default_provider)
            .to_string();

        let cache_key = format!("{provider_name}:{query}");
        let ttl = Duration::from_secs(self.config.cache_ttl_secs);
        if let Some(results) = self.cache.get(&cache_key, ttl) {
            return Ok(results);
        }

        let cfg = self
            .config
            .providers
            .iter()
            .find(|p| p.name == provider_name)
            .cloned()
            .ok_or_else(|| format!("unknown provider: {provider_name}"))?;

        if !cfg.enabled {
            return Err(format!("provider {provider_name} is disabled"));
        }

        let max = self.config.max_results;
        let results = providers::execute(&cfg, query, max)?;

        self.cache.insert(cache_key, results.clone());
        Ok(results)
    }

    /// Search across all enabled providers and deduplicate by URL.
    pub fn search_all(&mut self, query: &str) -> Result<Vec<SearchResult>, String> {
        let configs: Vec<ProviderConfig> = self
            .config
            .providers
            .iter()
            .filter(|p| p.enabled)
            .cloned()
            .collect();

        let max = self.config.max_results;
        let mut seen_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut combined: Vec<SearchResult> = Vec::new();

        for cfg in &configs {
            if let Ok(results) = providers::execute(cfg, query, max) {
                for r in results {
                    if seen_urls.insert(r.url.clone()) {
                        combined.push(r);
                    }
                }
            }
        }

        Ok(combined)
    }

    // ------------------------------------------------------------------
    // Configuration
    // ------------------------------------------------------------------

    pub fn configure_provider(&mut self, name: &str, new_cfg: ProviderConfig) {
        if let Some(existing) = self.config.providers.iter_mut().find(|p| p.name == name) {
            *existing = new_cfg;
        } else {
            self.config.providers.push(new_cfg);
        }
    }

    /// Returns `(name, enabled, has_credentials)` for every configured provider.
    #[must_use]
    pub fn list_providers(&self) -> Vec<(String, bool, bool)> {
        self.config
            .providers
            .iter()
            .map(|p| {
                let has_creds = provider_has_credentials(p);
                (p.name.clone(), p.enabled, has_creds)
            })
            .collect()
    }

    /// Change the default provider (by name).
    pub fn set_default_provider(&mut self, name: &str) {
        self.config.default_provider = name.to_string();
    }

    #[must_use]
    pub fn default_provider(&self) -> &str {
        &self.config.default_provider
    }
}

// ---------------------------------------------------------------------------
// Config file loading
// ---------------------------------------------------------------------------

fn load_search_config_file() -> Option<SearchConfig> {
    let home = dirs_home()?;
    let path = home.join(".anvil").join("search.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;

    let default_provider = value
        .get("default")
        .and_then(|v| v.as_str())
        .unwrap_or("duckduckgo")
        .to_string();

    let cache_ttl_secs = value
        .get("cache_ttl_secs")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(300);

    #[allow(clippy::cast_possible_truncation)]
    let max_results = value
        .get("max_results")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8) as usize;

    let mut providers: Vec<ProviderConfig> = vec![
        // DuckDuckGo is always available — no credentials needed.
        ProviderConfig {
            name: "duckduckgo".to_string(),
            provider_type: SearchProvider::DuckDuckGo,
            api_key: None,
            base_url: None,
            cx: None,
            enabled: true,
        },
    ];

    if let Some(map) = value.get("providers").and_then(|v| v.as_object()) {
        for (name, cfg) in map {
            let enabled = cfg.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true);
            let api_key = cfg.get("api_key").and_then(|v| v.as_str()).map(String::from);
            let base_url = cfg.get("url").and_then(|v| v.as_str()).map(String::from);
            let cx = cfg.get("cx").and_then(|v| v.as_str()).map(String::from);

            if let Some(provider_type) = SearchProvider::from_str(name) {
                // Avoid duplicating DuckDuckGo entry.
                if let Some(existing) = providers.iter_mut().find(|p| p.name == *name) {
                    existing.enabled = enabled;
                    if api_key.is_some() {
                        existing.api_key = api_key;
                    }
                    if base_url.is_some() {
                        existing.base_url = base_url;
                    }
                } else {
                    providers.push(ProviderConfig {
                        name: name.clone(),
                        provider_type,
                        api_key,
                        base_url,
                        cx,
                        enabled,
                    });
                }
            }
        }
    }

    Some(SearchConfig {
        providers,
        default_provider,
        cache_ttl_secs,
        max_results,
    })
}

/// Overlay API keys from well-known environment variables onto any provider
/// that doesn't already have an API key set.
fn inject_env_api_keys(config: &mut SearchConfig) {
    for provider in &mut config.providers {
        if provider.api_key.is_some() {
            continue;
        }
        let env_var = match provider.provider_type {
            SearchProvider::Tavily => "TAVILY_API_KEY",
            SearchProvider::Exa => "EXA_API_KEY",
            SearchProvider::Brave => "BRAVE_SEARCH_API_KEY",
            SearchProvider::Google => "GOOGLE_SEARCH_API_KEY",
            SearchProvider::Perplexity => "PERPLEXITY_API_KEY",
            SearchProvider::Bing => "BING_SEARCH_API_KEY",
            SearchProvider::SearXNG => {
                // Base URL from env (no auth key)
                if provider.base_url.is_none() {
                    if let Ok(url) = std::env::var("SEARXNG_URL") {
                        if !url.is_empty() {
                            provider.base_url = Some(url);
                        }
                    }
                }
                continue;
            }
            SearchProvider::DuckDuckGo => continue,
        };
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                provider.api_key = Some(key);
            }
        }
    }
    // Also inject Google CX.
    for provider in &mut config.providers {
        if provider.provider_type == SearchProvider::Google && provider.cx.is_none() {
            if let Ok(cx) = std::env::var("GOOGLE_SEARCH_CX") {
                if !cx.is_empty() {
                    provider.cx = Some(cx);
                }
            }
        }
    }
}

fn provider_has_credentials(cfg: &ProviderConfig) -> bool {
    match cfg.provider_type {
        SearchProvider::DuckDuckGo | SearchProvider::SearXNG => true, // public instances need no key
        SearchProvider::Google => cfg.api_key.is_some() && cfg.cx.is_some(),
        _ => cfg.api_key.is_some(),
    }
}

// ---------------------------------------------------------------------------
// Home directory helper
// ---------------------------------------------------------------------------

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            #[cfg(target_os = "macos")]
            {
                std::env::var("USERPROFILE").ok().map(std::path::PathBuf::from)
            }
            #[cfg(not(target_os = "macos"))]
            None
        })
}

// ---------------------------------------------------------------------------
// Public convenience: format a provider list for display
// ---------------------------------------------------------------------------

#[must_use]
pub fn format_provider_list(providers: &[(String, bool, bool)]) -> String {
    if providers.is_empty() {
        return "No search providers configured.".to_string();
    }
    let mut lines = vec!["Search providers:".to_string()];
    for (name, enabled, creds) in providers {
        let status = if !enabled {
            "disabled"
        } else if *creds {
            "enabled, ready"
        } else {
            "enabled, no credentials"
        };
        lines.push(format!("  {name:<14}  {status}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_from_str_round_trips() {
        for name in &["duckduckgo", "ddg", "tavily", "exa", "searxng", "brave", "google", "perplexity", "bing"] {
            assert!(SearchProvider::from_str(name).is_some(), "failed for {name}");
        }
        assert!(SearchProvider::from_str("unknown").is_none());
    }

    #[test]
    fn search_engine_list_providers_default() {
        let engine = SearchEngine::new(SearchConfig::default());
        let list = engine.list_providers();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "duckduckgo");
        assert!(list[0].1, "duckduckgo should be enabled");
        assert!(list[0].2, "duckduckgo should report credentials ok");
    }

    #[test]
    fn format_provider_list_produces_output() {
        let providers = vec![
            ("duckduckgo".to_string(), true, true),
            ("tavily".to_string(), false, false),
        ];
        let out = format_provider_list(&providers);
        assert!(out.contains("duckduckgo"));
        assert!(out.contains("tavily"));
        assert!(out.contains("disabled"));
    }
}
