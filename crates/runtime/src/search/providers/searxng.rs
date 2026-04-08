use super::super::{ProviderConfig, SearchResult};
use super::build_blocking_client;

pub fn search(
    query: &str,
    max_results: usize,
    cfg: &ProviderConfig,
) -> Result<Vec<SearchResult>, String> {
    let base = cfg.base_url.as_deref().unwrap_or("https://searx.be");
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
    parse_response(&val, max_results)
}

fn parse_response(
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
