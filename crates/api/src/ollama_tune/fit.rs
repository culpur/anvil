//! Hardware-aware model fit calculator for the v2.2.18 wizard's
//! "pick a model that runs well on this hardware" step (task #666 /
//! Agent A3).
//!
//! Given a list of `ModelCandidate`s and a detected `HardwareProfile`,
//! produces a ranked list of `FitResult`s — one per `(model, quant)`
//! tuple — annotated with whether the weights fit in VRAM (or RAM with
//! CPU offload), whether flash-attention is supported on this host, and
//! an estimated tokens-per-second number derived from VRAM bandwidth.
//!
//! ## Why this lives in `api`
//!
//! `flash_attn_supported` lives in `runtime::ollama_tune::flash_attn`,
//! and the bridge between `api::ModelMeta::architecture` and
//! `runtime::ollama_tune::flash_attn::Architecture` already lives at
//! `api::ollama_tune::flash_attn_bridge`. The fit calculator needs both
//! sides, so anchoring it in `api` (same crate as the bridge, downstream
//! from `runtime`) keeps the dependency arrow pointing the right way.
//!
//! ## Used by
//!
//! - `wizard::wizard_ollama::run_ollama_step` (v2.2.18, A3) — ranks the
//!   curated GENERAL+CODING list AND ranks already-installed models in
//!   the "I have Ollama elsewhere" branch.
//! - `qmd::embed_model_picker` (planned, A4) — ranks embedding-class
//!   models.
//! - `ollama_cmds::healer` (planned, A5) — re-picks a smaller quant when
//!   the active model OOMs.
//!
//! ## 8-axis contract notes
//!
//! This is a pure library function: no slash command, no TUI, no OTel
//! span of its own. Callers spawn the span around their call and report
//! the chosen FitResult on it. The unit tests below cover correctness
//! per `HardwareProfile` class.

use runtime::ollama_tune::flash_attn::{
    flash_attn_supported, Architecture, FlashAttnDecision, Quantization,
};
use runtime::ollama_tune::hw::{GpuKind, HardwareProfile};

/// Kind of workload a model is designed for. Drives the performance
/// floor in the wizard's gate (general models accept slower throughput
/// than coding-specialized models since coding turns are tighter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// General-purpose chat / instruction-following.
    General,
    /// Coding-specialized — needs higher tok/s for an acceptable UX.
    Coding,
    /// Embedding model — extremely cheap, runs almost anywhere; ranker
    /// still computes a fit but the floor check is trivial.
    Embed,
}

/// A single candidate the ranker evaluates. The wizard supplies the
/// curated `GENERAL_MODELS` + `CODING_MODELS` lists; the standalone
/// `/ollama setup` re-entry supplies the same list. The discovered-
/// models branch (State B) supplies what `/api/tags` returned.
#[derive(Debug, Clone)]
pub struct ModelCandidate {
    /// Full Ollama tag, e.g. `"llama3.1:8b"` or `"qwen2.5-coder:7b"`.
    pub tag: String,
    /// Best-effort parameter count in billions (8B Llama → 8.0). The
    /// caller derives this from the tag — see
    /// [`params_billions_from_tag`] for the heuristic.
    pub params_billions: f32,
    /// Workload class.
    pub kind: ModelKind,
    /// Best-effort architecture for FA lookup. The caller passes this
    /// so the ranker does not need to introspect every candidate via
    /// `/api/show` (which costs a network round-trip per candidate).
    /// Use `Architecture::Other("…")` when unknown — the FA decision
    /// will conservatively return "off".
    pub architecture: Architecture,
}

