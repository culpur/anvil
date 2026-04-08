use super::super::{ProviderConfig, SearchResult};
use super::build_blocking_client;

pub fn search(
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
    let mut results = Vec::new();
    let mut pos = 0usize;

    while results.len() < limit {
        let class_marker = "class=\"result__a\"";
        let Some(class_offset) = html[pos..].find(class_marker) else {
            break;
        };
        let abs_class = pos + class_offset;

        let Some(tag_start) = html[..abs_class].rfind("<a") else {
            pos = abs_class + class_marker.len();
            continue;
        };

        let Some(rel) = html[tag_start..].find('>') else {
            pos = abs_class + class_marker.len();
            continue;
        };
        let tag_end_abs = tag_start + rel + 1;
        let tag_fragment = &html[tag_start..tag_end_abs];

        let href = extract_attr(tag_fragment, "href").unwrap_or_default();
        let url = decode_ddg_redirect(&href).unwrap_or_else(|| href.clone());

        let Some(close_rel) = html[tag_end_abs..].find("</a>") else {
            pos = tag_end_abs;
            continue;
        };
        let inner_text_end = tag_end_abs + close_rel;
        let title_raw = &html[tag_end_abs..inner_text_end];
        let title = strip_inner_tags(title_raw);

        pos = inner_text_end + 4;

        let snippet = if let Some(snip_start) = html[pos..].find("class=\"result__snippet\"") {
            let snip_abs = pos + snip_start;
            let snip_open = html[..snip_abs].rfind('<').unwrap_or(snip_abs);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_html_parsing_extracts_results() {
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
}
