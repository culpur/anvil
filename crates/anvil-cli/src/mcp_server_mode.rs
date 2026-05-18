// Task #626: SAFE-HEADLESS — `anvil mcp-server` is a stdio JSON-RPC
// server; stderr IS the documented log channel here (see the docs
// comment below).  No TUI runs in this mode.
#![allow(clippy::print_stdout, clippy::print_stderr)]

//! `anvil mcp-server` — expose Anvil as an MCP server on stdio.
//!
//! Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout.
//! ALL diagnostic/log output goes to stderr — stdout is the protocol channel.
//!
//! Supported methods:
//!   initialize     → return server capabilities
//!   tools/list     → return published tool definitions
//!   tools/call     → dispatch to Anvil tool implementations
//!
//! Unknown methods return JSON-RPC error -32601 (METHOD_NOT_FOUND).

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use runtime::{
    JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    McpInitializeResult, McpInitializeServerInfo, McpListToolsResult, McpTool,
    McpToolCallContent, McpToolCallResult,
};
use serde_json::Value;

use crate::mcp_server_tools::{dispatch, published_tools};

/// Configuration passed from the CLI argument parser.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Refuse write/edit/bash tools when `true`.
    pub read_only: bool,
    /// When `Some`, restrict published tools to exactly this set of names.
    pub tools_filter: Option<Vec<String>>,
}

// ── JSON-RPC constants ────────────────────────────────────────────────────────

const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_REQUEST: i64 = -32600;
const ANVIL_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Main entry point ──────────────────────────────────────────────────────────

/// Run the MCP server, reading requests from `reader` and writing responses to
/// `writer`.  Logs are emitted to stderr.
///
/// Returns when stdin is closed (EOF).
pub fn run_mcp_server(
    reader: impl BufRead,
    mut writer: impl Write,
    config: &McpServerConfig,
) -> io::Result<()> {
    eprintln!("[anvil mcp-server] starting (read_only={}, tools_filter={:?})", config.read_only, config.tools_filter);

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response_json = handle_line(trimmed, config);
        writeln!(writer, "{response_json}")?;
        writer.flush()?;
    }

    eprintln!("[anvil mcp-server] stdin closed, exiting");
    Ok(())
}

/// Parse one JSON-RPC request line and return the serialised response.
fn handle_line(line: &str, config: &McpServerConfig) -> String {
    let parsed: Result<JsonRpcRequest<Value>, _> = serde_json::from_str(line);
    match parsed {
        Err(parse_err) => {
            // Return a JSON-RPC parse error without an id.
            let resp: JsonRpcResponse<Value> = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: JsonRpcId::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("parse error: {parse_err}"),
                    data: None,
                }),
            };
            serde_json::to_string(&resp).unwrap_or_else(|_| r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#.to_string())
        }
        Ok(req) => {
            let resp_value = dispatch_request(&req, config);
            serde_json::to_string(&resp_value).unwrap_or_else(|e| {
                format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"serialization error: {e}"}}}}"#)
            })
        }
    }
}

/// Dispatch a parsed request to the appropriate handler and return the response
/// as a `serde_json::Value`.
fn dispatch_request(req: &JsonRpcRequest<Value>, config: &McpServerConfig) -> Value {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => handle_initialize(id),
        "initialized" => {
            // Notification — no response required but we return nothing by
            // sending a success with null result to keep the loop consistent.
            // Per spec, notifications have no id; silently ignore.
            return serde_json::Value::Null;
        }
        "tools/list" => handle_tools_list(id, config),
        "tools/call" => handle_tools_call(id, req.params.as_ref(), config),
        unknown => error_response(id, METHOD_NOT_FOUND, format!("unknown method: {unknown}")),
    }
}

fn handle_initialize(id: JsonRpcId) -> Value {
    eprintln!("[anvil mcp-server] initialize");
    let result = McpInitializeResult {
        protocol_version: "2025-03-26".to_string(),
        capabilities: serde_json::json!({ "tools": {} }),
        server_info: McpInitializeServerInfo {
            name: "anvil".to_string(),
            version: ANVIL_VERSION.to_string(),
        },
    };
    success_response(id, serde_json::to_value(result).unwrap_or(serde_json::Value::Null))
}

fn handle_tools_list(id: JsonRpcId, config: &McpServerConfig) -> Value {
    eprintln!("[anvil mcp-server] tools/list");
    let tools = build_tool_list(config);
    let result = McpListToolsResult {
        tools,
        next_cursor: None,
    };
    success_response(id, serde_json::to_value(result).unwrap_or(serde_json::Value::Null))
}

