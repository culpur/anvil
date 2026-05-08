//! Benchmark harness for measuring Ollama model performance under the
//! current `OllamaOptions`.
//!
//! Runs a fixed three-prompt suite against `/api/chat` (Ollama-native
//! NDJSON streaming, NOT the OpenAI-compat path) so we can read the
//! `eval_count`, `eval_duration`, and `prompt_eval_*` fields the daemon
//! emits on the final chunk. Wall-clock TTFT and total time are captured
//! independently as a sanity check on the daemon's self-reported numbers.
//!
//! Persistence is L7's responsibility on the CLI side — this module only
//! produces a `BenchResult` and pure formatters; the local-JSON store and
//! optional QMD ingest live in `crates/anvil-cli/src/ollama_bench.rs`.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ollama_tune::tuner::OllamaOptions;

// ── Public types ─────────────────────────────────────────────────────────────

/// Per-prompt measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    pub prompt_label: String,
    pub time_to_first_token_ms: u64,
    pub total_time_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub tokens_per_sec: f64,
}

/// Brief hardware snapshot — enough to identify which machine produced the
/// numbers without dragging the full `HardwareProfile` into every record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSummary {
    pub os: String,
    pub gpu_kind: String,
    pub gpu_name: Option<String>,
    pub vram_total_gb: u64,
    pub ram_total_gb: u64,
}

/// Aggregate stats across all prompts. Computed by `aggregate_from_prompts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aggregate {
    pub mean_tokens_per_sec: f64,
    pub median_ttft_ms: u64,
    pub max_completion_tokens: u32,
}

/// Top-level bench record. Serializes to JSON for local persistence and
/// to a markdown doc for QMD ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub model: String,
    pub timestamp: i64,
    pub anvil_version: String,
    pub host_summary: HostSummary,
    pub options: OllamaOptions,
    pub prompts: Vec<PromptResult>,
    pub aggregate: Aggregate,
}

#[derive(Debug, Clone)]
pub enum BenchError {
    DaemonUnreachable(String),
    ModelNotInstalled(String),
    StreamFailed(String),
}

impl std::fmt::Display for BenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DaemonUnreachable(s) => write!(f, "Ollama daemon unreachable: {s}"),
            Self::ModelNotInstalled(m) => write!(f, "Model not installed: {m}"),
            Self::StreamFailed(s) => write!(f, "Stream failed: {s}"),
        }
    }
}

impl std::error::Error for BenchError {}

// ── Prompt suite ─────────────────────────────────────────────────────────────

/// Three prompts that exercise different facets of model performance:
/// short-latency-bound, sustained-throughput, and long-input.
pub const PROMPT_SUITE: &[(&str, &str)] = &[
    (
        "short_qa",
        "What is 17 * 23? Reply with just the number.",
    ),
    (
        "code_gen",
        "Write a Rust function that returns the nth Fibonacci number using memoization. \
         Include doc comments and one unit test. Just the code, no prose.",
    ),
    (
        "summarization",
        "Summarize the following in 3 bullet points:\n\n\
         The science of distributed systems studies how multiple computers cooperate to \
         solve problems no single machine could handle alone. Early research focused on \
         consensus algorithms — protocols by which a group of nodes agree on a value \
         despite some of them being unreliable. The Paxos algorithm, published in 1989, \
         remains the canonical example, though its complexity has prompted simpler \
         alternatives like Raft. Modern distributed databases combine consensus with \
         replication strategies that trade off consistency, availability, and partition \
         tolerance — the famous CAP theorem proven by Eric Brewer and later formalized \
         by Seth Gilbert and Nancy Lynch. In practice, most production systems sit \
         somewhere on a continuum: strong consistency for financial ledgers, eventual \
         consistency for social media feeds. The choice is rarely all-or-nothing. \
         Recent work on causal consistency, snapshot isolation, and CRDTs (conflict-free \
         replicated data types) has expanded the design space considerably, allowing \
         systems to offer useful guarantees without the full cost of strong consistency. \
         Modern engineering teams spend enormous effort tuning these tradeoffs to match \
         business requirements: a payment system might use Paxos-backed replication \
         while a chat app uses CRDTs, and both can run on the same underlying cloud \
         infrastructure with different tradeoffs encoded in their data layer.",
    ),
];

// ── Pure helpers (testable without network) ──────────────────────────────────

