//! Auto-tune-on-load (Task #371).
//!
//! Every time Anvil sends an `/api/chat` request to Ollama, this module
//! resolves the full `OllamaOptions` to attach. The user's per-model
//! overrides in `settings.json` win over the tuner; explicit env-var
//! overrides (`ANVIL_OLLAMA_NUM_CTX` / `ANVIL_CONTEXT_SIZE`) win over both.
//!
//! The resolver is cached per `(model, modified_at, settings_mtime)`:
//!   * Cache miss → detect HW, fetch ModelMeta, load OllamaConfig, run
//!     `tune()`, apply overrides, store result.
//!   * Cache hit → return the stored options without I/O.
//!
//! On `TuneError::OomDetected`, we fall back to CPU-only (`num_gpu = 0`)
//! plus a conservative `num_ctx`, log to stderr, and proceed. Refusing the
//! request would mean the model just doesn't respond and the user has no
//! idea why; CPU fallback at least makes the model usable while preserving
//! the user's ability to tune manually.
//!
//! Cloud-tagged models (`*:cloud`) bypass the tuner entirely — the cloud
//! daemon ignores most of the options that matter (num_gpu, flash_attention,
//! kv_cache_type) and the cloud context window is already exposed via
//! `cloud_model_context_window`.
//!
//! Bench-history feedback: if a prior `/ollama bench <model>` produced very
//! low tok/s (under [`SLOW_BENCH_TOK_PER_SEC`]) on a balanced/quality
//! policy, we bias *this* resolution toward `Policy::Speed` so the next
//! request gets full GPU offload at a smaller context. The user can still
//! override with `/ollama option`. This is a one-shot bias — once the
//! refined config bench-runs at acceptable speed, the bias clears.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

use crate::ollama_tune::tuner::{
    tune, KvCacheType, OllamaOptions, Policy, TuneError, TuneResult, UserPolicy,
};
use crate::ollama_tune::OllamaConfig;
use crate::providers::ollama::{cloud_model_context_window, is_ollama_cloud_model};
use crate::providers::ollama_show::ModelMeta;

/// Below this measured tokens/sec on a *local* model, we bias toward
/// `Policy::Speed` next time. Cloud models are exempt.
pub const SLOW_BENCH_TOK_PER_SEC: f64 = 5.0;

/// How long a cached resolution stays fresh before we re-check
/// settings/meta mtimes. The mtimes are also checked on every lookup so
/// this is a defense against pathological clock skew rather than the
/// primary invalidation mechanism.
const CACHE_MAX_AGE_SECS: u64 = 600;

// ── Cache ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CacheEntry {
    options: OllamaOptions,
    cached_at: Instant,
    /// `modified_at` from the last `/api/show` we observed for this model.
    /// `None` for cloud models or when the daemon didn't expose one.
    model_modified_at: Option<String>,
    /// Settings.json mtime nanoseconds. We re-resolve when settings change.
    settings_mtime_ns: u64,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop the cached resolution for `model` (or all models when `model` is
/// `None`). Used by the L5b mutator handlers after a settings change so
/// the next request picks up the new override.
pub fn invalidate_cache(model: Option<&str>) {
    if let Ok(mut guard) = cache().lock() {
        match model {
            Some(m) => {
                guard.remove(m);
            }
            None => guard.clear(),
        }
    }
}

fn settings_mtime_ns() -> u64 {
    let dir = match std::env::var("ANVIL_HOME") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => match std::env::var("ANVIL_CONFIG_HOME") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => match dirs_next::home_dir() {
                Some(h) => h.join(".anvil"),
                None => return 0,
            },
        },
    };
    let path = dir.join("settings.json");
    std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ── Public entry points ──────────────────────────────────────────────────────