fn handle_tools_call(id: JsonRpcId, params: Option<&Value>, config: &McpServerConfig) -> Value {
    let params = match params {
        Some(p) => p,
        None => {
            return error_response(id, INVALID_REQUEST, "tools/call requires params".to_string());
        }
    };

    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(name) => name.to_string(),
        None => {
            return error_response(id, INVALID_REQUEST, "tools/call params missing 'name'".to_string());
        }
    };

    eprintln!("[anvil mcp-server] tools/call name={tool_name}");

    // Check that the tool is in our published set (after filtering).
    let published = build_tool_list(config);
    if !published.iter().any(|t| t.name == tool_name) {
        let result = tool_error_result(format!("unknown or unavailable tool: {tool_name}"));
        return success_response(id, serde_json::to_value(result).unwrap_or(serde_json::Value::Null));
    }

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    match dispatch(&tool_name, &arguments) {
        Ok(output) => {
            let result = tool_ok_result(output);
            success_response(id, serde_json::to_value(result).unwrap_or(serde_json::Value::Null))
        }
        Err(msg) => {
            let result = tool_error_result(msg);
            success_response(id, serde_json::to_value(result).unwrap_or(serde_json::Value::Null))
        }
    }
}

// ── Helper builders ───────────────────────────────────────────────────────────

fn build_tool_list(config: &McpServerConfig) -> Vec<McpTool> {
    published_tools()
        .into_iter()
        .filter(|tool| {
            // Gate write tools when --read-only is set.
            if config.read_only && tool.write_gated {
                return false;
            }
            // Gate by explicit --tools filter.
            if let Some(ref filter) = config.tools_filter {
                return filter.iter().any(|f| f == tool.name);
            }
            true
        })
        .map(|tool| McpTool {
            name: tool.name.to_string(),
            description: Some(tool.description.to_string()),
            input_schema: Some(tool.input_schema.clone()),
            annotations: None,
            meta: None,
        })
        .collect()
}

fn success_response(id: JsonRpcId, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_to_value(&id),
        "result": result
    })
}

fn error_response(id: JsonRpcId, code: i64, message: String) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_to_value(&id),
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn id_to_value(id: &JsonRpcId) -> Value {
    match id {
        JsonRpcId::Number(n) => Value::Number((*n).into()),
        JsonRpcId::String(s) => Value::String(s.clone()),
        JsonRpcId::Null => Value::Null,
    }
}

fn tool_ok_result(text: String) -> McpToolCallResult {
    let mut data = BTreeMap::new();
    data.insert("text".to_string(), Value::String(text));
    McpToolCallResult {
        content: vec![McpToolCallContent {
            kind: "text".to_string(),
            data,
        }],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    }
}

