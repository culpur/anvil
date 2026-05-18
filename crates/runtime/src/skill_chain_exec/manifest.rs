//! Chain manifest parsing and validation.
//!
//! `chain.yaml` v0.1 schema:
//!
//! ```yaml
//! apiVersion: anvil.culpur.net/chain/v1
//! slug: example-chain
//! version: 0.1.0
//! description: An example skill chain
//! nodes:
//!   - id: fetch
//!     skill: web-fetch@1.2.3
//!     inputs:
//!       url: "https://example.com"
//!   - id: summarize
//!     skill: text-summarize@2.0.0
//!     inputs:
//!       text: "{{ fetch.outputs.body }}"
//!     when: "len(fetch.outputs.body) > 100"
//! edges:
//!   - from: fetch
//!     to: summarize
//! ```
//!
//! Validation:
//! - All required fields present (`apiVersion`, `slug`, `version`, `nodes`)
//! - `apiVersion` matches `anvil.culpur.net/chain/v1`
//! - Node ids are unique and non-empty
//! - Edges only reference declared node ids
//! - No cycles (verified by [`crate::skill_chain_exec::executor`])

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Top-level chain manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub slug: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub nodes: Vec<ChainNode>,
    #[serde(default)]
    pub edges: Vec<ChainEdge>,
}

/// One node in the chain DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainNode {
    pub id: String,
    pub skill: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// A directed edge `from -> to` between two declared node ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEdge {
    pub from: String,
    pub to: String,
}

/// Parse errors for chain manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainManifestError {
    /// YAML deserialisation failure.
    Yaml(String),
    /// Required field missing or empty.
    MissingField(&'static str),
    /// Unsupported `apiVersion` value.
    UnsupportedApiVersion(String),
    /// Duplicate node `id` within the manifest.
    DuplicateNodeId(String),
    /// Edge references an unknown node id.
    UnknownNodeRef { edge_from: String, edge_to: String, missing: String },
    /// Node id (or other identifier) contains invalid characters.
    InvalidIdentifier(String),
}

impl std::fmt::Display for ChainManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Yaml(msg) => write!(f, "chain manifest: YAML parse error: {msg}"),
            Self::MissingField(name) => write!(f, "chain manifest: missing or empty field `{name}`"),
            Self::UnsupportedApiVersion(v) => write!(
                f,
                "chain manifest: unsupported apiVersion `{v}` (expected `{EXPECTED_API_VERSION}`)"
            ),
            Self::DuplicateNodeId(id) => write!(f, "chain manifest: duplicate node id `{id}`"),
            Self::UnknownNodeRef { edge_from, edge_to, missing } => write!(
                f,
                "chain manifest: edge {edge_from} -> {edge_to} references unknown node `{missing}`"
            ),
            Self::InvalidIdentifier(id) => write!(
                f,
                "chain manifest: invalid identifier `{id}` (must match [a-zA-Z][a-zA-Z0-9_-]*)"
            ),
        }
    }
}

impl std::error::Error for ChainManifestError {}

/// The single supported manifest apiVersion for v0.1.
pub const EXPECTED_API_VERSION: &str = "anvil.culpur.net/chain/v1";

