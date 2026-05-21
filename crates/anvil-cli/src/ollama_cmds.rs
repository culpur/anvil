//! Read-only `/ollama` slash commands: `list`, `show <model>`, `ps`, `tune <model>`.
//!
//! Layered to keep the formatters pure and unit-testable:
//!
//! 1. **Pure formatters** (`format_models_table`, `format_running_models_table`,
//!    `format_tune_result`, `format_tune_error`, `format_show_result`) take
//!    plain values and return `String`. No I/O, no clock, no env reads.
//! 2. **Daemon trait** (`OllamaDaemon`) abstracts the three HTTP endpoints
//!    we need (`/api/tags`, `/api/ps`, `/api/show`). The real impl wraps
//!    `api::fetch_models_list`, `api::fetch_running_models`, and
//!    `api::fetch_model_meta_cached` via a tokio runtime; tests substitute
//!    a stub that returns canned responses.
//! 3. **Sub-handlers** (`cmd_list`, `cmd_ps`, `cmd_show`, `cmd_tune`)
//!    combine the formatter + daemon. Each returns `String` for testability.
//! 4. **Dispatch** (`run_ollama_command`) picks one of the sub-handlers
//!    based on the first whitespace-delimited token.
//!
//! Errors never panic — every daemon failure becomes a human-readable
//! string suitable for `tui.push_system(...)`.
//!
//! Per `ollama-cloud-auth`, the real daemon impl always talks to
//! `localhost:11434` (or `OLLAMA_HOST`); cloud-tagged models are routed
//! through the local daemon, never to `ollama.com` directly.

use std::sync::Arc;

use api::{
    OllamaConfig, OllamaModel, OllamaModelOverride, RunningModel,
    fetch_model_meta_cached, fetch_models_list, fetch_running_models,
    ModelMeta, ModelMetaError,
    ollama_tune::tuner::{tune, KvCacheType, OllamaOptions, Policy, Reasoning, TuneError, TuneResult, UserPolicy},
};
use rust_i18n::t;
use runtime::ollama_tune::hw::{detect_cached, HardwareProfile, GpuKind};

const GIB_F64: f64 = (1024 * 1024 * 1024) as f64;

// ─── Daemon abstraction ──────────────────────────────────────────────────────

/// Abstracts the three Ollama daemon endpoints `/ollama` reads. Implemented
/// by `LiveOllamaDaemon` for the real CLI and by `StubOllamaDaemon` (in
/// `mod tests` below) for unit tests.
pub trait OllamaDaemon {
    fn list_models(&self) -> Result<Vec<OllamaModel>, ModelMetaError>;
    fn running_models(&self) -> Result<Vec<RunningModel>, ModelMetaError>;
    fn show_model(&self, model: &str) -> Result<Arc<ModelMeta>, ModelMetaError>;
}

/// Real daemon impl that bridges to `api::fetch_*` async functions via a
/// tokio runtime. Mirrors the pattern used elsewhere in `main.rs` for hub
/// installs and similar sync-from-async call sites.
pub struct LiveOllamaDaemon {
    pub host: String,
}

impl LiveOllamaDaemon {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    fn block_on<F, T>(fut: F) -> Result<T, ModelMetaError>
    where
        F: std::future::Future<Output = Result<T, ModelMetaError>>,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(fut),
            Err(_) => match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(fut),
                Err(e) => Err(ModelMetaError::DaemonUnreachable(format!(
                    "tokio runtime: {e}"
                ))),
            },
        }
    }
}

impl OllamaDaemon for LiveOllamaDaemon {
    fn list_models(&self) -> Result<Vec<OllamaModel>, ModelMetaError> {
        Self::block_on(fetch_models_list(&self.host))
    }
    fn running_models(&self) -> Result<Vec<RunningModel>, ModelMetaError> {
        Self::block_on(fetch_running_models(&self.host))
    }
    fn show_model(&self, model: &str) -> Result<Arc<ModelMeta>, ModelMetaError> {
        Self::block_on(fetch_model_meta_cached(&self.host, model))
    }
}

// ─── Public entry point ──────────────────────────────────────────────────────

/// Top-level dispatch. Splits `arg` by whitespace, uses the first token as
/// the sub-command (`list` / `show` / `ps` / `tune`), and forwards the
/// remainder. Returns a string ready to hand to `tui.push_system(...)`.
///
/// `ollama_host` is the explicit host (or `""` to fall back to
/// `OLLAMA_HOST` / `http://localhost:11434`).
#[must_use]
pub fn run_ollama_command(arg: Option<&str>, ollama_host: &str) -> String {
    // Manage-side subcommands (`pull`, `rm`, `cp`, `create`) are owned by a
    // parallel work-stream and dispatched separately at the call site once
    // their HTTP plumbing is wired into anvil-cli. This entry point handles
    // only the read-only L5 sub-commands; the routing here keeps them
    // mutually exclusive so the read-side never accidentally swallows a
    // manage verb.
    let raw_for_route = arg.unwrap_or("").trim();
    let route_sub = raw_for_route
        .split_whitespace()
        .next()
        .unwrap_or("");
    if matches!(
        route_sub,
        "pull" | "rm" | "remove" | "delete" | "cp" | "copy" | "create" | "requantize" | "bench"
    ) {
        let _ = ollama_host; // suppress unused on this branch
        return format!(
            "/ollama {route_sub} is handled by the manage-side dispatcher (see crate::ollama_manage)."
        );
    }

    let daemon = LiveOllamaDaemon::new(ollama_host);
    let mut config = OllamaConfig::load();
    let hw = detect_cached();
    let policy = config.policy.clone();

    // L5b mutator subcommands (`option` / `policy` / `keepalive` / `reset`)
    // need a `&mut OllamaConfig` so they can persist via `OllamaConfig::save`.
    // Route to the mutator dispatcher first; on no-match it falls through
    // to the read-only L5 path. `current_model` is sourced from
    // `OLLAMA_MODEL` for now; the live-CLI integration will pass the
    // actually-bound model once the harness wires this in.
    if matches!(route_sub, "option" | "policy" | "keepalive" | "reset") {
        let current_model = std::env::var("OLLAMA_MODEL").unwrap_or_default();
        return run_ollama_command_mut(raw_for_route, ollama_host, &current_model, &mut config);
    }

    run_ollama_command_with(&daemon, arg, &hw, &policy, &config)
}

// ─── L5b: mutator dispatcher ─────────────────────────────────────────────────

/// Mutator dispatcher for `/ollama …` (L5b: option / policy / keepalive /
/// reset). Owns a `&mut OllamaConfig` so the four sub-handlers can
/// persist changes via [`OllamaConfig::save`]. `arg` is the full
/// argument string after `/ollama` (without the leading slash, may be
/// empty). `_host` is reserved for future live-tuner re-injection;
/// today the four mutators just edit settings.json.
///
/// Returns a `String` ready to hand to `tui.push_system(...)`. Errors are
/// reported as text — never panics.
#[must_use]
pub fn run_ollama_command_mut(
    arg: &str,
    _host: &str,
    current_model: &str,
    config: &mut OllamaConfig,
) -> String {
    let trimmed = arg.trim();
    let mut tokens = trimmed.split_whitespace();
    let first = tokens.next().unwrap_or("");
    let rest: Vec<&str> = tokens.collect();

    match first {
        "option" => cmd_option(&rest, current_model, config),
        "policy" => cmd_policy(&rest, config),
        "keepalive" => cmd_keepalive(&rest, current_model, config),
        "reset" => cmd_reset(&rest, current_model, config),
        "" => mutator_help(),
        other => format!(
            "Unknown /ollama mutator subcommand: {other}\n{}",
            mutator_help()
        ),
    }
}

fn mutator_help() -> String {
    "/ollama mutator subcommands:\n  \
     option <key> [<value>]            Per-model override on current model\n  \
     policy <speed|balanced|quality>   Global tuner policy\n  \
     keepalive <duration>              Per-model keep_alive_secs (e.g. 5m, 1h, forever)\n  \
     reset [<model>]                   Clear overrides for a model"
        .to_string()
}

// ─── L5b: allowed option keys ────────────────────────────────────────────────

