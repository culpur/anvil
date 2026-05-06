use plugins::HookSpec;

use crate::json::JsonValue;

use super::helpers::expect_object;
use super::ConfigError;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeHookConfig {
    pub(super) pre_tool_use: Vec<HookSpec>,
    pub(super) post_tool_use: Vec<HookSpec>,
}

impl RuntimeHookConfig {
    #[must_use]
    pub const fn new(pre_tool_use: Vec<HookSpec>, post_tool_use: Vec<HookSpec>) -> Self {
        Self {
            pre_tool_use,
            post_tool_use,
        }
    }

    /// Convenience constructor that accepts plain strings (backward-compat).
    #[must_use]
    pub fn from_commands(
        pre_tool_use: Vec<String>,
        post_tool_use: Vec<String>,
    ) -> Self {
        Self {
            pre_tool_use: pre_tool_use.into_iter().map(HookSpec::Command).collect(),
            post_tool_use: post_tool_use.into_iter().map(HookSpec::Command).collect(),
        }
    }

    #[must_use]
    pub fn pre_tool_use(&self) -> &[HookSpec] {
        &self.pre_tool_use
    }

    #[must_use]
    pub fn post_tool_use(&self) -> &[HookSpec] {
        &self.post_tool_use
    }

    #[must_use]
    pub fn merged(&self, other: &Self) -> Self {
        let mut merged = self.clone();
        merged.extend(other);
        merged
    }

    pub fn extend(&mut self, other: &Self) {
        extend_unique(&mut self.pre_tool_use, other.pre_tool_use());
        extend_unique(&mut self.post_tool_use, other.post_tool_use());
    }
}

pub fn parse_optional_hooks_config(root: &JsonValue) -> Result<RuntimeHookConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimeHookConfig::default());
    };
    let Some(hooks_value) = object.get("hooks") else {
        return Ok(RuntimeHookConfig::default());
    };
    let hooks = expect_object(hooks_value, "merged settings.hooks")?;
    Ok(RuntimeHookConfig {
        pre_tool_use: parse_hook_spec_array(hooks, "PreToolUse", "merged settings.hooks")?,
        post_tool_use: parse_hook_spec_array(hooks, "PostToolUse", "merged settings.hooks")?,
    })
}

/// Parse a JSON array that may contain bare strings or tagged hook-spec objects.
///
/// Bare strings become `HookSpec::Command`.  Tagged objects (e.g.
/// `{"type":"prompt","body":"..."}`) deserialize via `serde_json` into their
/// respective `HookSpec` variant.
///
/// Partial-tolerance: a single malformed entry is logged to stderr and
/// skipped rather than aborting the entire array.  This matches Claude
/// Code's settings.json parsing behavior — one bad hook should not wipe
/// out every other valid hook in the same array.
fn parse_hook_spec_array(
    object: &std::collections::BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Vec<HookSpec>, ConfigError> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigError::Parse(format!(
            "{context}: field {key} must be an array"
        )));
    };
    // Re-serialize each element through serde_json so the existing
    // HookSpec serde(untagged) logic handles both bare strings and tagged objects.
    let mut hooks = Vec::with_capacity(array.len());
    for item in array {
        // Convert our internal JsonValue to a serde_json::Value via render().
        let json_str = item.render();
        match serde_json::from_str::<HookSpec>(&json_str) {
            Ok(spec) => hooks.push(spec),
            Err(error) => {
                eprintln!(
                    "anvil: skipping malformed hook entry in {context}.{key}: {error}"
                );
            }
        }
    }
    Ok(hooks)
}

fn extend_unique(target: &mut Vec<HookSpec>, values: &[HookSpec]) {
    for value in values {
        push_unique(target, value.clone());
    }
}

fn push_unique(target: &mut Vec<HookSpec>, value: HookSpec) {
    if !target.iter().any(|existing| existing == &value) {
        target.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// BUG-34/35 parity: a malformed entry sandwiched between two
    /// valid entries must not invalidate the whole array.  The two
    /// valid hooks survive, the bad one is dropped with a stderr
    /// warning.
    #[test]
    fn parse_hook_spec_array_keeps_valid_entries_around_a_malformed_one() {
        let mut object = BTreeMap::new();
        // First entry: valid bare-string command.
        // Second entry: number — neither a bare string nor a tagged
        // object, so HookSpec deserialization will fail.
        // Third entry: valid bare-string command.
        let parsed = JsonValue::parse(
            r#"{"PreToolUse":["./hooks/pre.sh", 12345, "./hooks/also-pre.sh"]}"#,
        )
        .expect("seed JSON parses");
        for (key, value) in parsed.as_object().expect("object root") {
            object.insert(key.clone(), value.clone());
        }

        let hooks = parse_hook_spec_array(&object, "PreToolUse", "test")
            .expect("tolerant parse should not error");
        assert_eq!(hooks.len(), 2, "two valid hooks should survive");
        assert_eq!(hooks[0], HookSpec::Command("./hooks/pre.sh".to_string()));
        assert_eq!(
            hooks[1],
            HookSpec::Command("./hooks/also-pre.sh".to_string())
        );
    }
}
