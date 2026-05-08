//! `/api/show` introspection for the Ollama daemon.
//!
//! The Anvil tuner needs rich, per-model metadata (architecture, quantization,
//! context window, layer/head counts, on-disk size) to make routing and
//! `num_ctx`/`num_gpu` decisions.  The Ollama `/api/show` endpoint exposes this
//! information for any locally-installed model — and, with a bit of fallback
//! logic, for cloud-tagged models too.
//!
//! This module:
//!
//! 1. Defines `ModelMeta`, `Architecture`, and `Quantization` value types
//!    that the tuner can reason over.
//! 2. Provides `parse_show_response` — a pure function over JSON that the
//!    tests exercise extensively.
//! 3. Provides `fetch_model_meta` — the network-touching wrapper.  Honors the
//!    `ollama-cloud-auth` rule: ALWAYS talks to `localhost:11434` (or
//!    `OLLAMA_HOST`), never to `ollama.com` directly.
//! 4. Provides `fetch_model_meta_cached` — a process-wide cache keyed on
//!    `(host, model, modified_at)` so a refetch is only needed when the
//!    upstream model file actually changes.
//!
//! No new crate dependencies are introduced — all parsing is done with
//! `serde_json::Value` and the error type is a hand-rolled enum implementing
//! `std::error::Error` (the workspace does not pull in `thiserror`).

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ollama::{DEFAULT_OLLAMA_BASE_URL, OLLAMA_HOST_ENV};

// ─── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Architecture {
    Llama,
    Qwen2,
    Qwen3,
    Mistral,
    Mixtral,
    Gemma2,
    Gemma3,
    DeepseekV2,
    DeepseekV3,
    Phi3,
    CommandR,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum Quantization {
    Q4_0,
    Q4_1,
    Q4_K_M,
    Q4_K_S,
    Q5_0,
    Q5_1,
    Q5_K_M,
    Q5_K_S,
    Q6_K,
    Q8_0,
    F16,
    BF16,
    F32,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    pub name: String,
    pub modified_at: Option<String>,
    /// Total model file size on disk, in bytes.  May be 0 when neither
    /// `/api/show.size` nor `/api/tags` exposes the value (e.g. some cloud
    /// manifests).
    pub size_bytes: u64,
    /// "7B", "70B", "480B", etc. — verbatim from `details.parameter_size`.
    pub parameter_size: String,
    /// Parsed total parameter count, e.g. `"7B" -> 7_000_000_000`.
    pub parameter_count: u64,
    pub quantization: Quantization,
    /// Effective max context length in tokens.  Source priority:
    /// 1. `model_info["<arch>.context_length"]` (GGUF metadata)
    /// 2. `parameters` string `num_ctx <n>` directive
    /// 3. 4096 (universal floor)
    pub context_length: u32,
    pub architecture: Architecture,
    pub layer_count: Option<u32>,
    pub head_count: Option<u32>,
    pub head_count_kv: Option<u32>,
    pub embedding_length: Option<u32>,
    /// `details.families` — typically a single-element list, e.g. `["llama"]`.
    pub families: Vec<String>,
    /// `details.format` — almost always `"gguf"`.
    pub format: Option<String>,
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ModelMetaError {
    DaemonUnreachable(String),
    ModelNotInstalled(String),
    Parse(String),
    Http { status: u16 },
}

impl Display for ModelMetaError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonUnreachable(msg) => write!(f, "Ollama daemon unreachable: {msg}"),
            Self::ModelNotInstalled(model) => write!(f, "Model not installed: {model}"),
            Self::Parse(msg) => write!(f, "Malformed /api/show response: {msg}"),
            Self::Http { status } => write!(f, "HTTP error: {status}"),
        }
    }
}

impl Error for ModelMetaError {}

// ─── Parsing helpers (pure) ──────────────────────────────────────────────────

/// Resolve an architecture-prefixed GGUF integer key, e.g.
/// `gguf_int(model_info, "llama", "context_length")` reads
/// `model_info["llama.context_length"]`.
///
/// GGUF metadata values can arrive as either JSON numbers or numeric strings,
/// so both are accepted.  Returns `None` on missing/unparseable keys.
fn gguf_int(model_info: &Value, arch: &str, suffix: &str) -> Option<u32> {
    let key = format!("{arch}.{suffix}");
    let raw = model_info.get(&key)?;
    match raw {
        Value::Number(n) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
        Value::String(s) => s.parse::<u32>().ok(),
        _ => None,
    }
}

