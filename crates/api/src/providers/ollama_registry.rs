//! Ollama OCI registry client for listing remote tags + manifest sizes.
//!
//! This module powers `/ollama requantize` (Task #370, "L8") — the helper
//! that lets a user discover alternate quantization variants of a model
//! they already have installed locally.  Unlike the rest of `providers/`,
//! this module talks to the **public Ollama registry** (an OCI-style
//! container registry at `registry.ollama.ai`) rather than the local
//! daemon.  We don't pull anything from the registry directly — we only
//! enumerate tags and surface manifest sizes so the user can pick a target
//! and run `/ollama pull <tag>` themselves.
//!
//! # Why the OCI registry, not `ollama.com/library/<model>/tags`?
//!
//! Two options exist for tag discovery:
//!
//! 1. **HTML scrape** of `https://ollama.com/library/<base>/tags`.  The
//!    page lists tags + sizes but the layout has changed twice in the past
//!    year — brittle and not contractually stable.
//! 2. **OCI registry** at `registry.ollama.ai/v2/library/<base>/tags/list`.
//!    Returns clean JSON (`{"name":"library/<base>","tags":[...]}`) and
//!    follows the documented OCI Distribution Spec.  Stable.
//!
//! We use **option 2**.  The endpoints we hit:
//!
//!   * `GET https://registry.ollama.ai/v2/library/<base>/tags/list`
//!     — returns `{"name":"library/<base>","tags":["latest","7b",...]}`.
//!   * `GET https://registry.ollama.ai/v2/library/<base>/manifests/<tag>`
//!     with `Accept: application/vnd.docker.distribution.manifest.v2+json`
//!     — returns an OCI manifest with `config` + `layers[]` (each carrying
//!     a `size` field in bytes).  Summing `config.size + sum(layers[].size)`
//!     gives the on-wire pull size.
//!
//! # Honesty contract
//!
//! Per the `ollama-cloud-auth` rule the **chat path** must always go through
//! the local daemon at `localhost:11434`.  This module is an **introspection
//! helper** — it reads (never writes) the public registry to discover what
//! quant variants exist.  It does NOT pull, push, or authenticate — purely
//! read-only HTTP GETs.  The user still runs `/ollama pull` (which routes
//! to the local daemon) to actually fetch a tag.
//!
//! # No new crate dependencies
//!
//! We use only `reqwest` + `serde_json` (already pulled in by the workspace).
//! A single 5-second timeout applies to every call — registry availability
//! must never block the user's TUI.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const REGISTRY_BASE_URL: &str = "https://registry.ollama.ai";
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(5);
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.v2+json";

// ─── Public types ────────────────────────────────────────────────────────────

/// One tag from the Ollama registry, decomposed into its semantic parts.
///
/// Tag conventions on the Ollama registry are not formally documented but
/// in practice fall into a small number of shapes:
///
///   * `latest`                                 → no size, no quant
///   * `7b`, `8b`, `13b`, `70b`, `480b`         → size only, no quant
///   * `7b-instruct`                             → size + variant suffix, no quant
///   * `7b-q4_K_M`, `8b-q5_K_M`, `70b-q8_0`     → size + quant
///   * `7b-instruct-q4_K_M`                      → size + variant + quant
///   * `q4_K_M`, `q5_K_M`                        → quant only (rare)
///
/// The parser is tolerant — anything it can't recognise lands in the
/// `Unknown` shape (`size_tag = None`, `quantization = None`) so the caller
/// can still surface the raw tag in a "no match" listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryTag {
    /// Full tag spec ready to feed to `/ollama pull`, e.g.
    /// `qwen3-coder:7b-instruct-q4_K_M`.
    pub name: String,
    /// Base model name with no `:tag` suffix, e.g. `qwen3-coder`.
    pub base: String,
    /// Size token from the tag if present, e.g. `7b`, `7b-instruct`,
    /// `70b-chat`.  `None` when the tag is quant-only or `latest`.
    pub size_tag: Option<String>,
    /// Quantization level parsed from the tag suffix, e.g. `q4_K_M`,
    /// `q5_0`, `q8_0`, `f16`.  Stored verbatim from the tag (mixed-case
    /// `q4_K_M` is the most common form on the registry).  `None` when no
    /// quant suffix was detected.
    pub quantization: Option<String>,
    /// Sum of `config.size + sum(layers[].size)` from the OCI manifest.
    /// `None` when the manifest fetch was skipped or failed — the caller
    /// surfaces "size unknown" in that case.
    pub total_bytes: Option<u64>,
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum RegistryError {
    /// Network failure or non-2xx HTTP status.
    Http(String),
    /// Manifest/tag-list payload didn't deserialize to the expected shape.
    Parse(String),
    /// The registry replied 404 — base model doesn't exist on the public
    /// `library/` namespace.
    UnknownBase(String),
}