const VALID_OPTION_KEYS: &[&str] = &[
    "num_ctx",
    "num_gpu",
    "num_thread",
    "flash_attention",
    "kv_cache_type",
    "keep_alive_secs",
    "num_batch",
];

fn valid_keys_block() -> String {
    let mut s = String::from("Valid keys:\n");
    for k in VALID_OPTION_KEYS {
        s.push_str("  ");
        s.push_str(k);
        s.push('\n');
    }
    s
}

// ─── L5b: /ollama option ─────────────────────────────────────────────────────

fn cmd_option(args: &[&str], current_model: &str, config: &mut OllamaConfig) -> String {
    if current_model.is_empty() {
        return t!("slash.ollama.no_current_model").to_string();
    }
    match args.len() {
        0 => format!(
            "Usage: /ollama option <key> <value>\n{}",
            valid_keys_block()
        ),
        1 => {
            let key = args[0];
            if !VALID_OPTION_KEYS.contains(&key) {
                return format!("Unknown option key: {key}\n{}", valid_keys_block());
            }
            show_current_option(key, current_model, config)
        }
        _ => {
            let key = args[0];
            let value = args[1..].join(" ");
            apply_option(key, &value, current_model, config)
        }
    }
}

fn show_current_option(key: &str, current_model: &str, config: &OllamaConfig) -> String {
    let ov = config.override_for(current_model);
    let val: String = match key {
        "num_ctx" => ov
            .and_then(|o| o.num_ctx)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        "num_gpu" => ov
            .and_then(|o| o.num_gpu)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        "num_thread" => ov
            .and_then(|o| o.num_thread)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        "flash_attention" => ov
            .and_then(|o| o.flash_attention)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        "kv_cache_type" => ov.and_then(|o| o.kv_cache_type).map_or_else(
            || "(unset — using tuner default)".into(),
            |v| format_kv_cache_type(v).to_string(),
        ),
        "keep_alive_secs" => ov
            .and_then(|o| o.keep_alive_secs)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        "num_batch" => ov
            .and_then(|o| o.num_batch)
            .map_or_else(|| "(unset — using tuner default)".into(), |v| v.to_string()),
        _ => return format!("Unknown option key: {key}\n{}", valid_keys_block()),
    };
    format!("{key} for {current_model} = {val}")
}

fn apply_option(
    key: &str,
    value: &str,
    current_model: &str,
    config: &mut OllamaConfig,
) -> String {
    let before = describe_override_value(key, config.override_for(current_model));

    let result: Result<String, String> = match key {
        "num_ctx" => parse_u32_range(value, 128, 2_000_000).map(|v| {
            config.set_override(current_model, |o| o.num_ctx = Some(v));
            v.to_string()
        }),
        "num_gpu" => parse_i32_range(value, -1, 999).map(|v| {
            config.set_override(current_model, |o| o.num_gpu = Some(v));
            v.to_string()
        }),
        "num_thread" => parse_u32_range(value, 1, 256).map(|v| {
            config.set_override(current_model, |o| o.num_thread = Some(v));
            v.to_string()
        }),
        "flash_attention" => parse_bool(value).map(|v| {
            config.set_override(current_model, |o| o.flash_attention = Some(v));
            v.to_string()
        }),
        "kv_cache_type" => parse_kv_cache_type(value).map(|v| {
            config.set_override(current_model, |o| o.kv_cache_type = Some(v));
            format_kv_cache_type(v).to_string()
        }),
        "keep_alive_secs" => parse_keep_alive_secs(value).map(|v| {
            config.set_override(current_model, |o| o.keep_alive_secs = Some(v));
            v.to_string()
        }),
        "num_batch" => parse_u32_range(value, 1, 8192).map(|v| {
            config.set_override(current_model, |o| o.num_batch = Some(v));
            v.to_string()
        }),
        _ => return format!("Unknown option key: {key}\n{}", valid_keys_block()),
    };

    match result {
        Ok(after) => match config.save() {
            Ok(()) => {
                // Drop the auto-tune cache for this model so the next chat
                // request picks up the new override immediately.
                api::ollama_tune::invalidate_cache(Some(current_model));
                format!(
                    "Set {key}={after} for {current_model}\n  before: {before}\n  after:  {after}\nRun /ollama tune to see the new effective options."
                )
            }
            Err(e) => format!("Set in-memory but failed to save settings.json: {e}"),
        },
        Err(e) => e,
    }
}

fn describe_override_value(key: &str, ov: Option<&OllamaModelOverride>) -> String {
    let Some(ov) = ov else {
        return "(unset)".to_string();
    };
    match key {
        "num_ctx" => ov.num_ctx.map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        "num_gpu" => ov.num_gpu.map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        "num_thread" => ov
            .num_thread
            .map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        "flash_attention" => ov
            .flash_attention
            .map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        "kv_cache_type" => ov.kv_cache_type.map_or_else(
            || "(unset)".to_string(),
            |v| format_kv_cache_type(v).to_string(),
        ),
        "keep_alive_secs" => ov
            .keep_alive_secs
            .map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        "num_batch" => ov
            .num_batch
            .map_or_else(|| "(unset)".to_string(), |v| v.to_string()),
        _ => "(unset)".into(),
    }
}

// ─── L5b: /ollama policy ─────────────────────────────────────────────────────

fn cmd_policy(args: &[&str], config: &mut OllamaConfig) -> String {
    match args.len() {
        0 => format!("Current policy: {}", policy_label(config.policy.policy)),
        _ => {
            let raw = args[0];
            let parsed = match raw.to_ascii_lowercase().as_str() {
                "speed" => Policy::Speed,
                "balanced" => Policy::Balanced,
                "quality" => Policy::Quality,
                other => {
                    return format!(
                        "Unknown policy: {other}\nValid policies: speed, balanced, quality"
                    );
                }
            };
            let before = policy_label(config.policy.policy);
            config.policy.policy = parsed;
            match config.save() {
                Ok(()) => {
                    // Policy is global — every cached model option becomes
                    // stale. Drop the entire cache.
                    api::ollama_tune::invalidate_cache(None);
                    format!(
                        "Policy set to {}\n  before: {before}\n  after:  {}\nRun /ollama tune to see the new effective options.",
                        policy_label(parsed),
                        policy_label(parsed)
                    )
                }
                Err(e) => format!("Policy set in-memory but failed to save settings.json: {e}"),
            }
        }
    }
}

fn policy_label(p: Policy) -> &'static str {
    match p {
        Policy::Speed => "speed",
        Policy::Balanced => "balanced",
        Policy::Quality => "quality",
    }
}

// ─── L5b: /ollama keepalive ──────────────────────────────────────────────────

fn cmd_keepalive(args: &[&str], current_model: &str, config: &mut OllamaConfig) -> String {
    if current_model.is_empty() {
        return "No current model selected. Set one with /model <name> first.".to_string();
    }
    if args.is_empty() {
        return "Usage: /ollama keepalive <duration>\nAccepts: 30s, 5m, 1h, forever, -1, or plain seconds (e.g. 300)".to_string();
    }
    let raw = args[0];
    let secs = match parse_duration_to_secs(raw) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let before = config
        .override_for(current_model)
        .and_then(|o| o.keep_alive_secs)
        .map_or_else(|| "(unset)".to_string(), human_keepalive);
    config.set_override(current_model, |o| o.keep_alive_secs = Some(secs));
    match config.save() {
        Ok(()) => {
            api::ollama_tune::invalidate_cache(Some(current_model));
            format!(
                "Keepalive for {current_model} set to {}\n  before: {before}\n  after:  {}",
                human_keepalive(secs),
                human_keepalive(secs)
            )
        }
        Err(e) => format!("Keepalive set in-memory but failed to save settings.json: {e}"),
    }
}

fn human_keepalive(secs: i64) -> String {
    if secs == -1 {
        return "forever".to_string();
    }
    if secs <= 0 {
        return format!("{secs}s");
    }
    if secs % 3600 == 0 {
        return format!("{}h", secs / 3600);
    }
    if secs % 60 == 0 {
        return format!("{}m", secs / 60);
    }
    format!("{secs}s")
}

// ─── L5b: /ollama reset ──────────────────────────────────────────────────────