/// Map the `general.architecture` GGUF tag to our `Architecture` enum.
fn architecture_from_str(arch: &str) -> Architecture {
    match arch {
        "llama" => Architecture::Llama,
        "qwen2" => Architecture::Qwen2,
        "qwen3" => Architecture::Qwen3,
        "mistral" => Architecture::Mistral,
        "mixtral" => Architecture::Mixtral,
        "gemma2" => Architecture::Gemma2,
        "gemma3" => Architecture::Gemma3,
        "deepseek2" => Architecture::DeepseekV2,
        "deepseek3" => Architecture::DeepseekV3,
        "phi3" => Architecture::Phi3,
        "command-r" => Architecture::CommandR,
        other => Architecture::Other(other.to_string()),
    }
}

/// Map a `details.quantization_level` string (e.g. `"Q4_K_M"`, `"F16"`) to
/// our `Quantization` enum.  Unrecognised values fall through to
/// `Unknown(string)` rather than failing — Ollama occasionally emits novel
/// quant labels for newly-supported model families.
fn quantization_from_str(q: &str) -> Quantization {
    match q {
        "Q4_0" => Quantization::Q4_0,
        "Q4_1" => Quantization::Q4_1,
        "Q4_K_M" => Quantization::Q4_K_M,
        "Q4_K_S" => Quantization::Q4_K_S,
        "Q5_0" => Quantization::Q5_0,
        "Q5_1" => Quantization::Q5_1,
        "Q5_K_M" => Quantization::Q5_K_M,
        "Q5_K_S" => Quantization::Q5_K_S,
        "Q6_K" => Quantization::Q6_K,
        "Q8_0" => Quantization::Q8_0,
        "F16" => Quantization::F16,
        "BF16" => Quantization::BF16,
        "F32" => Quantization::F32,
        other => Quantization::Unknown(other.to_string()),
    }
}

/// Parse a parameter-size string like `"7B"`, `"1.5B"`, `"350M"`, `"1T"` into
/// its numeric expansion.  Whitespace and the suffix character are
/// case-insensitive (`"7b"` and `"7B"` both yield `7_000_000_000`).
///
/// Returns `0` when the string is empty or unparseable, mirroring how Ollama
/// itself reports unknown sizes for some cloud manifests.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn parse_parameter_count(raw: &str) -> u64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let (number_part, multiplier) = match trimmed.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('B') => (&trimmed[..trimmed.len() - 1], 1_000_000_000_u64),
        Some('M') => (&trimmed[..trimmed.len() - 1], 1_000_000_u64),
        Some('T') => (&trimmed[..trimmed.len() - 1], 1_000_000_000_000_u64),
        Some('K') => (&trimmed[..trimmed.len() - 1], 1_000_u64),
        _ => (trimmed, 1_u64),
    };
    let Ok(value) = number_part.trim().parse::<f64>() else {
        return 0;
    };
    // Avoid f64 -> u64 truncation surprises by rounding to nearest.
    let scaled = value * (multiplier as f64);
    if scaled.is_finite() && scaled >= 0.0 {
        scaled.round() as u64
    } else {
        0
    }
}

/// Pull `num_ctx <number>` out of the `parameters` blob that `/api/show`
/// returns.  The blob is a multi-line text file in Modelfile syntax — values
/// may be quoted or bare.  Returns `None` if the directive is absent or the
/// value can't be parsed as `u32`.
fn extract_num_ctx_from_parameters(parameters: &str) -> Option<u32> {
    for line in parameters.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("num_ctx") else {
            continue;
        };
        let value = rest.trim().trim_matches('"').trim();
        if let Ok(n) = value.parse::<u32>() {
            return Some(n);
        }
    }
    None
}

