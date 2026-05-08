//! OOM refusal-flow suggestions for the Ollama tuner (Task #368, "L6").
//!
//! When [`crate::ollama_tune::tuner::tune`] returns
//! [`crate::ollama_tune::tuner::TuneError::OomDetected`], the `suggestions`
//! vector is — by design — empty. This module fills it.
//!
//! The function here is **pure**: feed in precomputed data, get a ranked
//! `Vec<String>` back. Never auto-applies anything; the user reads each
//! string and decides for themselves.
//!
//! Priority order (descending):
//!
//! 1. Smaller-quant variant of the same model already installed.
//! 2. Smaller-param installed model with same architecture.
//! 3. Drop `num_ctx` (only when the floor still fits).
//! 4. Switch policy to Speed (only when not already on Speed).
//! 5. CPU-only fallback (always emitted as the last-resort).
//! 6. Requantize hint (only when the requested quant is larger than q4_K_M).
//!
//! Final list is capped at 5 items to avoid overwhelming the user.

use runtime::ollama_tune::hw::HardwareProfile;

use crate::ollama_tune::tuner::{
    compute_vram_budget, kv_bytes, kv_dtype_bytes, quant_bytes_per_param, KvCacheType, Policy,
    UserPolicy, GIB,
};
use crate::providers::ollama_show::{ModelMeta, Quantization};

/// One installed-model view, typically derived from `/api/tags`.
///
/// L6 introduces an enum with a `Tagged` variant so future sources (e.g.
/// cloud catalogs, registry pulls) can be added without breaking callers.
#[derive(Debug, Clone)]
pub enum InstalledModel {
    /// A row from the local Ollama daemon's `/api/tags` response.
    Tagged {
        name: String,
        size_bytes: u64,
        parameter_size: String,
        quantization: Quantization,
        family: Option<String>,
    },
}

impl InstalledModel {
    fn name(&self) -> &str {
        match self {
            InstalledModel::Tagged { name, .. } => name,
        }
    }
    fn size_bytes(&self) -> u64 {
        match self {
            InstalledModel::Tagged { size_bytes, .. } => *size_bytes,
        }
    }
    fn parameter_size(&self) -> &str {
        match self {
            InstalledModel::Tagged { parameter_size, .. } => parameter_size,
        }
    }
    fn quantization(&self) -> &Quantization {
        match self {
            InstalledModel::Tagged { quantization, .. } => quantization,
        }
    }
    fn family(&self) -> Option<&str> {
        match self {
            InstalledModel::Tagged { family, .. } => family.as_deref(),
        }
    }
}

/// Maximum number of suggestions returned. The first N strings, in priority
/// order, are kept.
const MAX_SUGGESTIONS: usize = 5;

/// Build OOM suggestions. Pure: takes precomputed data, returns strings.
#[must_use]
pub fn build_oom_suggestions(
    requested_model: &ModelMeta,
    _requested_estimated_vram_bytes: u64,
    available_vram_bytes: u64,
    hw: &HardwareProfile,
    policy: &UserPolicy,
    installed: &[InstalledModel],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // 1. Smaller-quant variant of the same base model already installed.
    if let Some(s) =
        suggest_smaller_installed_quant(requested_model, available_vram_bytes, installed)
    {
        out.push(s);
    }

    // 2. Smaller-param installed model with same architecture / family.
    if let Some(s) =
        suggest_smaller_installed_arch(requested_model, available_vram_bytes, installed)
    {
        out.push(s);
    }

    // 3. Drop num_ctx.
    if let Some(s) = suggest_lower_ctx(requested_model, hw, policy) {
        out.push(s);
    }

    // 4. Switch policy to Speed.
    if let Some(s) = suggest_speed_policy(requested_model, hw, policy) {
        out.push(s);
    }

    // 5. CPU fallback. Always emitted (last resort).
    out.push(suggest_cpu_fallback(requested_model));

    // 6. Requantize hint (only when requested quant is larger than q4_K_M).
    if let Some(s) = suggest_requantize(requested_model) {
        out.push(s);
    }

    // Cap the list.
    out.truncate(MAX_SUGGESTIONS);
    out
}

