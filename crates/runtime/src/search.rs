use std::collections::HashMap;
use std::time::{Duration, Instant};

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
    /// Base URL override (used for SearXNG custom instances).
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

pub struct SearchEngine {
    config: SearchConfig,
    cache: HashMap<String, (Instant, Vec<SearchResult>)>,
}

impl SearchEngine {
    #[must_use]
    pub fn new(config: SearchConfig) -> Self {
        Self {
            config,
            cache: HashMap::new(),
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
        if let Some((ts, results)) = self.cache.get(&cache_key) {
            if ts.elapsed() < ttl {
                return Ok(results.clone());
            }
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
        let results = execute_provider_search(&cfg, query, max)?;

        self.cache.insert(cache_key, (Instant::now(), results.clone()));
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
            match execute_provider_search(cfg, query, max) {
                Ok(results) => {
                    for r in results {
                        if seen_urls.insert(r.url.clone()) {
                            combined.push(r);
                        }
                    }
                }
                Err(_) => {}
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
        .and_then(|v| v.as_u64())
        .unwrap_or(300);

    let max_results = value
        .get("max_results")
        .and_then(|v| v.as_u64())
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
            let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
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
        SearchProvider::DuckDuckGo => true,
        SearchProvider::SearXNG => true, // public instances need no key
        SearchProvider::Google => cfg.api_key.is_some() && cfg.cx.is_some(),
        _ => cfg.api_key.is_some(),
    }
}

// ---------------------------------------------------------------------------
// Per-provider search implementations (blocking reqwest)
// ---------------------------------------------------------------------------

fn execute_provider_search(
    cfg: &ProviderConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    match cfg.provider_type {
        SearchProvider::DuckDuckGo => search_duckduckgo(query, max_results, cfg),
        SearchProvider::Tavily => search_tavily(query, max_results, cfg),
        SearchProvider::Exa => search_exa(query, max_results, cfg),
        SearchProvider::SearXNG => search_searxng(query, max_results, cfg),
        SearchProvider::Brave => search_brave(query, max_results, cfg),
        SearchProvider::Google => search_google(query, max_results, cfg),
        SearchProvider::Perplexity => search_perplexity(query, max_results, cfg),
        SearchProvider::Bing => search_bing(query, max_results, cfg),
    }
}

// ------ DuckDuckGo ---------------------------------------------------------

fn search_duckduckgo(
    query: &str,
    max_results: usize,
    _cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let base = std::env::var("ANVIL_WEB_SEARCH_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://html.duckduckgo.com/html".to_string());

    let client = build_blocking_client()?;
    let response = client
        .post(&base)
        .form(&[("q", query)])
        .send()
        .map_err(|e| e.to_string())?;

    let html = response.text().map_err(|e| e.to_string())?;
    let hits = parse_ddg_html(&html, max_results);
    Ok(hits)
}

fn parse_ddg_html(html: &str, limit: usize) -> Vec<SearchResult> {
    // DuckDuckGo HTML returns result links inside <a class="result__a"> tags.
    // We do a simple text scan — no external HTML parser dependency.
    let mut results = Vec::new();
    let mut pos = 0usize;

    while results.len() < limit {
        // Look for the class marker anywhere in the remaining text.
        let class_marker = "class=\"result__a\"";
        let Some(class_offset) = html[pos..].find(class_marker) else {
            break;
        };
        let abs_class = pos + class_offset;

        // Walk backwards from class_offset to find the opening '<a'.
        let tag_start = match html[..abs_class].rfind("<a") {
            Some(idx) => idx,
            None => {
                // No opening tag found — skip past this class marker.
                pos = abs_class + class_marker.len();
                continue;
            }
        };

        // The tag_fragment runs from <a ... to the first >.
        let tag_end_abs = match html[tag_start..].find('>') {
            Some(rel) => tag_start + rel + 1,
            None => {
                pos = abs_class + class_marker.len();
                continue;
            }
        };
        let tag_fragment = &html[tag_start..tag_end_abs];

        // Extract href from within the opening tag.
        let href = extract_attr(tag_fragment, "href").unwrap_or_default();
        let url = decode_ddg_redirect(&href).unwrap_or_else(|| href.clone());

        // Find the closing </a> after the opening tag.
        let Some(close_rel) = html[tag_end_abs..].find("</a>") else {
            pos = tag_end_abs;
            continue;
        };
        let inner_text_end = tag_end_abs + close_rel;
        let title_raw = &html[tag_end_abs..inner_text_end];
        let title = strip_inner_tags(title_raw);

        // Advance past this </a>.
        pos = inner_text_end + 4;

        // Snippet lives in the next element with class result__snippet.
        let snippet = if let Some(snip_start) = html[pos..].find("class=\"result__snippet\"") {
            let snip_abs = pos + snip_start;
            // Find the element containing the snippet.
            let snip_open = html[..snip_abs].rfind('<').unwrap_or(snip_abs);
            let _snip_tag = html[snip_abs..].split_whitespace().next().unwrap_or("span");
            extract_tag_text(&html[snip_open..], "span")
                .or_else(|| extract_tag_text(&html[snip_open..], "a"))
                .unwrap_or_default()
        } else {
            String::new()
        };

        if !url.is_empty() && !title.is_empty() {
            results.push(SearchResult {
                title: strip_html_entities(&title),
                url,
                snippet: strip_html_entities(&snippet),
                source: "duckduckgo".to_string(),
            });
        }
    }

    results
}

fn decode_ddg_redirect(href: &str) -> Option<String> {
    // DuckDuckGo wraps links as //duckduckgo.com/l/?uddg=<encoded-url>
    if !href.contains("uddg=") {
        return None;
    }
    let uddg = href.split("uddg=").nth(1)?;
    let encoded = uddg.split('&').next()?;
    percent_decode(encoded)
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    loop {
        match bytes.next() {
            None => break,
            Some(b'%') => {
                let hi = bytes.next()?;
                let lo = bytes.next()?;
                let val = hex_nibble(hi)? << 4 | hex_nibble(lo)?;
                out.push(val as char);
            }
            Some(b'+') => out.push(' '),
            Some(b) => out.push(b as char),
        }
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ------ Tavily -------------------------------------------------------------

fn search_tavily(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Tavily: missing API key (set TAVILY_API_KEY)".to_string())?;

    let client = build_blocking_client()?;
    let body = serde_json::json!({
        "query": query,
        "max_results": max_results,
        "search_depth": "basic",
    });

    let response = client
        .post("https://api.tavily.com/search")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_tavily_response(&val, max_results)
}

fn parse_tavily_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = val
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Tavily: unexpected response format".to_string())?;

    Ok(results
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("content")
                    .or_else(|| r.get("snippet"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "tavily".to_string(),
            })
        })
        .collect())
}

// ------ Exa ----------------------------------------------------------------

fn search_exa(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Exa: missing API key (set EXA_API_KEY)".to_string())?;

    let client = build_blocking_client()?;
    let body = serde_json::json!({
        "query": query,
        "numResults": max_results,
        "type": "auto",
    });

    let response = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_exa_response(&val, max_results)
}

fn parse_exa_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = val
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Exa: unexpected response format".to_string())?;

    Ok(results
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("snippet")
                    .or_else(|| r.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "exa".to_string(),
            })
        })
        .collect())
}