/// Parse a YAML string into a validated [`ChainManifest`].
///
/// Performs full validation: required fields, apiVersion, duplicate ids,
/// edge references.  Cycle detection lives in
/// [`crate::skill_chain_exec::executor::topo_sort`].
pub fn parse_manifest(yaml: &str) -> Result<ChainManifest, ChainManifestError> {
    let manifest: ChainManifest =
        serde_yaml::from_str(yaml).map_err(|e| ChainManifestError::Yaml(e.to_string()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Read and parse a manifest from disk.
pub fn read_manifest(path: &Path) -> Result<ChainManifest, ChainManifestError> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| ChainManifestError::Yaml(format!("read {}: {e}", path.display())))?;
    parse_manifest(&body)
}

/// Validate a parsed manifest.  Pure function; no I/O.
pub fn validate_manifest(manifest: &ChainManifest) -> Result<(), ChainManifestError> {
    if manifest.api_version != EXPECTED_API_VERSION {
        return Err(ChainManifestError::UnsupportedApiVersion(
            manifest.api_version.clone(),
        ));
    }
    if manifest.slug.trim().is_empty() {
        return Err(ChainManifestError::MissingField("slug"));
    }
    if !is_valid_identifier(&manifest.slug) {
        return Err(ChainManifestError::InvalidIdentifier(manifest.slug.clone()));
    }
    if manifest.version.trim().is_empty() {
        return Err(ChainManifestError::MissingField("version"));
    }
    if manifest.nodes.is_empty() {
        return Err(ChainManifestError::MissingField("nodes"));
    }

    let mut seen = std::collections::HashSet::new();
    for node in &manifest.nodes {
        if node.id.trim().is_empty() {
            return Err(ChainManifestError::MissingField("nodes[*].id"));
        }
        if !is_valid_identifier(&node.id) {
            return Err(ChainManifestError::InvalidIdentifier(node.id.clone()));
        }
        if node.skill.trim().is_empty() {
            return Err(ChainManifestError::MissingField("nodes[*].skill"));
        }
        if !seen.insert(node.id.clone()) {
            return Err(ChainManifestError::DuplicateNodeId(node.id.clone()));
        }
    }

    for edge in &manifest.edges {
        if !seen.contains(&edge.from) {
            return Err(ChainManifestError::UnknownNodeRef {
                edge_from: edge.from.clone(),
                edge_to: edge.to.clone(),
                missing: edge.from.clone(),
            });
        }
        if !seen.contains(&edge.to) {
            return Err(ChainManifestError::UnknownNodeRef {
                edge_from: edge.from.clone(),
                edge_to: edge.to.clone(),
                missing: edge.to.clone(),
            });
        }
    }
    Ok(())
}

/// Identifier validation: must start with [a-zA-Z], then [a-zA-Z0-9_-]*.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        None => return false,
        Some(c) if !c.is_ascii_alphabetic() => return false,
        Some(_) => {}
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
apiVersion: anvil.culpur.net/chain/v1
slug: example-chain
version: 0.1.0
description: An example
nodes:
  - id: fetch
    skill: web-fetch@1.2.3
    inputs:
      url: "https://example.com"
  - id: summarize
    skill: text-summarize@2.0.0
    inputs:
      text: "{{ fetch.outputs.body }}"
    when: "len(fetch.outputs.body) > 100"
edges:
  - from: fetch
    to: summarize
"#;

    #[test]
    fn parses_a_valid_manifest() {
        let m = parse_manifest(VALID).expect("valid manifest");
        assert_eq!(m.slug, "example-chain");
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.nodes[0].id, "fetch");
        assert_eq!(m.nodes[1].when.as_deref(), Some("len(fetch.outputs.body) > 100"));
    }

    #[test]
    fn rejects_unknown_api_version() {
        let yaml = r#"
apiVersion: anvil.culpur.net/chain/v999
slug: x
version: 0.1.0
nodes: [{id: a, skill: y@1}]
"#;
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::UnsupportedApiVersion(_)));
    }

    #[test]
    fn rejects_duplicate_node_ids() {
        let yaml = r#"
apiVersion: anvil.culpur.net/chain/v1
slug: dup
version: 0.1.0
nodes:
  - id: a
    skill: s@1
  - id: a
    skill: s@2
"#;
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::DuplicateNodeId(ref id) if id == "a"));
    }

    #[test]
    fn rejects_unknown_edge_ref() {
        let yaml = r#"
apiVersion: anvil.culpur.net/chain/v1
slug: orphan
version: 0.1.0
nodes:
  - id: a
    skill: s@1
edges:
  - from: a
    to: ghost
"#;
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::UnknownNodeRef { ref missing, .. } if missing == "ghost"));
    }

    #[test]
    fn rejects_empty_nodes() {
        let yaml = r#"
apiVersion: anvil.culpur.net/chain/v1
slug: empty
version: 0.1.0
nodes: []
"#;
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::MissingField("nodes")));
    }

    #[test]
    fn rejects_invalid_slug() {
        let yaml = r#"
apiVersion: anvil.culpur.net/chain/v1
slug: "1bad"
version: 0.1.0
nodes:
  - id: a
    skill: s@1
"#;
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::InvalidIdentifier(_)));
    }

    #[test]
    fn rejects_garbage_yaml() {
        let yaml = "::: not yaml :::";
        let err = parse_manifest(yaml).unwrap_err();
        assert!(matches!(err, ChainManifestError::Yaml(_)));
    }

    #[test]
    fn identifier_validation_accepts_typical_slugs() {
        assert!(is_valid_identifier("fetch"));
        assert!(is_valid_identifier("text-summarize"));
        assert!(is_valid_identifier("a"));
        assert!(is_valid_identifier("snake_case"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("1leading-digit"));
        assert!(!is_valid_identifier("has space"));
        assert!(!is_valid_identifier("has.dot"));
    }
}