/// Compute aggregate stats from per-prompt results. Pure.
#[must_use]
pub fn aggregate_from_prompts(prompts: &[PromptResult]) -> Aggregate {
    if prompts.is_empty() {
        return Aggregate {
            mean_tokens_per_sec: 0.0,
            median_ttft_ms: 0,
            max_completion_tokens: 0,
        };
    }
    let mean = prompts.iter().map(|p| p.tokens_per_sec).sum::<f64>() / prompts.len() as f64;
    let mut ttfts: Vec<u64> = prompts.iter().map(|p| p.time_to_first_token_ms).collect();
    ttfts.sort_unstable();
    let median = ttfts[ttfts.len() / 2];
    let max_tokens = prompts.iter().map(|p| p.completion_tokens).max().unwrap_or(0);
    Aggregate {
        mean_tokens_per_sec: mean,
        median_ttft_ms: median,
        max_completion_tokens: max_tokens,
    }
}

/// Compute tokens-per-second from Ollama's reported `eval_count` and
/// `eval_duration` (ns). Returns 0.0 on zero duration.
#[must_use]
pub fn tok_per_sec(eval_count: u32, eval_duration_ns: u64) -> f64 {
    if eval_duration_ns == 0 {
        return 0.0;
    }
    let secs = eval_duration_ns as f64 / 1_000_000_000.0;
    f64::from(eval_count) / secs
}

/// Produce a filesystem-safe slug from a model name.
#[must_use]
pub fn model_slug(model: &str) -> String {
    model
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | ' ' => '-',
            c => c,
        })
        .collect()
}

/// Format a `BenchResult` as human-readable text for `/ollama bench` output.
#[must_use]
pub fn format_bench_summary(result: &BenchResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("Bench: {} @ {}\n", result.model, result.timestamp));
    out.push_str(&format!(
        "  Hardware: {} {} ({} GB unified/VRAM)\n",
        result.host_summary.os,
        result.host_summary.gpu_kind,
        result.host_summary.vram_total_gb,
    ));
    if let Some(name) = &result.host_summary.gpu_name {
        out.push_str(&format!("  GPU: {name}\n"));
    }
    out.push_str(&format!(
        "  Options: num_ctx={}, num_gpu={}, flash_attention={}, kv_cache={:?}\n",
        result.options.num_ctx,
        result.options.num_gpu,
        result.options.flash_attention,
        result.options.kv_cache_type,
    ));
    out.push_str(&format!(
        "\nThroughput: mean {:.1} tok/s, median TTFT {} ms\n\n",
        result.aggregate.mean_tokens_per_sec, result.aggregate.median_ttft_ms,
    ));

    out.push_str("Per-prompt:\n");
    out.push_str("  label          tok/s    TTFT ms   total ms   tokens\n");
    out.push_str("  ─────          ─────    ───────   ────────   ──────\n");
    for p in &result.prompts {
        out.push_str(&format!(
            "  {:<14} {:>5.1}    {:>7}   {:>8}   {:>6}\n",
            p.prompt_label,
            p.tokens_per_sec,
            p.time_to_first_token_ms,
            p.total_time_ms,
            p.completion_tokens,
        ));
    }
    out
}

/// Format a `BenchResult` as a markdown doc with frontmatter for QMD ingest.
#[must_use]
pub fn format_bench_qmd_doc(result: &BenchResult) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("model: {}\n", result.model));
    out.push_str(&format!("timestamp: {}\n", result.timestamp));
    out.push_str(&format!("gpu_kind: {}\n", result.host_summary.gpu_kind));
    out.push_str(&format!(
        "mean_tokens_per_sec: {:.2}\n",
        result.aggregate.mean_tokens_per_sec,
    ));
    out.push_str(&format!("anvil_version: {}\n", result.anvil_version));
    out.push_str("---\n\n");

    out.push_str(&format!("# Bench: {} @ unix {}\n\n", result.model, result.timestamp));
    out.push_str(&format!(
        "- **Hardware**: {} {} {} GB\n",
        result.host_summary.os, result.host_summary.gpu_kind, result.host_summary.vram_total_gb,
    ));
    out.push_str(&format!(
        "- **Options**: num_ctx={}, num_gpu={}, flash_attention={}, kv_cache_type={:?}\n",
        result.options.num_ctx,
        result.options.num_gpu,
        result.options.flash_attention,
        result.options.kv_cache_type,
    ));
    out.push_str(&format!(
        "- **Throughput**: mean {:.1} tok/s, median TTFT {} ms\n\n",
        result.aggregate.mean_tokens_per_sec, result.aggregate.median_ttft_ms,
    ));

    out.push_str("## Per-prompt\n\n");
    out.push_str("| label | tok/s | TTFT ms | total ms | tokens |\n");
    out.push_str("|-------|-------|---------|----------|--------|\n");
    for p in &result.prompts {
        out.push_str(&format!(
            "| {} | {:.1} | {} | {} | {} |\n",
            p.prompt_label,
            p.tokens_per_sec,
            p.time_to_first_token_ms,
            p.total_time_ms,
            p.completion_tokens,
        ));
    }
    out
}

