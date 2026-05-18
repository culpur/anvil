//! DAG walker for the skill-chain executor.
//!
//! Responsibilities:
//! - Build a node-id → adjacency map from the manifest's `edges` plus the
//!   implicit ordering produced by `{{ ref.outputs.* }}` bindings.
//! - Topologically sort with cycle detection.
//! - For each node:
//!   * Evaluate `when:` if present (skip on false).
//!   * Bind every input from upstream outputs (skip on upstream null).
//!   * Hand off to a [`NodeRunner`] callback.
//!   * Capture stdout as `<node-id>.outputs.body`.
//!   * Mark downstream-of-failure nodes as `Skipped` with the upstream error.
//! - Surface per-node status + final outputs as [`ChainRunResult`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Instant;

use serde::Serialize;

use crate::otel;
use crate::skill_chain_exec::expr::{self, Scope, Value};
use crate::skill_chain_exec::manifest::{ChainManifest, ChainNode};
use crate::skill_chain_exec::output_binding::{self, BindError};

/// Per-node execution status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeStatus {
    /// Node has not yet been visited.
    Pending,
    /// Node ran successfully; its captured output body is included.
    Success { body: String },
    /// Node skipped (because of `when: false` or upstream null).
    Skipped { reason: String },
    /// Node failed during execution; downstream nodes mark `Skipped`.
    Failed { error: String },
}

impl NodeStatus {
    /// `true` for `Success` only.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// Capture the body of a successful run; empty otherwise.
    #[must_use]
    pub fn body(&self) -> &str {
        match self {
            Self::Success { body } => body.as_str(),
            _ => "",
        }
    }
}

/// Final per-chain result.
#[derive(Debug, Clone, Serialize)]
pub struct ChainRunResult {
    pub slug: String,
    pub version: String,
    /// Per-node status keyed by node id, in topological order.
    pub nodes: Vec<(String, NodeStatus)>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
}

impl ChainRunResult {
    /// `true` when every node finished `Success`.  Skipped + Failed both
    /// count against this — the caller decides whether `Skipped` from
    /// `when: false` is acceptable.
    #[must_use]
    pub fn fully_successful(&self) -> bool {
        self.nodes.iter().all(|(_, s)| s.is_success())
    }

    /// `true` when at least one node ended in `Failed`.
    #[must_use]
    pub fn had_failure(&self) -> bool {
        self.nodes.iter().any(|(_, s)| matches!(s, NodeStatus::Failed { .. }))
    }

    /// Render a deterministic human-readable summary for TUI / CLI display.
    #[must_use]
    pub fn render_summary(&self) -> String {
        let mut lines = vec![format!(
            "Chain `{}` v{} — {} node(s) in {} ms",
            self.slug,
            self.version,
            self.nodes.len(),
            self.duration_ms,
        )];
        for (id, status) in &self.nodes {
            match status {
                NodeStatus::Success { body } => {
                    let trimmed = body.trim();
                    let preview = if trimmed.is_empty() {
                        "(empty)".to_string()
                    } else if trimmed.len() > 80 {
                        format!("{}…", &trimmed[..80])
                    } else {
                        trimmed.to_string()
                    };
                    lines.push(format!("  [ok]      {id} → {preview}"));
                }
                NodeStatus::Skipped { reason } => {
                    lines.push(format!("  [skip]    {id} — {reason}"));
                }
                NodeStatus::Failed { error } => {
                    lines.push(format!("  [fail]    {id} — {error}"));
                }
                NodeStatus::Pending => {
                    lines.push(format!("  [pending] {id}"));
                }
            }
        }
        lines.join("\n")
    }
}

/// Per-node invocation contract.
///
/// A runner receives the manifest's `ChainNode` plus the already-bound input
/// map and returns the captured stdout (= `outputs.body`).  Errors are
/// surfaced as the node's `Failed` status.
///
/// v0.1 ships a [`StaticEchoRunner`] that simply echoes its inputs.
/// Sub-track C will plug a real skill-execution runner here.
pub trait NodeRunner {
    /// Execute one node and return its captured stdout.
    fn run(
        &mut self,
        node: &ChainNode,
        inputs: &HashMap<String, String>,
    ) -> Result<String, String>;
}

/// Test/fallback runner: echoes a deterministic synthesis of inputs.
///
/// Output format: `node=<id> skill=<skill> inputs=<sorted k=v pairs>`.
///
/// This is the v0.1 default; sub-track C swaps in a real skill runner.
pub struct StaticEchoRunner;

