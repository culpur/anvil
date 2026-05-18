use plugins::HookSpec;

use crate::hooks::RuntimeHookSpec;
use crate::json::JsonValue;

use super::helpers::expect_object;
use super::ConfigError;

/// Phase 5.3 #19: `RuntimeHookConfig` now holds `Vec<RuntimeHookSpec>` instead
/// of `Vec<plugins::HookSpec>` so that `mcp_tool` hook entries can be parsed
/// from `settings.json` and dispatched by `HookRunner::collect_specs`.
///
/// Prior to this migration (Stream B), `mcp_tool` hooks were only reachable via
/// direct construction (`HookRunner::from_runtime_specs` or the `_extra` vectors).
/// Now a plugin can ship:
///   ```json
///   { "type": "mcp_tool", "server": "vault", "tool": "redact", "input": {} }
///   ```
/// inside any hook event array and have it dispatched correctly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeHookConfig {
    pub(super) pre_tool_use: Vec<RuntimeHookSpec>,
    pub(super) post_tool_use: Vec<RuntimeHookSpec>,
    // v2.2.11: new CC-parity event hooks.
    pub(super) session_start: Vec<RuntimeHookSpec>,
    pub(super) session_end: Vec<RuntimeHookSpec>,
    pub(super) file_changed: Vec<RuntimeHookSpec>,
    pub(super) cwd_changed: Vec<RuntimeHookSpec>,
    pub(super) permission_request: Vec<RuntimeHookSpec>,
    pub(super) permission_denied: Vec<RuntimeHookSpec>,
    pub(super) post_tool_batch: Vec<RuntimeHookSpec>,
    pub(super) notification: Vec<RuntimeHookSpec>,
    // Task #566: Stop hook event — fires at end-of-turn when no tool_use
    // blocks were emitted.  A hook returning `{"decision":"block"}` keeps
    // the turn loop running; any other / missing decision allows stop.
    pub(super) stop: Vec<RuntimeHookSpec>,
}

impl RuntimeHookConfig {
    /// Construct from explicit `HookSpec` lists (backward-compat API used by
    /// tests and `from_commands`).
    #[must_use]
    pub fn new(pre_tool_use: Vec<HookSpec>, post_tool_use: Vec<HookSpec>) -> Self {
        Self {
            pre_tool_use: pre_tool_use.into_iter().map(RuntimeHookSpec::Plugin).collect(),
            post_tool_use: post_tool_use.into_iter().map(RuntimeHookSpec::Plugin).collect(),
            session_start: Vec::new(),
            session_end: Vec::new(),
            file_changed: Vec::new(),
            cwd_changed: Vec::new(),
            permission_request: Vec::new(),
            permission_denied: Vec::new(),
            post_tool_batch: Vec::new(),
            notification: Vec::new(),
            stop: Vec::new(),
        }
    }

    /// Convenience constructor that accepts plain strings (backward-compat).
    #[must_use]
    pub fn from_commands(
        pre_tool_use: Vec<String>,
        post_tool_use: Vec<String>,
    ) -> Self {
        Self {
            pre_tool_use: pre_tool_use.into_iter().map(|s| RuntimeHookSpec::Plugin(HookSpec::Command(s))).collect(),
            post_tool_use: post_tool_use.into_iter().map(|s| RuntimeHookSpec::Plugin(HookSpec::Command(s))).collect(),
            session_start: Vec::new(),
            session_end: Vec::new(),
            file_changed: Vec::new(),
            cwd_changed: Vec::new(),
            permission_request: Vec::new(),
            permission_denied: Vec::new(),
            post_tool_batch: Vec::new(),
            notification: Vec::new(),
            stop: Vec::new(),
        }
    }

    #[must_use]
    pub fn pre_tool_use(&self) -> &[RuntimeHookSpec] {
        &self.pre_tool_use
    }

    #[must_use]
    pub fn post_tool_use(&self) -> &[RuntimeHookSpec] {
        &self.post_tool_use
    }

    /// v2.2.11: fires after config + MCP servers loaded, before first prompt.
    #[must_use]
    pub fn session_start(&self) -> &[RuntimeHookSpec] {
        &self.session_start
    }