// ─── Priority 1: smaller-quant variant of same base model ────────────────────

fn suggest_smaller_installed_quant(
    requested: &ModelMeta,
    available_vram_bytes: u64,
    installed: &[InstalledModel],
) -> Option<String> {
    let req_base = base_name(&requested.name);
    let req_quant_rank = quant_rank(&requested.quantization);
    let req_param_count = requested.parameter_count;

    let mut best: Option<(&InstalledModel, u64)> = None;

    for cand in installed {
        if cand.name() == requested.name {
            continue;
        }
        if base_name(cand.name()) != req_base {
            continue;
        }
        // Either smaller quant OR smaller param count for the same base name.
        let cand_quant_rank = quant_rank(cand.quantization());
        let cand_param_count = parse_param_count(cand.parameter_size()).unwrap_or(0);
        let smaller_quant = cand_quant_rank < req_quant_rank;
        let smaller_params =
            req_param_count > 0 && cand_param_count > 0 && cand_param_count < req_param_count;
        if !smaller_quant && !smaller_params {
            continue;
        }
        // Conservative fit-check: candidate's on-disk size must fit in budget.
        if cand.size_bytes() == 0 || cand.size_bytes() > available_vram_bytes {
            continue;
        }
        match best {
            None => best = Some((cand, cand.size_bytes())),
            Some((_, prev_size)) if cand.size_bytes() < prev_size => {
                best = Some((cand, cand.size_bytes()));
            }
            _ => {}
        }
    }

    best.map(|(m, sz)| {
        format!(
            "Try the smaller variant: {} (~{} GB, would fit in your {} GB budget)",
            m.name(),
            fmt_gb(sz),
            fmt_gb(available_vram_bytes),
        )
    })
}

// ─── Priority 2: smaller-param installed model with same architecture ────────

fn suggest_smaller_installed_arch(
    requested: &ModelMeta,
    available_vram_bytes: u64,
    installed: &[InstalledModel],
) -> Option<String> {
    let req_base = base_name(&requested.name);
    let req_arch_keys = arch_keys(requested);
    let req_param_count = requested.parameter_count;
    if req_param_count == 0 {
        return None;
    }

    let mut best: Option<(&InstalledModel, u64)> = None;
    for cand in installed {
        if cand.name() == requested.name {
            continue;
        }
        if base_name(cand.name()) == req_base {
            continue; // priority 1 handles same-base-name
        }
        // Match architecture / family.
        let cand_keys = installed_arch_keys(cand);
        if !req_arch_keys
            .iter()
            .any(|a| cand_keys.iter().any(|b| a == b))
        {
            continue;
        }
        let cand_params = parse_param_count(cand.parameter_size()).unwrap_or(0);
        if cand_params == 0 || cand_params >= req_param_count {
            continue;
        }
        if cand.size_bytes() == 0 || cand.size_bytes() > available_vram_bytes {
            continue;
        }
        // Prefer smallest fitting candidate (least chance of OOM again).
        match best {
            None => best = Some((cand, cand.size_bytes())),
            Some((_, prev_size)) if cand.size_bytes() < prev_size => {
                best = Some((cand, cand.size_bytes()));
            }
            _ => {}
        }
    }

    best.map(|(m, sz)| {
        format!(
            "Use {} instead — same architecture, fits in {} GB.",
            m.name(),
            fmt_gb(sz),
        )
    })
}

// ─── Priority 3: drop num_ctx ────────────────────────────────────────────────

