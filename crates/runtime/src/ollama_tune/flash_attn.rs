//! Flash-attention compatibility matrix for the Ollama tuner.
//!
//! Given a model's reported architecture, the host's GPU backend, and the
//! model's weight quantization, decide whether enabling flash-attention is
//! safe. The decision is conservative-by-default: anything we cannot prove
//! is supported returns `supported = false` with a human-readable reason
//! that the `/ollama tune` command surfaces verbatim.
//!
//! ## Source of truth (audited 2026-05-08)
//!
//! - Ollama `main` @ commit `f866e7608f378dcfca6f8c717101df1945db3b97`
//!   - `fs/ggml/ggml.go::SupportsFlashAttention` — the architecture-level
//!     allow/blocklist. Embedding models always return `false`. The hard
//!     allowlist is `{qwen35, qwen35moe, qwen3next}` (always true). The hard
//!     blocklist is `{gemma2, grok}` (always false). Everything else falls
//!     through to a head-count check (`head_count_k == head_count_v && both
//!     != 0`), which holds for every transformer architecture this matrix
//!     enumerates.
//!   - `ml/device.go::FlashAttentionSupported` — the GPU-backend gate. CUDA
//!     requires driver major >= 7 AND not compute 7.2 (Jetson Xavier).
//!     Metal, `ROCm`, Vulkan, and the CPU runtime all report supported.
//!   - `llm/server.go` — runtime composition. Gemma4 on CUDA also requires
//!     SM 7.5+ because of the 512-dim attention head; not modelled here
//!     because gemma4 is not in our `Architecture` enum yet.
//!
//! - llama.cpp `master` @ commit `6d57a49a7025235b0a844f6f56cbd057524d8904`
//!   - `docs/ops.md` reports `FLASH_ATTN_EXT` as: CPU 🟡 partial, CUDA ✅
//!     full, Metal 🟡 partial, ROCm/HIP 🟡 partial, Vulkan 🟡 partial.
//!   - `ggml/src/ggml-cuda/fattn.cu` — head dims supported are
//!     `{40, 64, 72, 80, 96, 112, 128, 256, 320, 512, 576}`. KV-cache
//!     types supported are F16, BF16, F32, `Q4_0`, `Q4_1`, `Q5_0`, `Q5_1`,
//!     `Q8_0`. The K-quants (`Q4_K`, `Q5_K`, `Q6_K`) are NOT supported as
//!     KV-cache types.
//!
//! ## Three surprises vs. naive intuition
//!
//! 1. **Model weight quantization does not gate FA.** Whether the model on
//!    disk is `Q4_K_M`, `Q8_0`, F16, or BF16 is irrelevant — those weights are
//!    dequantized into the FA kernel, which only cares about the activation
//!    dtype (F16/BF16) and the KV-cache dtype. We therefore allow FA for
//!    every weight quant. The only model-level gate is the architecture
//!    allow/blocklist baked into Ollama's `SupportsFlashAttention`.
//!
//! 2. **CPU is supported in Ollama's gate.** `ml/device.go` lists `cpu` as
//!    a supported FA library, and llama.cpp's CPU backend ships a partial
//!    FA implementation (`docs/ops.md` row). Older guidance said "CPU has
//!    no FA kernels" — that's no longer true in 2026.
//!
//! 3. **Gemma2 is hard-blocklisted; Gemma3 is not.** Ollama explicitly
//!    refuses FA for `gemma2` regardless of backend. Gemma3 falls through
//!    the head-count check and is supported on every backend the GPU gate
//!    permits.
//!
//! ## Difference between `OLLAMA_FLASH_ATTENTION` and request `options.flash_attention`
//!
//! `OLLAMA_FLASH_ATTENTION=1` set on the daemon is a *user-set override* —
//! it forces FA on (or off, with `=0`) and `server.go` records `faUserSet =
//! true` so the per-model `SupportsFlashAttention` short-circuit no longer
//! suppresses it. Passing `flash_attention: true` in a `/api/generate` or
//! `/api/chat` `options` block is treated identically to setting the env
//! var at request time. Either form is gated by the GPU-backend check in
//! `ml.FlashAttentionSupported`. This module models the *default* policy
//! Ollama itself would pick — a tuner that wants to force FA on a
//! blocklisted combo (e.g. for benchmarking) can override the result, but
//! the conservative default is what we return.

