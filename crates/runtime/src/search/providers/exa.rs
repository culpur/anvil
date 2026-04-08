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
    parse_response(&val, max_results)
}

fn parse_response(
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