fn suggest_lower_ctx(
    requested: &ModelMeta,
    hw: &HardwareProfile,
    policy: &UserPolicy,
) -> Option<String> {
    let budget = compute_vram_budget(hw, policy);
    let weights = weights_bytes(requested);
    let layers = requested.layer_count.unwrap_or(32).max(1);
    let head_count = requested.head_count.unwrap_or(8).max(1);
    let head_count_kv = requested.head_count_kv.unwrap_or(head_count).max(1);
    let embedding_length = requested.embedding_length.unwrap_or(4096).max(1);
    let head_dim = (embedding_length / head_count).max(1);
    let kv_dtype_b = kv_dtype_bytes(KvCacheType::F16);

    // Halve from model default down to context_min, looking for the largest
    // ctx that fits weights + KV in budget at the same quant + full offload.
    let starting_ctx = requested.context_length.max(policy.context_min);
    let mut ctx = starting_ctx;
    let mut best_fit: Option<u32> = None;
    loop {
        let kv = kv_bytes(ctx, layers, head_count_kv, head_dim, kv_dtype_b);
        if (weights + kv) as u64 <= budget {
            best_fit = Some(ctx);
            break;
        }
        if ctx <= policy.context_min {
            break;
        }
        let next = (ctx / 2).max(policy.context_min);
        if next == ctx {
            break;
        }
        ctx = next;
    }

    let chosen = best_fit?;
    if chosen >= starting_ctx {
        return None; // no real reduction
    }
    Some(format!(
        "Lower context: --num-ctx {} (currently model default {}) would fit",
        chosen, requested.context_length
    ))
}

// ─── Priority 4: switch policy to Speed ──────────────────────────────────────

fn suggest_speed_policy(
    requested: &ModelMeta,
    hw: &HardwareProfile,
    policy: &UserPolicy,
) -> Option<String> {
    if matches!(policy.policy, Policy::Speed) {
        return None;
    }
    // Speed caps initial ctx at 8192 (or context_min, whichever is higher).
    let speed_ctx = requested.context_length.min(8192).max(policy.context_min);

    // Conservative fit-check: weights + KV at speed_ctx must fit budget.
    let budget = compute_vram_budget(hw, policy);
    let weights = weights_bytes(requested);
    let layers = requested.layer_count.unwrap_or(32).max(1);
    let head_count = requested.head_count.unwrap_or(8).max(1);
    let head_count_kv = requested.head_count_kv.unwrap_or(head_count).max(1);
    let embedding_length = requested.embedding_length.unwrap_or(4096).max(1);
    let head_dim = (embedding_length / head_count).max(1);
    let kv = kv_bytes(
        speed_ctx,
        layers,
        head_count_kv,
        head_dim,
        kv_dtype_bytes(KvCacheType::F16),
    );
    if (weights + kv) as u64 > budget {
        // Speed wouldn't help — skip.
        return None;
    }

    Some(format!(
        "Switch policy: '/ollama policy speed' (would use num_ctx={speed_ctx}, all-GPU)"
    ))
}

// ─── Priority 5: CPU fallback (always) ───────────────────────────────────────

fn suggest_cpu_fallback(requested: &ModelMeta) -> String {
    let speed_hint = if requested.parameter_count >= 70_000_000_000 {
        " — slower but works on any RAM size; expect ~1 token/sec"
    } else if requested.parameter_count <= 8_000_000_000 && requested.parameter_count > 0 {
        " — slower but works on any RAM size; expect ~10-30 tokens/sec on modern CPUs"
    } else {
        " — slower but works on any RAM size"
    };
    format!("CPU-only fallback: '/ollama option num_gpu 0'{speed_hint}")
}

// ─── Priority 6: requantize hint ─────────────────────────────────────────────