fn cmd_reset(args: &[&str], current_model: &str, config: &mut OllamaConfig) -> String {
    let target_owned: String = args
        .first()
        .copied()
        .map(str::to_string)
        .unwrap_or_else(|| current_model.to_string());
    if target_owned.is_empty() {
        return "No model specified and no current model selected.".to_string();
    }
    if config.override_for(&target_owned).is_none() {
        return format!("No overrides to clear for {target_owned}");
    }
    config.clear_override(&target_owned);
    match config.save() {
        Ok(()) => {
            api::ollama_tune::invalidate_cache(Some(&target_owned));
            format!("Cleared overrides for {target_owned}")
        }
        Err(e) => format!("Cleared in-memory but failed to save settings.json: {e}"),
    }
}

// ─── L5b: parsers ────────────────────────────────────────────────────────────

fn parse_u32_range(s: &str, lo: u32, hi: u32) -> Result<u32, String> {
    let v: u32 = s
        .trim()
        .parse()
        .map_err(|_| format!("Expected unsigned integer in [{lo}, {hi}], got: {s}"))?;
    if v < lo || v > hi {
        return Err(format!("Value {v} out of range [{lo}, {hi}]"));
    }
    Ok(v)
}

fn parse_i32_range(s: &str, lo: i32, hi: i32) -> Result<i32, String> {
    let v: i32 = s
        .trim()
        .parse()
        .map_err(|_| format!("Expected signed integer in [{lo}, {hi}], got: {s}"))?;
    if v < lo || v > hi {
        return Err(format!("Value {v} out of range [{lo}, {hi}]"));
    }
    Ok(v)
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" => Ok(true),
        "false" | "off" | "0" => Ok(false),
        other => Err(format!(
            "Expected boolean (true|false|on|off|1|0), got: {other}"
        )),
    }
}

fn parse_kv_cache_type(s: &str) -> Result<KvCacheType, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "f16" => Ok(KvCacheType::F16),
        "q8_0" => Ok(KvCacheType::Q8_0),
        "q4_0" => Ok(KvCacheType::Q4_0),
        other => Err(format!(
            "Expected one of f16, q8_0, q4_0; got: {other}"
        )),
    }
}

fn parse_keep_alive_secs(s: &str) -> Result<i64, String> {
    let trimmed = s.trim();
    if trimmed == "-1" {
        return Ok(-1);
    }
    let v: i64 = trimmed
        .parse()
        .map_err(|_| format!("Expected -1 or a positive integer, got: {s}"))?;
    if v == -1 || v > 0 {
        Ok(v)
    } else {
        Err(format!("keep_alive_secs must be -1 or > 0, got: {v}"))
    }
}

/// Parse a human duration into seconds. Accepts:
///   * `forever` or `-1`              → -1 (resident forever)
///   * `<n>s`, `<n>m`, `<n>h`         → n in seconds, n*60, n*3600
///   * bare `<n>` (positive integer)  → n seconds
fn parse_duration_to_secs(s: &str) -> Result<i64, String> {
    let trimmed = s.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower == "forever" || lower == "-1" {
        return Ok(-1);
    }
    let (digits, multiplier): (&str, i64) = if let Some(stripped) = lower.strip_suffix('s') {
        (stripped, 1)
    } else if let Some(stripped) = lower.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = lower.strip_suffix('h') {
        (stripped, 3600)
    } else {
        (lower.as_str(), 1)
    };
    let n: i64 = digits.parse().map_err(|_| {
        format!("Expected duration like 30s, 5m, 1h, forever, or plain seconds; got: {s}")
    })?;
    if n <= 0 {
        return Err(format!(
            "Duration must be positive (or 'forever' for -1), got: {s}"
        ));
    }
    n.checked_mul(multiplier)
        .ok_or_else(|| format!("Duration overflow when computing seconds for: {s}"))
}

/// Testable variant — accepts a stub daemon and explicit hardware/policy/
/// config so unit tests can drive every code path without touching the
/// network or the filesystem.
pub fn run_ollama_command_with<D: OllamaDaemon>(
    daemon: &D,
    arg: Option<&str>,
    hw: &HardwareProfile,
    policy: &UserPolicy,
    config: &OllamaConfig,
) -> String {
    let raw = arg.unwrap_or("").trim();
    if raw.is_empty() || raw == "help" || raw == "--help" {
        return usage();
    }

    let mut parts = raw.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match sub {
        "list" | "ls" => cmd_list(daemon),
        "ps" => cmd_ps(daemon),
        "show" => {
            if rest.is_empty() {
                t!("slash.ollama.usage_show").to_string()
            } else {
                cmd_show(daemon, rest, config)
            }
        }
        "tune" => {
            if rest.is_empty() {
                t!("slash.ollama.usage_tune").to_string()
            } else {
                cmd_tune(daemon, rest, hw, policy)
            }
        }
        other => format!(
            "{}\n\n{}",
            t!("slash.ollama.unknown_sub", sub = other),
            usage(),
        ),
    }
}

fn usage() -> String {
    [
        "Usage:",
        "  /ollama list                  List installed Ollama models",
        "  /ollama show <model>          Modelfile defaults + tuned options + overrides",
        "  /ollama ps                    Running models on the local daemon",
        "  /ollama tune <model>          Show tuner reasoning for a model",
        "  /ollama pull <model>          Download a model (refused if free disk < 2 GB)",
        "  /ollama rm <model>            Delete an installed model (typed confirmation required)",
        "  /ollama cp <src> <dst>        Copy a local model under a new tag",
        "  /ollama create <name>         Create a model from a Modelfile (opens $EDITOR)",
        "  /ollama requantize <model> <quant>",
        "                                Suggest a registry tag matching <quant> (no auto-pull)",
        "  /ollama bench <model>         Run 3-prompt benchmark, persist result, recall on /tune",
    ]
    .join("\n")
}

// ─── Sub-handlers ────────────────────────────────────────────────────────────

fn cmd_list<D: OllamaDaemon>(daemon: &D) -> String {
    match daemon.list_models() {
        Ok(models) => format_models_table(&models),
        Err(e) => format_daemon_error(&e),
    }
}

fn cmd_ps<D: OllamaDaemon>(daemon: &D) -> String {
    match daemon.running_models() {
        Ok(models) => format_running_models_table(&models),
        Err(e) => format_daemon_error(&e),
    }
}

fn cmd_show<D: OllamaDaemon>(daemon: &D, model: &str, config: &OllamaConfig) -> String {
    let meta = match daemon.show_model(model) {
        Ok(m) => m,
        Err(e) => return format_daemon_error(&e),
    };
    let hw = detect_cached();
    let tuned = match tune(&hw, &meta, &config.policy) {
        Ok(r) => r.options,
        Err(_) => {
            // OOM at show time — surface the meta + a note instead of failing.
            return format_show_meta_only(&meta, config.override_for(model));
        }
    };
    format_show_result(&meta, &tuned, config.override_for(model))
}

fn cmd_tune<D: OllamaDaemon>(daemon: &D, model: &str, hw: &HardwareProfile, policy: &UserPolicy) -> String {
    let meta = match daemon.show_model(model) {
        Ok(m) => m,
        Err(e) => return format_daemon_error(&e),
    };
    match tune(hw, &meta, policy) {
        Ok(result) => {
            let mut out = format_tune_result(model, hw, &meta, &result);
            // L7 recall: append "Last bench:" line if a prior bench exists.
            if let Some(line) = crate::ollama_bench::format_recall_line(model, &result.options) {
                out.push('\n');
                out.push_str(&line);
            }
            out
        }
        Err(err) => format_tune_error(model, hw, &meta, &err),
    }
}

// ─── Pure formatters ─────────────────────────────────────────────────────────