impl NodeRunner for StaticEchoRunner {
    fn run(
        &mut self,
        node: &ChainNode,
        inputs: &HashMap<String, String>,
    ) -> Result<String, String> {
        let mut kvs: Vec<(&String, &String)> = inputs.iter().collect();
        kvs.sort_by(|a, b| a.0.cmp(b.0));
        let body = kvs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ");
        Ok(format!("node={} skill={} inputs={{{}}}", node.id, node.skill, body))
    }
}

/// Errors that prevent chain execution from even starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainExecError {
    /// Manifest contains a cycle — node ids in the cycle are listed.
    Cycle(Vec<String>),
}

impl std::fmt::Display for ChainExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cycle(ids) => {
                write!(f, "chain: cycle detected involving nodes: {}", ids.join(", "))
            }
        }
    }
}

impl std::error::Error for ChainExecError {}

/// Run a chain to completion using the supplied [`NodeRunner`].
///
/// Returns a [`ChainRunResult`] with per-node status.  Returns
/// [`ChainExecError::Cycle`] if the manifest contains a directed cycle.
///
/// OTel events emitted on each invocation:
/// - `anvil.chain.start` — once at the top
/// - `anvil.chain.node_complete` — once per node (status attribute)
/// - `anvil.chain.end` — once at the bottom
pub fn execute_chain<R: NodeRunner>(
    manifest: &ChainManifest,
    runner: &mut R,
) -> Result<ChainRunResult, ChainExecError> {
    otel::emit_event(
        "anvil.chain.start",
        &[
            ("slug", manifest.slug.as_str()),
            ("version", manifest.version.as_str()),
            ("nodes", &manifest.nodes.len().to_string()),
        ],
    );
    let start = Instant::now();

    let order = topo_sort(manifest)?;
    let nodes_by_id: HashMap<&str, &ChainNode> =
        manifest.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let upstream: HashMap<String, HashSet<String>> = upstream_map(manifest);

    let mut scope: Scope = Scope::new();
    let mut statuses: Vec<(String, NodeStatus)> = Vec::with_capacity(order.len());

    for node_id in &order {
        let node = nodes_by_id.get(node_id.as_str()).expect("node id resolved");

        // ── Skip if any upstream is not Success.
        let upstream_failure = upstream.get(node_id).and_then(|ups| {
            ups.iter().find_map(|u| {
                let prior = statuses.iter().find(|(id, _)| id == u);
                match prior {
                    Some((id, NodeStatus::Failed { .. })) => {
                        Some(format!("upstream {id} failed"))
                    }
                    Some((id, NodeStatus::Skipped { .. })) => {
                        Some(format!("upstream {id} skipped"))
                    }
                    _ => None,
                }
            })
        });
        if let Some(reason) = upstream_failure {
            let status = NodeStatus::Skipped { reason: reason.clone() };
            scope.insert(format!("{}.outputs.body", node_id), Value::Null);
            statuses.push((node_id.clone(), status));
            otel::emit_event(
                "anvil.chain.node_complete",
                &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "skipped")],
            );
            continue;
        }

        // ── Evaluate when:
        if let Some(when_src) = &node.when {
            match expr::evaluate(when_src, &scope) {
                Ok(true) => {}
                Ok(false) => {
                    scope.insert(format!("{}.outputs.body", node_id), Value::Null);
                    let status = NodeStatus::Skipped {
                        reason: format!("when: {when_src} → false"),
                    };
                    statuses.push((node_id.clone(), status));
                    otel::emit_event(
                        "anvil.chain.node_complete",
                        &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "skipped_when")],
                    );
                    continue;
                }
                Err(e) => {
                    let status = NodeStatus::Failed {
                        error: format!("when evaluation: {e}"),
                    };
                    scope.insert(format!("{}.outputs.body", node_id), Value::Null);
                    statuses.push((node_id.clone(), status));
                    otel::emit_event(
                        "anvil.chain.node_complete",
                        &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "failed_when")],
                    );
                    continue;
                }
            }
        }

        // ── Bind inputs.
        let bound = match output_binding::bind_inputs(&node.inputs, &scope) {
            Ok(b) => b,
            Err(BindError::UpstreamUnavailable(path)) => {
                let status = NodeStatus::Skipped {
                    reason: format!("upstream `{path}` unavailable"),
                };
                scope.insert(format!("{}.outputs.body", node_id), Value::Null);
                statuses.push((node_id.clone(), status));
                otel::emit_event(
                    "anvil.chain.node_complete",
                    &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "skipped_upstream")],
                );
                continue;
            }
            Err(e) => {
                let status = NodeStatus::Failed {
                    error: format!("input binding: {e}"),
                };
                scope.insert(format!("{}.outputs.body", node_id), Value::Null);
                statuses.push((node_id.clone(), status));
                otel::emit_event(
                    "anvil.chain.node_complete",
                    &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "failed_binding")],
                );
                continue;
            }
        };

        // ── Execute.
        match runner.run(node, &bound) {
            Ok(body) => {
                scope.insert(
                    format!("{}.outputs.body", node_id),
                    Value::String(body.clone()),
                );
                statuses.push((node_id.clone(), NodeStatus::Success { body }));
                otel::emit_event(
                    "anvil.chain.node_complete",
                    &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "success")],
                );
            }
            Err(error) => {
                scope.insert(format!("{}.outputs.body", node_id), Value::Null);
                statuses.push((node_id.clone(), NodeStatus::Failed { error }));
                otel::emit_event(
                    "anvil.chain.node_complete",
                    &[("slug", manifest.slug.as_str()), ("node", node_id.as_str()), ("status", "failed")],
                );
            }
        }
    }

    let duration_ms = start.elapsed().as_millis();
    let result = ChainRunResult {
        slug: manifest.slug.clone(),
        version: manifest.version.clone(),
        nodes: statuses,
        duration_ms,
    };
    otel::emit_event(
        "anvil.chain.end",
        &[
            ("slug", manifest.slug.as_str()),
            ("ok", if result.fully_successful() { "true" } else { "false" }),
            ("duration_ms", &duration_ms.to_string()),
        ],
    );
    Ok(result)
}

