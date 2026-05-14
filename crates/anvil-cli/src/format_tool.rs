//! Tool call and result formatting: converts raw tool JSON into human-readable
//! display strings shown in the REPL and TUI.
//!
//! Also contains `response_to_events` which translates a completed
//! `MessageResponse` into the `AssistantEvent` sequence that the runtime uses.
//!
//! # ResultBlock schema
//!
//! Tools produce structured output via [`ResultBlock`].  Three variants cover
//! all tool output shapes:
//!
//! - [`ResultBlock::Text`]    — free-form prose or command output
//! - [`ResultBlock::Code`]    — syntax-highlighted content with a language tag
//! - [`ResultBlock::KvTable`] — key-value pairs rendered as a summary table
//!
//! Each surface (TUI scrollback, viewer.html, pretty-card) renders
//! `Vec<ResultBlock>` consistently via [`render_result_blocks_tui`].

use std::fmt::Write as FmtWrite;
use std::io::Write;

use api::{
    MessageResponse, OutputContentBlock,
};
use crate::render::TerminalRenderer;
use runtime::{AssistantEvent, RuntimeError, TokenUsage};

// ─── ResultBlock schema ───────────────────────────────────────────────────────

/// A single presentable unit of tool output.
///
/// Three variants are sufficient for every tool in the current registry:
/// prose text, syntax-highlighted code, and key-value summary rows.
/// Consumers assemble a `Vec<ResultBlock>` and pass it to
/// [`render_result_blocks_tui`] (or their surface equivalent) to get a
/// consistent display string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultBlock {
    /// Free-form prose, plain command output, or multi-line status text.
    Text { content: String },
    /// Syntax-highlighted content (code, JSON, TOML, shell script, …).
    Code { language: String, content: String },
    /// A list of key-value pairs rendered as a compact summary table.
    KvTable { rows: Vec<(String, String)> },
}

/// Render a sequence of [`ResultBlock`]s to a TUI-compatible ANSI string.
///
/// - `Text` blocks are emitted as-is (no colour).
/// - `Code` blocks are wrapped with a faint language label header.
/// - `KvTable` blocks render each row as `  key : value` with right-padding
///   on the key column.
pub(crate) fn render_result_blocks_tui(blocks: &[ResultBlock]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            ResultBlock::Text { content } => {
                if !content.is_empty() {
                    parts.push(content.clone());
                }
            }
            ResultBlock::Code { language, content } => {
                if content.is_empty() {
                    continue;
                }
                if language.is_empty() {
                    parts.push(content.clone());
                } else {
                    parts.push(format!(
                        "\x1b[2m[{language}]\x1b[0m\n{content}"
                    ));
                }
            }
            ResultBlock::KvTable { rows } => {
                if rows.is_empty() {
                    continue;
                }
                let max_key = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
                let rendered: Vec<String> = rows
                    .iter()
                    .map(|(k, v)| format!("  {k:<max_key$} : {v}"))
                    .collect();
                parts.push(rendered.join("\n"));
            }
        }
    }
    parts.join("\n")
}