fn tool_error_result(message: String) -> McpToolCallResult {
    let mut data = BTreeMap::new();
    data.insert("text".to_string(), Value::String(message));
    McpToolCallResult {
        content: vec![McpToolCallContent {
            kind: "text".to_string(),
            data,
        }],
        structured_content: None,
        is_error: Some(true),
        meta: None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn default_config() -> McpServerConfig {
        McpServerConfig {
            read_only: false,
            tools_filter: None,
        }
    }

    fn read_only_config() -> McpServerConfig {
        McpServerConfig {
            read_only: true,
            tools_filter: None,
        }
    }

    fn send_request(json: &str, config: &McpServerConfig) -> Value {
        let reader = Cursor::new(json.to_string() + "\n");
        let mut output = Vec::new();
        run_mcp_server(reader, &mut output, config).expect("mcp server run");
        let out_str = String::from_utf8(output).expect("utf8");
        let trimmed = out_str.trim();
        // notifications return Null → empty output
        if trimmed.is_empty() {
            return Value::Null;
        }
        serde_json::from_str(trimmed).expect("valid json response")
    }

    // ── 1. initialize returns capabilities ────────────────────────────────────

    #[test]
    fn mcp_server_initialize_returns_capabilities() {
        let req = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{}}"#;
        let resp = send_request(req, &default_config());
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(resp["error"].is_null(), "should have no error");
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["serverInfo"]["name"], "anvil");
        assert!(!result["capabilities"].is_null());
    }

    // ── 2. tools/list returns exactly 9 published tools ───────────────────────

    #[test]
    fn mcp_server_tools_list_returns_published_tools() {
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":2,"params":{}}"#;
        let resp = send_request(req, &default_config());
        assert!(resp["error"].is_null());
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 9, "expected exactly 9 published tools, got {}", tools.len());
        let names: Vec<&str> = tools.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for expected in &["read_file", "write_file", "edit_file", "glob_search",
                          "grep_search", "bash", "goal_status", "vault_get", "vault_list"] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    // ── 3. --read-only excludes write tools ───────────────────────────────────

    #[test]
    fn mcp_server_read_only_excludes_write_tools() {
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":3,"params":{}}"#;
        let resp = send_request(req, &read_only_config());
        assert!(resp["error"].is_null());
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for forbidden in &["write_file", "edit_file", "bash"] {
            assert!(!names.contains(forbidden), "write-gated tool present in read-only mode: {forbidden}");
        }
        // read tools still present
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"glob_search"));
        assert!(names.contains(&"grep_search"));
    }

    // ── 4. tools/call dispatches read_file ────────────────────────────────────

    #[test]
    fn mcp_server_tools_call_dispatches_read_file() {
        use tempfile::NamedTempFile;
        use std::io::Write as IoWrite;

        let mut tmp = NamedTempFile::new().expect("tempfile");
        tmp.write_all(b"hello from mcp test").expect("write tempfile");
        let path = tmp.path().to_string_lossy();

        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"tools/call","id":4,"params":{{"name":"read_file","arguments":{{"path":"{path}"}}}}}}"#
        );
        let resp = send_request(&req, &default_config());
        assert!(resp["error"].is_null(), "unexpected error: {:?}", resp["error"]);
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"].as_array().expect("content array");
        assert!(!content.is_empty());
        let text = content[0]["text"].as_str().unwrap_or("");
        assert!(text.contains("hello from mcp test"), "expected file contents in output, got: {text}");
    }

    // ── 5. tools/call returns error for unknown tool ──────────────────────────

    #[test]
    fn mcp_server_tools_call_returns_error_for_unknown_tool() {
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":5,"params":{"name":"no_such_tool","arguments":{}}}"#;
        let resp = send_request(req, &default_config());
        assert!(resp["error"].is_null(), "should not be a protocol error");
        assert_eq!(resp["result"]["isError"], true);
    }

    // ── 6. vault tool refuses when locked ─────────────────────────────────────

    #[test]
    fn mcp_server_vault_tool_refuses_when_locked() {
        // The test process has no vault unlocked, so vault_get/vault_list must
        // return isError: true with the correct message.
        let req = r#"{"jsonrpc":"2.0","method":"tools/call","id":6,"params":{"name":"vault_get","arguments":{"label":"test"}}}"#;
        let resp = send_request(req, &default_config());
        assert!(resp["error"].is_null(), "should not be a protocol error");
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("vault locked"), "expected vault locked message, got: {text}");
    }

    // ── 7. logs go to stderr not stdout ───────────────────────────────────────

    #[test]
    fn mcp_server_logs_to_stderr_not_stdout() {
        // Build the release binary path.
        let exe = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/release/anvil");

        if !exe.exists() {
            // Binary not built yet — skip but don't fail.
            eprintln!("skipping mcp_server_logs_to_stderr_not_stdout: release binary not found at {}", exe.display());
            return;
        }

        let input = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{}}"#;

        let output = std::process::Command::new(&exe)
            .arg("mcp-server")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as IoWrite;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(input.as_bytes());
                    let _ = stdin.write_all(b"\n");
                }
                child.wait_with_output()
            });

        match output {
            Ok(out) => {
                let stdout_str = String::from_utf8_lossy(&out.stdout);
                let stderr_str = String::from_utf8_lossy(&out.stderr);

                // stdout must be valid JSON-RPC
                for line in stdout_str.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        let parsed: Result<Value, _> = serde_json::from_str(trimmed);
                        assert!(parsed.is_ok(), "stdout line is not valid JSON: {trimmed}");
                        let val = parsed.unwrap();
                        assert_eq!(val["jsonrpc"], "2.0", "stdout line missing jsonrpc field");
                    }
                }

                // stderr must contain at least one log line
                assert!(!stderr_str.is_empty(), "expected log output on stderr");
            }
            Err(e) => {
                eprintln!("skipping mcp_server_logs_to_stderr_not_stdout: could not run binary: {e}");
            }
        }
    }

    // ── 8. --tools filter restricts published set ─────────────────────────────

    #[test]
    fn mcp_server_tools_filter_flag_restricts_set() {
        let config = McpServerConfig {
            read_only: false,
            tools_filter: Some(vec!["read_file".to_string(), "bash".to_string()]),
        };
        let req = r#"{"jsonrpc":"2.0","method":"tools/list","id":8,"params":{}}"#;
        let resp = send_request(req, &config);
        assert!(resp["error"].is_null());
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2, "expected exactly 2 tools with filter, got {}", tools.len());
        let names: Vec<&str> = tools.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"bash"));
    }
}
