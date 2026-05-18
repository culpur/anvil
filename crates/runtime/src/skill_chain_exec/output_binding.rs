//! Bind one node's outputs to another's inputs.
//!
//! Substitutes `{{ <path> }}` references inside input strings with values
//! pulled from a scope.  v0.1 only supports string substitution: each
//! upstream node exposes a single `outputs.body` value (its captured
//! stdout).  When an upstream node is skipped or failed, its outputs
//! resolve to `null` and any binding referencing them returns
//! [`BindError::UpstreamUnavailable`] so the executor can mark the
//! downstream node as `Skipped`.

use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::skill_chain_exec::expr::{Scope, Value};

/// Errors surfaced when binding outputs into inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// Reference path was syntactically wrong (e.g. unclosed `{{`).
    Malformed(String),
    /// Reference path was not in the scope at all.
    UnknownReference(String),
    /// Reference path resolved to `Null` (upstream failed/skipped).
    UpstreamUnavailable(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(s) => write!(f, "binding: malformed reference `{s}`"),
            Self::UnknownReference(p) => write!(f, "binding: unknown reference `{p}`"),
            Self::UpstreamUnavailable(p) => {
                write!(f, "binding: upstream `{p}` unavailable")
            }
        }
    }
}

impl std::error::Error for BindError {}

/// Bind a single input string against `scope`, substituting `{{ path }}`
/// references with the scope value.  Literal strings (no `{{`) pass through.
pub fn bind_input(template: &str, scope: &Scope) -> Result<String, BindError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let close = after_open
            .find("}}")
            .ok_or_else(|| BindError::Malformed(rest.to_string()))?;
        let path = after_open[..close].trim();
        let value = scope
            .get(path)
            .ok_or_else(|| BindError::UnknownReference(path.to_string()))?;
        match value {
            Value::String(s) => out.push_str(s),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Null => return Err(BindError::UpstreamUnavailable(path.to_string())),
        }
        rest = &after_open[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Bind every value in a `BTreeMap<String, String>` input bag.
///
/// Returns the first binding error encountered.
pub fn bind_inputs(
    inputs: &BTreeMap<String, String>,
    scope: &Scope,
) -> Result<HashMap<String, String>, BindError> {
    let mut out = HashMap::with_capacity(inputs.len());
    for (k, v) in inputs {
        out.insert(k.clone(), bind_input(v, scope)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope_with(body: &str) -> Scope {
        let mut s = Scope::new();
        s.insert(
            "fetch.outputs.body".to_string(),
            Value::String(body.to_string()),
        );
        s
    }

    #[test]
    fn substitutes_a_single_reference() {
        let scope = scope_with("the page body");
        assert_eq!(
            bind_input("Summarize: {{ fetch.outputs.body }}", &scope).unwrap(),
            "Summarize: the page body"
        );
    }

    #[test]
    fn substitutes_multiple_references() {
        let mut s = Scope::new();
        s.insert("a.outputs.body".to_string(), Value::String("A".to_string()));
        s.insert("b.outputs.body".to_string(), Value::String("B".to_string()));
        assert_eq!(
            bind_input("{{ a.outputs.body }}|{{ b.outputs.body }}", &s).unwrap(),
            "A|B"
        );
    }

    #[test]
    fn literal_passes_through() {
        let scope = scope_with("ignored");
        assert_eq!(bind_input("static value", &scope).unwrap(), "static value");
    }

    #[test]
    fn unknown_reference_errors() {
        let scope = scope_with("body");
        let err = bind_input("{{ ghost.outputs.body }}", &scope).unwrap_err();
        assert!(matches!(err, BindError::UnknownReference(ref p) if p == "ghost.outputs.body"));
    }

    #[test]
    fn upstream_null_errors() {
        let mut s = Scope::new();
        s.insert("dead.outputs.body".to_string(), Value::Null);
        let err = bind_input("x: {{ dead.outputs.body }}", &s).unwrap_err();
        assert!(matches!(err, BindError::UpstreamUnavailable(_)));
    }

    #[test]
    fn unclosed_brace_errors() {
        let scope = scope_with("body");
        let err = bind_input("hi {{ fetch.outputs.body", &scope).unwrap_err();
        assert!(matches!(err, BindError::Malformed(_)));
    }

    #[test]
    fn bind_inputs_round_trips_every_key() {
        let scope = scope_with("BODY");
        let mut inputs = BTreeMap::new();
        inputs.insert("text".to_string(), "{{ fetch.outputs.body }}".to_string());
        inputs.insert("prefix".to_string(), "static".to_string());
        let bound = bind_inputs(&inputs, &scope).unwrap();
        assert_eq!(bound.get("text").unwrap(), "BODY");
        assert_eq!(bound.get("prefix").unwrap(), "static");
    }
}
