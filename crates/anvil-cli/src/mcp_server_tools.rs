/// MCP server tool registry — maps MCP tool names to Anvil tool implementations.
///
/// Published MVP tools (9):
///   1. read_file    — file_ops
///   2. write_file   — file_ops, gated by --read-only
///   3. edit_file    — file_ops, gated by --read-only
///   4. glob_search  — search_ops
///   5. grep_search  — search_ops
///   6. bash         — runtime/bash, gated by --read-only
///   7. goal_status  — runtime/goals (GoalManager::active_goal)
///   8. vault_get    — vault_session, refuses if vault locked
///   9. vault_list   — vault, refuses if vault locked
///
/// Deferred to v2.4 (36 remaining MVP tools):
///   All other tools surfaced by mvp_tool_specs() — WebFetch, WebSearch, TodoWrite,
///   Skill, Agent, ToolSearch, NotebookEdit, Sleep, SendUserMessage, Config,
///   StructuredOutput, REPL, PowerShell, ListMcpResourcesTool, ReadMcpResourceTool,
///   LSPTool, RemoteTrigger, CronCreate, CronList, CronDelete, TeamCreate, TeamAddMember,
///   TeamRemoveMember, TeamList, TeamDelegate, TeamStatus, EnterWorktree, ExitWorktree,
///   EnterPlanMode, ExitPlanMode, SendMessage, TaskCreate, TaskGet, TaskList, TaskUpdate,
///   TaskOutput, TaskStop — deferred pending MCP security review and v2.4 surface expansion.

use runtime::{GoalManager, vault_is_session_unlocked, with_session_vault};
use serde_json::Value;
use tools::execute_tool;

/// Description of a tool published through the MCP server surface.
#[derive(Debug, Clone)]
pub struct McpServerTool {
    /// Tool name as seen by MCP clients.
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// JSON Schema for the tool's input.
    pub input_schema: Value,
    /// Whether this tool is blocked when the server is in read-only mode.
    pub write_gated: bool,
}

/// Return the 9 tools published for the MCP server surface.
///
/// Call sites filter this list based on `--read-only` and `--tools` flags.
#[must_use]
pub fn published_tools() -> Vec<McpServerTool> {
    use serde_json::json;
    vec![
        McpServerTool {
            name: "read_file",
            description: "Read a text file from the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            write_gated: false,
        },
        McpServerTool {
            name: "write_file",
            description: "Write a text file in the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            write_gated: true,
        },
        McpServerTool {
            name: "edit_file",
            description: "Replace text in a workspace file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }),
            write_gated: true,
        },
        McpServerTool {
            name: "glob_search",
            description: "Find files by glob pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            write_gated: false,
        },
        McpServerTool {
            name: "grep_search",
            description: "Search file contents with a regex pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "output_mode": { "type": "string" },
                    "-B": { "type": "integer", "minimum": 0 },
                    "-A": { "type": "integer", "minimum": 0 },
                    "-C": { "type": "integer", "minimum": 0 },
                    "context": { "type": "integer", "minimum": 0 },
                    "-n": { "type": "boolean" },
                    "-i": { "type": "boolean" },
                    "type": { "type": "string" },
                    "head_limit": { "type": "integer", "minimum": 1 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "multiline": { "type": "boolean" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            write_gated: false,
        },
        McpServerTool {
            name: "bash",
            description: "Execute a shell command in the current workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" },
                    "dangerouslyDisableSandbox": { "type": "boolean" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            write_gated: true,
        },
        McpServerTool {
            name: "goal_status",
            description: "Return the status of the current active goal.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            write_gated: false,
        },
        McpServerTool {
            name: "vault_get",
            description: "Retrieve a credential secret from the session vault by label.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string" }
                },
                "required": ["label"],
                "additionalProperties": false
            }),
            write_gated: false,
        },
        McpServerTool {
            name: "vault_list",
            description: "List all credential labels stored in the session vault.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            write_gated: false,
        },
    ]
}

/// Dispatch a `tools/call` to the correct Anvil implementation.
///
/// Returns `Ok(output)` on success.
/// Returns `Err(message)` on tool failure or access denial — callers should
/// set `isError: true` in the MCP response.
pub fn dispatch(name: &str, arguments: &Value) -> Result<String, String> {
    match name {
        "goal_status" => dispatch_goal_status(),
        "vault_get" => dispatch_vault_get(arguments),
        "vault_list" => dispatch_vault_list(),
        // All other published tools delegate directly to execute_tool.
        other => execute_tool(other, arguments),
    }
}

fn dispatch_goal_status() -> Result<String, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let manager = GoalManager::new(cwd);
    match manager.active_goal() {
        Ok(Some(goal)) => {
            let json = serde_json::json!({
                "id": goal.id,
                "description": goal.description,
                "status": goal.status.to_string(),
            });
            serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
        }
        Ok(None) => Ok("no active goal".to_string()),
        Err(e) => Err(format!("goal error: {e}")),
    }
}

fn dispatch_vault_get(arguments: &Value) -> Result<String, String> {
    if !vault_is_session_unlocked() {
        return Err("vault locked, run anvil vault unlock first".to_string());
    }
    let label = arguments
        .get("label")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing required argument: label".to_string())?;
    with_session_vault(|vm| vm.get_credential(label))
        .map(|cred| cred.secret)
        .map_err(|e| format!("vault error: {e}"))
}

fn dispatch_vault_list() -> Result<String, String> {
    if !vault_is_session_unlocked() {
        return Err("vault locked, run anvil vault unlock first".to_string());
    }
    with_session_vault(|vm| vm.list_credentials())
        .map(|labels| labels.join("\n"))
        .map_err(|e| format!("vault error: {e}"))
}