// ------ SearXNG ------------------------------------------------------------

fn search_searxng(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let base = cfg
        .base_url
        .as_deref()
        .unwrap_or("https://searx.be");

    let url = format!("{base}/search");
    let client = build_blocking_client()?;
    let mut req = client
        .get(&url)
        .query(&[("q", query), ("format", "json")]);

    if let Some(key) = cfg.api_key.as_deref() {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    let response = req.send().map_err(|e| e.to_string())?;
    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_searxng_response(&val, max_results)
}

fn parse_searxng_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = val
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "SearXNG: unexpected response format".to_string())?;

    Ok(results
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "searxng".to_string(),
            })
        })
        .collect())
}

// ------ Brave --------------------------------------------------------------

fn search_brave(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Brave: missing API key (set BRAVE_SEARCH_API_KEY)".to_string())?;

    let client = build_blocking_client()?;
    let response = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &max_results.to_string())])
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_brave_response(&val, max_results)
}

fn parse_brave_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = val
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Brave: unexpected response format".to_string())?;

    Ok(results
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "brave".to_string(),
            })
        })
        .collect())
}

// ------ Google Custom Search -----------------------------------------------

fn search_google(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Google: missing API key (set GOOGLE_SEARCH_API_KEY)".to_string())?;
    let cx = cfg
        .cx
        .as_deref()
        .ok_or_else(|| "Google: missing CX (set GOOGLE_SEARCH_CX)".to_string())?;

    let client = build_blocking_client()?;
    let count = max_results.min(10); // Google CSE max is 10 per call
    let response = client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[
            ("key", api_key),
            ("cx", cx),
            ("q", query),
            ("num", &count.to_string()),
        ])
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_google_response(&val, max_results)
}

