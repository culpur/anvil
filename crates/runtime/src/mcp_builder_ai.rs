//! AI-assisted MCP spec generation (task #679).
//!
//! Takes a free-form natural-language description, sends it through the
//! active session's model, and parses the response into an
//! `AiSpec`-shaped JSON.  The CLI side converts `AiSpec` into the
//! crate-local `McpBuilderSpec` (which carries the `PathBuf
//! output_dir`) and surfaces validation errors back to the wizard.
//!
//! ## Design split
//!
//! - `build_system_prompt` — pure, returns the string instructing the
//!   model to emit STRICTLY JSON matching the schema below.  Tests
//!   verify it contains the right shape.
//! - `parse_spec` — pure, takes a raw model response, strips optional
//!   markdown code fences, runs `serde_json::from_str` to an
//!   intermediate `AiSpec`, then validates kebab/snake-case
//!   constraints + depth caps.  Returns an actionable error string on
//!   failure; the wizard renders the error in a modal and offers
//!   "Try again" or "Switch to manual".
//!
//! The output of `parse_spec` is an `AiSpec` value — the CLI converts
//! that into its `McpBuilderSpec` by attaching the user-chosen
//! `output_dir` from the wizard's earlier prompts.
//!
//! ## Schema the model must emit
//!
//! ```json
//! {
//!   "name": "kebab-case-name",
//!   "language": "node" | "typescript" | "python",
//!   "tools": [
//!     {
//!       "name": "snake_case_tool",
//!       "description": "...",
//!       "inputs": [
//!         {
//!           "name": "snake_case_input",
//!           "description": "...",
//!           "kind": <kind>,
//!           "optional": false
//!         }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! `<kind>` is one of:
//!
//! ```text
//! {"type": "string"}
//! {"type": "number"}
//! {"type": "boolean"}
//! {"type": "enum", "values": ["a", "b"]}
//! {"type": "array", "items": <kind>}
//! {"type": "object", "fields": [{"name": "...", "kind": <kind>, "optional": false}, ...]}
//! ```
//!
//! The parser caps nesting at depth 4 (i.e. the wizard's manual path
//! caps object-of-object at 2 levels; arrays + objects can nest up to
//! depth 4 total combined).  Beyond that the model is told to flatten.

use serde::Deserialize;

/// Maximum nesting depth for `Array`/`Object` schemas.  Matches the
/// wizard's manual cap of "objects nested 2 levels deep", plus a
/// little headroom for `array<object<array<string>>>`-style legitimate
/// shapes.
const MAX_KIND_DEPTH: usize = 4;

/// Build the system prompt that instructs the model to emit a strict
/// JSON spec matching `AiSpec`.  Pure — easy to unit-test.
#[must_use]
pub fn build_system_prompt() -> String {
    r#"You are the MCP-Builder spec synthesizer for Anvil.

Your sole job: read the user's natural-language description of an MCP
server they want to build, and emit STRICTLY a single JSON object — no
prose, no commentary, no markdown code fences. Just the JSON object.

The JSON object MUST match this schema exactly:

{
  "name": "<kebab-case project name, 3..32 chars, [a-z0-9-], no leading digit, no double or trailing hyphen>",
  "language": "node" | "typescript" | "python",
  "tools": [
    {
      "name": "<snake_case identifier, [a-z][a-z0-9_]*, max 48 chars>",
      "description": "<one-sentence English description of what the tool does>",
      "inputs": [
        {
          "name": "<snake_case identifier>",
          "description": "<short English description>",
          "kind": <kind>,
          "optional": false
        }
      ]
    }
  ]
}

Where <kind> is exactly one of:

  {"type": "string"}
  {"type": "number"}
  {"type": "boolean"}
  {"type": "enum", "values": ["literal1", "literal2"]}
  {"type": "array", "items": <kind>}
  {"type": "object", "fields": [{"name": "...", "kind": <kind>, "optional": false}, ...]}

Constraints:
- Use "node" unless the user explicitly says "TypeScript" or "Python".
- Names: project = kebab-case; tool + input + object-field = snake_case.
- Keep nesting shallow — at most 4 levels of array/object combined.
- "tools" must have at least 1 entry; each tool must declare at least 0 inputs (an empty array is fine).
- Do NOT include extra top-level keys. Do NOT include "output_dir".
- Do NOT wrap the JSON in markdown fences. Emit ONLY the bare JSON object.

If the user request is vague, pick reasonable defaults and emit a
working spec — never ask follow-up questions.
"#
    .to_string()
}