impl Display for RegistryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "registry HTTP error: {msg}"),
            Self::Parse(msg) => write!(f, "registry parse error: {msg}"),
            Self::UnknownBase(name) => {
                write!(f, "model not found in Ollama registry: {name}")
            }
        }
    }
}

impl Error for RegistryError {}

// ─── Pure parser ─────────────────────────────────────────────────────────────

/// Decompose a `<base>:<tag>` string (or just `<tag>` when the caller has
/// already split off the base) into its semantic parts.
///
/// Accepts both `qwen3-coder:7b-instruct-q4_K_M` and `7b-instruct-q4_K_M`.
/// When no colon is present, `base` is left empty — the caller is
/// responsible for splicing the model name back in via `with_base()`.
///
/// The function is **pure** and never panics: any unrecognised input
/// becomes `RegistryTag { quantization: None, size_tag: None, .. }`.
#[must_use]
pub fn parse_tag_components(full_tag: &str) -> RegistryTag {
    let trimmed = full_tag.trim();
    let (base, tag) = match trimmed.split_once(':') {
        Some((b, t)) => (b.to_string(), t.to_string()),
        None => (String::new(), trimmed.to_string()),
    };

    // Recombine for the canonical .name field — preserves whatever the
    // caller passed us.
    let name = if base.is_empty() {
        tag.clone()
    } else {
        format!("{base}:{tag}")
    };

    if tag.is_empty() || tag == "latest" {
        return RegistryTag {
            name,
            base,
            size_tag: None,
            quantization: None,
            total_bytes: None,
        };
    }

    // Strategy: split on '-', then walk the segments looking for a quant
    // suffix anywhere in the chain.  Everything before the quant token
    // becomes the size_tag; everything else is dropped.  When no quant
    // token is detected the whole tag becomes the size_tag.
    //
    // A "quant token" is any segment that:
    //   * starts with `q` followed by a digit (q4, q5, q6, q8, …) — the
    //     vast majority of llama.cpp quants, OR
    //   * matches `f16` / `bf16` / `f32` (full-precision variants), OR
    //   * starts with `iq` followed by a digit (newer importance-matrix
    //     quants like `iq4_xs`).
    //
    // Quant tokens may be single segments (`q4`) or multi-segment
    // (`q4_K_M`, `q5_K_S`).  Once we find a quant-starting segment we
    // greedily consume the rest of the tag as the quant — the remaining
    // segments are quant qualifiers (`K_M`, `K_S`, `0`) not size info.
    let segments: Vec<&str> = tag.split('-').collect();
    let mut quant_start: Option<usize> = None;
    for (i, seg) in segments.iter().enumerate() {
        if is_quant_token_start(seg) {
            quant_start = Some(i);
            break;
        }
    }

    let (size_tag, quantization) = match quant_start {
        Some(0) => {
            // Tag like `q4_K_M` with no preceding size.
            (None, Some(segments.join("-")))
        }
        Some(i) => {
            let size = segments[..i].join("-");
            let quant = segments[i..].join("-");
            (
                if size.is_empty() { None } else { Some(size) },
                Some(quant),
            )
        }
        None => {
            // No quant suffix detected — assume the whole tag is a size token.
            (Some(tag.clone()), None)
        }
    };

    RegistryTag {
        name,
        base,
        size_tag,
        quantization,
        total_bytes: None,
    }
}

/// Heuristic: is `seg` the start of a quantization token?  Used by the
/// tag parser to find the boundary between the size and quant portions
/// of a multi-segment tag.
///
/// We match case-insensitively because the registry mixes cases freely
/// (`q4_K_M`, `Q4_K_M`, `iq4_XS`, `f16`, `F16`, …).
fn is_quant_token_start(seg: &str) -> bool {
    let lower = seg.to_ascii_lowercase();
    if lower == "f16" || lower == "bf16" || lower == "f32" {
        return true;
    }
    // q<digit>… or iq<digit>…
    let bytes = lower.as_bytes();
    if bytes.first() == Some(&b'q') && bytes.get(1).is_some_and(u8::is_ascii_digit) {
        return true;
    }
    if bytes.starts_with(b"iq")
        && bytes.get(2).is_some_and(u8::is_ascii_digit)
    {
        return true;
    }
    false
}

