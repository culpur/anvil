//! Ollama tuner — hardware-aware Ollama parameter selection.
//!
//! The `hw` submodule introspects the host (RAM, GPU, CPU features) and
//! exposes a [`hw::HardwareProfile`] consumed downstream to pick `num_gpu`,
//! `num_ctx`, `flash_attention`, etc. for individual Ollama models.

pub mod flash_attn;
pub mod hw;