// ── Stream-chunk parsing (testable; runs against fixture lines) ──────────────

/// One parsed final-chunk's worth of timing/count fields. The daemon emits
/// these only on the chunk where `done == true`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FinalCounters {
    pub eval_count: u32,
    pub eval_duration_ns: u64,
    pub prompt_eval_count: u32,
}

/// Parse one NDJSON line from `/api/chat`. Returns `(content_delta, Option<final_counters>)`.
/// Returns `(None, None)` on garbage.
#[must_use]
pub fn parse_chat_chunk(line: &str) -> (Option<String>, Option<FinalCounters>) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };

    let delta = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(String::from);

    let done = v.get("done").and_then(Value::as_bool).unwrap_or(false);
    if !done {
        return (delta, None);
    }

    let final_ = FinalCounters {
        eval_count: v
            .get("eval_count")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(0),
        eval_duration_ns: v.get("eval_duration").and_then(Value::as_u64).unwrap_or(0),
        prompt_eval_count: v
            .get("prompt_eval_count")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(0),
    };
    (delta, Some(final_))
}

// ── Async runner ─────────────────────────────────────────────────────────────

fn resolve_bench_host(host: &str) -> String {
    let trimmed = host.trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string())
    } else {
        trimmed
    }
}

/// Run one prompt against the daemon and capture its `PromptResult`.
async fn run_one_prompt(
    host: &str,
    model: &str,
    label: &str,
    prompt: &str,
    options: &OllamaOptions,
) -> Result<PromptResult, BenchError> {
    use futures_util::StreamExt;

    let url = format!("{host}/api/chat");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| BenchError::DaemonUnreachable(e.to_string()))?;

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": true,
        "options": {
            "num_ctx": options.num_ctx,
            "num_gpu": options.num_gpu,
            "num_thread": options.num_thread,
            "flash_attention": options.flash_attention,
            "num_batch": options.num_batch,
        },
        "keep_alive": options.keep_alive_secs,
    });

    let started = Instant::now();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| BenchError::DaemonUnreachable(e.to_string()))?;

    if response.status().as_u16() == 404 {
        return Err(BenchError::ModelNotInstalled(model.to_string()));
    }
    if !response.status().is_success() {
        return Err(BenchError::StreamFailed(format!(
            "HTTP {}",
            response.status()
        )));
    }

    let mut stream = response.bytes_stream();
    let mut buf = String::new();
    let mut ttft_ms: Option<u64> = None;
    let mut final_counters: Option<FinalCounters> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| BenchError::StreamFailed(e.to_string()))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].to_string();
            buf.drain(..=nl);
            if line.trim().is_empty() {
                continue;
            }
            let (delta, final_) = parse_chat_chunk(&line);
            if delta.is_some() && ttft_ms.is_none() {
                ttft_ms = Some(started.elapsed().as_millis() as u64);
            }
            if let Some(f) = final_ {
                final_counters = Some(f);
            }
        }
    }

    let total_ms = started.elapsed().as_millis() as u64;
    let final_counters = final_counters.ok_or_else(|| {
        BenchError::StreamFailed("stream ended without a final 'done' chunk".to_string())
    })?;

    Ok(PromptResult {
        prompt_label: label.to_string(),
        time_to_first_token_ms: ttft_ms.unwrap_or(total_ms),
        total_time_ms: total_ms,
        prompt_tokens: final_counters.prompt_eval_count,
        completion_tokens: final_counters.eval_count,
        tokens_per_sec: tok_per_sec(final_counters.eval_count, final_counters.eval_duration_ns),
    })
}

/// Progress callback invoked as the bench runs. Each call carries a
/// pair `(prompt_index, total_prompts)` along with a status string.
/// Callers pass a no-op closure when they don't care about progress.
pub type BenchProgressCb<'a> = &'a (dyn Fn(usize, usize, &str) + Send + Sync);

/// No-op progress callback. Use when you don't want progress updates.
pub fn noop_progress(_idx: usize, _total: usize, _msg: &str) {}