/// Build a `node_id -> set(upstream_ids)` map from explicit `edges` AND
/// implicit `{{ ref.outputs.* }}` references in input templates.
pub fn upstream_map(manifest: &ChainManifest) -> HashMap<String, HashSet<String>> {
    let mut up: HashMap<String, HashSet<String>> = HashMap::new();
    let known_ids: HashSet<&str> = manifest.nodes.iter().map(|n| n.id.as_str()).collect();
    for node in &manifest.nodes {
        up.entry(node.id.clone()).or_default();
        for value in node.inputs.values() {
            collect_refs(value, &known_ids, &mut |dep| {
                if dep != node.id {
                    up.entry(node.id.clone()).or_default().insert(dep.to_string());
                }
            });
        }
        if let Some(when) = &node.when {
            collect_refs(when, &known_ids, &mut |dep| {
                if dep != node.id {
                    up.entry(node.id.clone()).or_default().insert(dep.to_string());
                }
            });
        }
    }
    for edge in &manifest.edges {
        up.entry(edge.to.clone()).or_default().insert(edge.from.clone());
    }
    up
}

/// Walk every `{{ ref.outputs.<…> }}` and bare `<node>.outputs.<…>` reference
/// inside `text` and call `sink` with the node id portion when it matches a
/// declared node.
fn collect_refs(text: &str, known_ids: &HashSet<&str>, sink: &mut impl FnMut(&str)) {
    // {{ <node>.outputs.<…> }} bindings
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find("}}") {
            let inner = after[..close].trim();
            if let Some(node_id) = inner.split('.').next()
                && known_ids.contains(node_id)
            {
                sink(node_id);
            }
            rest = &after[close + 2..];
        } else {
            break;
        }
    }
    // bare references in `when:` expressions, e.g. `len(fetch.outputs.body)`
    for (i, c) in text.char_indices() {
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            let mut end = start + c.len_utf8();
            for (j, cc) in text[end..].char_indices() {
                if cc.is_ascii_alphanumeric() || cc == '_' || cc == '-' || cc == '.' {
                    end = start + c.len_utf8() + j + cc.len_utf8();
                } else {
                    break;
                }
            }
            let tok = &text[start..end];
            if let Some(node_id) = tok.split('.').next()
                && known_ids.contains(node_id)
                && tok.contains('.')
            {
                sink(node_id);
            }
        }
    }
}