/// Render the `/api/tags` payload as a column-aligned ASCII table.
#[must_use]
pub fn format_models_table(models: &[OllamaModel]) -> String {
    if models.is_empty() {
        return format!(
            "{}\n\n{}",
            t!("slash.ollama.no_models_installed"),
            t!("slash.ollama.install_hint"),
        );
    }

    // Header + rows
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(models.len() + 1);
    rows.push([
        "NAME".to_string(),
        "SIZE".to_string(),
        "QUANT".to_string(),
        "CTX".to_string(),
        "MODIFIED".to_string(),
    ]);

    for m in models {
        let size = format_size_gb(m.size);
        let (quant, _family) = match &m.details {
            Some(d) => (
                d.quantization_level.clone().unwrap_or_else(|| "—".to_string()),
                d.family.clone().unwrap_or_default(),
            ),
            None => ("—".to_string(), String::new()),
        };
        // Ollama's /api/tags doesn't carry the model's max ctx — the user
        // can drill in with /ollama show <model>. Display "—" as a stable
        // placeholder; cloud models override below.
        let ctx = if m.name.contains(":cloud") || m.name.ends_with("-cloud") {
            "cloud".to_string()
        } else {
            "—".to_string()
        };
        let modified = match &m.modified_at {
            Some(s) => humanize_modified(s),
            None => "—".to_string(),
        };
        rows.push([m.name.clone(), size, quant, ctx, modified]);
    }

    render_table(&rows)
}

/// Render the `/api/ps` payload.
#[must_use]
pub fn format_running_models_table(running: &[RunningModel]) -> String {
    if running.is_empty() {
        return t!("slash.ollama.no_models_running").to_string();
    }
    let mut rows: Vec<[String; 3]> = Vec::with_capacity(running.len() + 1);
    rows.push([
        "NAME".to_string(),
        "VRAM".to_string(),
        "EXPIRES".to_string(),
    ]);
    for m in running {
        let vram = format_size_gb(m.size_vram);
        let expires = match &m.expires_at {
            Some(s) => humanize_expires(s),
            None => "—".to_string(),
        };
        rows.push([m.name.clone(), vram, expires]);
    }
    render_table(&rows)
}

/// Render the per-decision tuner output for `/ollama tune <model>`.
#[must_use]
pub fn format_tune_result(
    model: &str,
    hw: &HardwareProfile,
    meta: &ModelMeta,
    result: &TuneResult,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Tuning {model} on {}\n\n",
        describe_hardware_short(hw)
    ));
    s.push_str(&format!(
        "  Hardware:    {}\n",
        describe_hardware_long(hw)
    ));
    s.push_str(&format!(
        "  Model:       {} · {} · {} ctx · {} layers · {} params\n",
        meta.name,
        format_quantization(&meta.quantization),
        meta.context_length,
        meta.layer_count.map_or("?".to_string(), |n| n.to_string()),
        if meta.parameter_size.is_empty() {
            "?".to_string()
        } else {
            meta.parameter_size.clone()
        },
    ));
    s.push_str(&format!(
        "  Policy:      {}\n\n",
        result.reasoning.policy_summary
    ));

    s.push_str("Recommended options:\n");
    let opts = &result.options;
    let r = &result.reasoning;
    s.push_str(&format_option_with_reason(
        "num_gpu",
        &format_num_gpu(opts.num_gpu, meta.layer_count),
        &r.num_gpu,
    ));
    s.push_str(&format_option_with_reason(
        "num_ctx",
        &opts.num_ctx.to_string(),
        &r.num_ctx,
    ));
    s.push_str(&format_option_with_reason(
        "flash_attention",
        &opts.flash_attention.to_string(),
        &r.flash_attention,
    ));
    s.push_str(&format_option_with_reason(
        "kv_cache_type",
        &format_kv_cache_type(opts.kv_cache_type),
        &r.kv_cache_type,
    ));
    s.push_str(&format_option_with_reason(
        "num_thread",
        &opts.num_thread.to_string(),
        &r.num_thread,
    ));
    s.push_str(&format_option_with_reason(
        "low_vram",
        &opts.low_vram.to_string(),
        &r.low_vram,
    ));
    s.push_str(&format_option_with_reason(
        "num_batch",
        &opts.num_batch.to_string(),
        &format!("default {} unless ≥16GiB free VRAM with full offload", 512),
    ));
    s.push_str(&format_option_with_reason(
        "mmap",
        &opts.mmap.to_string(),
        &r.mmap,
    ));
    s.push_str(&format_option_with_reason(
        "keep_alive",
        &format_keep_alive(opts.keep_alive_secs),
        "default; user can override via OllamaModelOverride",
    ));

    s.push_str(&format!(
        "\nEstimated VRAM usage: {} / {} budget ({})\n",
        format_gib(result.estimated_total_vram_bytes as f64),
        describe_budget(hw),
        if result.fits_in_vram { "fits" } else { "partial offload" },
    ));

    s
}

/// Render an OOM (or other) `TuneError`.
#[must_use]
pub fn format_tune_error(
    model: &str,
    _hw: &HardwareProfile,
    _meta: &ModelMeta,
    err: &TuneError,
) -> String {
    match err {
        TuneError::OomDetected {
            model_estimated_vram_bytes,
            available_vram_bytes,
            suggestions,
        } => {
            let mut s = String::new();
            s.push_str(&format!("/ollama tune {model}\n"));
            s.push_str("WARNING: Model too large for available VRAM\n");
            s.push_str(&format!(
                "   Estimated VRAM:   {}\n",
                format_gib(*model_estimated_vram_bytes as f64)
            ));
            s.push_str(&format!(
                "   Available:        {}\n",
                format_gib(*available_vram_bytes as f64)
            ));
            s.push_str("   Suggestions:\n");
            if suggestions.is_empty() {
                s.push_str("     - (suggestions populated by L6 — leave a TODO if L6 not landed yet)\n");
            } else {
                for sug in suggestions {
                    s.push_str(&format!("     - {sug}\n"));
                }
            }
            s
        }
    }
}

/// Render the side-by-side `Default | Tuned | Override` view for
/// `/ollama show <model>`. `override_` may be `None` (no overrides set).
#[must_use]
pub fn format_show_result(
    meta: &ModelMeta,
    tuned: &OllamaOptions,
    override_: Option<&OllamaModelOverride>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("Model: {}\n", meta.name));
    s.push_str(&format!(
        "Architecture: {} · {} · {} ctx · {} params\n\n",
        format_architecture(&meta.architecture),
        format_quantization(&meta.quantization),
        meta.context_length,
        if meta.parameter_size.is_empty() {
            "?".to_string()
        } else {
            meta.parameter_size.clone()
        },
    ));

    s.push_str("Modelfile defaults:\n");
    s.push_str(&format!("  num_ctx          {}\n", meta.context_length));
    s.push_str(&format!(
        "  parameter_size   {}\n",
        if meta.parameter_size.is_empty() { "—" } else { &meta.parameter_size }
    ));
    s.push_str(&format!(
        "  quantization     {}\n",
        format_quantization(&meta.quantization)
    ));
    s.push_str(&format!(
        "  format           {}\n",
        meta.format.as_deref().unwrap_or("—")
    ));
    s.push('\n');

    s.push_str("Tuner output (current hardware + policy):\n");
    s.push_str(&format!(
        "  num_gpu          {}\n",
        format_num_gpu(tuned.num_gpu, meta.layer_count)
    ));
    s.push_str(&format!("  num_ctx          {}\n", tuned.num_ctx));
    s.push_str(&format!(
        "  flash_attention  {}\n",
        tuned.flash_attention
    ));
    s.push_str(&format!(
        "  kv_cache_type    {}\n",
        format_kv_cache_type(tuned.kv_cache_type)
    ));
    s.push_str(&format!("  num_thread       {}\n", tuned.num_thread));
    s.push_str(&format!("  low_vram         {}\n", tuned.low_vram));
    s.push_str(&format!("  num_batch        {}\n", tuned.num_batch));
    s.push_str(&format!("  mmap             {}\n", tuned.mmap));
    s.push_str(&format!(
        "  keep_alive       {}\n",
        format_keep_alive(tuned.keep_alive_secs)
    ));
    s.push('\n');

    s.push_str("Active overrides (from settings.json):\n");
    match override_ {
        None => s.push_str("  (none)\n"),
        Some(ov) if is_override_empty(ov) => s.push_str("  (none)\n"),
        Some(ov) => {
            if let Some(v) = ov.num_ctx {
                s.push_str(&format!("  num_ctx          {v}    [user override]\n"));
            }
            if let Some(v) = ov.num_gpu {
                s.push_str(&format!("  num_gpu          {v}    [user override]\n"));
            }
            if let Some(v) = ov.num_thread {
                s.push_str(&format!("  num_thread       {v}    [user override]\n"));
            }
            if let Some(v) = ov.flash_attention {
                s.push_str(&format!("  flash_attention  {v}    [user override]\n"));
            }
            if let Some(v) = ov.kv_cache_type {
                s.push_str(&format!(
                    "  kv_cache_type    {}    [user override]\n",
                    format_kv_cache_type(v)
                ));
            }
            if let Some(v) = ov.keep_alive_secs {
                s.push_str(&format!(
                    "  keep_alive       {}    [user override]\n",
                    format_keep_alive(v)
                ));
            }
            if let Some(v) = ov.num_batch {
                s.push_str(&format!("  num_batch        {v}    [user override]\n"));
            }
        }
    }

    s
}