fn suggest_requantize(requested: &ModelMeta) -> Option<String> {
    // Only emit when requested quant is *strictly larger* than q4_K_M. If
    // already at q4 or smaller, no point suggesting q4.
    let rank = quant_rank(&requested.quantization);
    let q4km_rank = quant_rank(&Quantization::Q4_K_M);
    if rank <= q4km_rank {
        return None;
    }
    Some(format!(
        "Or pull a smaller-quant variant: '/ollama requantize {} q4_K_M' (when available)",
        requested.name,
    ))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Strip `:<tag>` from an Ollama model name. `qwen3-coder:latest` → `qwen3-coder`.
fn base_name(name: &str) -> &str {
    name.split_once(':').map_or(name, |(b, _)| b)
}

/// Conservative parameter-count parse, mirroring the private helper in
/// `providers::ollama_show`. `"7B"` → `7_000_000_000`. Returns `None` for
/// empty/unparseable input.
///
/// TODO: refactor `parse_parameter_count` in `providers::ollama_show` to
/// `pub(crate)` and reuse instead of duplicating.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn parse_param_count(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (num_part, mult): (&str, u64) =
        match trimmed.chars().last().map(|c| c.to_ascii_uppercase()) {
            Some('B') => (&trimmed[..trimmed.len() - 1], 1_000_000_000),
            Some('M') => (&trimmed[..trimmed.len() - 1], 1_000_000),
            Some('T') => (&trimmed[..trimmed.len() - 1], 1_000_000_000_000),
            Some('K') => (&trimmed[..trimmed.len() - 1], 1_000),
            _ => (trimmed, 1),
        };
    let value: f64 = num_part.trim().parse().ok()?;
    let scaled = value * (mult as f64);
    if scaled.is_finite() && scaled >= 0.0 {
        Some(scaled.round() as u64)
    } else {
        None
    }
}

/// Total weights footprint in bytes for a given model (params * bpp).
#[allow(clippy::cast_precision_loss)]
fn weights_bytes(m: &ModelMeta) -> f64 {
    (m.parameter_count as f64) * quant_bytes_per_param(&m.quantization)
}

