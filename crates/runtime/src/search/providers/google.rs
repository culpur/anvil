use super::super::{ProviderConfig, SearchResult};
use super::build_blocking_client;

pub fn search(
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
    parse_response(&val, max_results)
}

fn parse_response(
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