/// One-line summary of a tool's result, suitable for the TUI's compact
/// post-call line `{name} [{status}]: {summary}`. Without this, the summary
/// was the first line of the raw JSON output — for tools that return
/// `{"key": "value"}` the first line is just `{`, telling the user
/// absolutely nothing.
///
/// Explicit arms exist for every tool in `mvp_tool_specs()`.  The generic
/// fallback at the bottom covers plugin and MCP tools with unknown schemas.
pub(crate) fn tool_result_summary(name: &str, output: &str, is_error: bool) -> String {
    if is_error {
        return truncate_for_summary(output.trim(), 200);
    }
    let parsed: serde_json::Value = match serde_json::from_str(output) {
        Ok(v) => v,
        Err(_) => return truncate_for_summary(output.trim(), 200),
    };

    match name {
        // ── Core file/shell tools ─────────────────────────────────────────────
        "bash" | "Bash" => {
            // Bash result: {"stdout": "...", "stderr": "...", "interrupted": bool, ...}
            let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let stderr = parsed.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            let body = if !stdout.trim().is_empty() { stdout } else { stderr };
            let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
            let first = first_visible_line(body);
            if first.is_empty() {
                "(no output)".to_string()
            } else if lines > 1 {
                format!("{} (+{} more line{})", truncate_for_summary(first, 140), lines - 1, if lines - 1 == 1 { "" } else { "s" })
            } else {
                truncate_for_summary(first, 200)
            }
        }
        "read_file" | "Read" => {
            // Real runtime schema (see runtime::file_ops::ReadFileOutput +
            // TextFilePayload): { "kind": "text", "file": { "filePath": "...",
            // "content": "...", "numLines": N, "startLine": S, "totalLines": T } }.
            // Fall back through several shapes so the summary still works for
            // older payloads, MCP variants, or tests that emit a flatter object.
            let file = parsed.get("file").unwrap_or(&parsed);
            let total_lines = file
                .get("totalLines")
                .and_then(|v| v.as_u64())
                .or_else(|| parsed.get("totalLines").and_then(|v| v.as_u64()));
            let num_lines = file
                .get("numLines")
                .and_then(|v| v.as_u64())
                .or_else(|| parsed.get("numLines").and_then(|v| v.as_u64()))
                // Legacy / MCP shape: "lines" at the top level.
                .or_else(|| parsed.get("lines").and_then(|v| v.as_u64()))
                // Last resort: count the lines in the content string if present.
                .or_else(|| {
                    file.get("content")
                        .and_then(|v| v.as_str())
                        .or_else(|| parsed.get("content").and_then(|v| v.as_str()))
                        .map(|s| s.lines().count() as u64)
                })
                .unwrap_or(0);
            // Prefer "showing N of M lines" when both are known and differ;
            // otherwise fall back to the single count we have.
            match total_lines {
                Some(total) if total > num_lines && num_lines > 0 => format!(
                    "{num_lines} of {total} line{}",
                    if total == 1 { "" } else { "s" },
                ),
                _ => format!(
                    "{num_lines} line{}",
                    if num_lines == 1 { "" } else { "s" },
                ),
            }
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            if path == "?" || path.is_empty() {
                "wrote file".to_string()
            } else {
                format!("wrote {path}")
            }
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            if path == "?" || path.is_empty() {
                "applied edit".to_string()
            } else {
                format!("edited {path}")
            }
        }
        "glob_search" | "Glob" => {
            let count = parsed.get("numFiles")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    parsed.get("filenames").and_then(|v| v.as_array()).map_or(0, |a| a.len() as u64)
                });
            format!("{count} file{}", if count == 1 { "" } else { "s" })
        }
        "grep_search" | "Grep" => {
            let matches = parsed.get("numMatches")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    parsed.get("matches").and_then(|v| v.as_array()).map_or(0, |a| a.len() as u64)
                });
            let files = parsed.get("numFiles").and_then(|v| v.as_u64()).unwrap_or(0);
            if files > 0 {
                format!("{matches} match{} in {files} file{}", if matches == 1 { "" } else { "es" }, if files == 1 { "" } else { "s" })
            } else {
                format!("{matches} match{}", if matches == 1 { "" } else { "es" })
            }
        }
        // ── Web tools ─────────────────────────────────────────────────────────
        "WebFetch" => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(first_visible_line(s), 200);
            }
            let snippet = parsed.get("content")
                .or_else(|| parsed.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if snippet.is_empty() {
                "fetched page".to_string()
            } else {
                truncate_for_summary(first_visible_line(snippet), 200)
            }
        }
        "WebSearch" => {
            // {"results": [{...}], "query": "..."}
            let count = parsed.get("results")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            let query = parsed.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.is_empty() {
                format!("{count} result{}", if count == 1 { "" } else { "s" })
            } else {
                format!("{count} result{} for \"{query}\"", if count == 1 { "" } else { "s" })
            }
        }
        // ── Task management tools ─────────────────────────────────────────────
        "TodoWrite" => {
            let count = parsed.get("todos")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{count} task{} saved", if count == 1 { "" } else { "s" })
        }
        "TaskCreate" => {
            let id = parsed.get("taskId")
                .or_else(|| parsed.get("task_id"))
                .or_else(|| parsed.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("task created (id={id})")
        }
        "TaskGet" => {
            let status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let id = parsed.get("taskId")
                .or_else(|| parsed.get("task_id"))
                .or_else(|| parsed.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("task {id}: {status}")
        }
        "TaskList" => {
            let count = parsed.get("tasks")
                .and_then(|v| v.as_array())
                .map_or_else(|| parsed.as_array().map_or(0, |a| a.len()), |a| a.len());
            format!("{count} task{}", if count == 1 { "" } else { "s" })
        }
        "TaskUpdate" => {
            let status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("updated");
            format!("task {status}")
        }
        "TaskOutput" => {
            let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let body = if stdout.trim().is_empty() {
                parsed.get("output").and_then(|v| v.as_str()).unwrap_or("")
            } else {
                stdout
            };
            let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
            let first = first_visible_line(body);
            if first.is_empty() {
                "(no output)".to_string()
            } else if lines > 1 {
                format!("{} (+{} more line{})", truncate_for_summary(first, 120), lines - 1, if lines - 1 == 1 { "" } else { "s" })
            } else {
                truncate_for_summary(first, 200)
            }
        }
        "TaskStop" => {
            parsed.get("message")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_summary(s, 200))
                .unwrap_or_else(|| "task stopped".to_string())
        }
        // ── Agent / Skill tools ───────────────────────────────────────────────
        "Agent" => {
            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("spawned");
            if name == "?" {
                format!("agent {status}")
            } else {
                format!("agent {name}: {status}")
            }
        }
        "Skill" => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(first_visible_line(s), 200);
            }
            let skill = parsed.get("skill").and_then(|v| v.as_str()).unwrap_or("?");
            format!("skill loaded: {skill}")
        }
        // ── Configuration / system tools ──────────────────────────────────────
        "Config" => {
            let setting = parsed.get("setting").and_then(|v| v.as_str()).unwrap_or("?");
            let value = parsed.get("value")
                .map(|v| truncate_for_summary(&v.to_string(), 80))
                .unwrap_or_default();
            if value.is_empty() {
                format!("{setting}")
            } else {
                format!("{setting} = {value}")
            }
        }
        "StructuredOutput" => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(s.trim(), 200);
            }
            if let Some(obj) = parsed.as_object() {
                let keys: Vec<_> = obj.keys().take(4).map(String::as_str).collect();
                return truncate_for_summary(&format!("{{ {} }}", keys.join(", ")), 200);
            }
            "structured output".to_string()
        }
        "ToolSearch" => {
            let count = parsed.get("tools")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{count} tool{}", if count == 1 { "" } else { "s" })
        }
        // ── Execution tools ───────────────────────────────────────────────────
        "NotebookEdit" => {
            let path = parsed.get("notebook_path").and_then(|v| v.as_str()).unwrap_or("?");
            let mode = parsed.get("edit_mode").and_then(|v| v.as_str()).unwrap_or("edited");
            format!("{mode} cell in {path}")
        }
        "Sleep" => "done".to_string(),
        "SendUserMessage" | "Brief" => {
            let msg = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("");
            if msg.is_empty() {
                "message sent".to_string()
            } else {
                truncate_for_summary(first_visible_line(msg), 200)
            }
        }
        "REPL" => {
            let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let output_field = parsed.get("output").and_then(|v| v.as_str()).unwrap_or("");
            let body = if !stdout.trim().is_empty() { stdout } else { output_field };
            let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
            let first = first_visible_line(body);
            if first.is_empty() {
                "(no output)".to_string()
            } else if lines > 1 {
                format!("{} (+{} more line{})", truncate_for_summary(first, 120), lines - 1, if lines - 1 == 1 { "" } else { "s" })
            } else {
                truncate_for_summary(first, 200)
            }
        }
        "PowerShell" => {
            let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let stderr = parsed.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            let body = if !stdout.trim().is_empty() { stdout } else { stderr };
            let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
            let first = first_visible_line(body);
            if first.is_empty() {
                "(no output)".to_string()
            } else if lines > 1 {
                format!("{} (+{} more line{})", truncate_for_summary(first, 120), lines - 1, if lines - 1 == 1 { "" } else { "s" })
            } else {
                truncate_for_summary(first, 200)
            }
        }
        "AskUserQuestion" => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(s.trim(), 200);
            }
            parsed.get("answer").or_else(|| parsed.get("response"))
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_summary(s, 200))
                .unwrap_or_else(|| "answered".to_string())
        }
        // ── MCP resource tools ────────────────────────────────────────────────
        "ListMcpResourcesTool" => {
            mcp_list_summary(&parsed, "resource")
        }
        "ReadMcpResourceTool" => {
            let uri = parsed.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
            if let Some(content) = parsed.get("content").and_then(|v| v.as_str()) {
                let lines = content.lines().count();
                format!("{uri} ({lines} line{})", if lines == 1 { "" } else { "s" })
            } else {
                format!("read {uri}")
            }
        }
        // ── LSP tool ──────────────────────────────────────────────────────────
        "LSPTool" => {
            lsp_tool_summary(&parsed)
        }
        // ── Cron tools ────────────────────────────────────────────────────────
        "CronCreate" => {
            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("cron job");
            let expr = parsed.get("cron_expression").and_then(|v| v.as_str()).unwrap_or("?");
            format!("scheduled \"{name}\" ({expr})")
        }
        "CronList" => {
            let count = parsed.get("entries")
                .and_then(|v| v.as_array())
                .map_or_else(|| parsed.as_array().map_or(0, |a| a.len()), |a| a.len());
            format!("{count} cron entr{}", if count == 1 { "y" } else { "ies" })
        }
        "CronDelete" => {
            parsed.get("message")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_summary(s, 200))
                .unwrap_or_else(|| "cron entry deleted".to_string())
        }
        // ── Team tools ────────────────────────────────────────────────────────
        "TeamCreate" => {
            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let id = parsed.get("id")
                .or_else(|| parsed.get("team_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("team \"{name}\" created (id={id})")
        }
        "TeamAddMember" => {
            let member = parsed.get("name")
                .or_else(|| parsed.get("member_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("member {member} added")
        }
        "TeamRemoveMember" => {
            let member = parsed.get("member_name")
                .or_else(|| parsed.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("member {member} removed")
        }
        "TeamList" => {
            let count = parsed.get("teams")
                .and_then(|v| v.as_array())
                .map_or_else(|| parsed.as_array().map_or(0, |a| a.len()), |a| a.len());
            format!("{count} team{}", if count == 1 { "" } else { "s" })
        }
        "TeamDelegate" => {
            let member = parsed.get("member_name")
                .or_else(|| parsed.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("delegated");
            format!("delegated to {member}: {status}")
        }
        "TeamStatus" => {
            let members = parsed.get("members")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{members} member{}", if members == 1 { "" } else { "s" })
        }
        // ── Worktree tools ────────────────────────────────────────────────────
        "EnterWorktree" => {
            let path = parsed.get("path")
                .or_else(|| parsed.get("worktree_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("entered worktree {path}")
        }
        "ExitWorktree" => {
            parsed.get("message")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_summary(s, 200))
                .unwrap_or_else(|| "exited worktree".to_string())
        }
        // ── Plan mode tools ───────────────────────────────────────────────────
        "EnterPlanMode" => "plan mode active".to_string(),
        "ExitPlanMode" => "plan mode exited".to_string(),
        // ── Remote / automation tools ─────────────────────────────────────────
        "RemoteTrigger" => {
            let status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{url}: {status}")
        }
        "SendMessage" => {
            parsed.get("message")
                .and_then(|v| v.as_str())
                .map(|s| truncate_for_summary(s, 200))
                .unwrap_or_else(|| "message sent".to_string())
        }
        "read_release_notes" => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(first_visible_line(s), 200);
            }
            "release notes".to_string()
        }
        // ── Fallback: plugin / MCP / unknown tools ────────────────────────────
        _ => {
            if let Some(s) = parsed.as_str() {
                return truncate_for_summary(s.trim(), 200);
            }
            for key in &["message", "summary", "result", "name", "id", "status"] {
                if let Some(v) = parsed.get(key).and_then(|v| v.as_str()) {
                    if !v.trim().is_empty() {
                        return truncate_for_summary(&format!("{key}={}", v), 200);
                    }
                }
            }
            // Last resort: list the top-level keys so the user knows roughly
            // what the tool returned (e.g. "keys: id, name, members").
            if let Some(obj) = parsed.as_object() {
                if obj.is_empty() {
                    return "(empty result)".to_string();
                }
                let keys: Vec<_> = obj.keys().take(6).map(String::as_str).collect();
                return truncate_for_summary(&format!("keys: {}", keys.join(", ")), 200);
            }
            truncate_for_summary(output.trim(), 200)
        }
    }
}

/// Summarise a ListMcpResourcesTool result as "{N} resources".
fn mcp_list_summary(parsed: &serde_json::Value, kind: &str) -> String {
    let count = parsed.get("resources")
        .and_then(|v| v.as_array())
        .map_or_else(|| parsed.as_array().map_or(0, |a| a.len()), |a| a.len());
    format!("{count} {kind}{}", if count == 1 { "" } else { "s" })
}

/// Summarise an LSPTool result as a compact kv-style line.
fn lsp_tool_summary(parsed: &serde_json::Value) -> String {
    let action = parsed.get("action").and_then(|v| v.as_str()).unwrap_or("?");
    match action {
        "diagnostics" => {
            let count = parsed.get("diagnostics")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{count} diagnostic{}", if count == 1 { "" } else { "s" })
        }
        "definition" | "references" => {
            let count = parsed.get("locations")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{count} location{}", if count == 1 { "" } else { "s" })
        }
        other => {
            if let Some(s) = parsed.get("result").and_then(|v| v.as_str()) {
                truncate_for_summary(first_visible_line(s), 200)
            } else {
                format!("lsp {other}")
            }
        }
    }
}

pub(crate) fn tool_call_detail(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    match name {
        "bash" | "Bash" => parsed
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| truncate_for_summary(cmd, 200))
            .unwrap_or_default(),
        "read_file" | "Read" => format!("Reading {}", extract_tool_path(&parsed)),
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .map_or(0, |c| c.lines().count());
            format!("Writing {path}  ({lines} lines)")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let new = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let mut out = format!("Editing {path}");
            if !old.is_empty() || !new.is_empty() {
                let _ = write!(out, "\n- {}\n+ {}",
                    truncate_for_summary(first_visible_line(old), 72),
                    truncate_for_summary(first_visible_line(new), 72),
                );
            }
            out
        }
        "glob_search" | "Glob" => {
            let pattern = parsed.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let scope = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("{pattern}\nin {scope}")
        }
        "grep_search" | "Grep" => {
            let pattern = parsed.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let scope = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let glob = parsed.get("glob").and_then(|v| v.as_str()).unwrap_or("");
            if glob.is_empty() {
                format!("{pattern}\nin {scope}")
            } else {
                format!("{pattern}\nin {scope} ({glob})")
            }
        }
        "WebSearch" | "web_search" => parsed
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        "WebFetch" => {
            let url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("?");
            let prompt = parsed.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            if prompt.is_empty() {
                url.to_string()
            } else {
                format!("{url}\n{}", truncate_for_summary(prompt, 80))
            }
        }
        "TodoWrite" => {
            let count = parsed.get("todos")
                .and_then(|v| v.as_array())
                .map_or(0, |a| a.len());
            format!("{count} task{}", if count == 1 { "" } else { "s" })
        }
        "Agent" => {
            let desc = parsed.get("description")
                .or_else(|| parsed.get("prompt"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            truncate_for_summary(desc, 200)
        }
        "Skill" => {
            let skill = parsed.get("skill").and_then(|v| v.as_str()).unwrap_or("?");
            format!("loading skill: {skill}")
        }
        "Config" => {
            let setting = parsed.get("setting").and_then(|v| v.as_str()).unwrap_or("?");
            let value = parsed.get("value")
                .map(|v| truncate_for_summary(&v.to_string(), 60))
                .unwrap_or_default();
            if value.is_empty() {
                format!("get {setting}")
            } else {
                format!("set {setting} = {value}")
            }
        }
        "TaskCreate" => {
            let desc = parsed.get("description").and_then(|v| v.as_str()).unwrap_or("?");
            let cmd = parsed.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.is_empty() {
                truncate_for_summary(desc, 200)
            } else {
                format!("{}\n$ {}", truncate_for_summary(desc, 80), truncate_for_summary(cmd, 120))
            }
        }
        "TaskGet" | "TaskOutput" | "TaskStop" | "TaskUpdate" => {
            let id = parsed.get("task_id")
                .or_else(|| parsed.get("taskId"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("task {id}")
        }
        "ListMcpResourcesTool" => {
            let server = parsed.get("server_name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("list resources on {server}")
        }
        "ReadMcpResourceTool" => {
            let server = parsed.get("server_name").and_then(|v| v.as_str()).unwrap_or("?");
            let uri = parsed.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{server}: {uri}")
        }
        "LSPTool" => {
            let action = parsed.get("action").and_then(|v| v.as_str()).unwrap_or("?");
            let path = parsed.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
            let loc = match (parsed.get("line").and_then(|v| v.as_u64()), parsed.get("character").and_then(|v| v.as_u64())) {
                (Some(l), Some(c)) => format!(":{l}:{c}"),
                (Some(l), None) => format!(":{l}"),
                _ => String::new(),
            };
            format!("{action} {path}{loc}")
        }
        "REPL" => {
            let lang = parsed.get("language").and_then(|v| v.as_str()).unwrap_or("?");
            let code = parsed.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let lines = code.lines().count();
            format!("[{lang}] {lines} line{}", if lines == 1 { "" } else { "s" })
        }
        "PowerShell" => parsed
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| truncate_for_summary(cmd, 200))
            .unwrap_or_default(),
        "NotebookEdit" => {
            let path = parsed.get("notebook_path").and_then(|v| v.as_str()).unwrap_or("?");
            let mode = parsed.get("edit_mode").and_then(|v| v.as_str()).unwrap_or("edit");
            format!("{mode} cell in {path}")
        }
        "RemoteTrigger" => {
            let url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("?");
            let prompt = parsed.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            format!("{url}\n{}", truncate_for_summary(prompt, 120))
        }
        "CronCreate" => {
            let expr = parsed.get("cron_expression").and_then(|v| v.as_str()).unwrap_or("?");
            let prompt = parsed.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            format!("{expr}: {}", truncate_for_summary(prompt, 120))
        }
        "TeamCreate" => {
            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("create team \"{name}\"")
        }
        "TeamAddMember" => {
            let name = parsed.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let role = parsed.get("role").and_then(|v| v.as_str()).unwrap_or("?");
            format!("add {name} as {role}")
        }
        "TeamDelegate" => {
            let member = parsed.get("member_name").and_then(|v| v.as_str()).unwrap_or("?");
            let prompt = parsed.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            format!("{member}: {}", truncate_for_summary(prompt, 120))
        }
        "EnterWorktree" => {
            let branch = parsed.get("branch").and_then(|v| v.as_str()).unwrap_or("auto");
            format!("branch: {branch}")
        }
        "SendMessage" => {
            let id = parsed.get("agent_id").map(|v| v.to_string()).unwrap_or_else(|| "?".to_string());
            let msg = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("");
            format!("→ agent {id}: {}", truncate_for_summary(msg, 120))
        }
        "AskUserQuestion" => {
            let q = parsed.get("question").and_then(|v| v.as_str()).unwrap_or("?");
            truncate_for_summary(q, 200)
        }
        _ => summarize_tool_payload(input),
    }
}

#[cfg(test)]
pub(crate) fn format_tool_call_start(name: &str, input: &str) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(input).unwrap_or(serde_json::Value::String(input.to_string()));

    let detail = match name {
        "bash" | "Bash" => format_bash_call(&parsed),
        "read_file" | "Read" => {
            let path = extract_tool_path(&parsed);
            format!("\x1b[2m📄 Reading {path}…\x1b[0m")
        }
        "write_file" | "Write" => {
            let path = extract_tool_path(&parsed);
            let lines = parsed
                .get("content")
                .and_then(|value| value.as_str())
                .map_or(0, |content| content.lines().count());
            format!("\x1b[1;32m✏️ Writing {path}\x1b[0m \x1b[2m({lines} lines)\x1b[0m")
        }
        "edit_file" | "Edit" => {
            let path = extract_tool_path(&parsed);
            let old_value = parsed
                .get("old_string")
                .or_else(|| parsed.get("oldString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let new_value = parsed
                .get("new_string")
                .or_else(|| parsed.get("newString"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            format!(
                "\x1b[1;33m📝 Editing {path}\x1b[0m{}",
                format_patch_preview(old_value, new_value)
                    .map(|preview| format!("\n{preview}"))
                    .unwrap_or_default()
            )
        }
        "glob_search" | "Glob" => format_search_start("🔎 Glob", &parsed),
        "grep_search" | "Grep" => format_search_start("🔎 Grep", &parsed),
        "web_search" | "WebSearch" => parsed
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => summarize_tool_payload(input),
    };

    let border = "─".repeat(name.len() + 8);
    format!(
        "\x1b[38;5;245m╭─ \x1b[1;36m{name}\x1b[0;38;5;245m ─╮\x1b[0m\n\x1b[38;5;245m│\x1b[0m {detail}\n\x1b[38;5;245m╰{border}╯\x1b[0m"
    )
}

#[cfg(test)]
pub(crate) fn format_tool_result(name: &str, output: &str, is_error: bool) -> String {
    let icon = if is_error {
        "\x1b[1;31m✗\x1b[0m"
    } else {
        "\x1b[1;32m✓\x1b[0m"
    };
    if is_error {
        let summary = truncate_for_summary(output.trim(), 160);
        return if summary.is_empty() {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
        } else {
            format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n\x1b[38;5;203m{summary}\x1b[0m")
        };
    }

    let parsed: serde_json::Value =
        serde_json::from_str(output).unwrap_or(serde_json::Value::String(output.to_string()));
    match name {
        "bash" | "Bash" => format_bash_result(icon, &parsed),
        "read_file" | "Read" => format_read_result(icon, &parsed),
        "write_file" | "Write" => format_write_result(icon, &parsed),
        "edit_file" | "Edit" => format_edit_result(icon, &parsed),
        "glob_search" | "Glob" => format_glob_result(icon, &parsed),
        "grep_search" | "Grep" => format_grep_result(icon, &parsed),
        _ => format_generic_tool_result(icon, name, &parsed),
    }
}

#[cfg(test)]
const DISPLAY_TRUNCATION_NOTICE: &str =
    "\x1b[2m… output truncated for display; full result preserved in session.\x1b[0m";
#[cfg(test)]
const READ_DISPLAY_MAX_LINES: usize = 80;
#[cfg(test)]
const READ_DISPLAY_MAX_CHARS: usize = 6_000;
#[cfg(test)]
const TOOL_OUTPUT_DISPLAY_MAX_LINES: usize = 60;
#[cfg(test)]
const TOOL_OUTPUT_DISPLAY_MAX_CHARS: usize = 4_000;

pub(crate) fn extract_tool_path(parsed: &serde_json::Value) -> String {
    parsed
        .get("file_path")
        .or_else(|| parsed.get("filePath"))
        .or_else(|| parsed.get("path"))
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_string()
}

#[cfg(test)]
pub(crate) fn format_search_start(label: &str, parsed: &serde_json::Value) -> String {
    let pattern = parsed
        .get("pattern")
        .and_then(|value| value.as_str())
        .unwrap_or("?");
    let scope = parsed
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or(".");
    format!("{label} {pattern}\n\x1b[2min {scope}\x1b[0m")
}

#[cfg(test)]
pub(crate) fn format_patch_preview(old_value: &str, new_value: &str) -> Option<String> {
    if old_value.is_empty() && new_value.is_empty() {
        return None;
    }
    Some(format!(
        "\x1b[38;5;203m- {}\x1b[0m\n\x1b[38;5;70m+ {}\x1b[0m",
        truncate_for_summary(first_visible_line(old_value), 72),
        truncate_for_summary(first_visible_line(new_value), 72)
    ))
}

#[cfg(test)]
pub(crate) fn format_bash_call(parsed: &serde_json::Value) -> String {
    let command = parsed
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if command.is_empty() {
        String::new()
    } else {
        format!(
            "\x1b[48;5;236;38;5;255m $ {} \x1b[0m",
            truncate_for_summary(command, 160)
        )
    }
}

pub(crate) fn first_visible_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
}

#[cfg(test)]
pub(crate) fn format_bash_result(icon: &str, parsed: &serde_json::Value) -> String {
    let mut lines = vec![format!("{icon} \x1b[38;5;245mbash\x1b[0m")];
    if let Some(task_id) = parsed
        .get("backgroundTaskId")
        .and_then(|value| value.as_str())
    {
        write!(&mut lines[0], " backgrounded ({task_id})").expect("write to string");
    } else if let Some(status) = parsed
        .get("returnCodeInterpretation")
        .and_then(|value| value.as_str())
        .filter(|status| !status.is_empty())
    {
        write!(&mut lines[0], " {status}").expect("write to string");
    }

    if let Some(stdout) = parsed.get("stdout").and_then(|value| value.as_str())
        && !stdout.trim().is_empty() {
            lines.push(truncate_output_for_display(
                stdout,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            ));
        }
    if let Some(stderr) = parsed.get("stderr").and_then(|value| value.as_str())
        && !stderr.trim().is_empty() {
            lines.push(format!(
                "\x1b[38;5;203m{}\x1b[0m",
                truncate_output_for_display(
                    stderr,
                    TOOL_OUTPUT_DISPLAY_MAX_LINES,
                    TOOL_OUTPUT_DISPLAY_MAX_CHARS,
                )
            ));
        }

    lines.join("\n\n")
}

#[cfg(test)]
pub(crate) fn format_read_result(icon: &str, parsed: &serde_json::Value) -> String {
    let file = parsed.get("file").unwrap_or(parsed);
    let path = extract_tool_path(file);
    let start_line = file
        .get("startLine")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let num_lines = file
        .get("numLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_lines = file
        .get("totalLines")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(num_lines);
    let content = file
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let end_line = start_line.saturating_add(num_lines.saturating_sub(1));

    format!(
        "{icon} \x1b[2m📄 Read {path} (lines {}-{} of {})\x1b[0m\n{}",
        start_line,
        end_line.max(start_line),
        total_lines,
        truncate_output_for_display(content, READ_DISPLAY_MAX_LINES, READ_DISPLAY_MAX_CHARS)
    )
}

#[cfg(test)]
pub(crate) fn format_write_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let kind = parsed
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("write");
    let line_count = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .map_or(0, |content| content.lines().count());
    format!(
        "{icon} \x1b[1;32m✏️ {} {path}\x1b[0m \x1b[2m({line_count} lines)\x1b[0m",
        if kind == "create" { "Wrote" } else { "Updated" },
    )
}

#[cfg(test)]
pub(crate) fn format_structured_patch_preview(parsed: &serde_json::Value) -> Option<String> {
    let hunks = parsed.get("structuredPatch")?.as_array()?;
    let mut preview = Vec::new();
    for hunk in hunks.iter().take(2) {
        let lines = hunk.get("lines")?.as_array()?;
        for line in lines.iter().filter_map(|value| value.as_str()).take(6) {
            match line.chars().next() {
                Some('+') => preview.push(format!("\x1b[38;5;70m{line}\x1b[0m")),
                Some('-') => preview.push(format!("\x1b[38;5;203m{line}\x1b[0m")),
                _ => preview.push(line.to_string()),
            }
        }
    }
    if preview.is_empty() {
        None
    } else {
        Some(preview.join("\n"))
    }
}

#[cfg(test)]
pub(crate) fn format_edit_result(icon: &str, parsed: &serde_json::Value) -> String {
    let path = extract_tool_path(parsed);
    let suffix = if parsed
        .get("replaceAll")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        " (replace all)"
    } else {
        ""
    };
    let preview = format_structured_patch_preview(parsed).or_else(|| {
        let old_value = parsed
            .get("oldString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let new_value = parsed
            .get("newString")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        format_patch_preview(old_value, new_value)
    });

    match preview {
        Some(preview) => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m\n{preview}"),
        None => format!("{icon} \x1b[1;33m📝 Edited {path}{suffix}\x1b[0m"),
    }
}

#[cfg(test)]
pub(crate) fn format_glob_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if filenames.is_empty() {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files")
    } else {
        format!("{icon} \x1b[38;5;245mglob_search\x1b[0m matched {num_files} files\n{filenames}")
    }
}

#[cfg(test)]
pub(crate) fn format_grep_result(icon: &str, parsed: &serde_json::Value) -> String {
    let num_matches = parsed
        .get("numMatches")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let num_files = parsed
        .get("numFiles")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let content = parsed
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let filenames = parsed
        .get("filenames")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|value| value.as_str())
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let summary = format!(
        "{icon} \x1b[38;5;245mgrep_search\x1b[0m {num_matches} matches across {num_files} files"
    );
    if !content.trim().is_empty() {
        format!(
            "{summary}\n{}",
            truncate_output_for_display(
                content,
                TOOL_OUTPUT_DISPLAY_MAX_LINES,
                TOOL_OUTPUT_DISPLAY_MAX_CHARS,
            )
        )
    } else if !filenames.is_empty() {
        format!("{summary}\n{filenames}")
    } else {
        summary
    }
}

#[cfg(test)]
pub(crate) fn format_generic_tool_result(icon: &str, name: &str, parsed: &serde_json::Value) -> String {
    let rendered_output = match parsed {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(parsed).unwrap_or_else(|_| parsed.to_string())
        }
        _ => parsed.to_string(),
    };
    let preview = truncate_output_for_display(
        &rendered_output,
        TOOL_OUTPUT_DISPLAY_MAX_LINES,
        TOOL_OUTPUT_DISPLAY_MAX_CHARS,
    );

    if preview.is_empty() {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m")
    } else if preview.contains('\n') {
        format!("{icon} \x1b[38;5;245m{name}\x1b[0m\n{preview}")
    } else {
        format!("{icon} \x1b[38;5;245m{name}:\x1b[0m {preview}")
    }
}

pub(crate) fn summarize_tool_payload(payload: &str) -> String {
    let compact = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value.to_string(),
        Err(_) => payload.trim().to_string(),
    };
    truncate_for_summary(&compact, 96)
}

pub(crate) fn truncate_for_summary(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
pub(crate) fn truncate_output_for_display(content: &str, max_lines: usize, max_chars: usize) -> String {
    let original = content.trim_end_matches('\n');
    if original.is_empty() {
        return String::new();
    }

    let mut preview_lines = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated = false;

    for (index, line) in original.lines().enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }

        let newline_cost = usize::from(!preview_lines.is_empty());
        let available = max_chars.saturating_sub(used_chars + newline_cost);
        if available == 0 {
            truncated = true;
            break;
        }

        let line_chars = line.chars().count();
        if line_chars > available {
            preview_lines.push(line.chars().take(available).collect::<String>());
            truncated = true;
            break;
        }

        preview_lines.push(line.to_string());
        used_chars += newline_cost + line_chars;
    }

    let mut preview = preview_lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(DISPLAY_TRUNCATION_NOTICE);
    }
    preview
}

pub(crate) fn push_output_block(
    block: OutputContentBlock,
    out: &mut (impl Write + ?Sized),
    events: &mut Vec<AssistantEvent>,
    pending_tool: &mut Option<(String, String, String)>,
    streaming_tool_input: bool,
) -> Result<(), RuntimeError> {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                let rendered = TerminalRenderer::new().markdown_to_ansi(&text);
                write!(out, "{rendered}")
                    .and_then(|()| out.flush())
                    .map_err(|error| RuntimeError::new(error.to_string()))?;
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            // During streaming, the initial content_block_start has an empty input ({}).
            // The real input arrives via input_json_delta events. In
            // non-streaming responses, preserve a legitimate empty object.
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            *pending_tool = Some((id, name, initial_input));
        }
        OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
    }
    Ok(())
}

