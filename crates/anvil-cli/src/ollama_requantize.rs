//! `/ollama requantize <model> <target_quant>` — discover alternate quant
//! variants of a locally-installed model and surface a copy-paste-friendly
//! pull suggestion.
//!
//! Task #370 ("L8").  This command never auto-pulls — it inspects the user's
//! current install via `/api/tags` + `/api/show`, queries the public OCI
//! registry at `registry.ollama.ai/v2/library/<base>/tags/list`, picks the
//! best-matching tag for the requested target quant, and prints a
//! `/ollama pull <tag>` line the user can copy.
//!
//! ## Layering
//!
//! Pure formatters ([`format_requantize_summary`], [`format_no_variants`],
//! [`format_already_target_quant`]) take plain values and return `String`
//! — no I/O.  These are exhaustively unit-tested.
//!
//! [`cmd_requantize_blocking`] is the network-touching entry point.  It
//! resolves `host` defaults, fetches `/api/tags` + `/api/show` against
//! the local daemon, then queries the registry for the base.  Each error
//! path returns a user-readable string — never panics, never bubbles a
//! raw `reqwest::Error`.

use std::time::Duration;

use api::{
    available_quants, list_registry_tags, normalize_quant, parse_tag_components,
    pick_best_match, RegistryError, RegistryTag,
};

// ─── Constants ───────────────────────────────────────────────────────────────

const DAEMON_TIMEOUT: Duration = Duration::from_secs(5);

// ─── Pure formatters ─────────────────────────────────────────────────────────

/// Summary lines for `format_requantize_summary` — pulled out so the
/// formatter can be exercised end-to-end without faking
/// `RegistryTag` instances.
#[derive(Debug, Clone)]
pub struct RequantizeSummary<'a> {
    /// Base model name, e.g. `qwen3-coder`.
    pub base: &'a str,
    /// Currently-installed full tag, e.g. `qwen3-coder:7b-q4_K_M`.
    pub current_full_name: &'a str,
    /// Current quant level (verbatim from `/api/show`), e.g. `Q4_K_M`.
    pub current_quant: &'a str,
    /// Target quant the user requested, e.g. `q5_K_M`.
    pub target_quant: &'a str,
    /// Bytes on disk for the currently-installed tag.  `0` means "unknown".
    pub current_size_bytes: u64,
    /// All registry tags whose normalised quant equals `target_quant`.
    pub target_variants: &'a [RegistryTag],
    /// Best match (size_tag-aligned with the user's current install) — or
    /// `None` to skip the "Suggested pull" block.
    pub best_match: Option<&'a RegistryTag>,
}

/// Render the multi-block summary the user sees after running
/// `/ollama requantize <model> <target_quant>`.  All formatting is line-
/// oriented and copy-paste safe (no shell-prompt prefixes, no ANSI
/// escapes).
#[must_use]
pub fn format_requantize_summary(summary: &RequantizeSummary<'_>) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Requantize {}: {} -> {}\n\n",
        summary.base,
        summary.current_quant,
        summary.target_quant,
    ));

    s.push_str("Currently installed:\n");
    s.push_str(&format!(
        "  {} ({})\n",
        summary.current_full_name,
        format_size(summary.current_size_bytes),
    ));
    s.push('\n');

    s.push_str(&format!(
        "Available {} variants from registry:\n",
        summary.target_quant,
    ));
    if summary.target_variants.is_empty() {
        s.push_str("  (none)\n");
    } else {
        for tag in summary.target_variants {
            let size = match tag.total_bytes {
                Some(b) => format_size(b),
                None => "size unknown".to_string(),
            };
            s.push_str(&format!("  {:<48}  ({})\n", tag.name, size));
        }
    }
    s.push('\n');

    if let Some(best) = summary.best_match {
        s.push_str("Suggested pull (matching your size):\n");
        s.push_str(&format!("  /ollama pull {}\n", best.name));
        if let (Some(target_bytes), current_bytes) =
            (best.total_bytes, summary.current_size_bytes)
        {
            if current_bytes > 0 && target_bytes > 0 {
                s.push('\n');
                s.push_str(&format!(
                    "Size diff: {}\n",
                    format_size_diff(current_bytes, target_bytes),
                ));
            }
        }
    } else if !summary.target_variants.is_empty() {
        // We have variants but no size-aligned match — surface the first
        // variant as a starting point so the user has *something* to pull.
        s.push_str("No size-matched variant — pick from the list above:\n");
        s.push_str(&format!(
            "  /ollama pull {}\n",
            summary.target_variants[0].name,
        ));
    }
    s
}