/// Normalize a quant string to lowercase-with-underscores form for
/// equality comparisons.  Accepts both `q5_K_M` (registry-canonical
/// mixed-case) and `q5_k_m` (CLI-friendly all-lowercase) and `Q5_K_M`
/// (shouty).  Also tolerates a leading dash (`-q5_K_M`) which is how
/// the suffix appears inside a tag.
#[must_use]
pub fn normalize_quant(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('-')
        .to_ascii_lowercase()
}

// ─── Match-picking (pure) ────────────────────────────────────────────────────

/// Pick the best registry tag matching `target_quant` for a user whose
/// currently-installed tag has `current_size_tag`.
///
/// Strategy:
///   1. Filter `tags` to those whose normalized quant equals
///      `normalize_quant(target_quant)`.
///   2. Among matches, prefer the one whose `size_tag` equals the user's
///      `current_size_tag` (e.g. user has `7b-q4_K_M` → suggest
///      `7b-q5_K_M` over `14b-q5_K_M`).
///   3. Fall back to the first match if no size_tag aligns.
///   4. Return `None` when no tag has the target quant.
///
/// Pure — no I/O, sorting is stable so the caller can rely on the listing
/// order from [`list_registry_tags`].
#[must_use]
pub fn pick_best_match<'a>(
    tags: &'a [RegistryTag],
    target_quant: &str,
    current_size_tag: Option<&str>,
) -> Option<&'a RegistryTag> {
    let want = normalize_quant(target_quant);
    let candidates: Vec<&RegistryTag> = tags
        .iter()
        .filter(|t| {
            t.quantization
                .as_deref()
                .map(normalize_quant)
                .map(|q| q == want)
                .unwrap_or(false)
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if let Some(want_size) = current_size_tag {
        if let Some(hit) = candidates.iter().find(|t| t.size_tag.as_deref() == Some(want_size))
        {
            return Some(*hit);
        }
    }
    candidates.first().copied()
}

/// Distinct quant levels available among `tags` — used for the
/// "available quants" listing when no match for the requested target was
/// found.  Returns lowercase normalised forms; preserves first-seen order.
#[must_use]
pub fn available_quants(tags: &[RegistryTag]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for t in tags {
        if let Some(q) = t.quantization.as_deref() {
            let n = normalize_quant(q);
            if !seen.iter().any(|s| s == &n) {
                seen.push(n);
            }
        }
    }
    seen
}

// ─── Network-touching API ────────────────────────────────────────────────────

/// Enumerate all registry tags for `base` (e.g. `qwen3-coder`) and parse
/// each into a [`RegistryTag`].  No manifest fetch — `total_bytes` is
/// `None` for every entry; the caller can fill that in selectively via
/// [`fetch_tag_manifest_size`] for the few tags the user actually cares
/// about.
///
/// Hard 5-second timeout.  Network errors collapse to
/// [`RegistryError::Http`] so the caller can surface a friendly message
/// without the user seeing a raw `reqwest::Error`.
pub async fn list_registry_tags(base: &str) -> Result<Vec<RegistryTag>, RegistryError> {
    let base = base.trim();
    if base.is_empty() {
        return Err(RegistryError::Http("empty base name".into()));
    }
    let url = format!("{REGISTRY_BASE_URL}/v2/library/{base}/tags/list");
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_TIMEOUT)
        .build()
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Err(RegistryError::UnknownBase(base.to_string()));
    }
    if !status.is_success() {
        return Err(RegistryError::Http(format!("HTTP {}", status.as_u16())));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| RegistryError::Parse(e.to_string()))?;
    Ok(parse_tag_list_response(base, &body))
}

/// Pure parser for the OCI tag-list response.  Extracted so the network
/// call stays a thin shell.
#[must_use]
pub fn parse_tag_list_response(base: &str, body: &Value) -> Vec<RegistryTag> {
    let tags = body
        .get("tags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tags.into_iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .map(|tag| {
            let combined = format!("{base}:{tag}");
            parse_tag_components(&combined)
        })
        .collect()
}

/// Fetch the OCI manifest for `<base>:<tag>` and return the sum of
/// `config.size + sum(layers[].size)` in bytes.
///
/// Hard 5-second timeout.  The OCI manifest media type is required via
/// the `Accept` header — without it the registry returns the legacy
/// docker manifest list which doesn't carry per-layer sizes inline.
pub async fn fetch_tag_manifest_size(base: &str, tag: &str) -> Result<u64, RegistryError> {
    let url = format!("{REGISTRY_BASE_URL}/v2/library/{base}/manifests/{tag}");
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_TIMEOUT)
        .build()
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    let response = client
        .get(&url)
        .header("Accept", MANIFEST_ACCEPT)
        .send()
        .await
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(RegistryError::Http(format!("HTTP {}", status.as_u16())));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| RegistryError::Parse(e.to_string()))?;
    Ok(parse_manifest_size(&body))
}

