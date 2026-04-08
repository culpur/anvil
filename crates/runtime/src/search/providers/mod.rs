pub mod brave;
pub mod bing;
pub mod duckduckgo;
pub mod exa;
pub mod google;
pub mod perplexity;
pub mod searxng;
pub mod tavily;

use super::{ProviderConfig, SearchProvider, SearchResult};

/// Dispatch to the correct provider implementation.
pub fn execute(
    cfg: &ProviderConfig,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    match cfg.provider_type {
        SearchProvider::DuckDuckGo => duckduckgo::search(query, max_results, cfg),
        SearchProvider::Tavily => tavily::search(query, max_results, cfg),
        SearchProvider::Exa => exa::search(query, max_results, cfg),
        SearchProvider::SearXNG => searxng::search(query, max_results, cfg),
        SearchProvider::Brave => brave::search(query, max_results, cfg),
        SearchProvider::Google => google::search(query, max_results, cfg),
        SearchProvider::Perplexity => perplexity::search(query, max_results, cfg),
        SearchProvider::Bing => bing::search(query, max_results, cfg),
    }
}

// ---------------------------------------------------------------------------
// Shared HTTP client helper (used by all provider modules)
// ---------------------------------------------------------------------------

use std::time::Duration;

pub fn build_blocking_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("anvil-search/0.1")
        .build()
        .map_err(|e| e.to_string())
}