/// Intermediate AI-emitted spec.  This is the shape the model produces
/// per `build_system_prompt`; the CLI wraps it into its own
/// `McpBuilderSpec` by attaching the wizard-chosen `output_dir`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AiSpec {
    pub name: String,
    pub language: AiLanguage,
    pub tools: Vec<AiToolSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiLanguage {
    Node,
    Typescript,
    Python,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AiToolSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<AiToolInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AiToolInput {
    pub name: String,
    pub description: String,
    pub kind: AiInputKind,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AiInputKind {
    String,
    Number,
    Boolean,
    Enum { values: Vec<String> },
    Array { items: Box<AiInputKind> },
    Object {
        #[serde(default)]
        fields: Vec<AiObjectField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AiObjectField {
    pub name: String,
    pub kind: AiInputKind,
    #[serde(default)]
    pub optional: bool,
}

/// Parse a raw model response into an `AiSpec`.
///
/// Steps:
///
/// 1. Trim leading/trailing whitespace.
/// 2. Strip wrapping `` ```json `` / `` ``` `` fences if present.
/// 3. `serde_json::from_str` into `AiSpec`.
/// 4. Validate names (kebab/snake-case), depth cap, non-empty tools.
///
/// Returns a human-readable error string on any failure — these strings
/// land in the wizard's error modal so they should be actionable.
pub fn parse_spec(raw: &str) -> Result<AiSpec, String> {
    let stripped = strip_code_fences(raw.trim());
    if stripped.is_empty() {
        return Err("model returned an empty response".into());
    }
    let spec: AiSpec = serde_json::from_str(stripped)
        .map_err(|e| format!("model output was not valid JSON: {e}"))?;
    validate_spec(&spec)?;
    Ok(spec)
}

/// Strip optional ` ```json ... ``` ` or ` ``` ... ``` ` fences a chatty
/// model might add despite the system prompt saying not to.
fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    // Try ```json first, then bare ```.
    for opener in ["```json", "```JSON", "```Json", "```"] {
        if let Some(rest) = s.strip_prefix(opener) {
            let rest = rest.trim_start_matches('\n').trim_start();
            if let Some(inner) = rest.strip_suffix("```") {
                return inner.trim();
            }
            // Opener present, closer missing — return the body anyway,
            // serde will produce a parse error the wizard can show.
            return rest.trim();
        }
    }
    s
}

fn validate_spec(spec: &AiSpec) -> Result<(), String> {
    validate_kebab(&spec.name, "project name")?;
    if spec.tools.is_empty() {
        return Err("spec has zero tools — model must emit at least one".into());
    }
    let mut seen_tools = std::collections::BTreeSet::new();
    for tool in &spec.tools {
        validate_snake(&tool.name, "tool name")?;
        if tool.description.trim().is_empty() {
            return Err(format!("tool '{}' has empty description", tool.name));
        }
        if !seen_tools.insert(tool.name.clone()) {
            return Err(format!("duplicate tool name '{}'", tool.name));
        }
        let mut seen_inputs = std::collections::BTreeSet::new();
        for input in &tool.inputs {
            validate_snake(&input.name, "input name")?;
            if input.description.trim().is_empty() {
                return Err(format!(
                    "input '{}' on tool '{}' has empty description",
                    input.name, tool.name
                ));
            }
            if !seen_inputs.insert(input.name.clone()) {
                return Err(format!(
                    "duplicate input name '{}' on tool '{}'",
                    input.name, tool.name
                ));
            }
            validate_kind(&input.kind, 1)?;
        }
    }
    Ok(())
}

fn validate_kind(kind: &AiInputKind, depth: usize) -> Result<(), String> {
    if depth > MAX_KIND_DEPTH {
        return Err(format!(
            "schema nests deeper than {MAX_KIND_DEPTH} levels — flatten it"
        ));
    }
    match kind {
        AiInputKind::String | AiInputKind::Number | AiInputKind::Boolean => Ok(()),
        AiInputKind::Enum { values } => {
            if values.is_empty() {
                return Err("enum kind must have at least one value".into());
            }
            for v in values {
                if v.trim().is_empty() {
                    return Err("enum has an empty-string value".into());
                }
            }
            Ok(())
        }
        AiInputKind::Array { items } => validate_kind(items, depth + 1),
        AiInputKind::Object { fields } => {
            let mut seen = std::collections::BTreeSet::new();
            for f in fields {
                validate_snake(&f.name, "object field name")?;
                if !seen.insert(f.name.clone()) {
                    return Err(format!("duplicate field name '{}'", f.name));
                }
                validate_kind(&f.kind, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn validate_kebab(s: &str, role: &str) -> Result<(), String> {
    if s.len() < 3 {
        return Err(format!("{role} '{s}' is shorter than 3 characters"));
    }
    if s.len() > 32 {
        return Err(format!("{role} '{s}' is longer than 32 characters"));
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err(format!("{role} '{s}' must start with a lowercase letter")),
    }
    let mut prev_hyphen = false;
    for c in chars {
        match c {
            'a'..='z' | '0'..='9' => prev_hyphen = false,
            '-' => {
                if prev_hyphen {
                    return Err(format!("{role} '{s}' has consecutive hyphens"));
                }
                prev_hyphen = true;
            }
            _ => {
                return Err(format!(
                    "{role} '{s}' has invalid character {c:?} (kebab-case only)"
                ));
            }
        }
    }
    if s.ends_with('-') {
        return Err(format!("{role} '{s}' ends with a hyphen"));
    }
    Ok(())
}

fn validate_snake(s: &str, role: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{role} is empty"));
    }
    if s.len() > 48 {
        return Err(format!("{role} '{s}' is longer than 48 characters"));
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err(format!("{role} '{s}' must start with a lowercase letter")),
    }
    for c in chars {
        match c {
            'a'..='z' | '0'..='9' | '_' => {}
            _ => {
                return Err(format!(
                    "{role} '{s}' has invalid character {c:?} (snake_case only)"
                ));
            }
        }
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn well_formed_json() -> &'static str {
        r#"{
            "name": "weather-api",
            "language": "node",
            "tools": [
                {
                    "name": "get_forecast",
                    "description": "Return the forecast for a city",
                    "inputs": [
                        {"name": "city", "description": "City name", "kind": {"type": "string"}, "optional": false},
                        {"name": "units", "description": "Unit system", "kind": {"type": "enum", "values": ["metric", "imperial"]}, "optional": true}
                    ]
                }
            ]
        }"#
    }

    #[test]
    fn system_prompt_contains_schema_keywords() {
        let p = build_system_prompt();
        // The prompt must mention each required JSON key + each kind
        // variant so the model knows the shape.
        for needle in [
            "\"name\"",
            "\"language\"",
            "\"tools\"",
            "\"inputs\"",
            "\"kind\"",
            "\"type\": \"string\"",
            "\"type\": \"number\"",
            "\"type\": \"boolean\"",
            "\"type\": \"enum\"",
            "\"type\": \"array\"",
            "\"type\": \"object\"",
            "kebab-case",
            "snake_case",
        ] {
            assert!(
                p.contains(needle),
                "system prompt missing keyword {needle:?}"
            );
        }
    }

    #[test]
    fn parse_spec_accepts_well_formed_json() {
        let spec = parse_spec(well_formed_json()).expect("well-formed json");
        assert_eq!(spec.name, "weather-api");
        assert_eq!(spec.language, AiLanguage::Node);
        assert_eq!(spec.tools.len(), 1);
        assert_eq!(spec.tools[0].name, "get_forecast");
        assert_eq!(spec.tools[0].inputs.len(), 2);
        match &spec.tools[0].inputs[1].kind {
            AiInputKind::Enum { values } => {
                assert_eq!(values, &vec!["metric".to_string(), "imperial".to_string()])
            }
            other => panic!("expected Enum, got {other:?}"),
        }
    }

    #[test]
    fn parse_spec_strips_markdown_json_fences() {
        let raw = format!("```json\n{}\n```", well_formed_json());
        let spec = parse_spec(&raw).expect("fenced json");
        assert_eq!(spec.name, "weather-api");
    }

    #[test]
    fn parse_spec_strips_bare_markdown_fences() {
        let raw = format!("```\n{}\n```", well_formed_json());
        let spec = parse_spec(&raw).expect("bare-fenced json");
        assert_eq!(spec.name, "weather-api");
    }

    #[test]
    fn parse_spec_rejects_empty_input() {
        let err = parse_spec("").unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
        let err2 = parse_spec("   \n  \n").unwrap_err();
        assert!(err2.contains("empty"), "got: {err2}");
    }

    #[test]
    fn parse_spec_rejects_invalid_json() {
        let err = parse_spec("not valid json at all").unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_missing_required_field() {
        // No "language" field.
        let raw = r#"{"name": "foo-bar", "tools": [{"name":"x","description":"y","inputs":[]}]}"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_invalid_kebab_name() {
        let raw = r#"{
            "name": "WeatherAPI",
            "language": "node",
            "tools": [{"name":"x","description":"y","inputs":[]}]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(
            err.contains("project name") && err.contains("lowercase"),
            "got: {err}",
        );

        let raw = r#"{
            "name": "ab",
            "language": "node",
            "tools": [{"name":"x","description":"y","inputs":[]}]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("shorter than 3"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_invalid_snake_tool_name() {
        let raw = r#"{
            "name": "foo-bar",
            "language": "node",
            "tools": [{"name":"GetForecast","description":"y","inputs":[]}]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("tool name") && err.contains("lowercase"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_zero_tools() {
        let raw = r#"{"name":"foo-bar","language":"node","tools":[]}"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("zero tools"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_duplicate_tool_names() {
        let raw = r#"{
            "name": "foo-bar",
            "language": "node",
            "tools": [
                {"name":"alpha","description":"a","inputs":[]},
                {"name":"alpha","description":"b","inputs":[]}
            ]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("duplicate tool"), "got: {err}");
    }

    #[test]
    fn parse_spec_rejects_nesting_beyond_cap() {
        // array<array<array<array<array<string>>>>> — depth 5, exceeds MAX_KIND_DEPTH = 4.
        let raw = r#"{
            "name": "foo-bar",
            "language": "node",
            "tools": [{
                "name":"x",
                "description":"y",
                "inputs":[{
                    "name":"deep",
                    "description":"too deep",
                    "kind": {"type":"array","items":{"type":"array","items":{"type":"array","items":{"type":"array","items":{"type":"array","items":{"type":"string"}}}}}},
                    "optional": false
                }]
            }]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("deeper than"), "got: {err}");
    }

    #[test]
    fn parse_spec_accepts_array_of_object_of_array_of_string_within_cap() {
        let raw = r#"{
            "name": "foo-bar",
            "language": "python",
            "tools": [{
                "name":"x",
                "description":"y",
                "inputs":[{
                    "name":"shape",
                    "description":"nested but legal",
                    "kind":{"type":"array","items":{"type":"object","fields":[{"name":"vals","kind":{"type":"array","items":{"type":"string"}},"optional":false}]}},
                    "optional": false
                }]
            }]
        }"#;
        let spec = parse_spec(raw).expect("depth=4 nesting must be accepted");
        assert_eq!(spec.language, AiLanguage::Python);
    }

    #[test]
    fn parse_spec_rejects_enum_with_no_values() {
        let raw = r#"{
            "name": "foo-bar",
            "language": "node",
            "tools": [{
                "name":"x",
                "description":"y",
                "inputs":[{
                    "name":"k",
                    "description":"d",
                    "kind":{"type":"enum","values":[]},
                    "optional": false
                }]
            }]
        }"#;
        let err = parse_spec(raw).unwrap_err();
        assert!(err.contains("enum"), "got: {err}");
    }

    #[test]
    fn parse_spec_handles_typescript_lowercase() {
        let raw = r#"{
            "name": "foo-bar",
            "language": "typescript",
            "tools": [{"name":"x","description":"y","inputs":[]}]
        }"#;
        let spec = parse_spec(raw).expect("typescript language");
        assert_eq!(spec.language, AiLanguage::Typescript);
    }
}