fn format_show_meta_only(meta: &ModelMeta, override_: Option<&OllamaModelOverride>) -> String {
    let mut s = String::new();
    s.push_str(&format!("Model: {}\n", meta.name));
    s.push_str("(tuner returned OOM at current hardware/policy — showing modelfile defaults only)\n\n");
    s.push_str("Modelfile defaults:\n");
    s.push_str(&format!("  num_ctx          {}\n", meta.context_length));
    s.push_str(&format!(
        "  parameter_size   {}\n",
        if meta.parameter_size.is_empty() { "—" } else { &meta.parameter_size }
    ));
    s.push_str(&format!(
        "  quantization     {}\n",
        format_quantization(&meta.quantization)
    ));
    s.push('\n');
    s.push_str("Active overrides (from settings.json):\n");
    match override_ {
        None => s.push_str("  (none)\n"),
        Some(ov) if is_override_empty(ov) => s.push_str("  (none)\n"),
        Some(_) => s.push_str("  (overrides present — see /ollama show on a smaller model)\n"),
    }
    s
}

// ─── Small helpers ───────────────────────────────────────────────────────────

fn format_daemon_error(e: &ModelMetaError) -> String {
    match e {
        ModelMetaError::DaemonUnreachable(_msg) => {
            "Ollama daemon unreachable. Is `ollama serve` running on http://localhost:11434?".to_string()
        }
        ModelMetaError::ModelNotInstalled(name) => {
            format!("Model not installed: {name}\n\nInstall it with `ollama pull {name}`.")
        }
        ModelMetaError::Http { status } => {
            format!("Ollama daemon returned HTTP {status}.")
        }
        ModelMetaError::Parse(msg) => {
            format!("Could not parse Ollama response: {msg}")
        }
    }
}

fn format_size_gb(bytes: u64) -> String {
    if bytes == 0 {
        return "0.0 GB".to_string();
    }
    #[allow(clippy::cast_precision_loss)]
    let gb = (bytes as f64) / 1_000_000_000.0;
    format!("{gb:.1} GB")
}

fn format_gib(bytes: f64) -> String {
    format!("{:.1} GiB", bytes / GIB_F64)
}

fn humanize_modified(rfc3339: &str) -> String {
    // Defensive: if the daemon returns something we can't parse, just hand
    // the user the date portion. Production Ollama always returns RFC3339.
    rfc3339
        .split('T')
        .next()
        .unwrap_or(rfc3339)
        .to_string()
}

fn humanize_expires(rfc3339: &str) -> String {
    // We don't have a chrono-style relative formatter without extra deps,
    // so we surface the wall-clock timestamp. The tests only assert the
    // string is non-empty.
    rfc3339.to_string()
}

fn format_num_gpu(num_gpu: i32, layer_count: Option<u32>) -> String {
    match num_gpu {
        -1 => match layer_count {
            Some(n) => format!("-1 (offload all {n} layers)"),
            None => "-1 (offload all layers)".to_string(),
        },
        0 => "0 (CPU-only)".to_string(),
        n => match layer_count {
            Some(total) => format!("{n} of {total} layers"),
            None => format!("{n} layers"),
        },
    }
}

fn format_kv_cache_type(kv: KvCacheType) -> &'static str {
    match kv {
        KvCacheType::F16 => "f16",
        KvCacheType::Q8_0 => "q8_0",
        KvCacheType::Q4_0 => "q4_0",
    }
}