    /// v2.2.11: fires on clean exit.
    #[must_use]
    pub fn session_end(&self) -> &[RuntimeHookSpec] {
        &self.session_end
    }

    /// v2.2.11: fires after Edit/Write/MultiEdit tool succeeds.
    #[must_use]
    pub fn file_changed(&self) -> &[RuntimeHookSpec] {
        &self.file_changed
    }

    /// v2.2.11: fires when cwd changes mid-session.
    #[must_use]
    pub fn cwd_changed(&self) -> &[RuntimeHookSpec] {
        &self.cwd_changed
    }

    /// v2.2.11: fires when permission prompt is about to be shown.
    #[must_use]
    pub fn permission_request(&self) -> &[RuntimeHookSpec] {
        &self.permission_request
    }

    /// v2.2.11: fires after a tool call is denied.
    #[must_use]
    pub fn permission_denied(&self) -> &[RuntimeHookSpec] {
        &self.permission_denied
    }

    /// v2.2.11: fires once per parallel tool batch.
    #[must_use]
    pub fn post_tool_batch(&self) -> &[RuntimeHookSpec] {
        &self.post_tool_batch
    }

    /// v2.2.11: fires when Anvil displays a notification to the user.
    #[must_use]
    pub fn notification(&self) -> &[RuntimeHookSpec] {
        &self.notification
    }

    /// Task #566: fires at end of an assistant message with no tool_use
    /// blocks (i.e. the turn loop is about to return control).  A hook
    /// returning `{"decision":"block","reason":"..."}` keeps the turn
    /// running.
    #[must_use]
    pub fn stop(&self) -> &[RuntimeHookSpec] {
        &self.stop
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
        extend_unique(&mut self.session_start, other.session_start());
        extend_unique(&mut self.session_end, other.session_end());
        extend_unique(&mut self.file_changed, other.file_changed());
        extend_unique(&mut self.cwd_changed, other.cwd_changed());
        extend_unique(&mut self.permission_request, other.permission_request());
        extend_unique(&mut self.permission_denied, other.permission_denied());
        extend_unique(&mut self.post_tool_batch, other.post_tool_batch());
        extend_unique(&mut self.notification, other.notification());
        extend_unique(&mut self.stop, other.stop());
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
        pre_tool_use: parse_runtime_hook_spec_array(hooks, "PreToolUse", "merged settings.hooks")?,
        post_tool_use: parse_runtime_hook_spec_array(hooks, "PostToolUse", "merged settings.hooks")?,
        // v2.2.11: new CC-parity event hooks.
        session_start: parse_runtime_hook_spec_array(hooks, "SessionStart", "merged settings.hooks")?,
        session_end: parse_runtime_hook_spec_array(hooks, "SessionEnd", "merged settings.hooks")?,
        file_changed: parse_runtime_hook_spec_array(hooks, "FileChanged", "merged settings.hooks")?,
        cwd_changed: parse_runtime_hook_spec_array(hooks, "CwdChanged", "merged settings.hooks")?,
        permission_request: parse_runtime_hook_spec_array(
            hooks,
            "PermissionRequest",
            "merged settings.hooks",
        )?,
        permission_denied: parse_runtime_hook_spec_array(
            hooks,
            "PermissionDenied",
            "merged settings.hooks",
        )?,
        post_tool_batch: parse_runtime_hook_spec_array(hooks, "PostToolBatch", "merged settings.hooks")?,
        notification: parse_runtime_hook_spec_array(hooks, "Notification", "merged settings.hooks")?,
        stop: parse_runtime_hook_spec_array(hooks, "Stop", "merged settings.hooks")?,
    })
}