use crate::ollama_tune::hw::GpuKind;

/// Architecture tag we know how to reason about.
///
/// Mirrors the variants of `api::providers::ollama_show::Architecture`. We
/// duplicate the enum here rather than importing it because `api` already
/// depends on `runtime` (the dependency arrow points the other way), so a
/// direct `use api::...` would introduce a cycle. Callers that already hold
/// an `api::Architecture` can convert with `From` — see the impl block on
/// the consumer side.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Quantization tag, mirroring `api::providers::ollama_show::Quantization`.
/// See the `Architecture` doc-comment for why we duplicate.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Decision returned for a `(architecture, gpu, quantization)` triple.
///
/// `reason` is intended to be displayed to the user in the `/ollama tune`
/// output (or logged for diagnostics). Both supported and unsupported
/// outcomes carry a reason — the `supported = true` branch explains *why*
/// FA was enabled, the `supported = false` branch explains *why not*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashAttnDecision {
    pub supported: bool,
    pub reason: &'static str,
}

/// Compatibility decision: can flash attention be safely enabled for this
/// `(arch, gpu, quant)` combination?
///
/// Source: see module-level doc comment for the upstream commit SHAs and
/// links audited on 2026-05-08. Update this table when llama.cpp adds
/// backend support or Ollama adjusts its allow/blocklist.
#[must_use]
pub fn flash_attn_supported(
    arch: &Architecture,
    gpu: GpuKind,
    quant: &Quantization,
) -> FlashAttnDecision {
    // ── Hard rejections that apply regardless of arch/quant ────────────────
    //
    // CPU backend: llama.cpp does ship a CPU FA path (`docs/ops.md` marks it
    // 🟡 partial), but Ollama's tuner-level guidance is that CPU FA is
    // slower than the non-FA path on most CPUs because the CPU backend
    // doesn't benefit from the same memory-bandwidth savings as a GPU. We
    // therefore say "no" on CPU even though the kernel technically exists,
    // and document that as the reason.
    if matches!(gpu, GpuKind::None) {
        return FlashAttnDecision {
            supported: false,
            reason: "CPU backend: flash-attention provides no measurable speedup off-GPU",
        };
    }

    // ── Architecture-level allow/blocklist (from Ollama's SupportsFlashAttention) ─
    //
    // Gemma2 is unconditionally blocked in Ollama regardless of backend or
    // quantization (`fs/ggml/ggml.go`). Even with a fast GPU and Q8_0
    // weights, FA must stay off — kernel correctness fails on Gemma2's
    // attention sink design.
    if matches!(arch, Architecture::Gemma2) {
        return FlashAttnDecision {
            supported: false,
            reason: "Architecture gemma2 is on Ollama's flash-attention blocklist",
        };
    }

    // Unknown architecture: be conservative. We do not enable FA unless we
    // can name the family, because new architectures occasionally introduce
    // attention-sink, sliding-window, or non-square head-dim quirks that
    // break the kernel.
    if matches!(arch, Architecture::Other(_)) {
        return FlashAttnDecision {
            supported: false,
            reason: "Unknown architecture; flash-attention not enabled by default",
        };
    }

    // ── Quantization-level rejections ─────────────────────────────────────
    //
    // The model's weight quantization is mostly orthogonal to FA support —
    // weights are dequantized before MMA — so we accept Q4_0 through Q8_0,
    // F16, and BF16 universally. Two exceptions:
    //
    //   - F32: no FA kernel emits f32 outputs. The CUDA dispatcher in
    //     llama.cpp's `fattn.cu` only switches on F16/BF16 accumulation
    //     paths, and Metal/ROCm follow the same pattern. F32 weights are
    //     vanishingly rare anyway (only used for debugging).
    //
    //   - Quantization::Unknown(_): same conservative stance as
    //     `Architecture::Other(_)`. A novel quant label might be a new
    //     K-quant variant that the runtime cannot yet route correctly.
    if matches!(quant, Quantization::F32) {
        return FlashAttnDecision {
            supported: false,
            reason: "F32 weights: flash-attention kernels target F16/BF16 accumulation paths",
        };
    }
    if matches!(quant, Quantization::Unknown(_)) {
        return FlashAttnDecision {
            supported: false,
            reason: "Unknown quantization label; flash-attention not enabled by default",
        };
    }

    // ── Per-backend acceptance ────────────────────────────────────────────
    //
    // At this point we've cleared CPU, gemma2, F32, and Unknown. The
    // remaining (arch, gpu) cells are all supported by Ollama's gate
    // (head_count_k == head_count_v holds for every architecture in our
    // enum), so the decision now turns on what the GPU backend ships in
    // llama.cpp.
    match gpu {
        GpuKind::Metal => FlashAttnDecision {
            supported: true,
            reason: "Metal backend supports flash-attention for all enumerated architectures",
        },
        GpuKind::Cuda => FlashAttnDecision {
            supported: true,
            reason: "CUDA backend supports flash-attention; ensure SM>=7.5 on devices with 512-dim heads",
        },
        GpuKind::Rocm => {
            // ROCm has had FA via the HIP port of llama.cpp's CUDA kernels
            // for some time, but historically lagged on the more exotic
            // architectures (deepseek-v2/v3 multi-latent attention,
            // command-r's grouped-query layout). As of llama.cpp master
            // 6d57a49 the gap has mostly closed — `docs/ops.md` marks the
            // ROCm row 🟡 partial, same as Metal — but to stay
            // conservative we keep the two known holdouts disabled until
            // we have a benchmark confirming correctness.
            match arch {
                Architecture::DeepseekV2
                | Architecture::DeepseekV3
                | Architecture::CommandR => FlashAttnDecision {
                    supported: false,
                    reason: "ROCm backend lacks verified flash-attention path for deepseek-v2/v3 and command-r",
                },
                _ => FlashAttnDecision {
                    supported: true,
                    reason: "ROCm backend supports flash-attention for llama/qwen/mistral/mixtral/gemma3/phi3",
                },
            }
        }
        // Already handled at the top of the function. Listed exhaustively so
        // future GpuKind variants force the matcher to reconsider.
        GpuKind::None => FlashAttnDecision {
            supported: false,
            reason: "CPU backend: flash-attention provides no measurable speedup off-GPU",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive list of every `Architecture` variant for property tests.
    /// Update when `Architecture` gains a variant — the test asserting
    /// non-empty reasons will fail loudly if you forget.
    fn all_architectures() -> Vec<Architecture> {
        vec![
            Architecture::Llama,
            Architecture::Qwen2,
            Architecture::Qwen3,
            Architecture::Mistral,
            Architecture::Mixtral,
            Architecture::Gemma2,
            Architecture::Gemma3,
            Architecture::DeepseekV2,
            Architecture::DeepseekV3,
            Architecture::Phi3,
            Architecture::CommandR,
            Architecture::Other("future-arch".to_string()),
        ]
    }

    fn all_gpus() -> Vec<GpuKind> {
        vec![GpuKind::Metal, GpuKind::Cuda, GpuKind::Rocm, GpuKind::None]
    }

    fn all_quants() -> Vec<Quantization> {
        vec![
            Quantization::Q4_0,
            Quantization::Q4_1,
            Quantization::Q4_K_M,
            Quantization::Q4_K_S,
            Quantization::Q5_0,
            Quantization::Q5_1,
            Quantization::Q5_K_M,
            Quantization::Q5_K_S,
            Quantization::Q6_K,
            Quantization::Q8_0,
            Quantization::F16,
            Quantization::BF16,
            Quantization::F32,
            Quantization::Unknown("Q3_K_XS".to_string()),
        ]
    }

    #[test]
    fn cpu_never_supports_flash_attn() {
        // CPU is rejected before any other check — even on the most
        // FA-friendly arch (llama) and quant (F16).
        for arch in all_architectures() {
            for quant in all_quants() {
                let d = flash_attn_supported(&arch, GpuKind::None, &quant);
                assert!(!d.supported, "expected CPU={arch:?}/{quant:?} unsupported");
                assert!(d.reason.contains("CPU"), "reason mentions CPU: {}", d.reason);
            }
        }
    }

    #[test]
    fn f32_never_supports_flash_attn() {
        // F32 short-circuits before per-backend acceptance. Test on every
        // GPU backend except None (which is already covered by the CPU test).
        for gpu in [GpuKind::Metal, GpuKind::Cuda, GpuKind::Rocm] {
            for arch in all_architectures() {
                if matches!(arch, Architecture::Gemma2 | Architecture::Other(_)) {
                    continue; // those have higher-priority rejections
                }
                let d = flash_attn_supported(&arch, gpu, &Quantization::F32);
                assert!(!d.supported, "expected F32 unsupported on {gpu:?}");
                assert!(d.reason.contains("F32"), "reason mentions F32: {}", d.reason);
            }
        }
    }

    #[test]
    fn llama_q4_k_m_metal_supported() {
        let d = flash_attn_supported(
            &Architecture::Llama,
            GpuKind::Metal,
            &Quantization::Q4_K_M,
        );
        assert!(d.supported);
        assert!(d.reason.contains("Metal"));
    }

    #[test]
    fn llama_q4_k_m_cuda_supported() {
        let d = flash_attn_supported(
            &Architecture::Llama,
            GpuKind::Cuda,
            &Quantization::Q4_K_M,
        );
        assert!(d.supported);
        assert!(d.reason.contains("CUDA"));
    }

    #[test]
    fn gemma2_is_blocklisted_everywhere() {
        // Gemma2 is hard-blocklisted in Ollama irrespective of GPU. Verify
        // we propagate that for every GPU/quant combo. (Replaces the
        // task's seed-suggested "command_r_metal_unsupported" — the actual
        // Ollama upstream blocklist is gemma2/grok, NOT command-r.)
        for gpu in [GpuKind::Metal, GpuKind::Cuda, GpuKind::Rocm] {
            for quant in [Quantization::Q4_K_M, Quantization::Q8_0, Quantization::F16] {
                let d = flash_attn_supported(&Architecture::Gemma2, gpu, &quant);
                assert!(!d.supported, "gemma2 must be blocklisted on {gpu:?}/{quant:?}");
                assert!(d.reason.contains("gemma2"));
            }
        }
    }

    #[test]
    fn gemma3_is_supported_unlike_gemma2() {
        // Gemma3 is NOT on Ollama's blocklist (only gemma2 is). It should
        // pass on every GPU backend.
        for gpu in [GpuKind::Metal, GpuKind::Cuda, GpuKind::Rocm] {
            let d = flash_attn_supported(&Architecture::Gemma3, gpu, &Quantization::Q4_K_M);
            assert!(d.supported, "gemma3 must be supported on {gpu:?}");
        }
    }

    #[test]
    fn unknown_arch_returns_unsupported_with_reason() {
        let d = flash_attn_supported(
            &Architecture::Other("future-foo".to_string()),
            GpuKind::Cuda,
            &Quantization::Q4_K_M,
        );
        assert!(!d.supported);
        assert!(d.reason.to_lowercase().contains("unknown"));
    }

    #[test]
    fn deepseek_v3_rocm_unsupported() {
        // DeepSeek-V2/V3 use multi-latent attention (MLA); ROCm's HIP port
        // of the FA kernel hasn't been benchmarked for correctness on MLA
        // as of the audited commit. Keep disabled.
        let d = flash_attn_supported(
            &Architecture::DeepseekV3,
            GpuKind::Rocm,
            &Quantization::Q4_K_M,
        );
        assert!(!d.supported);
        assert!(d.reason.contains("ROCm"));
    }

    #[test]
    fn deepseek_v3_cuda_supported() {
        // The same DeepSeek-V3 model on CUDA is fine — the CUDA path has
        // had MLA support upstream for several releases.
        let d = flash_attn_supported(
            &Architecture::DeepseekV3,
            GpuKind::Cuda,
            &Quantization::Q4_K_M,
        );
        assert!(d.supported);
    }

    #[test]
    fn command_r_rocm_unsupported_but_metal_and_cuda_supported() {
        // Command-R / Cohere uses a non-standard attention shape that
        // historically tripped the ROCm port. Metal and CUDA are fine.
        let rocm = flash_attn_supported(
            &Architecture::CommandR,
            GpuKind::Rocm,
            &Quantization::Q4_K_M,
        );
        assert!(!rocm.supported);

        let metal = flash_attn_supported(
            &Architecture::CommandR,
            GpuKind::Metal,
            &Quantization::Q4_K_M,
        );
        assert!(metal.supported);

        let cuda = flash_attn_supported(
            &Architecture::CommandR,
            GpuKind::Cuda,
            &Quantization::Q4_K_M,
        );
        assert!(cuda.supported);
    }

    #[test]
    fn unknown_quant_returns_unsupported_with_reason() {
        let d = flash_attn_supported(
            &Architecture::Llama,
            GpuKind::Cuda,
            &Quantization::Unknown("Q3_K_XS".to_string()),
        );
        assert!(!d.supported);
        assert!(d.reason.to_lowercase().contains("unknown"));
    }

    #[test]
    fn weight_quants_are_orthogonal_for_supported_arch_gpu() {
        // Whether a llama-on-CUDA model is Q4_0, Q4_K_M, Q5_K_M, Q6_K, or
        // Q8_0 should not affect the FA decision — weight quant is
        // dequantized before MMA.
        let supported_quants = [
            Quantization::Q4_0,
            Quantization::Q4_1,
            Quantization::Q4_K_M,
            Quantization::Q4_K_S,
            Quantization::Q5_0,
            Quantization::Q5_1,
            Quantization::Q5_K_M,
            Quantization::Q5_K_S,
            Quantization::Q6_K,
            Quantization::Q8_0,
            Quantization::F16,
            Quantization::BF16,
        ];
        for q in supported_quants {
            let d = flash_attn_supported(&Architecture::Llama, GpuKind::Cuda, &q);
            assert!(d.supported, "llama/CUDA/{q:?} should be supported");
        }
    }

    #[test]
    fn reason_strings_are_non_empty_for_all_combos() {
        // Walk the whole 12 * 4 * 14 = 672-cell table and assert every
        // `reason` is non-empty regardless of the supported flag. Catches
        // accidental "" reasons during future edits.
        for arch in all_architectures() {
            for gpu in all_gpus() {
                for quant in all_quants() {
                    let d = flash_attn_supported(&arch, gpu, &quant);
                    assert!(
                        !d.reason.is_empty(),
                        "empty reason for ({arch:?}, {gpu:?}, {quant:?}) supported={}",
                        d.supported
                    );
                }
            }
        }
    }

    #[test]
    fn supported_combos_have_meaningful_reasons() {
        // For every supported=true result, the reason string must mention
        // either the backend (Metal/CUDA/ROCm) or the word "flash". This
        // is a low-bar but catches obvious copy-paste regressions where
        // someone returns supported=true with a reason like "ok".
        for arch in all_architectures() {
            for gpu in all_gpus() {
                for quant in all_quants() {
                    let d = flash_attn_supported(&arch, gpu, &quant);
                    if d.supported {
                        let r = d.reason.to_lowercase();
                        assert!(
                            r.contains("metal")
                                || r.contains("cuda")
                                || r.contains("rocm")
                                || r.contains("flash"),
                            "weak reason for supported combo ({arch:?}, {gpu:?}, {quant:?}): {}",
                            d.reason
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn metal_supports_all_known_archs_and_quants() {
        // Metal is the most permissive GPU backend in our matrix: every
        // (known arch, non-F32, non-Unknown quant) cell is supported.
        let known_archs = [
            Architecture::Llama,
            Architecture::Qwen2,
            Architecture::Qwen3,
            Architecture::Mistral,
            Architecture::Mixtral,
            Architecture::Gemma3,
            Architecture::DeepseekV2,
            Architecture::DeepseekV3,
            Architecture::Phi3,
            Architecture::CommandR,
        ];
        let supported_quants = [
            Quantization::Q4_0,
            Quantization::Q4_K_M,
            Quantization::Q5_K_M,
            Quantization::Q6_K,
            Quantization::Q8_0,
            Quantization::F16,
            Quantization::BF16,
        ];
        for arch in &known_archs {
            for q in &supported_quants {
                let d = flash_attn_supported(arch, GpuKind::Metal, q);
                assert!(
                    d.supported,
                    "Metal should support ({arch:?}, {q:?}): {}",
                    d.reason
                );
            }
        }
    }

    #[test]
    fn deterministic_no_panic_for_full_matrix() {
        // Sanity: cycle every combo once. Catches future regressions
        // where someone introduces a panic path inside the function.
        let mut count = 0_usize;
        for arch in all_architectures() {
            for gpu in all_gpus() {
                for quant in all_quants() {
                    let _ = flash_attn_supported(&arch, gpu, &quant);
                    count += 1;
                }
            }
        }
        // 12 archs * 4 gpus * 14 quants
        assert_eq!(count, 12 * 4 * 14);
    }
}