fn format_keep_alive(secs: i64) -> String {
    if secs < 0 {
        return "forever".to_string();
    }
    if secs == 0 {
        return "0s".to_string();
    }
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 && s == 0 {
        format!("{m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn format_quantization(q: &api::Quantization) -> String {
    use api::Quantization::*;
    match q {
        Q4_0 => "Q4_0".into(),
        Q4_1 => "Q4_1".into(),
        Q4_K_M => "Q4_K_M".into(),
        Q4_K_S => "Q4_K_S".into(),
        Q5_0 => "Q5_0".into(),
        Q5_1 => "Q5_1".into(),
        Q5_K_M => "Q5_K_M".into(),
        Q5_K_S => "Q5_K_S".into(),
        Q6_K => "Q6_K".into(),
        Q8_0 => "Q8_0".into(),
        F16 => "F16".into(),
        BF16 => "BF16".into(),
        F32 => "F32".into(),
        Unknown(s) if s.is_empty() => "?".into(),
        Unknown(s) => s.clone(),
    }
}

fn format_architecture(a: &api::Architecture) -> String {
    use api::Architecture::*;
    match a {
        Llama => "llama".into(),
        Qwen2 => "qwen2".into(),
        Qwen3 => "qwen3".into(),
        Mistral => "mistral".into(),
        Mixtral => "mixtral".into(),
        Gemma2 => "gemma2".into(),
        Gemma3 => "gemma3".into(),
        DeepseekV2 => "deepseek2".into(),
        DeepseekV3 => "deepseek3".into(),
        Phi3 => "phi3".into(),
        CommandR => "command-r".into(),
        Other(s) if s.is_empty() => "?".into(),
        Other(s) => s.clone(),
    }
}

fn describe_hardware_short(hw: &HardwareProfile) -> String {
    let unified = hw.gpu_kind == GpuKind::Metal && hw.vram_total_bytes == hw.ram_total_bytes && hw.ram_total_bytes > 0;
    let gpu = match hw.gpu_kind {
        GpuKind::Metal => "Apple Silicon",
        GpuKind::Cuda => "CUDA",
        GpuKind::Rocm => "ROCm",
        GpuKind::None => "CPU-only",
    };
    if unified {
        format!(
            "{gpu} {} unified",
            format_gib(hw.ram_total_bytes as f64)
        )
    } else {
        format!(
            "{gpu} ({})",
            hw.gpu_name.clone().unwrap_or_else(|| "unknown".to_string())
        )
    }
}

fn describe_hardware_long(hw: &HardwareProfile) -> String {
    let unified = hw.gpu_kind == GpuKind::Metal && hw.vram_total_bytes == hw.ram_total_bytes && hw.ram_total_bytes > 0;
    let gpu_label = match hw.gpu_kind {
        GpuKind::Metal => "Metal",
        GpuKind::Cuda => "CUDA",
        GpuKind::Rocm => "ROCm",
        GpuKind::None => "CPU",
    };
    let mem = if unified {
        format!("{} unified", format_gib(hw.ram_total_bytes as f64))
    } else {
        format!(
            "{} VRAM",
            format_gib(hw.vram_total_bytes as f64)
        )
    };
    format!(
        "{gpu_label} · {mem} · {} cores",
        hw.cpu_threads
    )
}

fn describe_budget(hw: &HardwareProfile) -> String {
    let unified = hw.gpu_kind == GpuKind::Metal && hw.vram_total_bytes == hw.ram_total_bytes && hw.ram_total_bytes > 0;
    if unified {
        format!("~{}", format_gib((hw.ram_available_bytes as f64) * 0.70))
    } else {
        format_gib(hw.vram_free_bytes as f64)
    }
}

fn format_option_with_reason(name: &str, value: &str, reason: &str) -> String {
    let header = format!("  {name:<18} {value}\n");
    let reason_lines = wrap_reason(reason, 60);
    let mut out = header;
    for line in reason_lines {
        out.push_str(&format!("                      Reasoning: {line}\n"));
    }
    out
}

fn wrap_reason(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        } else {
            cur.push(' ');
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn render_table<const N: usize>(rows: &[[String; N]]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut widths = [0usize; N];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let mut out = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == N {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{:<width$}  ", cell, width = widths[i]));
            }
        }
        out.push('\n');
    }
    out
}

fn is_override_empty(ov: &OllamaModelOverride) -> bool {
    ov.num_ctx.is_none()
        && ov.num_gpu.is_none()
        && ov.num_thread.is_none()
        && ov.flash_attention.is_none()
        && ov.kv_cache_type.is_none()
        && ov.keep_alive_secs.is_none()
        && ov.num_batch.is_none()
}

// Suppress "unused imports" if the user-policy enums are referenced via
// re-export only — keep the canonical paths visible for reviewers.
#[allow(dead_code)]
fn _unused_policy_enum_marker() -> Policy {
    Policy::Balanced
}
#[allow(dead_code)]
fn _unused_reasoning_marker(r: &Reasoning) -> &str {
    &r.policy_summary
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use api::{Architecture, OllamaModelDetails, Quantization};

    // ── Fixtures ─────────────────────────────────────────────────────────────

    const GIB: u64 = 1024 * 1024 * 1024;

    fn sample_models() -> Vec<OllamaModel> {
        vec![
            OllamaModel {
                name: "qwen3:8b".into(),
                modified_at: Some("2026-04-12T08:14:22Z".into()),
                size: 5_234_567_890,
                details: Some(OllamaModelDetails {
                    format: Some("gguf".into()),
                    family: Some("qwen3".into()),
                    parameter_size: Some("8B".into()),
                    quantization_level: Some("Q4_K_M".into()),
                }),
            },
            OllamaModel {
                name: "qwen3-coder:latest".into(),
                modified_at: Some("2026-05-01T10:00:00Z".into()),
                size: 18_600_000_000,
                details: Some(OllamaModelDetails {
                    format: Some("gguf".into()),
                    family: Some("qwen3".into()),
                    parameter_size: Some("32B".into()),
                    quantization_level: Some("Q4_K_M".into()),
                }),
            },
            OllamaModel {
                name: "minimax-m2.5:cloud".into(),
                modified_at: None,
                size: 0,
                details: Some(OllamaModelDetails {
                    format: Some("gguf".into()),
                    family: Some("minimax".into()),
                    parameter_size: Some("?".into()),
                    quantization_level: Some("cloud".into()),
                }),
            },
        ]
    }

    fn sample_running() -> Vec<RunningModel> {
        vec![RunningModel {
            name: "qwen3:8b".into(),
            size_vram: 5_234_567_890,
            expires_at: Some("2026-05-08T13:45:00Z".into()),
        }]
    }

    fn sample_meta(name: &str) -> ModelMeta {
        ModelMeta {
            name: name.into(),
            modified_at: Some("2026-04-12T08:14:22Z".into()),
            size_bytes: 5_234_567_890,
            parameter_size: "8B".into(),
            parameter_count: 8_000_000_000,
            quantization: Quantization::Q4_K_M,
            context_length: 32_768,
            architecture: Architecture::Qwen3,
            layer_count: Some(36),
            head_count: Some(32),
            head_count_kv: Some(8),
            embedding_length: Some(4096),
            families: vec!["qwen3".into()],
            format: Some("gguf".into()),
        }
    }

    fn sample_hw_apple_unified() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 32 * GIB,
            ram_available_bytes: 24 * GIB,
            gpu_kind: GpuKind::Metal,
            gpu_name: Some("Apple M2 Pro".into()),
            vram_total_bytes: 32 * GIB,
            vram_free_bytes: 0,
            cpu_threads: 12,
            perf_cores: Some(8),
            has_avx2: false,
            has_avx512: false,
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    fn sample_hw_cuda() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 48 * GIB,
            gpu_kind: GpuKind::Cuda,
            gpu_name: Some("RTX 4090".into()),
            vram_total_bytes: 24 * GIB,
            vram_free_bytes: 22 * GIB,
            cpu_threads: 16,
            perf_cores: None,
            has_avx2: true,
            has_avx512: false,
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    // ── Stub daemon ─────────────────────────────────────────────────────────

    enum StubResp<T> {
        Ok(T),
        Err(ModelMetaError),
    }

    struct StubDaemon {
        list: StubResp<Vec<OllamaModel>>,
        running: StubResp<Vec<RunningModel>>,
        show: StubResp<ModelMeta>,
        show_calls: std::cell::Cell<u32>,
        list_calls: std::cell::Cell<u32>,
    }

    impl StubDaemon {
        fn new() -> Self {
            Self {
                list: StubResp::Ok(sample_models()),
                running: StubResp::Ok(sample_running()),
                show: StubResp::Ok(sample_meta("qwen3:8b")),
                show_calls: std::cell::Cell::new(0),
                list_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl OllamaDaemon for StubDaemon {
        fn list_models(&self) -> Result<Vec<OllamaModel>, ModelMetaError> {
            self.list_calls.set(self.list_calls.get() + 1);
            match &self.list {
                StubResp::Ok(v) => Ok(v.clone()),
                StubResp::Err(e) => Err(clone_err(e)),
            }
        }
        fn running_models(&self) -> Result<Vec<RunningModel>, ModelMetaError> {
            match &self.running {
                StubResp::Ok(v) => Ok(v.clone()),
                StubResp::Err(e) => Err(clone_err(e)),
            }
        }
        fn show_model(&self, _model: &str) -> Result<Arc<ModelMeta>, ModelMetaError> {
            self.show_calls.set(self.show_calls.get() + 1);
            match &self.show {
                StubResp::Ok(m) => Ok(Arc::new(m.clone())),
                StubResp::Err(e) => Err(clone_err(e)),
            }
        }
    }

    fn clone_err(e: &ModelMetaError) -> ModelMetaError {
        match e {
            ModelMetaError::DaemonUnreachable(s) => ModelMetaError::DaemonUnreachable(s.clone()),
            ModelMetaError::ModelNotInstalled(s) => ModelMetaError::ModelNotInstalled(s.clone()),
            ModelMetaError::Parse(s) => ModelMetaError::Parse(s.clone()),
            ModelMetaError::Http { status } => ModelMetaError::Http { status: *status },
        }
    }

    // ── Pure-formatter tests ────────────────────────────────────────────────

    #[test]
    fn format_models_table_aligns_columns() {
        let out = format_models_table(&sample_models());
        // Header row plus one row per model.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "header + 3 rows; got {}", lines.len());
        assert!(lines[0].starts_with("NAME"));
        assert!(lines[0].contains("SIZE"));
        assert!(lines[0].contains("QUANT"));
        assert!(lines[0].contains("CTX"));
        assert!(lines[0].contains("MODIFIED"));
        // Column alignment: every row starts with the same indent (no
        // leading whitespace before NAME).
        for line in &lines {
            assert!(!line.starts_with(' '));
        }
        // Sizes show as GB.
        assert!(out.contains("5.2 GB"));
        assert!(out.contains("18.6 GB"));
    }

    #[test]
    fn format_models_table_handles_empty() {
        let out = format_models_table(&[]);
        assert!(out.contains("No Ollama models installed"));
    }

    #[test]
    fn format_models_table_renders_cloud_models_with_size_zero() {
        let out = format_models_table(&sample_models());
        // Cloud row: size 0 -> "0.0 GB", ctx column shows "cloud".
        assert!(out.contains("minimax-m2.5:cloud"));
        assert!(out.contains("0.0 GB"));
        assert!(out.contains("cloud"));
    }

    #[test]
    fn format_running_models_table_basic() {
        let out = format_running_models_table(&sample_running());
        assert!(out.contains("NAME"));
        assert!(out.contains("VRAM"));
        assert!(out.contains("EXPIRES"));
        assert!(out.contains("qwen3:8b"));
        assert!(out.contains("5.2 GB"));
    }

    #[test]
    fn format_running_models_table_handles_empty() {
        let out = format_running_models_table(&[]);
        assert!(out.contains("No Ollama models currently loaded"));
    }

    #[test]
    fn format_tune_result_includes_all_reasoning_strings() {
        let hw = sample_hw_cuda();
        let meta = sample_meta("qwen3:8b");
        let result = tune(&hw, &meta, &UserPolicy::default()).expect("tunes");
        let out = format_tune_result("qwen3:8b", &hw, &meta, &result);
        // Long reasoning lines are wrapped at 60 chars by `wrap_reason`, so a
        // single field's reasoning may not appear verbatim. Instead we
        // assert that the first whitespace-separated word of each reasoning
        // string appears in the output — that's a stable signal that the
        // reasoning is being surfaced under each option header.
        let first_word = |s: &str| -> String {
            s.split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        };
        for (label, reason) in [
            ("num_gpu", &result.reasoning.num_gpu),
            ("num_ctx", &result.reasoning.num_ctx),
            ("flash_attention", &result.reasoning.flash_attention),
            ("kv_cache_type", &result.reasoning.kv_cache_type),
            ("low_vram", &result.reasoning.low_vram),
            ("num_thread", &result.reasoning.num_thread),
            ("mmap", &result.reasoning.mmap),
        ] {
            let fw = first_word(reason);
            if !fw.is_empty() {
                assert!(
                    out.contains(&fw),
                    "expected reasoning first-word '{fw}' for {label} in:\n{out}"
                );
            }
        }
        // policy_summary is short — assert it whole.
        assert!(
            out.contains(&result.reasoning.policy_summary),
            "policy_summary should appear verbatim under 'Policy:'"
        );
        // Each option label appears as a column header.
        for label in [
            "num_gpu",
            "num_ctx",
            "flash_attention",
            "kv_cache_type",
            "num_thread",
            "low_vram",
            "num_batch",
            "mmap",
            "keep_alive",
        ] {
            assert!(
                out.contains(label),
                "expected option label '{label}' in output:\n{out}"
            );
        }
    }

    #[test]
    fn format_tune_result_apple_silicon_mentions_unified() {
        let hw = sample_hw_apple_unified();
        let meta = sample_meta("qwen3:8b");
        let result = tune(&hw, &meta, &UserPolicy::default()).expect("tunes");
        let out = format_tune_result("qwen3:8b", &hw, &meta, &result);
        assert!(
            out.to_lowercase().contains("unified"),
            "expected 'unified' in output for Apple Silicon, got:\n{out}"
        );
    }

    #[test]
    fn format_tune_error_oom_shows_estimated_vs_available() {
        let hw = sample_hw_cuda();
        let meta = sample_meta("qwen3-coder:480b");
        let err = TuneError::OomDetected {
            model_estimated_vram_bytes: 270 * GIB,
            available_vram_bytes: 18 * GIB,
            suggestions: Vec::new(),
        };
        let out = format_tune_error("qwen3-coder:480b", &hw, &meta, &err);
        assert!(out.to_lowercase().contains("warning"));
        assert!(out.contains("270.0 GiB"));
        assert!(out.contains("18.0 GiB"));
        assert!(
            out.to_lowercase().contains("l6"),
            "should mention L6 TODO for empty suggestions"
        );
    }

    #[test]
    fn format_show_result_shows_all_three_columns_when_override_set() {
        let meta = sample_meta("qwen3:8b");
        let hw = sample_hw_cuda();
        let r = tune(&hw, &meta, &UserPolicy::default()).expect("tunes");
        let mut ov = OllamaModelOverride::default();
        ov.num_ctx = Some(65_536);
        let out = format_show_result(&meta, &r.options, Some(&ov));
        assert!(out.contains("Modelfile defaults"));
        assert!(out.contains("Tuner output"));
        assert!(out.contains("Active overrides"));
        assert!(out.contains("[user override]"));
        assert!(out.contains("65536"));
    }

    #[test]
    fn format_show_result_shows_no_override_when_empty() {
        let meta = sample_meta("qwen3:8b");
        let hw = sample_hw_cuda();
        let r = tune(&hw, &meta, &UserPolicy::default()).expect("tunes");
        let out_none = format_show_result(&meta, &r.options, None);
        assert!(out_none.contains("Active overrides"));
        assert!(out_none.contains("(none)"));

        let empty_ov = OllamaModelOverride::default();
        let out_empty = format_show_result(&meta, &r.options, Some(&empty_ov));
        assert!(out_empty.contains("(none)"));
    }

    // ── Dispatch tests ──────────────────────────────────────────────────────

    #[test]
    fn dispatch_unknown_subcommand_shows_usage() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("blah"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("Unknown /ollama sub-command"));
        assert!(out.contains("Usage:"));
    }

    #[test]
    fn dispatch_show_without_model_arg_shows_usage() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("show"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("Usage: /ollama show <model>"));
        // Daemon must NOT be called for missing-arg dispatch.
        assert_eq!(stub.show_calls.get(), 0);
    }

    #[test]
    fn dispatch_tune_without_model_arg_shows_usage() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("tune"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("Usage: /ollama tune <model>"));
        assert_eq!(stub.show_calls.get(), 0);
    }

    #[test]
    fn dispatch_list_calls_tags_endpoint() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("list"), &hw, &cfg.policy, &cfg);
        assert_eq!(stub.list_calls.get(), 1);
        assert!(out.contains("qwen3:8b"));
    }

    #[test]
    fn dispatch_empty_arg_shows_usage() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, None, &hw, &cfg.policy, &cfg);
        assert!(out.contains("Usage:"));
        assert_eq!(stub.list_calls.get(), 0);
    }

    #[test]
    fn dispatch_ps_calls_running_endpoint() {
        let stub = StubDaemon::new();
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("ps"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("qwen3:8b"));
        assert!(out.contains("VRAM"));
    }

    #[test]
    fn dispatch_daemon_unreachable_yields_friendly_error() {
        let mut stub = StubDaemon::new();
        stub.list = StubResp::Err(ModelMetaError::DaemonUnreachable("connection refused".into()));
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("list"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("Ollama daemon unreachable"));
        // Never panics, never leaks the raw reqwest error.
        assert!(!out.contains("connection refused"));
    }

    #[test]
    fn dispatch_show_model_not_installed_friendly_error() {
        let mut stub = StubDaemon::new();
        stub.show = StubResp::Err(ModelMetaError::ModelNotInstalled("phantom:1b".into()));
        let cfg = OllamaConfig::default();
        let hw = sample_hw_cuda();
        let out = run_ollama_command_with(&stub, Some("show phantom:1b"), &hw, &cfg.policy, &cfg);
        assert!(out.contains("Model not installed"));
        assert!(out.contains("phantom:1b"));
        assert!(out.contains("ollama pull phantom:1b"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // L5b: mutator subcommand tests (option / policy / keepalive / reset).
    //
    // Uses an env-scoped tempdir so OllamaConfig::save() lands in isolation.
    // serial_test guards the env reads/writes; same pattern as
    // api::ollama_tune::policy_config tests.
    // ═══════════════════════════════════════════════════════════════════════

    use serial_test::serial;
    use tempfile::TempDir;

    #[allow(unsafe_code)]
    struct EnvGuard;
    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            // SAFETY: serial_test serialises every test that touches env;
            // this is the only writer to ANVIL_HOME inside this module.
            unsafe { std::env::set_var("ANVIL_HOME", dir) };
            Self
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set.
            unsafe { std::env::remove_var("ANVIL_HOME") };
        }
    }

    fn fresh_home() -> (TempDir, EnvGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = EnvGuard::set(dir.path());
        (dir, guard)
    }

    const MUT_MODEL: &str = "qwen3:8b";

    // ── option ──────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn option_invalid_key_shows_valid_list() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option not_a_real_key 42", "", MUT_MODEL, &mut cfg);
        assert!(
            out.contains("Unknown option key: not_a_real_key"),
            "got: {out}"
        );
        for k in VALID_OPTION_KEYS {
            assert!(out.contains(k), "missing valid key {k} in: {out}");
        }
    }

    #[test]
    #[serial]
    fn option_num_ctx_valid_value_sets_override() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option num_ctx 8192", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Set num_ctx=8192"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.num_ctx),
            Some(8192)
        );
    }

    #[test]
    #[serial]
    fn option_num_ctx_out_of_range_rejected() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        // 64 < 128 lower bound
        let out = run_ollama_command_mut("option num_ctx 64", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("out of range"), "got: {out}");
        assert!(
            cfg.override_for(MUT_MODEL).is_none(),
            "should not have set override"
        );
    }

    #[test]
    #[serial]
    fn option_num_gpu_negative_one_accepted() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option num_gpu -1", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Set num_gpu=-1"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.num_gpu),
            Some(-1)
        );
    }

    #[test]
    #[serial]
    fn option_num_gpu_out_of_range_rejected() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option num_gpu -2", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("out of range"), "got: {out}");
        assert!(cfg.override_for(MUT_MODEL).is_none());
    }

    #[test]
    #[serial]
    fn option_flash_attention_accepts_true_on_1() {
        let (_d, _g) = fresh_home();
        for v in ["true", "on", "1", "TRUE", "On"] {
            let mut cfg = OllamaConfig::default();
            let out = run_ollama_command_mut(
                &format!("option flash_attention {v}"),
                "",
                MUT_MODEL,
                &mut cfg,
            );
            assert!(out.contains("Set flash_attention=true"), "value {v}: {out}");
            assert_eq!(
                cfg.override_for(MUT_MODEL).and_then(|o| o.flash_attention),
                Some(true)
            );
        }
    }

    #[test]
    #[serial]
    fn option_flash_attention_accepts_false_off_0() {
        let (_d, _g) = fresh_home();
        for v in ["false", "off", "0", "FALSE", "Off"] {
            let mut cfg = OllamaConfig::default();
            let out = run_ollama_command_mut(
                &format!("option flash_attention {v}"),
                "",
                MUT_MODEL,
                &mut cfg,
            );
            assert!(out.contains("Set flash_attention=false"), "value {v}: {out}");
            assert_eq!(
                cfg.override_for(MUT_MODEL).and_then(|o| o.flash_attention),
                Some(false)
            );
        }
    }

    #[test]
    #[serial]
    fn option_flash_attention_rejects_garbage() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option flash_attention maybe", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Expected boolean"), "got: {out}");
        assert!(cfg.override_for(MUT_MODEL).is_none());
    }

    #[test]
    #[serial]
    fn option_kv_cache_type_case_insensitive() {
        let (_d, _g) = fresh_home();
        for (raw, want) in [
            ("f16", KvCacheType::F16),
            ("F16", KvCacheType::F16),
            ("q8_0", KvCacheType::Q8_0),
            ("Q8_0", KvCacheType::Q8_0),
            ("q4_0", KvCacheType::Q4_0),
        ] {
            let mut cfg = OllamaConfig::default();
            let out = run_ollama_command_mut(
                &format!("option kv_cache_type {raw}"),
                "",
                MUT_MODEL,
                &mut cfg,
            );
            assert!(out.contains("Set kv_cache_type"), "raw={raw}: {out}");
            assert_eq!(
                cfg.override_for(MUT_MODEL).and_then(|o| o.kv_cache_type),
                Some(want),
                "raw={raw}"
            );
        }
    }

    #[test]
    #[serial]
    fn option_kv_cache_type_rejects_unknown_value() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("option kv_cache_type bf16", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Expected one of f16"), "got: {out}");
        assert!(cfg.override_for(MUT_MODEL).is_none());
    }

    #[test]
    #[serial]
    fn option_no_value_shows_current() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override(MUT_MODEL, |o| o.num_ctx = Some(4096));
        let out = run_ollama_command_mut("option num_ctx", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("num_ctx for qwen3:8b = 4096"), "got: {out}");
    }

    // ── policy ──────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn policy_speed() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("policy speed", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Policy set to speed"), "got: {out}");
        assert!(matches!(cfg.policy.policy, Policy::Speed));
    }

    #[test]
    #[serial]
    fn policy_quality() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("policy quality", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Policy set to quality"), "got: {out}");
        assert!(matches!(cfg.policy.policy, Policy::Quality));
    }

    #[test]
    #[serial]
    fn policy_balanced() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.policy.policy = Policy::Speed;
        let out = run_ollama_command_mut("policy BALANCED", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Policy set to balanced"), "got: {out}");
        assert!(matches!(cfg.policy.policy, Policy::Balanced));
    }

    #[test]
    #[serial]
    fn policy_unknown_rejected() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("policy turbo", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Unknown policy: turbo"), "got: {out}");
        // unchanged
        assert!(matches!(cfg.policy.policy, Policy::Balanced));
    }

    #[test]
    #[serial]
    fn policy_no_arg_shows_current() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.policy.policy = Policy::Quality;
        let out = run_ollama_command_mut("policy", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Current policy: quality"), "got: {out}");
    }

    // ── keepalive ───────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn keepalive_forever_maps_to_minus_one() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive forever", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("set to forever"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(-1)
        );
    }

    #[test]
    #[serial]
    fn keepalive_5m_maps_to_300_secs() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive 5m", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("set to 5m"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(300)
        );
    }

    #[test]
    #[serial]
    fn keepalive_1h_maps_to_3600_secs() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive 1h", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("set to 1h"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(3600)
        );
    }

    #[test]
    #[serial]
    fn keepalive_bare_seconds() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive 300", "", MUT_MODEL, &mut cfg);
        // 300 normalises to "5m" in human_keepalive output.
        assert!(out.contains("set to 5m"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(300)
        );
    }

    #[test]
    #[serial]
    fn keepalive_invalid_format() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive abcd", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("Expected duration"), "got: {out}");
        assert!(cfg.override_for(MUT_MODEL).is_none());
    }

    #[test]
    #[serial]
    fn keepalive_30s_parses() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive 30s", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("set to 30s"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(30)
        );
    }

    #[test]
    #[serial]
    fn keepalive_minus_one_alias() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("keepalive -1", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("set to forever"), "got: {out}");
        assert_eq!(
            cfg.override_for(MUT_MODEL).and_then(|o| o.keep_alive_secs),
            Some(-1)
        );
    }

    // ── reset ───────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn reset_clears_overrides_for_current_model() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override(MUT_MODEL, |o| o.num_ctx = Some(4096));
        assert!(cfg.override_for(MUT_MODEL).is_some());
        let out = run_ollama_command_mut("reset", "", MUT_MODEL, &mut cfg);
        assert!(
            out.contains(&format!("Cleared overrides for {MUT_MODEL}")),
            "got: {out}"
        );
        assert!(cfg.override_for(MUT_MODEL).is_none());
    }

    #[test]
    #[serial]
    fn reset_no_overrides_returns_friendly_message() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("reset", "", MUT_MODEL, &mut cfg);
        assert!(
            out.contains(&format!("No overrides to clear for {MUT_MODEL}")),
            "got: {out}"
        );
    }

    #[test]
    #[serial]
    fn reset_named_model() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        cfg.set_override("llama3:70b", |o| o.num_gpu = Some(-1));
        cfg.set_override(MUT_MODEL, |o| o.num_ctx = Some(4096));
        let out = run_ollama_command_mut("reset llama3:70b", "", MUT_MODEL, &mut cfg);
        assert!(
            out.contains("Cleared overrides for llama3:70b"),
            "got: {out}"
        );
        assert!(cfg.override_for("llama3:70b").is_none());
        // current model's overrides untouched
        assert!(cfg.override_for(MUT_MODEL).is_some());
    }

    // ── dispatcher ──────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn mut_dispatcher_unknown_subcommand_lists_help() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("nope", "", MUT_MODEL, &mut cfg);
        assert!(
            out.contains("Unknown /ollama mutator subcommand: nope"),
            "got: {out}"
        );
        assert!(out.contains("option <key>"), "help text missing: {out}");
    }

    #[test]
    #[serial]
    fn mut_dispatcher_empty_arg_shows_help() {
        let (_d, _g) = fresh_home();
        let mut cfg = OllamaConfig::default();
        let out = run_ollama_command_mut("", "", MUT_MODEL, &mut cfg);
        assert!(out.contains("/ollama mutator subcommands"), "got: {out}");
    }
}