fn parse_google_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let items = val
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Google: unexpected response format".to_string())?;

    Ok(items
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("title")?.as_str()?.to_string(),
                url: r.get("link")?.as_str()?.to_string(),
                snippet: r
                    .get("snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "google".to_string(),
            })
        })
        .collect())
}

// ------ Perplexity ---------------------------------------------------------

fn search_perplexity(
    query: &str,
    _max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Perplexity: missing API key (set PERPLEXITY_API_KEY)".to_string())?;

    let client = build_blocking_client()?;
    let body = serde_json::json!({
        "model": "sonar-small-online",
        "messages": [{"role": "user", "content": format!("search: {query}")}],
    });

    let response = client
        .post("https://api.perplexity.ai/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_perplexity_response(&val, query)
}

fn parse_perplexity_response(
    val: &serde_json::Value,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let content = val
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Perplexity: unexpected response format".to_string())?;

    // Perplexity returns a natural-language answer, not a list of links.
    // Wrap the answer as a single synthesised result.
    Ok(vec![SearchResult {
        title: format!("Perplexity answer: {query}"),
        url: "https://www.perplexity.ai/".to_string(),
        snippet: content.to_string(),
        source: "perplexity".to_string(),
    }])
}

// ------ Bing ---------------------------------------------------------------

fn search_bing(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let api_key = cfg
        .api_key
        .as_deref()
        .ok_or_else(|| "Bing: missing API key (set BING_SEARCH_API_KEY)".to_string())?;

    let client = build_blocking_client()?;
    let response = client
        .get("https://api.bing.microsoft.com/v7.0/search")
        .header("Ocp-Apim-Subscription-Key", api_key)
        .query(&[("q", query), ("count", &max_results.to_string())])
        .send()
        .map_err(|e| e.to_string())?;

    let val: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    parse_bing_response(&val, max_results)
}

fn parse_bing_response(
    val: &serde_json::Value,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = val
        .get("webPages")
        .and_then(|w| w.get("value"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Bing: unexpected response format".to_string())?;

    Ok(results
        .iter()
        .take(limit)
        .filter_map(|r| {
            Some(SearchResult {
                title: r.get("name")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                snippet: r
                    .get("snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: "bing".to_string(),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Shared HTTP client helper
// ---------------------------------------------------------------------------

fn build_blocking_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("anvil-search/0.1")
        .build()
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// HTML mini-helpers (no external dep)
// ---------------------------------------------------------------------------

fn extract_attr(html: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find('"')?;
    Some(html[start..start + end].to_string())
}

fn extract_tag_text(html: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start_tag = html.find(&open)?;
    let inner_start = html[start_tag..].find('>')? + start_tag + 1;
    let close = format!("</{tag}>");
    let inner_end = html[inner_start..].find(&close)?;
    let raw = &html[inner_start..inner_start + inner_end];
    Some(strip_inner_tags(raw))
}

fn strip_inner_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut inside_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn strip_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
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
    fn ddg_html_parsing_extracts_results() {
        // Minimal DDG-like HTML fragment.
        let html = r#"
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com">
                Example Site
            </a>
            <span class="result__snippet">A description here</span>
        "#;
        let results = parse_ddg_html(html, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].url, "https://example.com");
        assert!(results[0].title.contains("Example Site"));
    }

    #[test]
    fn percent_decode_handles_encoded_url() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com").unwrap(),
            "https://example.com"
        );
    }

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