/// Format the "no variants found" block for when the registry has tags
/// for `base` but none with the requested `target_quant`.
#[must_use]
pub fn format_no_variants(base: &str, target_quant: &str, available: &[String]) -> String {
    let mut s = format!(
        "No {target_quant} variants of {base} available on the Ollama registry.\n"
    );
    if available.is_empty() {
        s.push_str("Available quants: (none could be parsed from the registry tag list)\n");
    } else {
        s.push_str(&format!("Available quants: {}\n", available.join(", ")));
    }
    s.push_str("Use a different target quant.\n");
    s
}

/// Format the no-op response when the user's current quant already
/// matches the target.
#[must_use]
pub fn format_already_target_quant(base: &str, quant: &str) -> String {
    format!("Already using {quant} for {base}. No action.\n")
}

/// Format the "model not installed" path — we can't proceed because we
/// have no local install to compare against.
#[must_use]
pub fn format_not_installed(base: &str) -> String {
    format!(
        "{base} is not installed locally — `/ollama requantize` needs an existing \
         install to compare against.\n\
         Pull a base tag first, e.g. `/ollama pull {base}:latest`.\n",
    )
}

// ─── Pure size helpers ───────────────────────────────────────────────────────

#[must_use]
fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "size unknown".to_string();
    }
    #[allow(clippy::cast_precision_loss)]
    let gb = (bytes as f64) / 1_000_000_000.0;
    format!("{gb:.1} GB")
}

#[must_use]
fn format_size_diff(from_bytes: u64, to_bytes: u64) -> String {
    if from_bytes == 0 || to_bytes == 0 {
        return "size diff unknown".to_string();
    }
    #[allow(clippy::cast_precision_loss)]
    let from_gb = (from_bytes as f64) / 1_000_000_000.0;
    #[allow(clippy::cast_precision_loss)]
    let to_gb = (to_bytes as f64) / 1_000_000_000.0;
    let delta = to_gb - from_gb;
    let sign = if delta >= 0.0 { "+" } else { "" };
    format!(
        "{sign}{delta:.1} GB ({from_gb:.1} GB -> {to_gb:.1} GB)",
    )
}

// ─── Argument parsing ────────────────────────────────────────────────────────

/// Pure: split `<model> <target_quant>` from the slash-arg remainder.
/// Returns `Err(usage_string)` on missing args.
pub fn parse_requantize_args(rest: &str) -> Result<(String, String), String> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(usage_line());
    }
    let mut parts = trimmed.split_whitespace();
    let model = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if model.is_empty() || target.is_empty() {
        return Err(usage_line());
    }
    Ok((model, target))
}

#[must_use]
pub fn usage_line() -> String {
    "Usage: /ollama requantize <model> <target_quant>\n\
     Example: /ollama requantize qwen3-coder q5_K_M\n"
        .to_string()
}

// ─── Network-touching entry point ────────────────────────────────────────────

/// Strip the tag from a `<base>:<tag>` string (or pass through unchanged).
fn strip_tag(model: &str) -> String {
    match model.split_once(':') {
        Some((base, _)) => base.to_string(),
        None => model.to_string(),
    }
}

/// Find the locally-installed tag whose base matches `base`.  Returns
/// the full tag (e.g. `qwen3-coder:7b-q4_K_M`) and the on-disk byte size
/// reported by `/api/tags`.
fn find_local_install(installed: &[(String, u64)], base: &str) -> Option<(String, u64)> {
    installed
        .iter()
        .find(|(name, _)| strip_tag(name) == base)
        .cloned()
}