/// The async resolver. Detects hardware, fetches model meta, runs tune,
/// applies overrides. Falls back to a safe default on every failure.
pub async fn resolve_request_options(host: &str, model: &str) -> OllamaOptions {
    // Cloud models: bypass the tuner. The cloud daemon ignores num_gpu /
    // flash_attention / kv_cache_type / low_vram, so we only emit num_ctx.
    if is_ollama_cloud_model(model) {
        let num_ctx = cloud_model_context_window(model).unwrap_or(128_000);
        return cloud_minimal_options(num_ctx);
    }

    let now_settings_mtime = settings_mtime_ns();

    // Cache check.
    if let Ok(guard) = cache().lock() {
        if let Some(entry) = guard.get(model) {
            let age = entry.cached_at.elapsed().as_secs();
            if age < CACHE_MAX_AGE_SECS && entry.settings_mtime_ns == now_settings_mtime {
                return entry.options.clone();
            }
        }
    }

    // Slow path: detect hw, fetch meta, tune, apply overrides.
    let resolved = match resolve_uncached(host, model).await {
        Ok(opts) => opts,
        Err(reason) => {
            eprintln!("[anvil::ollama_tune] auto-tune fell back to CPU defaults: {reason}");
            cpu_fallback_options()
        }
    };

    // Cache for next request.
    if let Ok(mut guard) = cache().lock() {
        guard.insert(
            model.to_string(),
            CacheEntry {
                options: resolved.clone(),
                cached_at: Instant::now(),
                model_modified_at: None,
                settings_mtime_ns: now_settings_mtime,
            },
        );
    }

    resolved
}

/// Synchronous wrapper for callers that already hold a runtime handle but
/// can't `.await`. Spins up a temporary current-thread runtime if none
/// exists. The cache layer means this is only ever a hot path on first
/// request per model.
pub fn resolve_request_options_blocking(host: &str, model: &str) -> OllamaOptions {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // We're inside a runtime; spawn-block to await the future.
            let host = host.to_string();
            let model = model.to_string();
            std::thread::scope(|s| {
                let h = handle.clone();
                s.spawn(move || h.block_on(resolve_request_options(&host, &model)))
                    .join()
                    .unwrap_or_else(|_| cpu_fallback_options())
            })
        }
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current-thread runtime")
            .block_on(resolve_request_options(host, model)),
    }
}

// ── Slow path ────────────────────────────────────────────────────────────────

async fn resolve_uncached(host: &str, model: &str) -> Result<OllamaOptions, String> {
    use runtime::ollama_tune::hw::detect_cached;

    let hw = detect_cached();
    let meta = crate::providers::ollama_show::fetch_model_meta_cached(host, model)
        .await
        .map_err(|e| format!("/api/show failed for {model}: {e}"))?;

    let mut config = OllamaConfig::load();
    let policy = bench_biased_policy(model, &config.policy);
    config.policy = policy;

    let result: TuneResult = match tune(&hw, &meta, &config.policy) {
        Ok(r) => r,
        Err(TuneError::OomDetected {
            model_estimated_vram_bytes,
            available_vram_bytes,
            ..
        }) => {
            return Err(format!(
                "OOM at tuner for {model}: model needs ~{}GB, {}GB available; using CPU fallback",
                model_estimated_vram_bytes / 1_073_741_824,
                available_vram_bytes / 1_073_741_824,
            ));
        }
    };

    let with_overrides = config.apply_override(model, result.options);
    Ok(with_overrides)
}

/// Read the most recent bench result for this model and bias the policy
/// toward `Speed` if the prior run was unacceptably slow.
fn bench_biased_policy(model: &str, current: &UserPolicy) -> UserPolicy {
    if matches!(current.policy, Policy::Speed) {
        return current.clone();
    }
    let Some(bench) = read_latest_bench_tok_per_sec(model) else {
        return current.clone();
    };
    if bench < SLOW_BENCH_TOK_PER_SEC {
        eprintln!(
            "[anvil::ollama_tune] previous bench for {model} was {bench:.1} tok/s; biasing to Speed policy"
        );
        let mut biased = current.clone();
        biased.policy = Policy::Speed;
        return biased;
    }
    current.clone()
}