/// Format bytes as a short "X GB" string for user-facing copy. Two
/// significant-ish figures: `< 10 GB` → one decimal, else integer.
#[allow(clippy::cast_precision_loss)]
fn fmt_gb(bytes: u64) -> String {
    let gb = (bytes as f64) / (GIB as f64);
    if gb >= 10.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

/// Lower number = smaller weights footprint per parameter (better candidate
/// for fit). Stable rank ordering used to compare two quants.
fn quant_rank(q: &Quantization) -> u32 {
    match q {
        Quantization::Q4_0 | Quantization::Q4_1 | Quantization::Q4_K_S | Quantization::Q4_K_M => 4,
        Quantization::Q5_0 | Quantization::Q5_1 | Quantization::Q5_K_S | Quantization::Q5_K_M => 5,
        Quantization::Q6_K => 6,
        Quantization::Q8_0 => 8,
        Quantization::F16 | Quantization::BF16 => 16,
        Quantization::F32 => 32,
        Quantization::Unknown(_) => 100, // worst-rank: never auto-recommend
    }
}

/// Architecture-matching keys for the requested model: parsed architecture
/// debug string + each family entry, all lowercased.
fn arch_keys(m: &ModelMeta) -> Vec<String> {
    let mut keys: Vec<String> = m.families.iter().map(|f| f.to_lowercase()).collect();
    let arch_str = format!("{:?}", m.architecture).to_lowercase();
    if !arch_str.is_empty() {
        keys.push(arch_str);
    }
    keys
}

fn installed_arch_keys(m: &InstalledModel) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    if let Some(f) = m.family() {
        keys.push(f.to_lowercase());
    }
    keys
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama_tune::tuner::{tune_with_suggestions, TuneError};
    use crate::providers::ollama_show::Architecture;
    use runtime::ollama_tune::hw::GpuKind;

    // ── Fixtures ────────────────────────────────────────────────────────────

    fn tight_hw() -> HardwareProfile {
        // 24 GB GPU, 22 GB free → budget 18 GB after 4 GB reserve.
        HardwareProfile {
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 32 * GIB,
            gpu_kind: GpuKind::Cuda,
            gpu_name: Some("Mock GPU 24GB".into()),
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

    fn meta(
        name: &str,
        params: u64,
        ctx: u32,
        arch: Architecture,
        q: Quantization,
    ) -> ModelMeta {
        ModelMeta {
            name: name.into(),
            modified_at: None,
            size_bytes: 0,
            parameter_size: pp_str(params),
            parameter_count: params,
            quantization: q,
            context_length: ctx,
            architecture: arch,
            layer_count: Some(32),
            head_count: Some(32),
            head_count_kv: Some(8),
            embedding_length: Some(4096),
            families: vec!["llama".into()],
            format: Some("gguf".into()),
        }
    }

    fn pp_str(params: u64) -> String {
        if params == 0 {
            String::new()
        } else if params >= 1_000_000_000 {
            format!("{}B", params / 1_000_000_000)
        } else {
            format!("{}M", params / 1_000_000)
        }
    }

    fn installed_tagged(
        name: &str,
        params: &str,
        q: Quantization,
        size_gb: u64,
        family: &str,
    ) -> InstalledModel {
        InstalledModel::Tagged {
            name: name.into(),
            size_bytes: size_gb * GIB,
            parameter_size: params.into(),
            quantization: q,
            family: Some(family.into()),
        }
    }

    fn balanced_policy() -> UserPolicy {
        UserPolicy {
            policy: Policy::Balanced,
            vram_reserve_gb: 4,
            context_min: 8192,
        }
    }

    // ── Tests ───────────────────────────────────────────────────────────────

    #[test]
    fn suggests_smaller_installed_quant() {
        // Request qwen3-coder:latest (100B q8_0) — won't fit; installed has
        // qwen3-coder:7b q4_K_M at 5GB, which fits in 18GB budget.
        let req = meta(
            "qwen3-coder:latest",
            100_000_000_000,
            32_768,
            Architecture::Qwen3,
            Quantization::Q8_0,
        );
        let installed = vec![installed_tagged(
            "qwen3-coder:7b",
            "7B",
            Quantization::Q4_K_M,
            5,
            "qwen3",
        )];
        let suggestions = build_oom_suggestions(
            &req,
            120 * GIB,
            18 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &installed,
        );
        assert!(
            suggestions.iter().any(|s| s.contains("qwen3-coder:7b")),
            "expected first-tier suggestion to mention qwen3-coder:7b: {suggestions:?}"
        );
        // Priority order: should be the *first* suggestion.
        assert!(
            suggestions[0].contains("qwen3-coder:7b"),
            "smaller-installed-quant should be first: {suggestions:?}"
        );
    }

    #[test]
    fn suggests_smaller_installed_arch_match() {
        // Request 70B llama; have 7B llama installed under a different base name.
        let req = meta(
            "llama-big:70b",
            70_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let installed = vec![installed_tagged(
            "llama-small:7b",
            "7B",
            Quantization::Q4_K_M,
            5,
            "llama",
        )];
        let suggestions = build_oom_suggestions(
            &req,
            45 * GIB,
            18 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &installed,
        );
        let joined = suggestions.join(" ");
        assert!(
            joined.contains("llama-small:7b") && joined.contains("same architecture"),
            "should recommend the 7B llama variant: {suggestions:?}"
        );
    }

    #[test]
    fn suggests_lower_ctx_when_smaller_ctx_fits() {
        // 13B-class model at default 128k ctx. Weights ≈ 7.3 GB, so ample room
        // in the 10 GB budget — but full ctx blows the KV cache. A smaller ctx
        // should be suggested.
        let mut req = meta(
            "midsize:13b",
            13_000_000_000,
            131_072,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        req.layer_count = Some(40);
        req.head_count = Some(40);
        req.head_count_kv = Some(40);
        req.embedding_length = Some(5120);
        let suggestions = build_oom_suggestions(
            &req,
            30 * GIB,
            10 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions.iter().any(|s| s.contains("--num-ctx")),
            "lower-ctx suggestion should appear for midsize model: {suggestions:?}"
        );
    }

    #[test]
    fn does_not_suggest_lower_ctx_below_context_min() {
        // 405B-class model: even at context_min the weights themselves
        // already exceed the budget — no ctx number can help.
        let mut req = meta(
            "huge:405b",
            405_000_000_000,
            131_072,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        req.layer_count = Some(126);
        req.head_count = Some(128);
        req.head_count_kv = Some(8);
        req.embedding_length = Some(16_384);

        let policy = UserPolicy {
            policy: Policy::Balanced,
            vram_reserve_gb: 4,
            context_min: 16_384,
        };
        let suggestions = build_oom_suggestions(
            &req,
            300 * GIB,
            18 * GIB,
            &tight_hw(),
            &policy,
            &[],
        );
        assert!(
            suggestions.iter().all(|s| !s.contains("--num-ctx")),
            "lower-ctx must be OMITTED when context_min itself doesn't fit: {suggestions:?}"
        );
    }

    #[test]
    fn suggests_speed_policy_when_on_balanced() {
        // 13B model: speed (8k ctx) puts weights+KV inside the budget.
        let req = meta(
            "mid:13b",
            13_000_000_000,
            32_768,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let suggestions = build_oom_suggestions(
            &req,
            18 * GIB,
            10 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions.iter().any(|s| s.contains("policy speed")),
            "balanced → speed suggestion expected: {suggestions:?}"
        );
    }

    #[test]
    fn does_not_suggest_speed_when_already_on_speed() {
        let req = meta(
            "mid:13b",
            13_000_000_000,
            32_768,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let policy = UserPolicy {
            policy: Policy::Speed,
            vram_reserve_gb: 4,
            context_min: 8192,
        };
        let suggestions = build_oom_suggestions(
            &req,
            18 * GIB,
            10 * GIB,
            &tight_hw(),
            &policy,
            &[],
        );
        assert!(
            suggestions.iter().all(|s| !s.contains("policy speed")),
            "speed→speed should NOT suggest 'policy speed': {suggestions:?}"
        );
    }

    #[test]
    fn always_suggests_cpu_fallback() {
        let req = meta(
            "any:7b",
            7_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let suggestions = build_oom_suggestions(
            &req,
            8 * GIB,
            4 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions.iter().any(|s| s.contains("num_gpu 0")),
            "CPU fallback must always be present: {suggestions:?}"
        );
    }

    #[test]
    fn cpu_suggestion_includes_speed_estimate_for_small_model() {
        let req = meta(
            "tiny:8b",
            8_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let suggestions = build_oom_suggestions(
            &req,
            10 * GIB,
            4 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions.iter().any(|s| s.contains("10-30 tokens/sec")),
            "small-model CPU estimate missing: {suggestions:?}"
        );
    }

    #[test]
    fn cpu_suggestion_includes_speed_estimate_for_large_model() {
        let req = meta(
            "big:70b",
            70_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let suggestions = build_oom_suggestions(
            &req,
            45 * GIB,
            4 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions.iter().any(|s| s.contains("1 token/sec")),
            "large-model CPU estimate missing: {suggestions:?}"
        );
    }

    #[test]
    fn suggestion_list_capped_at_5_items() {
        // Construct a scenario that emits all six possible suggestions.
        let req = meta(
            "qwen3-coder:latest",
            100_000_000_000,
            131_072,
            Architecture::Qwen3,
            Quantization::Q8_0, // requantize hint emits since q8 > q4
        );
        let installed = vec![
            installed_tagged("qwen3-coder:7b", "7B", Quantization::Q4_K_M, 5, "qwen3"),
            installed_tagged("llama:7b", "7B", Quantization::Q4_K_M, 4, "llama"),
        ];
        let suggestions = build_oom_suggestions(
            &req,
            120 * GIB,
            18 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &installed,
        );
        assert!(
            suggestions.len() <= 5,
            "must be capped at 5: got {}",
            suggestions.len()
        );
    }

    #[test]
    fn suggestions_in_priority_order() {
        let req = meta(
            "qwen3-coder:latest",
            100_000_000_000,
            131_072,
            Architecture::Qwen3,
            Quantization::Q4_K_M, // disable requantize hint to keep <=5
        );
        let installed = vec![installed_tagged(
            "qwen3-coder:7b",
            "7B",
            Quantization::Q4_K_M,
            5,
            "llama",
        )];
        let suggestions = build_oom_suggestions(
            &req,
            60 * GIB,
            18 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &installed,
        );
        let idx_installed = suggestions
            .iter()
            .position(|s| s.contains("qwen3-coder:7b"));
        let idx_speed = suggestions.iter().position(|s| s.contains("policy speed"));
        let idx_cpu = suggestions.iter().position(|s| s.contains("num_gpu 0"));

        if let (Some(a), Some(b)) = (idx_installed, idx_cpu) {
            assert!(a < b, "installed-quant must come before CPU: {suggestions:?}");
        }
        if let (Some(a), Some(b)) = (idx_speed, idx_cpu) {
            assert!(a < b, "speed must come before CPU: {suggestions:?}");
        }
    }

    #[test]
    fn requantize_suggestion_only_emitted_for_larger_quants() {
        // q8_0 → suggestion present.
        let req_q8 = meta(
            "any:13b",
            13_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q8_0,
        );
        let s_q8 = build_oom_suggestions(
            &req_q8,
            14 * GIB,
            4 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            s_q8.iter().any(|s| s.contains("requantize")),
            "q8_0 should suggest requantize: {s_q8:?}"
        );

        // q4_K_M → no requantize hint.
        let req_q4 = meta(
            "any:13b",
            13_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let s_q4 = build_oom_suggestions(
            &req_q4,
            14 * GIB,
            4 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            s_q4.iter().all(|s| !s.contains("requantize")),
            "q4_K_M must NOT suggest requantize: {s_q4:?}"
        );
    }

    #[test]
    fn tune_with_suggestions_passes_through_ok_unchanged() {
        // A scenario that tunes successfully (small model + big budget).
        let hw = HardwareProfile {
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 48 * GIB,
            gpu_kind: GpuKind::Cuda,
            gpu_name: Some("Mock GPU 24GB".into()),
            vram_total_bytes: 24 * GIB,
            vram_free_bytes: 22 * GIB,
            cpu_threads: 16,
            perf_cores: None,
            has_avx2: true,
            has_avx512: false,
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let m = meta(
            "small:7b",
            7_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        let r = tune_with_suggestions(&hw, &m, &balanced_policy(), &[])
            .expect("Ok passes through");
        assert!(r.fits_in_vram);
    }

    #[test]
    fn tune_with_suggestions_populates_oom_suggestions() {
        let hw = tight_hw();
        let mut m = meta(
            "huge:480b",
            480_000_000_000,
            8192,
            Architecture::Llama,
            Quantization::Q4_K_M,
        );
        m.layer_count = Some(96);
        let err = tune_with_suggestions(&hw, &m, &balanced_policy(), &[])
            .expect_err("must OOM");
        match err {
            TuneError::OomDetected { suggestions, .. } => {
                assert!(
                    !suggestions.is_empty(),
                    "OOM suggestions must be populated by wrapper"
                );
                assert!(suggestions.iter().any(|s| s.contains("num_gpu 0")));
            }
        }
    }

    #[test]
    fn empty_installed_list_only_emits_non_installed_suggestions() {
        let req = meta(
            "mid:13b",
            13_000_000_000,
            32_768,
            Architecture::Llama,
            Quantization::Q8_0, // also triggers requantize hint
        );
        let suggestions = build_oom_suggestions(
            &req,
            14 * GIB,
            10 * GIB,
            &tight_hw(),
            &balanced_policy(),
            &[],
        );
        assert!(
            suggestions
                .iter()
                .all(|s| !s.contains("Try the smaller variant")
                    && !s.contains("same architecture")),
            "no installed-* suggestions when list is empty: {suggestions:?}"
        );
        assert!(suggestions.iter().any(|s| s.contains("num_gpu 0")));
    }

    #[test]
    fn base_name_strips_tag() {
        assert_eq!(base_name("qwen3-coder:latest"), "qwen3-coder");
        assert_eq!(base_name("llama:7b-instruct-q4_K_M"), "llama");
        assert_eq!(base_name("plain"), "plain");
    }

    #[test]
    fn parse_param_count_smoke() {
        assert_eq!(parse_param_count("7B"), Some(7_000_000_000));
        assert_eq!(parse_param_count("1.5B"), Some(1_500_000_000));
        assert_eq!(parse_param_count("350M"), Some(350_000_000));
        assert_eq!(parse_param_count(""), None);
        assert_eq!(parse_param_count("???"), None);
    }
}