/// Pure: sum the byte sizes from an OCI manifest payload.
#[must_use]
pub fn parse_manifest_size(body: &Value) -> u64 {
    let config_size = body
        .get("config")
        .and_then(|c| c.get("size"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let layers_size: u64 = body
        .get("layers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("size").and_then(Value::as_u64))
                .sum()
        })
        .unwrap_or(0);
    config_size + layers_size
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Parser ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_tag_components_qwen3_7b_q4_k_m() {
        let t = parse_tag_components("qwen3:7b-instruct-q4_K_M");
        assert_eq!(t.base, "qwen3");
        assert_eq!(t.size_tag.as_deref(), Some("7b-instruct"));
        assert_eq!(t.quantization.as_deref(), Some("q4_K_M"));
        assert_eq!(t.name, "qwen3:7b-instruct-q4_K_M");
        assert!(t.total_bytes.is_none());
    }

    #[test]
    fn parse_tag_components_no_quant_suffix() {
        let t = parse_tag_components("qwen3:latest");
        assert_eq!(t.base, "qwen3");
        assert!(t.size_tag.is_none());
        assert!(t.quantization.is_none());
    }

    #[test]
    fn parse_tag_components_size_only() {
        let t = parse_tag_components("qwen3:7b");
        assert_eq!(t.base, "qwen3");
        assert_eq!(t.size_tag.as_deref(), Some("7b"));
        assert!(t.quantization.is_none());
    }

    #[test]
    fn parse_tag_components_unusual_format() {
        // A tag we've never seen before — must NOT panic and must round-trip
        // .name even when we can't make sense of the suffix.
        let t = parse_tag_components("frobnicate:zarglemorph42");
        assert_eq!(t.base, "frobnicate");
        assert_eq!(t.name, "frobnicate:zarglemorph42");
        // No quant token recognised → size_tag swallows the whole suffix.
        assert_eq!(t.size_tag.as_deref(), Some("zarglemorph42"));
        assert!(t.quantization.is_none());
    }

    #[test]
    fn parse_tag_components_quant_only_tag() {
        // Some models ship quant-only tags (no size segment).
        let t = parse_tag_components("foo:q4_K_M");
        assert_eq!(t.base, "foo");
        assert!(t.size_tag.is_none());
        assert_eq!(t.quantization.as_deref(), Some("q4_K_M"));
    }

    #[test]
    fn parse_tag_components_f16_full_precision() {
        let t = parse_tag_components("phi3:14b-f16");
        assert_eq!(t.size_tag.as_deref(), Some("14b"));
        assert_eq!(t.quantization.as_deref(), Some("f16"));
    }

    #[test]
    fn parse_tag_components_iq_quants() {
        // iquants (importance-matrix) — newer llama.cpp variants.
        let t = parse_tag_components("llama3:70b-iq4_xs");
        assert_eq!(t.size_tag.as_deref(), Some("70b"));
        assert_eq!(t.quantization.as_deref(), Some("iq4_xs"));
    }

    #[test]
    fn parse_tag_components_no_colon() {
        // Bare tag (no base prefix).
        let t = parse_tag_components("7b-q5_K_M");
        assert_eq!(t.base, "");
        assert_eq!(t.size_tag.as_deref(), Some("7b"));
        assert_eq!(t.quantization.as_deref(), Some("q5_K_M"));
        assert_eq!(t.name, "7b-q5_K_M");
    }

    #[test]
    fn parse_tag_components_empty_does_not_panic() {
        let t = parse_tag_components("");
        assert!(t.base.is_empty());
        assert!(t.size_tag.is_none());
        assert!(t.quantization.is_none());
    }

    // ── Quant normalization ─────────────────────────────────────────────────

    #[test]
    fn quant_normalize_case_insensitive() {
        assert_eq!(normalize_quant("q5_K_M"), "q5_k_m");
        assert_eq!(normalize_quant("Q5_K_M"), "q5_k_m");
        assert_eq!(normalize_quant("q5_k_m"), "q5_k_m");
        // Strip leading dash + whitespace.
        assert_eq!(normalize_quant("-q5_K_M"), "q5_k_m");
        assert_eq!(normalize_quant("  q8_0  "), "q8_0");
    }

    // ── Match-picker ─────────────────────────────────────────────────────────

    fn fixture_qwen3_tags() -> Vec<RegistryTag> {
        let raw = [
            "qwen3-coder:latest",
            "qwen3-coder:7b",
            "qwen3-coder:7b-q4_K_M",
            "qwen3-coder:7b-q5_K_M",
            "qwen3-coder:7b-instruct-q4_K_M",
            "qwen3-coder:7b-instruct-q5_K_M",
            "qwen3-coder:14b-q4_K_M",
            "qwen3-coder:14b-q5_K_M",
            "qwen3-coder:14b-q8_0",
        ];
        raw.iter().map(|s| parse_tag_components(s)).collect()
    }

    #[test]
    fn pick_best_match_prefers_same_size_tag() {
        let tags = fixture_qwen3_tags();
        let pick = pick_best_match(&tags, "q5_K_M", Some("7b")).expect("match");
        assert_eq!(pick.name, "qwen3-coder:7b-q5_K_M");
    }

    #[test]
    fn pick_best_match_falls_back_to_first_match() {
        let tags = fixture_qwen3_tags();
        // No 99b variant exists, so we fall back to the first q5_K_M tag.
        let pick = pick_best_match(&tags, "q5_K_M", Some("99b")).expect("match");
        // The fixture order has 7b-q5_K_M before 7b-instruct-q5_K_M and 14b-q5_K_M.
        assert_eq!(pick.name, "qwen3-coder:7b-q5_K_M");
    }

    #[test]
    fn pick_best_match_no_size_hint_picks_first() {
        let tags = fixture_qwen3_tags();
        let pick = pick_best_match(&tags, "q8_0", None).expect("match");
        assert_eq!(pick.name, "qwen3-coder:14b-q8_0");
    }

    #[test]
    fn pick_best_match_returns_none_when_no_matches() {
        let tags = fixture_qwen3_tags();
        assert!(pick_best_match(&tags, "q2_K", Some("7b")).is_none());
    }

    #[test]
    fn pick_best_match_case_insensitive() {
        let tags = fixture_qwen3_tags();
        // User typed shouty; registry is canonical-case.  Both should hit.
        let a = pick_best_match(&tags, "Q5_K_M", Some("7b")).expect("upper");
        let b = pick_best_match(&tags, "q5_k_m", Some("7b")).expect("lower");
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn available_quants_distinct_in_first_seen_order() {
        let tags = fixture_qwen3_tags();
        let qs = available_quants(&tags);
        assert_eq!(qs, vec!["q4_k_m".to_string(), "q5_k_m".into(), "q8_0".into()]);
    }

    // ── Tag-list response parser ────────────────────────────────────────────

    #[test]
    fn parse_tag_list_response_normal_case() {
        let body = json!({
            "name": "library/qwen3-coder",
            "tags": ["latest", "7b", "7b-q4_K_M", "14b-q5_K_M"],
        });
        let parsed = parse_tag_list_response("qwen3-coder", &body);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].name, "qwen3-coder:latest");
        assert_eq!(parsed[2].name, "qwen3-coder:7b-q4_K_M");
        assert_eq!(parsed[2].quantization.as_deref(), Some("q4_K_M"));
    }

    #[test]
    fn parse_tag_list_response_missing_tags_field() {
        // Defensive — a malformed response must NOT panic; we just return
        // an empty Vec so the caller can surface "no tags found".
        assert!(parse_tag_list_response("foo", &json!({})).is_empty());
        assert!(parse_tag_list_response("foo", &json!({"tags": null})).is_empty());
        assert!(parse_tag_list_response("foo", &json!({"tags": "not an array"})).is_empty());
    }

    // ── Manifest size parser ────────────────────────────────────────────────

    #[test]
    fn parse_manifest_size_sums_config_and_layers() {
        let body = json!({
            "schemaVersion": 2,
            "config": { "size": 1234, "digest": "sha256:abc" },
            "layers": [
                { "size": 1_000_000_000u64, "digest": "sha256:1" },
                { "size": 4_000_000_000u64, "digest": "sha256:2" },
                { "size": 234, "digest": "sha256:3" },
            ],
        });
        assert_eq!(parse_manifest_size(&body), 1234 + 1_000_000_000 + 4_000_000_000 + 234);
    }

    #[test]
    fn parse_manifest_size_missing_fields_yields_zero() {
        assert_eq!(parse_manifest_size(&json!({})), 0);
        assert_eq!(parse_manifest_size(&json!({"layers": []})), 0);
        assert_eq!(parse_manifest_size(&json!({"config": {}})), 0);
    }
}
