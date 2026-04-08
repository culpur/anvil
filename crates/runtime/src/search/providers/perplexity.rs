use super::super::{ProviderConfig, SearchResult};
use super::build_blocking_client;

pub fn search(
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
    parse_response(&val, query)
}

fn parse_response(
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