/// Synchronous entry point for the CLI dispatcher.  Wraps the async
/// implementation in a tokio runtime — mirrors the pattern used by
/// `cmd_pull_blocking` next door.
#[must_use]
pub fn cmd_requantize_blocking(host: &str, rest: &str) -> String {
    let (model, target_quant) = match parse_requantize_args(rest) {
        Ok(pair) => pair,
        Err(msg) => return msg,
    };
    let base = strip_tag(&model);
    block_on(cmd_requantize_async(host, &base, &target_quant))
}

fn block_on<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(fut),
    }
}

async fn cmd_requantize_async(host: &str, base: &str, target_quant: &str) -> String {
    // Step 1: enumerate locally-installed tags so we can find the user's
    // current install of `base`.  We hit `/api/tags` directly (not
    // `list_installed_models`, which discards size info) so we can
    // surface `current_size_bytes` in the summary.
    let installed = match fetch_local_tags_with_sizes(host).await {
        Ok(v) => v,
        Err(msg) => return msg,
    };
    let (current_full, current_size) = match find_local_install(&installed, base) {
        Some(p) => p,
        None => return format_not_installed(base),
    };

    // Step 2: read the current quant from the parsed local tag.  If we
    // can't parse one, fall back to the literal "unknown" so the summary
    // still renders.
    let current_components = parse_tag_components(&current_full);
    let current_quant_label = current_components
        .quantization
        .clone()
        .unwrap_or_else(|| "(unknown)".to_string());

    // Step 3: short-circuit when current quant already matches target.
    if !current_quant_label.starts_with('(')
        && normalize_quant(&current_quant_label) == normalize_quant(target_quant)
    {
        return format_already_target_quant(base, target_quant);
    }

    // Step 4: enumerate registry tags.
    let tags = match list_registry_tags(base).await {
        Ok(t) => t,
        Err(RegistryError::UnknownBase(_)) => {
            return format!(
                "{base} is not present in the public Ollama registry \
                 (no library/{base} namespace).\n\
                 Custom-published models are not supported by /ollama requantize.\n",
            );
        }
        Err(e) => return format!("Could not query Ollama registry: {e}\n"),
    };

    // Step 5: filter by target quant.
    let target_variants: Vec<RegistryTag> = tags
        .iter()
        .filter(|t| {
            t.quantization
                .as_deref()
                .map(normalize_quant)
                .map(|q| q == normalize_quant(target_quant))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if target_variants.is_empty() {
        let avail = available_quants(&tags);
        return format_no_variants(base, target_quant, &avail);
    }

    // Step 6: pick best match by size_tag alignment.
    let best = pick_best_match(
        &target_variants,
        target_quant,
        current_components.size_tag.as_deref(),
    )
    .cloned();

    // Step 7: opportunistically fetch manifest sizes for the (small)
    // candidate set — bounded fan-out, single 5s budget.  If any fail
    // we keep going with `total_bytes = None` and the formatter prints
    // "size unknown".  We deliberately do NOT fetch size for ALL tags —
    // only the target_variants — because manifest fetches per tag add
    // up quickly.
    let target_variants = enrich_with_sizes(base, target_variants).await;
    let best = match best {
        Some(b) => target_variants.iter().find(|t| t.name == b.name).cloned(),
        None => None,
    };

    let summary = RequantizeSummary {
        base,
        current_full_name: &current_full,
        current_quant: &current_quant_label,
        target_quant,
        current_size_bytes: current_size,
        target_variants: &target_variants,
        best_match: best.as_ref(),
    };
    format_requantize_summary(&summary)
}

/// Hit `/api/tags` once and return `(name, size)` pairs.  Mirrors
/// `extract_tag_names` but keeps the size field.
async fn fetch_local_tags_with_sizes(host: &str) -> Result<Vec<(String, u64)>, String> {
    let host = if host.is_empty() {
        std::env::var("OLLAMA_HOST")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://localhost:11434".into())
    } else {
        host.to_string()
    };
    let host = host.trim_end_matches('/').to_string();
    let url = format!("{host}/api/tags");
    let client = reqwest::Client::builder()
        .timeout(DAEMON_TIMEOUT)
        .build()
        .map_err(|e| format!("Could not build HTTP client: {e}\n"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|_| {
            "Ollama daemon unreachable. Is `ollama serve` running on http://localhost:11434?\n"
                .to_string()
        })?;
    if !resp.status().is_success() {
        return Err(format!("Ollama daemon returned HTTP {}\n", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Could not parse /api/tags response: {e}\n"))?;
    Ok(body
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let name = m.get("name").and_then(serde_json::Value::as_str)?;
                    let size = m
                        .get("size")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    Some((name.to_string(), size))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Best-effort: walk `tags` and try to fill each `total_bytes` from the
/// registry manifest endpoint.  Failures are silent — the field stays
/// `None` so the formatter prints "size unknown".
async fn enrich_with_sizes(base: &str, tags: Vec<RegistryTag>) -> Vec<RegistryTag> {
    let mut out = Vec::with_capacity(tags.len());
    for mut t in tags {
        // Use the tag suffix (everything after `:`) for the manifest call.
        let tag_suffix = match t.name.split_once(':') {
            Some((_, s)) => s.to_string(),
            None => t.name.clone(),
        };
        match api::fetch_tag_manifest_size(base, &tag_suffix).await {
            Ok(b) if b > 0 => t.total_bytes = Some(b),
            _ => {} // silent failure — keep total_bytes None
        }
        out.push(t);
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use api::parse_tag_components;

    fn tag(full: &str, bytes: Option<u64>) -> RegistryTag {
        let mut t = parse_tag_components(full);
        t.total_bytes = bytes;
        t
    }

    fn variants_q5() -> Vec<RegistryTag> {
        vec![
            tag("qwen3-coder:7b-q5_K_M", Some(6_400_000_000)),
            tag("qwen3-coder:7b-instruct-q5_K_M", Some(6_500_000_000)),
            tag("qwen3-coder:14b-q5_K_M", Some(10_100_000_000)),
        ]
    }

    // ── parse_requantize_args ──────────────────────────────────────────────

    #[test]
    fn parse_requantize_args_happy_path() {
        let (m, q) = parse_requantize_args("qwen3-coder q5_K_M").unwrap();
        assert_eq!(m, "qwen3-coder");
        assert_eq!(q, "q5_K_M");
    }

    #[test]
    fn parse_requantize_args_empty_returns_usage() {
        assert!(parse_requantize_args("").is_err());
        assert!(parse_requantize_args("   ").is_err());
    }

    #[test]
    fn parse_requantize_args_one_arg_returns_usage() {
        assert!(parse_requantize_args("qwen3-coder").is_err());
    }

    // ── format_requantize_summary ──────────────────────────────────────────

    #[test]
    fn format_requantize_with_matching_size() {
        let variants = variants_q5();
        let best = &variants[0]; // qwen3-coder:7b-q5_K_M
        let summary = RequantizeSummary {
            base: "qwen3-coder",
            current_full_name: "qwen3-coder:7b-q4_K_M",
            current_quant: "Q4_K_M",
            target_quant: "q5_K_M",
            current_size_bytes: 5_200_000_000,
            target_variants: &variants,
            best_match: Some(best),
        };
        let out = format_requantize_summary(&summary);
        assert!(out.contains("Requantize qwen3-coder: Q4_K_M -> q5_K_M"));
        assert!(out.contains("Currently installed:"));
        assert!(out.contains("qwen3-coder:7b-q4_K_M (5.2 GB)"));
        assert!(out.contains("Available q5_K_M variants from registry:"));
        assert!(out.contains("qwen3-coder:7b-q5_K_M"));
        assert!(out.contains("qwen3-coder:14b-q5_K_M"));
        assert!(out.contains("(6.4 GB)"));
        assert!(out.contains("(10.1 GB)"));
        assert!(out.contains("Suggested pull (matching your size):"));
        assert!(out.contains("/ollama pull qwen3-coder:7b-q5_K_M"));
        assert!(out.contains("Size diff: +1.2 GB"));
        // No shell-prompt prefix on the suggested line.
        assert!(!out.contains("$ /ollama pull"));
    }

    #[test]
    fn format_requantize_no_variants_found() {
        let out = format_no_variants(
            "qwen3-coder",
            "q5_K_M",
            &["q4_0", "q4_K_M", "q4_K_S", "q8_0", "f16"]
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
        );
        assert!(out.contains("No q5_K_M variants of qwen3-coder available"));
        assert!(out.contains("Available quants: q4_0, q4_K_M, q4_K_S, q8_0, f16"));
        assert!(out.contains("Use a different target quant."));
    }

    #[test]
    fn format_requantize_already_target_quant() {
        let out = format_already_target_quant("qwen3-coder", "q5_K_M");
        assert!(out.contains("Already using q5_K_M for qwen3-coder. No action."));
    }

    #[test]
    fn format_requantize_lists_alternates_when_no_size_match() {
        let variants = variants_q5();
        let summary = RequantizeSummary {
            base: "qwen3-coder",
            current_full_name: "qwen3-coder:99b-q4_K_M", // unrealistic size
            current_quant: "Q4_K_M",
            target_quant: "q5_K_M",
            current_size_bytes: 50_000_000_000,
            target_variants: &variants,
            best_match: None,
        };
        let out = format_requantize_summary(&summary);
        assert!(out.contains("No size-matched variant"));
        // Falls back to surfacing the first listed variant.
        assert!(out.contains("/ollama pull qwen3-coder:7b-q5_K_M"));
        // Should NOT include the "Size diff" block when best_match is None.
        assert!(!out.contains("Size diff:"));
    }

    #[test]
    fn format_requantize_handles_unknown_size() {
        let variants = vec![tag("foo:7b-q5_K_M", None)];
        let summary = RequantizeSummary {
            base: "foo",
            current_full_name: "foo:7b-q4_K_M",
            current_quant: "Q4_K_M",
            target_quant: "q5_K_M",
            current_size_bytes: 0, // unknown
            target_variants: &variants,
            best_match: Some(&variants[0]),
        };
        let out = format_requantize_summary(&summary);
        // Both unknown sizes surface as "size unknown".
        assert!(out.contains("size unknown"));
        // Size diff is suppressed when either side is unknown.
        assert!(!out.contains("Size diff:"));
    }

    // ── strip_tag / find_local_install ──────────────────────────────────────

    #[test]
    fn strip_tag_strips_suffix() {
        assert_eq!(strip_tag("qwen3-coder:7b-q4_K_M"), "qwen3-coder");
        assert_eq!(strip_tag("qwen3-coder"), "qwen3-coder");
    }

    #[test]
    fn find_local_install_matches_base() {
        let installed = vec![
            ("qwen3:8b".to_string(), 5_000_000_000),
            ("qwen3-coder:7b-q4_K_M".to_string(), 4_200_000_000),
            ("llama3.2:3b".to_string(), 2_000_000_000),
        ];
        let hit = find_local_install(&installed, "qwen3-coder").expect("found");
        assert_eq!(hit.0, "qwen3-coder:7b-q4_K_M");
        assert_eq!(hit.1, 4_200_000_000);
    }

    #[test]
    fn find_local_install_returns_none_when_not_present() {
        let installed = vec![("qwen3:8b".to_string(), 5_000_000_000)];
        assert!(find_local_install(&installed, "qwen3-coder").is_none());
    }

    // ── format_size / format_size_diff ──────────────────────────────────────

    #[test]
    fn format_size_diff_positive_and_negative() {
        // q4 -> q5 (larger)
        assert_eq!(
            format_size_diff(5_200_000_000, 6_400_000_000),
            "+1.2 GB (5.2 GB -> 6.4 GB)"
        );
        // q5 -> q4 (smaller)
        assert_eq!(
            format_size_diff(6_400_000_000, 5_200_000_000),
            "-1.2 GB (6.4 GB -> 5.2 GB)"
        );
    }

    #[test]
    fn format_size_diff_unknown_when_either_zero() {
        assert_eq!(format_size_diff(0, 6_400_000_000), "size diff unknown");
        assert_eq!(format_size_diff(5_200_000_000, 0), "size diff unknown");
    }

    // ── format_not_installed ────────────────────────────────────────────────

    #[test]
    fn format_not_installed_mentions_pull_hint() {
        let out = format_not_installed("qwen3-coder");
        assert!(out.contains("not installed"));
        assert!(out.contains("/ollama pull qwen3-coder:latest"));
    }
}