/// Run the full prompt suite against the daemon with a progress
/// callback. The callback is invoked once before each prompt starts
/// (status: "prompt-starting") and once after each prompt completes
/// (status: "prompt-complete" with the measured tok/s appended).
pub async fn run_bench_with_progress(
    host: &str,
    model: &str,
    options: &OllamaOptions,
    host_summary: HostSummary,
    anvil_version: String,
    progress: BenchProgressCb<'_>,
) -> Result<BenchResult, BenchError> {
    let host = resolve_bench_host(host);
    let total = PROMPT_SUITE.len();
    let mut prompts = Vec::with_capacity(total);
    for (idx, (label, prompt)) in PROMPT_SUITE.iter().enumerate() {
        progress(idx + 1, total, &format!("starting {label}"));
        let r = run_one_prompt(&host, model, label, prompt, options).await?;
        progress(
            idx + 1,
            total,
            &format!(
                "{label} done — {:.1} tok/s, {} tokens, {} ms",
                r.tokens_per_sec, r.completion_tokens, r.total_time_ms,
            ),
        );
        prompts.push(r);
    }
    let aggregate = aggregate_from_prompts(&prompts);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Ok(BenchResult {
        model: model.to_string(),
        timestamp,
        anvil_version,
        host_summary,
        options: options.clone(),
        prompts,
        aggregate,
    })
}

/// Run the full prompt suite without progress reporting. Thin wrapper
/// over [`run_bench_with_progress`] for callers that don't need updates.
pub async fn run_bench(
    host: &str,
    model: &str,
    options: &OllamaOptions,
    host_summary: HostSummary,
    anvil_version: String,
) -> Result<BenchResult, BenchError> {
    run_bench_with_progress(host, model, options, host_summary, anvil_version, &noop_progress).await
}