pub(crate) fn response_to_events(
    response: MessageResponse,
    out: &mut (impl Write + ?Sized),
) -> Result<Vec<AssistantEvent>, RuntimeError> {
    let mut events = Vec::new();
    let mut pending_tool = None;

    for block in response.content {
        push_output_block(block, out, &mut events, &mut pending_tool, false)?;
        if let Some((id, name, input)) = pending_tool.take() {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(TokenUsage {
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
        cache_read_input_tokens: response.usage.cache_read_input_tokens,
    }));
    events.push(AssistantEvent::MessageStop);
    Ok(events)
}

#[cfg(test)]
mod read_summary_tests {
    use super::tool_result_summary;

    /// Real shape emitted by runtime::file_ops::read_file — nested {"file":{...}}
    /// with numLines + totalLines + content. This is the case that was reporting
    /// "0 lines" before the fix.
    #[test]
    fn real_runtime_payload_reports_line_count() {
        let payload = r#"{
            "kind": "text",
            "file": {
                "filePath": "/tmp/SKILL.md",
                "content": "line 1\nline 2\nline 3",
                "numLines": 3,
                "startLine": 1,
                "totalLines": 3
            }
        }"#;
        let summary = tool_result_summary("read_file", payload, false);
        assert_eq!(summary, "3 lines");
    }

    /// Partial read (offset/limit applied) — shows "N of M lines".
    #[test]
    fn partial_read_shows_n_of_m() {
        let payload = r#"{
            "kind": "text",
            "file": {
                "filePath": "/tmp/big.txt",
                "content": "a\nb\nc",
                "numLines": 3,
                "startLine": 10,
                "totalLines": 500
            }
        }"#;
        let summary = tool_result_summary("read_file", payload, false);
        assert_eq!(summary, "3 of 500 lines");
    }

    /// Singular form.
    #[test]
    fn single_line_uses_singular() {
        let payload = r#"{
            "kind": "text",
            "file": {
                "filePath": "/tmp/x",
                "content": "only one",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1
            }
        }"#;
        let summary = tool_result_summary("read_file", payload, false);
        assert_eq!(summary, "1 line");
    }

    /// Flat-shape fallback (no "file" wrapper) — MCP or legacy payloads.
    #[test]
    fn flat_legacy_shape_still_works() {
        let payload = r#"{"content": "one\ntwo", "lines": 2}"#;
        let summary = tool_result_summary("read_file", payload, false);
        assert_eq!(summary, "2 lines");
    }

    /// No numeric field, no content — defaults cleanly to 0 (rare error path).
    #[test]
    fn empty_payload_reports_zero() {
        let summary = tool_result_summary("read_file", "{}", false);
        assert_eq!(summary, "0 lines");
    }

    /// Read tool alias should behave the same.
    #[test]
    fn read_alias_matches() {
        let payload = r#"{"file":{"numLines":5,"totalLines":5,"content":"a\nb\nc\nd\ne"}}"#;
        let summary = tool_result_summary("Read", payload, false);
        assert_eq!(summary, "5 lines");
    }
}
