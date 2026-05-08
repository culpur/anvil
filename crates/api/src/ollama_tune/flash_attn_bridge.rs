//! Thin mapping layer between `api::ModelMeta` and the duplicate
//! `Architecture` / `Quantization` enums declared inside
//! `runtime::ollama_tune::flash_attn`.
//!
//! The flash-attention module duplicates those enums to avoid an
//! `api` ↔ `runtime` dependency cycle. The tuner needs both the api
//! version (carried by `ModelMeta`) and the flash-attention matcher,
//! so we collapse the difference here. Tuner callers see exactly one
//! function: [`flash_attn_supported_for_meta`].

use runtime::ollama_tune::flash_attn::{
    flash_attn_supported, Architecture as FaArch, FlashAttnDecision, Quantization as FaQuant,
};
use runtime::ollama_tune::hw::GpuKind;

use crate::providers::ollama_show::{Architecture, ModelMeta, Quantization};

/// Map an `api::Architecture` to the equivalent variant used by the
/// flash-attention matcher in `runtime`.
fn map_arch(arch: &Architecture) -> FaArch {
    match arch {
        Architecture::Llama => FaArch::Llama,
        Architecture::Qwen2 => FaArch::Qwen2,
        Architecture::Qwen3 => FaArch::Qwen3,
        Architecture::Mistral => FaArch::Mistral,
        Architecture::Mixtral => FaArch::Mixtral,
        Architecture::Gemma2 => FaArch::Gemma2,
        Architecture::Gemma3 => FaArch::Gemma3,
        Architecture::DeepseekV2 => FaArch::DeepseekV2,
        Architecture::DeepseekV3 => FaArch::DeepseekV3,
        Architecture::Phi3 => FaArch::Phi3,
        Architecture::CommandR => FaArch::CommandR,
        Architecture::Other(s) => FaArch::Other(s.clone()),
    }
}

/// Map an `api::Quantization` to the equivalent variant used by the
/// flash-attention matcher in `runtime`.
fn map_quant(quant: &Quantization) -> FaQuant {
    match quant {
        Quantization::Q4_0 => FaQuant::Q4_0,
        Quantization::Q4_1 => FaQuant::Q4_1,
        Quantization::Q4_K_M => FaQuant::Q4_K_M,
        Quantization::Q4_K_S => FaQuant::Q4_K_S,
        Quantization::Q5_0 => FaQuant::Q5_0,
        Quantization::Q5_1 => FaQuant::Q5_1,
        Quantization::Q5_K_M => FaQuant::Q5_K_M,
        Quantization::Q5_K_S => FaQuant::Q5_K_S,
        Quantization::Q6_K => FaQuant::Q6_K,
        Quantization::Q8_0 => FaQuant::Q8_0,
        Quantization::F16 => FaQuant::F16,
        Quantization::BF16 => FaQuant::BF16,
        Quantization::F32 => FaQuant::F32,
        Quantization::Unknown(s) => FaQuant::Unknown(s.clone()),
    }
}

/// Bridge wrapper used by the tuner. Accepts the api types directly,
/// converts them to the duplicate enums local to `flash_attn`, and
/// returns the matcher's decision unchanged.
#[must_use]
pub fn flash_attn_supported_for_meta(meta: &ModelMeta, gpu: GpuKind) -> FlashAttnDecision {
    let arch = map_arch(&meta.architecture);
    let quant = map_quant(&meta.quantization);
    flash_attn_supported(&arch, gpu, &quant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ollama_show::ModelMeta;

    fn meta(arch: Architecture, quant: Quantization) -> ModelMeta {
        ModelMeta {
            name: "t".into(),
            modified_at: None,
            size_bytes: 0,
            parameter_size: "7B".into(),
            parameter_count: 7_000_000_000,
            quantization: quant,
            context_length: 4096,
            architecture: arch,
            layer_count: Some(32),
            head_count: Some(32),
            head_count_kv: Some(8),
            embedding_length: Some(4096),
            families: vec![],
            format: Some("gguf".into()),
        }
    }

    #[test]
    fn bridge_maps_llama_q4_metal_supported() {
        let m = meta(Architecture::Llama, Quantization::Q4_K_M);
        let d = flash_attn_supported_for_meta(&m, GpuKind::Metal);
        assert!(d.supported);
    }

    #[test]
    fn bridge_maps_gemma2_blocklisted() {
        let m = meta(Architecture::Gemma2, Quantization::Q4_K_M);
        let d = flash_attn_supported_for_meta(&m, GpuKind::Cuda);
        assert!(!d.supported);
        assert!(d.reason.contains("gemma2"));
    }

    #[test]
    fn bridge_maps_cpu_unsupported() {
        let m = meta(Architecture::Llama, Quantization::Q4_K_M);
        let d = flash_attn_supported_for_meta(&m, GpuKind::None);
        assert!(!d.supported);
    }

    #[test]
    fn bridge_maps_unknown_quant() {
        let m = meta(
            Architecture::Llama,
            Quantization::Unknown("Q3_K_XS".into()),
        );
        let d = flash_attn_supported_for_meta(&m, GpuKind::Cuda);
        assert!(!d.supported);
    }

    #[test]
    fn bridge_maps_other_arch() {
        let m = meta(
            Architecture::Other("future".into()),
            Quantization::Q4_K_M,
        );
        let d = flash_attn_supported_for_meta(&m, GpuKind::Cuda);
        assert!(!d.supported);
    }
}