/// Parse a JSON array that may contain bare strings, tagged hook-spec objects
/// (`Command`, `Prompt`, `Exec`), or runtime-only `mcp_tool` objects.
///
/// Uses `RuntimeHookSpec::from_json_value` so that `mcp_tool` entries are
/// accepted and dispatched correctly (Phase 5.3 #19 fix).
///
/// Partial-tolerance: a single malformed entry is logged to stderr and
/// skipped rather than aborting the entire array.
fn parse_runtime_hook_spec_array(
    object: &std::collections::BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Vec<RuntimeHookSpec>, ConfigError> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigError::Parse(format!(
            "{context}: field {key} must be an array"
        )));
    };
    let mut hooks = Vec::with_capacity(array.len());
    for item in array {
        // Re-serialize to serde_json::Value so RuntimeHookSpec::from_json_value
        // can work with the full serde type system.
        let json_str = item.render();
        match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(val) => match RuntimeHookSpec::from_json_value(&val) {
                Some(spec) => hooks.push(spec),
                None => {
                    // from_json_value already printed a warning.
                }
            },
            Err(error) => {
                eprintln!(
                    "anvil: skipping malformed hook entry in {context}.{key}: {error}"
                );
            }
        }
    }
    Ok(hooks)
}

fn extend_unique(target: &mut Vec<RuntimeHookSpec>, values: &[RuntimeHookSpec]) {
    for value in values {
        push_unique(target, value.clone());
    }
}

fn push_unique(target: &mut Vec<RuntimeHookSpec>, value: RuntimeHookSpec) {
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

        let hooks = parse_runtime_hook_spec_array(&object, "PreToolUse", "test")
            .expect("tolerant parse should not error");
        assert_eq!(hooks.len(), 2, "two valid hooks should survive");
        assert_eq!(
            hooks[0],
            RuntimeHookSpec::Plugin(HookSpec::Command("./hooks/pre.sh".to_string()))
        );
        assert_eq!(
            hooks[1],
            RuntimeHookSpec::Plugin(HookSpec::Command("./hooks/also-pre.sh".to_string()))
        );
    }

    /// Phase 5.3 #19: verify that an mcp_tool hook entry in settings.json is
    /// parsed correctly into a RuntimeHookSpec::McpTool variant.
    #[test]
    fn parse_hook_spec_array_accepts_mcp_tool_entries() {
        let mut object = BTreeMap::new();
        let parsed = JsonValue::parse(
            r#"{"PreToolUse":[
                "./hooks/pre.sh",
                {"type":"mcp_tool","server":"vault","tool":"redact","input":{"k":"v"}}
            ]}"#,
        )
        .expect("seed JSON parses");
        for (key, value) in parsed.as_object().expect("object root") {
            object.insert(key.clone(), value.clone());
        }

        let hooks = parse_runtime_hook_spec_array(&object, "PreToolUse", "test")
            .expect("tolerant parse should not error");
        assert_eq!(hooks.len(), 2, "both entries should survive");

        // First: Command hook.
        assert_eq!(
            hooks[0],
            RuntimeHookSpec::Plugin(HookSpec::Command("./hooks/pre.sh".to_string()))
        );

        // Second: McpTool hook.
        match &hooks[1] {
            RuntimeHookSpec::McpTool { server, tool, input } => {
                assert_eq!(server, "vault");
                assert_eq!(tool, "redact");
                assert_eq!(input, &serde_json::json!({"k": "v"}));
            }
            other => panic!("expected McpTool variant, got: {other:?}"),
        }
    }

    /// Phase 5.3 #19: mcp_tool entry missing required fields should be skipped.
    #[test]
    fn mcp_tool_entry_without_server_is_skipped() {
        let mut object = BTreeMap::new();
        let parsed = JsonValue::parse(
            r#"{"PreToolUse":[
                {"type":"mcp_tool","tool":"redact"},
                "./hooks/good.sh"
            ]}"#,
        )
        .expect("seed JSON parses");
        for (key, value) in parsed.as_object().expect("object root") {
            object.insert(key.clone(), value.clone());
        }

        let hooks = parse_runtime_hook_spec_array(&object, "PreToolUse", "test")
            .expect("tolerant parse should not error");
        // The mcp_tool entry without server should be dropped; the command survives.
        assert_eq!(hooks.len(), 1, "malformed mcp_tool must be skipped");
        assert_eq!(
            hooks[0],
            RuntimeHookSpec::Plugin(HookSpec::Command("./hooks/good.sh".to_string()))
        );
    }
}