/// Kahn topological sort.  Returns nodes in execution order or
/// [`ChainExecError::Cycle`] when impossible.
pub fn topo_sort(manifest: &ChainManifest) -> Result<Vec<String>, ChainExecError> {
    let upstream = upstream_map(manifest);
    // Compute in-degree (number of upstream parents not yet emitted).
    let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
    for node in &manifest.nodes {
        let ud = upstream.get(&node.id).map_or(0, HashSet::len);
        in_degree.insert(node.id.clone(), ud);
    }
    // children[parent] = set(downstream ids)
    let mut children: HashMap<String, HashSet<String>> = HashMap::new();
    for (child, parents) in &upstream {
        for p in parents {
            children.entry(p.clone()).or_default().insert(child.clone());
        }
    }

    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(k, _)| k.clone())
        .collect();
    let mut order = Vec::with_capacity(manifest.nodes.len());
    while let Some(n) = queue.pop_front() {
        order.push(n.clone());
        if let Some(kids) = children.get(&n) {
            for c in kids {
                if let Some(d) = in_degree.get_mut(c) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(c.clone());
                    }
                }
            }
        }
    }
    if order.len() != manifest.nodes.len() {
        let remaining: Vec<String> = in_degree
            .iter()
            .filter(|&(_, &d)| d > 0)
            .map(|(k, _)| k.clone())
            .collect();
        return Err(ChainExecError::Cycle(remaining));
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_chain_exec::manifest::{parse_manifest, ChainEdge, ChainNode};

    fn manifest_from(yaml: &str) -> ChainManifest {
        parse_manifest(yaml).expect("manifest")
    }

    fn linear() -> ChainManifest {
        manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: linear
version: 0.1.0
nodes:
  - id: a
    skill: noop@1
  - id: b
    skill: noop@1
    inputs:
      x: "{{ a.outputs.body }}"
  - id: c
    skill: noop@1
    inputs:
      x: "{{ b.outputs.body }}"
"#,
        )
    }

    fn diamond() -> ChainManifest {
        manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: diamond
version: 0.1.0
nodes:
  - id: root
    skill: noop@1
  - id: left
    skill: noop@1
    inputs: {x: "{{ root.outputs.body }}"}
  - id: right
    skill: noop@1
    inputs: {x: "{{ root.outputs.body }}"}
  - id: merge
    skill: noop@1
    inputs:
      l: "{{ left.outputs.body }}"
      r: "{{ right.outputs.body }}"
"#,
        )
    }

    #[test]
    fn topo_sort_linear_preserves_order() {
        let order = topo_sort(&linear()).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn topo_sort_diamond_puts_root_first_and_merge_last() {
        let order = topo_sort(&diamond()).unwrap();
        assert_eq!(order[0], "root");
        assert_eq!(order[3], "merge");
        // left and right can be in either order
        assert!(order[1..3].contains(&"left".to_string()));
        assert!(order[1..3].contains(&"right".to_string()));
    }

    #[test]
    fn topo_sort_multiple_roots() {
        let m = manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: multi-root
version: 0.1.0
nodes:
  - id: r1
    skill: noop@1
  - id: r2
    skill: noop@1
  - id: sink
    skill: noop@1
    inputs:
      a: "{{ r1.outputs.body }}"
      b: "{{ r2.outputs.body }}"
"#,
        );
        let order = topo_sort(&m).unwrap();
        assert_eq!(order.last().unwrap(), "sink");
        // r1, r2 must both come before sink
        let r1_idx = order.iter().position(|s| s == "r1").unwrap();
        let r2_idx = order.iter().position(|s| s == "r2").unwrap();
        let sink_idx = order.iter().position(|s| s == "sink").unwrap();
        assert!(r1_idx < sink_idx);
        assert!(r2_idx < sink_idx);
    }

    #[test]
    fn topo_sort_multiple_leaves() {
        let m = manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: multi-leaf
version: 0.1.0
nodes:
  - id: root
    skill: noop@1
  - id: left
    skill: noop@1
    inputs: {x: "{{ root.outputs.body }}"}
  - id: right
    skill: noop@1
    inputs: {x: "{{ root.outputs.body }}"}
"#,
        );
        let order = topo_sort(&m).unwrap();
        assert_eq!(order[0], "root");
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn topo_sort_detects_cycle_via_explicit_edges() {
        let mut m = manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: cycle
version: 0.1.0
nodes:
  - id: a
    skill: noop@1
  - id: b
    skill: noop@1
"#,
        );
        m.edges = vec![
            ChainEdge { from: "a".to_string(), to: "b".to_string() },
            ChainEdge { from: "b".to_string(), to: "a".to_string() },
        ];
        let err = topo_sort(&m).unwrap_err();
        assert!(matches!(err, ChainExecError::Cycle(_)));
    }

    #[test]
    fn execute_linear_runs_every_node() {
        let m = linear();
        let mut runner = StaticEchoRunner;
        let result = execute_chain(&m, &mut runner).unwrap();
        assert!(result.fully_successful(), "{result:#?}");
        assert_eq!(result.nodes.len(), 3);
        let (id_a, status_a) = &result.nodes[0];
        assert_eq!(id_a, "a");
        assert!(matches!(status_a, NodeStatus::Success { .. }));
        // c's input must contain b's output body
        let (id_c, status_c) = &result.nodes[2];
        assert_eq!(id_c, "c");
        let body_c = status_c.body();
        assert!(body_c.contains("node=c"), "{body_c}");
    }

    /// Partial-failure surfacing: when one node fails, only its downstream
    /// is skipped — independent siblings still run.
    #[test]
    fn partial_failure_skips_downstream_only() {
        let m = diamond();
        struct FailLeftRunner;
        impl NodeRunner for FailLeftRunner {
            fn run(
                &mut self,
                node: &ChainNode,
                _inputs: &HashMap<String, String>,
            ) -> Result<String, String> {
                if node.id == "left" {
                    Err("simulated failure".to_string())
                } else {
                    Ok(format!("body-of-{}", node.id))
                }
            }
        }

        let result = execute_chain(&m, &mut FailLeftRunner).unwrap();
        let by_id: HashMap<&str, &NodeStatus> = result
            .nodes
            .iter()
            .map(|(id, s)| (id.as_str(), s))
            .collect();
        assert!(matches!(by_id["root"], NodeStatus::Success { .. }));
        assert!(matches!(by_id["right"], NodeStatus::Success { .. }));
        assert!(matches!(by_id["left"], NodeStatus::Failed { .. }));
        // merge must be skipped because `left` failed
        match by_id["merge"] {
            NodeStatus::Skipped { reason } => {
                assert!(reason.contains("left"), "expected upstream-left reason: {reason}");
            }
            other => panic!("expected merge to be Skipped, got {other:?}"),
        }
    }

    /// when: false → node skipped; downstream sees null and is itself
    /// skipped via the upstream-unavailable path.
    #[test]
    fn when_false_skips_node_and_downstream() {
        let m = manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: when-gated
version: 0.1.0
nodes:
  - id: a
    skill: noop@1
  - id: b
    skill: noop@1
    inputs:
      x: "{{ a.outputs.body }}"
    when: "false"
  - id: c
    skill: noop@1
    inputs:
      y: "{{ b.outputs.body }}"
"#,
        );
        let result = execute_chain(&m, &mut StaticEchoRunner).unwrap();
        let by_id: HashMap<&str, &NodeStatus> = result
            .nodes
            .iter()
            .map(|(id, s)| (id.as_str(), s))
            .collect();
        assert!(matches!(by_id["a"], NodeStatus::Success { .. }));
        assert!(matches!(by_id["b"], NodeStatus::Skipped { .. }));
        assert!(matches!(by_id["c"], NodeStatus::Skipped { .. }));
    }

    /// when: true → node runs normally.
    #[test]
    fn when_true_runs_node() {
        let m = manifest_from(
            r#"
apiVersion: anvil.culpur.net/chain/v1
slug: when-on
version: 0.1.0
nodes:
  - id: a
    skill: noop@1
  - id: b
    skill: noop@1
    inputs:
      x: "{{ a.outputs.body }}"
    when: "len(a.outputs.body) > 0"
"#,
        );
        let result = execute_chain(&m, &mut StaticEchoRunner).unwrap();
        let by_id: HashMap<&str, &NodeStatus> = result
            .nodes
            .iter()
            .map(|(id, s)| (id.as_str(), s))
            .collect();
        assert!(matches!(by_id["a"], NodeStatus::Success { .. }));
        assert!(matches!(by_id["b"], NodeStatus::Success { .. }));
    }

    /// Render a summary string for human display.
    #[test]
    fn render_summary_includes_each_node_with_status() {
        let m = linear();
        let result = execute_chain(&m, &mut StaticEchoRunner).unwrap();
        let s = result.render_summary();
        assert!(s.contains("Chain `linear`"));
        assert!(s.contains("[ok]      a"));
        assert!(s.contains("[ok]      b"));
        assert!(s.contains("[ok]      c"));
    }
}