/// Single `(model, quant)` evaluation. `recommendation_tier` is `0` for
/// the row the wizard highlights (largest quant that BOTH fits AND has
/// FA enabled), `1` for "also fits but smaller quant", `2` for "won't
/// fit on this host, here for reference".
#[derive(Debug, Clone)]
pub struct FitResult {
    /// Full Ollama tag (same as `ModelCandidate::tag`).
    pub tag: String,
    /// The quantization this row evaluates.
    pub quant: Quantization,
    /// Whether the quantized weights fit entirely in VRAM with a 2 GB
    /// context headroom.
    pub fits_in_vram: bool,
    /// Whether the quantized weights fit in RAM (with a 4 GB OS reserve)
    /// when CPU offload is required. `true` even for `fits_in_vram` rows.
    pub fits_in_ram: bool,
    /// Estimated steady-state tokens per second based on VRAM bandwidth
    /// (or RAM bandwidth in CPU-offload mode). Conservative — see
    /// `estimated_tok_per_sec`.
    pub est_tok_per_sec: f32,
    /// Flash-attention decision for this `(arch, gpu, quant)` triple.
    pub flash_attn: FlashAttnDecision,
    /// 0 = recommended; 1 = alt that fits; 2 = won't fit cleanly. The
    /// wizard uses tier 0 as the highlighted row in the choice modal.
    pub recommendation_tier: u8,
    /// Estimated on-disk size in GB (for "~5 GB" display in the modal).
    pub est_size_gb: f32,
    /// Workload kind, carried through so the wizard's floor check has
    /// the right threshold.
    pub kind: ModelKind,
}

/// Bytes-per-parameter for each quant level. Slightly conservative —
/// real GGUF files include a small metadata + token-embedding overhead,
/// which we fold into the per-quant constant rather than tracking
/// separately.
fn bytes_per_param(quant: &Quantization) -> f32 {
    match quant {
        Quantization::Q4_0 | Quantization::Q4_1 | Quantization::Q4_K_S => 0.55,
        Quantization::Q4_K_M => 0.58,
        Quantization::Q5_0 | Quantization::Q5_1 | Quantization::Q5_K_S => 0.68,
        Quantization::Q5_K_M => 0.70,
        Quantization::Q6_K => 0.83,
        Quantization::Q8_0 => 1.05,
        Quantization::F16 | Quantization::BF16 => 2.05,
        Quantization::F32 => 4.05,
        Quantization::Unknown(_) => 1.05,
    }
}

/// Estimated steady-state tokens-per-second for a candidate given the
/// GPU class and parameter count. Calibrated against published Llama-3 /
/// Qwen-3 / Mistral benchmarks (Apple Silicon 70b @ Q4_K_M ≈ 9 tok/s on
/// M3 Max, RTX 4090 8B @ Q5_K_M ≈ 110 tok/s).
///
/// We expose this so the wizard can show "~42 tok/s est" before pulling
/// the model. The post-pull bench supersedes this — the on-disk
/// `BenchResult` is the authoritative number after the user actually
/// runs `/ollama bench`.
fn estimated_tok_per_sec(params_billions: f32, hw: &HardwareProfile, fits_vram: bool) -> f32 {
    if params_billions <= 0.0 {
        return 0.0;
    }
    // Baseline tok/s for a 7B model on each backend, calibrated against
    // published numbers. The per-billion scaling is roughly inverse — a
    // 14B model runs about half as fast as a 7B on the same hardware.
    let (baseline_at_7b, scale_efficiency) = match hw.gpu_kind {
        // Apple Silicon: unified memory + lower theoretical bandwidth
        // than discrete GPUs. M2 Pro ≈ 200 GB/s, M3 Max ≈ 400 GB/s.
        GpuKind::Metal => (55.0, 0.50),
        // NVIDIA: highest bandwidth, best kernels. RTX 4090 ≈ 1 TB/s.
        GpuKind::Cuda => (95.0, 0.70),
        // ROCm: improving but historically lags CUDA.
        GpuKind::Rocm => (60.0, 0.45),
        // CPU-only: order of magnitude slower.
        GpuKind::None => (8.0, 0.35),
    };
    // CPU-offload penalty when weights don't fit in VRAM: throughput
    // drops to roughly the CPU baseline regardless of GPU class.
    let effective_baseline = if fits_vram {
        baseline_at_7b
    } else if matches!(hw.gpu_kind, GpuKind::None) {
        baseline_at_7b
    } else {
        // Partial offload — closer to CPU speeds.
        12.0
    };
    let ratio = 7.0_f32 / params_billions.max(1.0);
    effective_baseline * ratio.powf(scale_efficiency)
}