#[allow(dead_code)]
async fn run_bench_legacy_inline(
    host: &str,
    model: &str,
    options: &OllamaOptions,
    host_summary: HostSummary,
    anvil_version: String,
) -> Result<BenchResult, BenchError> {
    let host = resolve_bench_host(host);
    let mut prompts = Vec::with_capacity(PROMPT_SUITE.len());
    for (label, prompt) in PROMPT_SUITE {
        let r = run_one_prompt(&host, model, label, prompt, options).await?;
        prompts.push(r);
    }
    let aggregate = aggregate_from_prompts(&prompts);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Ok(BenchResult {
        model: model.to_string(),
        timestamp,
        anvil_version,
        host_summary,
        options: options.clone(),
        prompts,
        aggregate,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama_tune::tuner::{KvCacheType, OllamaOptions};

    fn fake_options() -> OllamaOptions {
        OllamaOptions {
            num_gpu: -1,
            num_ctx: 32768,
            num_thread: 11,
            flash_attention: true,
            kv_cache_type: KvCacheType::F16,
            low_vram: false,
            main_gpu: 0,
            keep_alive_secs: 300,
            mmap: true,
            num_batch: 1024,
        }
    }

    fn fake_host() -> HostSummary {
        HostSummary {
            os: "macos".to_string(),
            gpu_kind: "metal".to_string(),
            gpu_name: Some("Apple M2 Pro".to_string()),
            vram_total_gb: 32,
            ram_total_gb: 32,
        }
    }

    fn fake_prompts() -> Vec<PromptResult> {
        vec![
            PromptResult {
                prompt_label: "short_qa".into(),
                time_to_first_token_ms: 89,
                total_time_ms: 230,
                prompt_tokens: 18,
                completion_tokens: 12,
                tokens_per_sec: 52.1,
            },
            PromptResult {
                prompt_label: "code_gen".into(),
                time_to_first_token_ms: 142,
                total_time_ms: 4180,
                prompt_tokens: 32,
                completion_tokens: 202,
                tokens_per_sec: 48.3,
            },
            PromptResult {
                prompt_label: "summarization".into(),
                time_to_first_token_ms: 195,
                total_time_ms: 6700,
                prompt_tokens: 280,
                completion_tokens: 276,
                tokens_per_sec: 41.2,
            },
        ]
    }

    fn fake_result() -> BenchResult {
        let prompts = fake_prompts();
        let aggregate = aggregate_from_prompts(&prompts);
        BenchResult {
            model: "qwen3:8b".to_string(),
            timestamp: 1_715_200_000,
            anvil_version: "2.2.10".to_string(),
            host_summary: fake_host(),
            options: fake_options(),
            prompts,
            aggregate,
        }
    }

    #[test]
    fn aggregate_computes_mean_and_median() {
        let agg = aggregate_from_prompts(&fake_prompts());
        // mean of 52.1, 48.3, 41.2 = 47.2
        assert!((agg.mean_tokens_per_sec - 47.2).abs() < 0.05);
        // median TTFT of [89, 142, 195] = 142
        assert_eq!(agg.median_ttft_ms, 142);
        assert_eq!(agg.max_completion_tokens, 276);
    }

    #[test]
    fn aggregate_handles_empty_prompts() {
        let agg = aggregate_from_prompts(&[]);
        assert_eq!(agg.mean_tokens_per_sec, 0.0);
        assert_eq!(agg.median_ttft_ms, 0);
        assert_eq!(agg.max_completion_tokens, 0);
    }

    #[test]
    fn tok_per_sec_basic() {
        // 200 tokens in 5_000_000_000 ns = 5 sec → 40 tok/s
        assert!((tok_per_sec(200, 5_000_000_000) - 40.0).abs() < 0.001);
    }

    #[test]
    fn tok_per_sec_zero_duration_returns_zero() {
        assert_eq!(tok_per_sec(100, 0), 0.0);
    }

    #[test]
    fn model_slug_replaces_unsafe_chars() {
        assert_eq!(model_slug("qwen3-coder:480b-cloud"), "qwen3-coder-480b-cloud");
        assert_eq!(model_slug("models/foo:latest"), "models-foo-latest");
        assert_eq!(model_slug("plain"), "plain");
    }

    #[test]
    fn parse_chat_chunk_with_delta_only() {
        let (delta, final_) = parse_chat_chunk(r#"{"message":{"content":"Hello"}}"#);
        assert_eq!(delta.as_deref(), Some("Hello"));
        assert!(final_.is_none());
    }

    #[test]
    fn parse_chat_chunk_with_done_and_counters() {
        let line = r#"{"message":{"content":""},"done":true,"eval_count":200,"eval_duration":5000000000,"prompt_eval_count":18}"#;
        let (_delta, final_) = parse_chat_chunk(line);
        let f = final_.expect("final counters");
        assert_eq!(f.eval_count, 200);
        assert_eq!(f.eval_duration_ns, 5_000_000_000);
        assert_eq!(f.prompt_eval_count, 18);
    }

    #[test]
    fn parse_chat_chunk_garbage_returns_none() {
        let (delta, final_) = parse_chat_chunk("not json");
        assert!(delta.is_none());
        assert!(final_.is_none());
    }

    #[test]
    fn parse_chat_chunk_done_with_missing_counter_fields_uses_zero() {
        let (_, final_) = parse_chat_chunk(r#"{"done":true}"#);
        let f = final_.unwrap();
        assert_eq!(f.eval_count, 0);
        assert_eq!(f.eval_duration_ns, 0);
    }

    #[test]
    fn format_bench_summary_includes_per_prompt_table() {
        let s = format_bench_summary(&fake_result());
        assert!(s.contains("short_qa"));
        assert!(s.contains("code_gen"));
        assert!(s.contains("summarization"));
        assert!(s.contains("Throughput"));
        assert!(s.contains("47.2") || s.contains("47.1"));
    }

    #[test]
    fn format_bench_qmd_doc_has_frontmatter_and_table() {
        let s = format_bench_qmd_doc(&fake_result());
        assert!(s.starts_with("---\n"));
        assert!(s.contains("model: qwen3:8b"));
        assert!(s.contains("gpu_kind: metal"));
        assert!(s.contains("| short_qa |"));
        assert!(s.contains("| label | tok/s | TTFT ms | total ms | tokens |"));
    }

    #[test]
    fn bench_result_serde_round_trip() {
        let r = fake_result();
        let json = serde_json::to_string(&r).expect("serialize");
        let r2: BenchResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r.model, r2.model);
        assert_eq!(r.prompts.len(), r2.prompts.len());
        assert!((r.aggregate.mean_tokens_per_sec - r2.aggregate.mean_tokens_per_sec).abs() < 0.001);
    }

    #[test]
    fn prompt_suite_has_three_entries() {
        assert_eq!(PROMPT_SUITE.len(), 3);
        let labels: Vec<_> = PROMPT_SUITE.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, vec!["short_qa", "code_gen", "summarization"]);
    }

    #[test]
    fn resolve_bench_host_strips_trailing_slash() {
        assert_eq!(
            resolve_bench_host("http://localhost:11434/"),
            "http://localhost:11434"
        );
    }
}
