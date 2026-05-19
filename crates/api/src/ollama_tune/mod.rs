//! Ollama tuner — pure decision engine that converts a `(HardwareProfile,
//! ModelMeta, UserPolicy)` triple into a fully-populated `OllamaOptions`
//! plus a per-decision `Reasoning` block.
//!
//! Lives in `api` (not `runtime`) so it can simultaneously use:
//!   * `api::ModelMeta` / `Architecture` / `Quantization` (this crate's
//!     `/api/show` introspection types), and
//!   * `runtime::ollama_tune::{hw, flash_attn}` (the upstream sibling
//!     modules: hardware introspection + flash-attention compatibility
//!     matrix).
//!
//! `runtime` cannot depend on `api` (the dependency arrow already points
//! the other way), so the tuner anchored here is the natural meeting
//! point. The flash-attention compatibility module duplicates the
//! `Architecture` / `Quantization` enums to avoid the same cycle, and the
//! [`flash_attn_bridge`] submodule provides the small mapping layer that
//! lets us keep the duplicate enums invisible to tuner callers.

pub mod auto_tune;
pub mod bench;
pub mod fit;
pub mod flash_attn_bridge;
pub mod oom_suggestions;
pub mod policy_config;
pub mod tuner;

pub use fit::{params_billions_from_tag, rank_models, FitResult, ModelCandidate, ModelKind};

pub use auto_tune::{
    apply_env_overrides, cpu_fallback_options, invalidate_cache, options_to_request_json,
    resolve_request_options, resolve_request_options_blocking,
};
pub use bench::{
    aggregate_from_prompts, format_bench_qmd_doc, format_bench_summary, model_slug, noop_progress,
    run_bench, run_bench_with_progress, Aggregate, BenchError, BenchProgressCb, BenchResult,
    HostSummary, PromptResult,
};
pub use flash_attn_bridge::flash_attn_supported_for_meta;
pub use policy_config::{OllamaConfig, OllamaConfigError, OllamaModelOverride};
pub use tuner::{
    tune, KvCacheType, OllamaOptions, Policy, Reasoning, TuneError, TuneResult, UserPolicy,
};