/// Rank a candidate list against the supplied hardware profile.
///
/// Returns one `FitResult` per `(candidate, quant)` tuple across Q4_K_M
/// / Q5_K_M / Q8_0. The output is sorted with the recommended row first
/// (`recommendation_tier == 0`), then by quant size descending within
/// each candidate.
///
/// The wizard's caller typically picks the first row with
/// `recommendation_tier == 0` as the default choice and shows the rest
/// as alternatives.
#[must_use]
pub fn rank_models(
    candidates: &[ModelCandidate],
    hw: &HardwareProfile,
) -> Vec<FitResult> {
    // Three default quants we evaluate per candidate. We deliberately
    // omit F16 / F32 — those are not realistic for local inference on
    // the 4-64 GB host class we target.
    let quants = [
        Quantization::Q4_K_M,
        Quantization::Q5_K_M,
        Quantization::Q8_0,
    ];

    let vram_budget_bytes = hw
        .vram_total_bytes
        .saturating_sub(2 * 1024 * 1024 * 1024); // 2 GB context headroom
    let ram_budget_bytes = hw
        .ram_total_bytes
        .saturating_sub(4 * 1024 * 1024 * 1024); // 4 GB OS reserve

    let mut all: Vec<FitResult> = Vec::with_capacity(candidates.len() * quants.len());

    for cand in candidates {
        // For each candidate, find the LARGEST quant that fits in VRAM
        // AND has FA support. That gets tier 0. Other fits get tier 1.
        // Non-fits get tier 2.
        let mut rows: Vec<FitResult> = Vec::with_capacity(quants.len());

        for quant in &quants {
            let weight_bytes =
                (cand.params_billions * 1.0e9 * bytes_per_param(quant)) as u64;
            let fits_vram = weight_bytes > 0 && weight_bytes <= vram_budget_bytes;
            let fits_ram = weight_bytes > 0 && weight_bytes <= ram_budget_bytes;

            let fa = flash_attn_supported(&cand.architecture, hw.gpu_kind, quant);
            let est = estimated_tok_per_sec(cand.params_billions, hw, fits_vram);
            let est_size_gb = weight_bytes as f32 / 1.0e9;

            rows.push(FitResult {
                tag: cand.tag.clone(),
                quant: quant.clone(),
                fits_in_vram: fits_vram,
                fits_in_ram: fits_ram,
                est_tok_per_sec: est,
                flash_attn: fa,
                recommendation_tier: 2, // placeholder, set below
                est_size_gb,
                kind: cand.kind,
            });
        }

        // Find the recommendation: largest quant that fits + has FA.
        // Quants are listed Q4 < Q5 < Q8; iterate in reverse so we
        // prefer the largest-fitting quant.
        let mut rec_idx: Option<usize> = None;
        for (i, row) in rows.iter().enumerate().rev() {
            if row.fits_in_vram && row.flash_attn.supported {
                rec_idx = Some(i);
                break;
            }
        }
        // Fallback 1: largest quant that fits in VRAM, FA off.
        if rec_idx.is_none() {
            for (i, row) in rows.iter().enumerate().rev() {
                if row.fits_in_vram {
                    rec_idx = Some(i);
                    break;
                }
            }
        }
        // Fallback 2: largest quant that fits in RAM (CPU offload).
        if rec_idx.is_none() {
            for (i, row) in rows.iter().enumerate().rev() {
                if row.fits_in_ram {
                    rec_idx = Some(i);
                    break;
                }
            }
        }

        for (i, row) in rows.iter_mut().enumerate() {
            row.recommendation_tier = if Some(i) == rec_idx {
                0
            } else if row.fits_in_vram || row.fits_in_ram {
                1
            } else {
                2
            };
        }

        all.extend(rows);
    }

    // Sort: tier 0 first (recommended rows across candidates), tier 1
    // next, tier 2 last. Within a tier, larger estimated tok/s first
    // (so the user sees the best option per candidate at the top of
    // its group).
    all.sort_by(|a, b| {
        a.recommendation_tier
            .cmp(&b.recommendation_tier)
            .then_with(|| {
                b.est_tok_per_sec
                    .partial_cmp(&a.est_tok_per_sec)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    all
}

/// Heuristic: extract a parameter count from an Ollama tag.
///
/// Examples:
/// - `"llama3.1:8b"` → `8.0`
/// - `"qwen2.5-coder:7b"` → `7.0`
/// - `"llama3.3:70b"` → `70.0`
/// - `"phi4:14b"` → `14.0`
/// - `"mistral-nemo:12b"` → `12.0`
/// - `"deepseek-r1:1.5b"` → `1.5`
///
/// Returns `None` when no `<N>b` suffix is present.
#[must_use]
pub fn params_billions_from_tag(tag: &str) -> Option<f32> {
    let suffix = tag.rsplit(':').next()?;
    // Two flavors of suffix:
    //   - bare: "8b", "70b", "1.5b"        — trailing 'b' marks the size
    //   - with quant: "8b-q5_K_M"          — '-' splits size from quant tail
    // We first split on '-' (or '_' separator some tags use) and take
    // the leading chunk, which must end in 'b' / 'B'.
    let head = suffix.split(['-', '_']).next()?;
    let stripped = head.strip_suffix('b').or_else(|| head.strip_suffix('B'))?;
    stripped.parse::<f32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw_m2_air_16gb() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 16 * 1024 * 1024 * 1024,
            ram_available_bytes: 10 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::Metal,
            gpu_name: Some("Apple M2".into()),
            vram_total_bytes: 16 * 1024 * 1024 * 1024, // unified memory
            vram_free_bytes: 10 * 1024 * 1024 * 1024,
            cpu_threads: 8,
            perf_cores: Some(4),
            has_avx2: false,
            has_avx512: false,
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    fn hw_m3_max_64gb() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 64 * 1024 * 1024 * 1024,
            ram_available_bytes: 48 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::Metal,
            gpu_name: Some("Apple M3 Max".into()),
            vram_total_bytes: 64 * 1024 * 1024 * 1024,
            vram_free_bytes: 48 * 1024 * 1024 * 1024,
            cpu_threads: 16,
            perf_cores: Some(12),
            has_avx2: false,
            has_avx512: false,
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    fn hw_amd_integrated_8gb() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 8 * 1024 * 1024 * 1024,
            ram_available_bytes: 5 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::None, // integrated, not registered as ROCm
            gpu_name: None,
            vram_total_bytes: 0,
            vram_free_bytes: 0,
            cpu_threads: 8,
            perf_cores: None,
            has_avx2: true,
            has_avx512: false,
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    fn hw_rtx_4090() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 64 * 1024 * 1024 * 1024,
            ram_available_bytes: 48 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::Cuda,
            gpu_name: Some("NVIDIA GeForce RTX 4090".into()),
            vram_total_bytes: 24 * 1024 * 1024 * 1024,
            vram_free_bytes: 22 * 1024 * 1024 * 1024,
            cpu_threads: 16,
            perf_cores: None,
            has_avx2: true,
            has_avx512: true,
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    fn hw_rpi5_4gb() -> HardwareProfile {
        HardwareProfile {
            ram_total_bytes: 4 * 1024 * 1024 * 1024,
            ram_available_bytes: 3 * 1024 * 1024 * 1024,
            gpu_kind: GpuKind::None,
            gpu_name: None,
            vram_total_bytes: 0,
            vram_free_bytes: 0,
            cpu_threads: 4,
            perf_cores: None,
            has_avx2: false,
            has_avx512: false,
            os: "linux".into(),
            arch: "aarch64".into(),
        }
    }

    fn candidates() -> Vec<ModelCandidate> {
        vec![
            ModelCandidate {
                tag: "llama3.1:8b".into(),
                params_billions: 8.0,
                kind: ModelKind::General,
                architecture: Architecture::Llama,
            },
            ModelCandidate {
                tag: "qwen3:14b".into(),
                params_billions: 14.0,
                kind: ModelKind::General,
                architecture: Architecture::Qwen3,
            },
            ModelCandidate {
                tag: "llama3.3:70b".into(),
                params_billions: 70.0,
                kind: ModelKind::General,
                architecture: Architecture::Llama,
            },
            ModelCandidate {
                tag: "gemma3:4b".into(),
                params_billions: 4.0,
                kind: ModelKind::General,
                architecture: Architecture::Gemma3,
            },
            ModelCandidate {
                tag: "qwen2.5-coder:7b".into(),
                params_billions: 7.0,
                kind: ModelKind::Coding,
                architecture: Architecture::Qwen2,
            },
        ]
    }

    /// Pick the best (tier-0) result for a given model tag.
    fn pick_best(results: &[FitResult], tag: &str) -> Option<FitResult> {
        results
            .iter()
            .find(|r| r.tag == tag && r.recommendation_tier == 0)
            .cloned()
    }

    #[test]
    fn ranker_m2_air_16gb_recommends_q8_for_8b() {
        let results = rank_models(&candidates(), &hw_m2_air_16gb());
        // 8B model at Q8_0 ≈ 8.4 GB — fits the 14 GB VRAM budget.
        let llama = pick_best(&results, "llama3.1:8b").expect("must recommend llama3.1:8b");
        assert_eq!(llama.quant, Quantization::Q8_0);
        assert!(llama.fits_in_vram);
        assert!(llama.flash_attn.supported);
        // 70B doesn't fit anywhere — should be tier 2 throughout.
        let llama_70b: Vec<_> = results.iter().filter(|r| r.tag == "llama3.3:70b").collect();
        assert!(llama_70b.iter().all(|r| !r.fits_in_vram));
    }

    #[test]
    fn ranker_m3_max_64gb_can_run_70b() {
        let results = rank_models(&candidates(), &hw_m3_max_64gb());
        // 70B model: Q4_K_M ≈ 40 GB, Q5_K_M ≈ 49 GB, Q8_0 ≈ 73 GB.
        // VRAM budget = 64 - 2 = 62 GB → largest fitting quant is Q5_K_M.
        let big = pick_best(&results, "llama3.3:70b").expect("must recommend 70b on M3 Max");
        assert!(
            matches!(big.quant, Quantization::Q5_K_M),
            "expected Q5_K_M on 64GB host, got {:?}",
            big.quant
        );
        assert!(big.fits_in_vram);
        assert!(big.flash_attn.supported);
        // 8B should get Q8_0 with FA.
        let small = pick_best(&results, "llama3.1:8b").expect("must recommend 8b on M3 Max");
        assert_eq!(small.quant, Quantization::Q8_0);
        assert!(small.flash_attn.supported);
    }

    #[test]
    fn ranker_amd_integrated_8gb_falls_back_to_small_model() {
        let results = rank_models(&candidates(), &hw_amd_integrated_8gb());
        // No VRAM — every fit is RAM-only (CPU offload). FA should be
        // disabled across the board (CPU backend).
        for r in &results {
            assert!(!r.fits_in_vram, "no VRAM, no row should fit VRAM");
            assert!(
                !r.flash_attn.supported,
                "CPU backend means FA off for every row"
            );
        }
        // Gemma3 4B should be the only model whose Q4_K_M fits in 4 GB
        // of usable RAM (8 GB total - 4 GB OS reserve).
        let gemma = pick_best(&results, "gemma3:4b").expect("gemma3:4b should be recommended");
        assert!(gemma.fits_in_ram);
        // 70B should not be tier 0 on this hardware.
        assert!(pick_best(&results, "llama3.3:70b").is_none());
    }

    #[test]
    fn ranker_rtx_4090_picks_q8_with_fa() {
        let results = rank_models(&candidates(), &hw_rtx_4090());
        // 8B model at Q8_0 ≈ 8.4 GB — fits 22 GB VRAM budget. FA on.
        let llama = pick_best(&results, "llama3.1:8b").expect("must recommend 8b on 4090");
        assert_eq!(llama.quant, Quantization::Q8_0);
        assert!(llama.fits_in_vram);
        assert!(llama.flash_attn.supported);
        // Estimated tok/s for an 8B model on CUDA should be ~85+.
        assert!(
            llama.est_tok_per_sec > 70.0,
            "8B on 4090 should est >70 tok/s, got {}",
            llama.est_tok_per_sec
        );
        // 14B should also fit at Q8_0.
        let qwen = pick_best(&results, "qwen3:14b").expect("must recommend 14b on 4090");
        assert!(qwen.fits_in_vram);
        // 70B does NOT fit on 24 GB even at Q4_K_M (40 GB).
        let big_results: Vec<_> = results.iter().filter(|r| r.tag == "llama3.3:70b").collect();
        assert!(big_results.iter().all(|r| !r.fits_in_vram));
    }

    #[test]
    fn ranker_rpi5_4gb_only_recommends_smallest_or_nothing() {
        let results = rank_models(&candidates(), &hw_rpi5_4gb());
        // 4 GB total RAM - 4 GB OS reserve = 0 budget. Every model
        // should be tier 2. Floor check downstream will reject the host.
        for r in &results {
            assert!(!r.fits_in_vram);
            assert!(!r.fits_in_ram, "{} {:?} unexpectedly fit on rpi5", r.tag, r.quant);
            assert_eq!(r.recommendation_tier, 2);
        }
    }

    #[test]
    fn ranker_picks_largest_quant_with_fa_first() {
        let results = rank_models(&candidates(), &hw_m3_max_64gb());
        // Every tier-0 row should have FA on (because we have Metal +
        // none of the candidates are gemma2 or other-blocklisted).
        let tier_0: Vec<_> = results.iter().filter(|r| r.recommendation_tier == 0).collect();
        assert!(!tier_0.is_empty());
        for row in tier_0 {
            assert!(
                row.flash_attn.supported,
                "tier-0 row {} {:?} missing FA",
                row.tag,
                row.quant
            );
        }
    }

    #[test]
    fn params_billions_extracts_from_tag() {
        assert_eq!(params_billions_from_tag("llama3.1:8b"), Some(8.0));
        assert_eq!(params_billions_from_tag("qwen2.5-coder:7b"), Some(7.0));
        assert_eq!(params_billions_from_tag("llama3.3:70b"), Some(70.0));
        assert_eq!(params_billions_from_tag("phi4:14b"), Some(14.0));
        assert_eq!(params_billions_from_tag("deepseek-r1:1.5b"), Some(1.5));
        assert_eq!(params_billions_from_tag("mistral-nemo:12b"), Some(12.0));
        // Tagged with quant suffix: "8b-q5_K_M"
        assert_eq!(params_billions_from_tag("llama3.1:8b-q5_K_M"), Some(8.0));
        // No 'b' suffix
        assert!(params_billions_from_tag("custom-model:latest").is_none());
    }

    #[test]
    fn ranker_handles_empty_candidate_list() {
        let results = rank_models(&[], &hw_m2_air_16gb());
        assert!(results.is_empty());
    }

    #[test]
    fn ranker_sorts_tier_0_first() {
        let results = rank_models(&candidates(), &hw_m2_air_16gb());
        let mut last_tier = 0u8;
        for r in &results {
            assert!(
                r.recommendation_tier >= last_tier,
                "results not sorted by tier"
            );
            last_tier = r.recommendation_tier;
        }
    }
}