/// Read `<benchmarks_dir>/<slug>-<largest-ts>.json` and return its mean
/// tokens/sec. We can't depend on the anvil-cli crate from here, so the
/// benchmarks-dir layout is duplicated. The format is stable (both
/// directions are JSON-serde'd via the same `BenchResult` struct).
fn read_latest_bench_tok_per_sec(model: &str) -> Option<f64> {
    let dir = if let Ok(p) = std::env::var("ANVIL_HOME") {
        std::path::PathBuf::from(p).join("benchmarks")
    } else if let Ok(p) = std::env::var("ANVIL_CONFIG_HOME") {
        std::path::PathBuf::from(p).join("benchmarks")
    } else {
        dirs_next::home_dir()?.join(".anvil").join("benchmarks")
    };
    let slug = crate::ollama_tune::bench::model_slug(model);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut latest: Option<(i64, std::path::PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?.to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let stem = name.strip_suffix(".json")?;
        let rest = stem.strip_prefix(&slug)?;
        let ts_part = rest.strip_prefix('-')?;
        if !ts_part.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let ts: i64 = ts_part.parse().ok()?;
        match &latest {
            Some((best_ts, _)) if *best_ts >= ts => {}
            _ => latest = Some((ts, path)),
        }
    }
    let (_, path) = latest?;
    let bytes = std::fs::read(&path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("aggregate")?
        .get("mean_tokens_per_sec")?
        .as_f64()
}

// ── Defaults / fallbacks ─────────────────────────────────────────────────────

/// Conservative CPU-only fallback. Used when:
///   * The daemon is unreachable.
///   * The tuner returns OOM and we can't refine without user input.
///   * Hardware detection fails.
///
/// The user can always override via `/ollama option num_gpu -1` to push
/// layers back onto the GPU once they understand the constraint.
pub fn cpu_fallback_options() -> OllamaOptions {
    OllamaOptions {
        num_gpu: 0,
        num_ctx: 8192,
        num_thread: 4,
        flash_attention: false,
        kv_cache_type: KvCacheType::F16,
        low_vram: false,
        main_gpu: 0,
        keep_alive_secs: 300,
        mmap: !cfg!(target_os = "windows"),
        num_batch: 512,
    }
}

/// Cloud-only options: only `num_ctx` matters; daemon ignores the rest.
fn cloud_minimal_options(num_ctx: u32) -> OllamaOptions {
    OllamaOptions {
        num_gpu: -1,
        num_ctx,
        num_thread: 1,
        flash_attention: false,
        kv_cache_type: KvCacheType::F16,
        low_vram: false,
        main_gpu: 0,
        keep_alive_secs: 300,
        mmap: false,
        num_batch: 512,
    }
}

// ── Serialization to /api/chat options ───────────────────────────────────────

/// Convert `OllamaOptions` to the JSON shape Ollama's `/api/chat` accepts
/// in the `options` field. Field names match Ollama's documented schema.
#[must_use]
pub fn options_to_request_json(opts: &OllamaOptions) -> Value {
    let kv_cache_type = match opts.kv_cache_type {
        KvCacheType::F16 => "f16",
        KvCacheType::Q8_0 => "q8_0",
        KvCacheType::Q4_0 => "q4_0",
    };
    json!({
        "num_gpu": opts.num_gpu,
        "num_ctx": opts.num_ctx,
        "num_thread": opts.num_thread,
        "flash_attention": opts.flash_attention,
        "kv_cache_type": kv_cache_type,
        "low_vram": opts.low_vram,
        "main_gpu": opts.main_gpu,
        "mmap": opts.mmap,
        "num_batch": opts.num_batch,
    })
}

/// Apply explicit env-var overrides on top of an `OllamaOptions`. Used by
/// `openai_compat` after `resolve_request_options` so that
/// `ANVIL_OLLAMA_NUM_CTX` / `ANVIL_CONTEXT_SIZE` remain a hard last-resort
/// override (matches the prior behavior).
#[must_use]
pub fn apply_env_overrides(mut opts: OllamaOptions) -> OllamaOptions {
    for var in ["ANVIL_OLLAMA_NUM_CTX", "ANVIL_CONTEXT_SIZE"] {
        if let Ok(raw) = std::env::var(var) {
            if let Some(n) = parse_num_ctx_env(&raw) {
                opts.num_ctx = u32::try_from(n.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
                break;
            }
        }
    }
    opts
}

fn parse_num_ctx_env(raw: &str) -> Option<u64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let (digits, mult): (&str, u64) = if let Some(r) = lower.strip_suffix('k') {
        (r, 1_000)
    } else if let Some(r) = lower.strip_suffix('m') {
        (r, 1_000_000)
    } else {
        (lower.as_str(), 1)
    };
    digits.trim().parse::<u64>().ok().map(|n| n * mult)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama_tune::tuner::{KvCacheType, OllamaOptions};
    use serial_test::serial;

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

    #[test]
    fn cpu_fallback_safe_defaults() {
        let opts = cpu_fallback_options();
        assert_eq!(opts.num_gpu, 0);
        assert!(opts.num_ctx >= 4096);
        assert!(opts.num_thread >= 1);
        assert!(!opts.flash_attention);
    }

    #[test]
    fn cloud_minimal_uses_provided_num_ctx() {
        let opts = cloud_minimal_options(200_000);
        assert_eq!(opts.num_ctx, 200_000);
        // num_gpu = -1 is fine — cloud daemon ignores it.
        assert_eq!(opts.num_gpu, -1);
    }

    #[test]
    fn options_to_request_json_emits_all_fields() {
        let opts = fake_options();
        let v = options_to_request_json(&opts);
        // Every field the tuner produces must end up in the request body.
        assert_eq!(v["num_gpu"], -1);
        assert_eq!(v["num_ctx"], 32768);
        assert_eq!(v["num_thread"], 11);
        assert_eq!(v["flash_attention"], true);
        assert_eq!(v["kv_cache_type"], "f16");
        assert_eq!(v["low_vram"], false);
        assert_eq!(v["main_gpu"], 0);
        assert_eq!(v["mmap"], true);
        assert_eq!(v["num_batch"], 1024);
    }

    #[test]
    fn options_to_request_json_kv_cache_type_lowercase() {
        let mut opts = fake_options();
        opts.kv_cache_type = KvCacheType::Q8_0;
        assert_eq!(options_to_request_json(&opts)["kv_cache_type"], "q8_0");
        opts.kv_cache_type = KvCacheType::Q4_0;
        assert_eq!(options_to_request_json(&opts)["kv_cache_type"], "q4_0");
    }

    #[test]
    fn apply_env_overrides_num_ctx_plain() {
        // No env set → no change.
        let opts = fake_options();
        let _ = std::env::var("ANVIL_OLLAMA_NUM_CTX"); // touch
        // Don't actually set env in this test — handled in serial tests below.
        let result = apply_env_overrides(opts.clone());
        // We're not setting the env so result should equal input. (Guarded by
        // serial_test in the env-mutation cases below; this case just
        // confirms the no-op path.)
        if std::env::var("ANVIL_OLLAMA_NUM_CTX").is_err()
            && std::env::var("ANVIL_CONTEXT_SIZE").is_err()
        {
            assert_eq!(result.num_ctx, opts.num_ctx);
        }
    }

    #[test]
    fn parse_num_ctx_env_accepts_plain() {
        assert_eq!(parse_num_ctx_env("65536"), Some(65_536));
    }

    #[test]
    fn parse_num_ctx_env_accepts_k_suffix() {
        assert_eq!(parse_num_ctx_env("128K"), Some(128_000));
        assert_eq!(parse_num_ctx_env("128k"), Some(128_000));
    }

    #[test]
    fn parse_num_ctx_env_accepts_m_suffix() {
        assert_eq!(parse_num_ctx_env("1M"), Some(1_000_000));
    }

    #[test]
    fn parse_num_ctx_env_rejects_garbage() {
        assert_eq!(parse_num_ctx_env(""), None);
        assert_eq!(parse_num_ctx_env("abc"), None);
        assert_eq!(parse_num_ctx_env("12x"), None);
    }

    #[test]
    #[serial(ollama_tune_cache)]
    fn invalidate_cache_specific_model() {
        // Seed the cache directly so we don't need a daemon.
        if let Ok(mut g) = cache().lock() {
            g.insert(
                "qwen3:8b".to_string(),
                CacheEntry {
                    options: fake_options(),
                    cached_at: Instant::now(),
                    model_modified_at: None,
                    settings_mtime_ns: 0,
                },
            );
            g.insert(
                "llama3.2:3b".to_string(),
                CacheEntry {
                    options: fake_options(),
                    cached_at: Instant::now(),
                    model_modified_at: None,
                    settings_mtime_ns: 0,
                },
            );
        }
        invalidate_cache(Some("qwen3:8b"));
        let g = cache().lock().unwrap();
        assert!(!g.contains_key("qwen3:8b"));
        assert!(g.contains_key("llama3.2:3b"));
        // Clean up so other tests don't see stale entries.
        drop(g);
        invalidate_cache(None);
    }

    #[test]
    #[serial(ollama_tune_cache)]
    fn invalidate_cache_all_models() {
        if let Ok(mut g) = cache().lock() {
            g.insert(
                "a".to_string(),
                CacheEntry {
                    options: fake_options(),
                    cached_at: Instant::now(),
                    model_modified_at: None,
                    settings_mtime_ns: 0,
                },
            );
            g.insert(
                "b".to_string(),
                CacheEntry {
                    options: fake_options(),
                    cached_at: Instant::now(),
                    model_modified_at: None,
                    settings_mtime_ns: 0,
                },
            );
        }
        invalidate_cache(None);
        let g = cache().lock().unwrap();
        assert!(g.is_empty());
    }

    // Cloud bypass uses the cloud_model_context_window registry directly.
    // We assert the resolver path by calling resolve_request_options on a
    // cloud model; it must not require a network round-trip because cloud
    // models bypass the tuner.
    #[test]
    fn cloud_resolution_does_not_need_daemon() {
        // resolve_request_options is async; we use the blocking wrapper
        // which spins up a current-thread runtime. The cloud branch
        // returns synchronously without any await/IO.
        let opts = resolve_request_options_blocking("http://does-not-resolve:1", "minimax-m2.5:cloud");
        assert_eq!(opts.num_ctx, 256_000);
    }

    #[test]
    fn cloud_unknown_falls_back_to_128k() {
        let opts =
            resolve_request_options_blocking("http://does-not-resolve:1", "some-new-model:cloud");
        assert_eq!(opts.num_ctx, 128_000);
    }

    #[test]
    #[allow(unsafe_code)] // env mutation is unsafe in Edition 2024
    fn read_latest_bench_returns_none_when_no_benchmarks_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var("ANVIL_HOME").ok();
        // SAFETY: tests below that mutate env are isolated by serial_test in
        // adjacent test binaries; this single read-only test creates the
        // tempdir then never writes a benchmarks/ subdir.
        unsafe {
            std::env::set_var("ANVIL_HOME", tmp.path());
        }
        let result = read_latest_bench_tok_per_sec("qwen3:8b");
        match prev_home {
            Some(v) => unsafe { std::env::set_var("ANVIL_HOME", v) },
            None => unsafe { std::env::remove_var("ANVIL_HOME") },
        }
        assert!(result.is_none());
    }
}
