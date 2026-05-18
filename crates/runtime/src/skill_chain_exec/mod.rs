//! Anvil-side skill-chain execution engine.
//!
//! AnvilHub Feature 2 — sub-track A.  This module reads `chain.yaml`
//! manifests and walks the resulting DAG, capturing per-node outputs and
//! surfacing partial failures.  The public API consumed by `commands::` and
//! the `anvil chain run` clap subcommand is intentionally small:
//!
//! - [`manifest::parse_manifest`] / [`manifest::read_manifest`] — load and
//!   validate a chain manifest from string or filesystem path.
//! - [`executor::execute_chain`] — run a parsed manifest with a supplied
//!   [`executor::NodeRunner`], returning a [`executor::ChainRunResult`].
//! - [`executor::StaticEchoRunner`] — v0.1 default runner that echoes its
//!   inputs.  Sub-track C will swap in a real skill-execution runner.
//!
//! ## Out-of-scope for sub-track A (deferred to other sub-tracks)
//!
//! - **B (passage backend)**: chain registry lookup is a no-op stub.
//! - **C (SKILL.md frontmatter)**: typed `inputs:`/`outputs:` blocks; the
//!   v0.1 binding model only supports `outputs.body` (captured stdout).
//! - **D (`/builder` UI)**: graph editor lives in anvilhub-web.
//! - **E (`/my-chains` + deep-link)**: install URL handler.
//!
//! ## OTel events
//!
//! Every `execute_chain` invocation emits:
//! - `anvil.chain.start` { slug, version, nodes }
//! - `anvil.chain.node_complete` { slug, node, status } per node
//! - `anvil.chain.end` { slug, ok, duration_ms }

pub mod executor;
pub mod expr;
pub mod manifest;
pub mod output_binding;

pub use executor::{
    execute_chain, topo_sort, ChainExecError, ChainRunResult, NodeRunner, NodeStatus,
    StaticEchoRunner,
};
pub use manifest::{
    parse_manifest, read_manifest, validate_manifest, ChainEdge, ChainManifest,
    ChainManifestError, ChainNode, EXPECTED_API_VERSION,
};