/// Parse a top-level `/api/show` JSON response into a `ModelMeta`.
///
/// This function is intentionally generous: missing fields fall back to safe
/// defaults rather than erroring out, because cloud manifests routinely omit
/// large chunks of `model_info`.  The only hard failure is when the input
/// JSON isn't an object at all.
pub fn parse_show_response(name: &str, json: &Value) -> Result<ModelMeta, ModelMetaError> {
    let object = json
        .as_object()
        .ok_or_else(|| ModelMetaError::Parse("response root is not a JSON object".to_string()))?;

    let modified_at = object
        .get("modified_at")
        .and_then(Value::as_str)
        .map(str::to_string);

    let details = object.get("details");

    let parameter_size = details
        .and_then(|d| d.get("parameter_size"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let parameter_count = parse_parameter_count(&parameter_size);

    let quantization = details
        .and_then(|d| d.get("quantization_level"))
        .and_then(Value::as_str)
        .map_or(Quantization::Unknown(String::new()), quantization_from_str);

    let format = details
        .and_then(|d| d.get("format"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let families: Vec<String> = details
        .and_then(|d| d.get("families"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Architecture is read from model_info first, falling back to
    // details.family — cloud manifests occasionally drop model_info entirely.
    let model_info = object.get("model_info");
    let arch_str: String = model_info
        .and_then(|m| m.get("general.architecture"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            details
                .and_then(|d| d.get("family"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let architecture = if arch_str.is_empty() {
        Architecture::Other(String::new())
    } else {
        architecture_from_str(&arch_str)
    };

    // GGUF metadata pulls — only meaningful when both model_info and arch are
    // present. The keys are namespaced under the architecture string returned
    // by `general.architecture`, so we use it verbatim as the prefix.
    let (layer_count, head_count, head_count_kv, embedding_length, gguf_ctx) =
        if let (Some(info), false) = (model_info, arch_str.is_empty()) {
            (
                gguf_int(info, &arch_str, "block_count"),
                gguf_int(info, &arch_str, "attention.head_count"),
                gguf_int(info, &arch_str, "attention.head_count_kv"),
                gguf_int(info, &arch_str, "embedding_length"),
                gguf_int(info, &arch_str, "context_length"),
            )
        } else {
            (None, None, None, None, None)
        };

    // Context length resolution: GGUF first, parameters Modelfile second,
    // 4096 floor third.  4096 is conservative but matches Ollama's own
    // default num_ctx, so callers never get a smaller-than-runtime value.
    let parameters_blob = object
        .get("parameters")
        .and_then(Value::as_str)
        .unwrap_or("");
    let context_length = gguf_ctx
        .or_else(|| extract_num_ctx_from_parameters(parameters_blob))
        .unwrap_or(4096);

    // size_bytes — /api/show exposes a top-level `size` field on most modern
    // Ollama versions.  When absent, leave at 0 and let the caller hydrate
    // via /api/tags if it cares.
    let size_bytes = object
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(ModelMeta {
        name: name.to_string(),
        modified_at,
        size_bytes,
        parameter_size,
        parameter_count,
        quantization,
        context_length,
        architecture,
        layer_count,
        head_count,
        head_count_kv,
        embedding_length,
        families,
        format,
    })
}

// ─── Network ─────────────────────────────────────────────────────────────────

/// Resolve the canonical Ollama host: explicit arg if provided, else
/// `OLLAMA_HOST`, else `http://localhost:11434`.  Per `ollama-cloud-auth`,
/// Anvil ALWAYS talks to the local daemon — never to `ollama.com` directly,
/// even for cloud-tagged models.
fn resolve_host(host: &str) -> String {
    if !host.is_empty() {
        return host.trim_end_matches('/').to_string();
    }
    std::env::var(OLLAMA_HOST_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Probe `/api/show` for `model` on the given Ollama host.  3-second hard
/// timeout.  Connection refused / timeout / DNS errors all collapse to
/// `DaemonUnreachable` — the tuner treats that as "no Ollama, try someone
/// else" rather than a fatal error.
pub async fn fetch_model_meta(
    ollama_host: &str,
    model: &str,
) -> Result<ModelMeta, ModelMetaError> {
    let host = resolve_host(ollama_host);
    let url = format!("{host}/api/show");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .post(&url)
        .json(&serde_json::json!({ "name": model }))
        .send()
        .await
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if status.as_u16() == 404 {
        return Err(ModelMetaError::ModelNotInstalled(model.to_string()));
    }
    if !status.is_success() {
        return Err(ModelMetaError::Http {
            status: status.as_u16(),
        });
    }

    let body: Value = response
        .json()
        .await
        .map_err(|e| ModelMetaError::Parse(e.to_string()))?;
    parse_show_response(model, &body)
}

// ─── /api/tags and /api/ps (read-only listing endpoints) ─────────────────────
//
// `fetch_model_meta` covers `/api/show`. The `/ollama list` and `/ollama ps`
// slash commands need two more read-only endpoints: `/api/tags` (every model
// installed on disk) and `/api/ps` (running models). Both are GET requests
// that return small JSON payloads; we share the same 3-second timeout and
// the same `ModelMetaError` variants so callers can use a single error type
// across all four endpoints.

/// One row from `/api/tags` — i.e. one locally-installed Ollama model.
///
/// Field naming matches the on-wire response so `serde` can deserialize the
/// envelope directly. `details.parameter_size` etc. are flattened into a
/// nested struct to keep the consumer pattern (Anvil's `/ollama list`
/// formatter) readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
}

/// One row from `/api/ps` — i.e. one currently-loaded model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningModel {
    pub name: String,
    /// VRAM resident bytes per Ollama. `0` for cloud models.
    #[serde(default)]
    pub size_vram: u64,
    /// Wall-clock expiry timestamp (RFC3339). `None` for keep_alive=-1.
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TagsEnvelope {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct PsEnvelope {
    #[serde(default)]
    models: Vec<RunningModel>,
}

/// Fetch the full installed-model list from `<host>/api/tags`.
///
/// Same 3-second timeout and error-collapsing rules as `fetch_model_meta`.
/// Per `ollama-cloud-auth`, only ever talks to the local daemon.
pub async fn fetch_models_list(ollama_host: &str) -> Result<Vec<OllamaModel>, ModelMetaError> {
    let host = resolve_host(ollama_host);
    let url = format!("{host}/api/tags");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(ModelMetaError::Http {
            status: status.as_u16(),
        });
    }

    let envelope: TagsEnvelope = response
        .json()
        .await
        .map_err(|e| ModelMetaError::Parse(e.to_string()))?;
    Ok(envelope.models)
}

/// Fetch the running-model list from `<host>/api/ps`.
pub async fn fetch_running_models(ollama_host: &str) -> Result<Vec<RunningModel>, ModelMetaError> {
    let host = resolve_host(ollama_host);
    let url = format!("{host}/api/ps");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ModelMetaError::DaemonUnreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(ModelMetaError::Http {
            status: status.as_u16(),
        });
    }

    let envelope: PsEnvelope = response
        .json()
        .await
        .map_err(|e| ModelMetaError::Parse(e.to_string()))?;
    Ok(envelope.models)
}

// ─── Cache ───────────────────────────────────────────────────────────────────

#[derive(Hash, PartialEq, Eq, Clone)]
struct CacheKey {
    host: String,
    model: String,
}

struct CacheEntry {
    modified_at: Option<String>,
    meta: Arc<ModelMeta>,
}

fn cache() -> &'static Mutex<HashMap<CacheKey, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cached variant of `fetch_model_meta`.
///
/// Cache key: `(host, model)`.  The cached entry's `modified_at` is compared
/// against a freshly-fetched response; when they match, the cached `Arc` is
/// returned and no re-parse is performed.  When they differ (or no entry
/// exists yet), the new response is stored and returned.
///
/// Note: we still hit the network on every call — `/api/show` is cheap (~5ms
/// against a local daemon).  The cache exists to avoid repeated *parsing* and
/// to give callers a stable `Arc<ModelMeta>` they can hand off without
/// cloning.  A future revision can add a TTL guard for callers that want to
/// skip the round-trip entirely.
pub async fn fetch_model_meta_cached(
    host: &str,
    model: &str,
) -> Result<Arc<ModelMeta>, ModelMetaError> {
    let fresh = fetch_model_meta(host, model).await?;
    let key = CacheKey {
        host: resolve_host(host),
        model: model.to_string(),
    };

    let mut guard = cache()
        .lock()
        .map_err(|_| ModelMetaError::Parse("model meta cache poisoned".to_string()))?;

    if let Some(existing) = guard.get(&key)
        && existing.modified_at == fresh.modified_at
    {
        return Ok(Arc::clone(&existing.meta));
    }
    let arc = Arc::new(fresh);
    guard.insert(
        key,
        CacheEntry {
            modified_at: arc.modified_at.clone(),
            meta: Arc::clone(&arc),
        },
    );
    Ok(arc)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic but trimmed `/api/show` response for `qwen3:8b`. The
    /// `model_info` block uses arch-prefixed keys (`qwen3.context_length`
    /// etc.) because that's how Ollama actually serializes GGUF metadata.
    const QWEN3_8B_FIXTURE: &str = r#"{
        "modelfile": "FROM qwen3:8b\n",
        "parameters": "stop \"<|im_start|>\"\nstop \"<|im_end|>\"\nnum_ctx 32768\n",
        "template": "{{ .Prompt }}",
        "modified_at": "2026-04-12T08:14:22.123456789Z",
        "size": 5234567890,
        "details": {
            "format": "gguf",
            "family": "qwen3",
            "families": ["qwen3"],
            "parameter_size": "8B",
            "quantization_level": "Q4_K_M"
        },
        "model_info": {
            "general.architecture": "qwen3",
            "qwen3.context_length": 40960,
            "qwen3.block_count": 36,
            "qwen3.attention.head_count": 32,
            "qwen3.attention.head_count_kv": 8,
            "qwen3.embedding_length": 4096
        }
    }"#;

    const LLAMA32_3B_FIXTURE: &str = r#"{
        "modelfile": "FROM llama3.2:3b\n",
        "parameters": "stop \"<|eot_id|>\"\n",
        "template": "{{ .Prompt }}",
        "modified_at": "2026-03-01T11:22:33Z",
        "size": 2019377934,
        "details": {
            "format": "gguf",
            "family": "llama",
            "families": ["llama"],
            "parameter_size": "3B",
            "quantization_level": "Q4_K_M"
        },
        "model_info": {
            "general.architecture": "llama",
            "llama.context_length": 131072,
            "llama.block_count": 28,
            "llama.attention.head_count": 24,
            "llama.attention.head_count_kv": 8,
            "llama.embedding_length": 3072
        }
    }"#;

    /// Cloud manifest — Ollama strips most `model_info` for cloud-tagged
    /// models.  We must still extract the parameter_size and quantization
    /// from `details`, and not crash on missing GGUF metadata.
    const QWEN3_CODER_480B_CLOUD_FIXTURE: &str = r#"{
        "modelfile": "FROM qwen3-coder:480b-cloud\n",
        "parameters": "",
        "template": "",
        "modified_at": "2026-04-30T00:00:00Z",
        "details": {
            "format": "gguf",
            "family": "qwen3",
            "families": ["qwen3"],
            "parameter_size": "480B",
            "quantization_level": "Q4_K_M"
        },
        "model_info": {}
    }"#;

    fn parse_str(name: &str, fixture: &str) -> ModelMeta {
        let value: Value = serde_json::from_str(fixture).expect("fixture is valid JSON");
        parse_show_response(name, &value).expect("fixture parses")
    }

    #[test]
    fn parse_qwen3_8b() {
        let meta = parse_str("qwen3:8b", QWEN3_8B_FIXTURE);
        assert_eq!(meta.name, "qwen3:8b");
        assert_eq!(meta.architecture, Architecture::Qwen3);
        assert_eq!(meta.parameter_size, "8B");
        assert_eq!(meta.parameter_count, 8_000_000_000);
        assert_eq!(meta.quantization, Quantization::Q4_K_M);
        assert_eq!(meta.context_length, 40_960);
        assert_eq!(meta.layer_count, Some(36));
        assert_eq!(meta.head_count, Some(32));
        assert_eq!(meta.head_count_kv, Some(8));
        assert_eq!(meta.embedding_length, Some(4096));
        assert_eq!(meta.size_bytes, 5_234_567_890);
        assert_eq!(meta.format.as_deref(), Some("gguf"));
        assert_eq!(meta.families, vec!["qwen3".to_string()]);
        assert_eq!(meta.modified_at.as_deref(), Some("2026-04-12T08:14:22.123456789Z"));
    }

    #[test]
    fn parse_llama3_2_3b() {
        let meta = parse_str("llama3.2:3b", LLAMA32_3B_FIXTURE);
        assert_eq!(meta.architecture, Architecture::Llama);
        assert_eq!(meta.parameter_size, "3B");
        assert_eq!(meta.parameter_count, 3_000_000_000);
        assert_eq!(meta.context_length, 131_072);
        assert_eq!(meta.layer_count, Some(28));
    }

    #[test]
    fn parse_qwen3_coder_480b_cloud() {
        let meta = parse_str("qwen3-coder:480b-cloud", QWEN3_CODER_480B_CLOUD_FIXTURE);
        assert_eq!(meta.parameter_size, "480B");
        assert_eq!(meta.parameter_count, 480_000_000_000);
        assert_eq!(meta.quantization, Quantization::Q4_K_M);
        // GGUF metadata absent and parameters blob empty — falls back to floor.
        assert_eq!(meta.context_length, 4096);
        assert_eq!(meta.layer_count, None);
        assert_eq!(meta.head_count, None);
        // Architecture comes from details.family even when model_info is empty.
        assert_eq!(meta.architecture, Architecture::Qwen3);
    }

    #[test]
    fn parameter_size_parses_b_m_t_suffixes() {
        assert_eq!(parse_parameter_count("7B"), 7_000_000_000);
        assert_eq!(parse_parameter_count("1.5B"), 1_500_000_000);
        assert_eq!(parse_parameter_count("350M"), 350_000_000);
        assert_eq!(parse_parameter_count("1T"), 1_000_000_000_000);
        // case-insensitive
        assert_eq!(parse_parameter_count("7b"), 7_000_000_000);
        // empty / garbage
        assert_eq!(parse_parameter_count(""), 0);
        assert_eq!(parse_parameter_count("???"), 0);
    }

    #[test]
    fn quantization_enum_round_trip() {
        let q = Quantization::Q4_K_M;
        let json = serde_json::to_string(&q).expect("serialize");
        assert_eq!(json, "\"Q4_K_M\"");
        let back: Quantization = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, Quantization::Q4_K_M);
    }

    #[test]
    fn architecture_unknown_falls_through_to_other() {
        assert_eq!(
            architecture_from_str("myarch"),
            Architecture::Other("myarch".to_string())
        );
    }

    #[test]
    fn gguf_int_handles_missing_key() {
        let info = serde_json::json!({ "llama.context_length": 8192 });
        assert_eq!(gguf_int(&info, "llama", "context_length"), Some(8192));
        assert_eq!(gguf_int(&info, "llama", "block_count"), None);
        assert_eq!(gguf_int(&info, "qwen3", "context_length"), None);
    }

    #[test]
    fn gguf_int_accepts_numeric_strings() {
        // Some Ollama builds serialize uint64 values as strings.
        let info = serde_json::json!({ "llama.context_length": "32768" });
        assert_eq!(gguf_int(&info, "llama", "context_length"), Some(32_768));
    }

    #[test]
    fn context_length_falls_back_to_parameters_field() {
        // No GGUF context_length, but parameters blob carries num_ctx 32768.
        let json = serde_json::json!({
            "parameters": "stop \"<|im_end|>\"\nnum_ctx 32768\n",
            "details": {
                "parameter_size": "7B",
                "quantization_level": "Q4_0",
                "family": "llama",
                "families": ["llama"],
                "format": "gguf"
            },
            "model_info": {
                "general.architecture": "llama"
            }
        });
        let meta = parse_show_response("test", &json).expect("parses");
        assert_eq!(meta.context_length, 32_768);
    }

    #[test]
    fn parse_handles_missing_details_field() {
        // Bare-bones response — no details, no model_info.  Should not panic
        // and should fall back to safe defaults.
        let json = serde_json::json!({
            "modelfile": "",
            "parameters": "",
            "template": ""
        });
        let meta = parse_show_response("mystery", &json).expect("parses");
        assert_eq!(meta.parameter_size, "");
        assert_eq!(meta.parameter_count, 0);
        assert_eq!(meta.context_length, 4096);
        assert_eq!(meta.architecture, Architecture::Other(String::new()));
        assert!(meta.families.is_empty());
        assert_eq!(meta.size_bytes, 0);
    }

    #[test]
    fn quantization_unknown_preserves_label() {
        assert_eq!(
            quantization_from_str("Q3_K_XS"),
            Quantization::Unknown("Q3_K_XS".to_string())
        );
    }

    #[test]
    fn extract_num_ctx_handles_quoted_value() {
        assert_eq!(
            extract_num_ctx_from_parameters("num_ctx \"16384\"\n"),
            Some(16_384)
        );
        assert_eq!(extract_num_ctx_from_parameters(""), None);
        assert_eq!(extract_num_ctx_from_parameters("foo bar"), None);
    }

    #[test]
    fn resolve_host_strips_trailing_slash() {
        assert_eq!(resolve_host("http://localhost:11434/"), "http://localhost:11434");
        assert_eq!(resolve_host("http://localhost:11434"), "http://localhost:11434");
    }

    #[test]
    fn model_meta_error_display_messages() {
        assert_eq!(
            ModelMetaError::ModelNotInstalled("qwen3:8b".to_string()).to_string(),
            "Model not installed: qwen3:8b"
        );
        assert_eq!(
            ModelMetaError::Http { status: 500 }.to_string(),
            "HTTP error: 500"
        );
    }
}
