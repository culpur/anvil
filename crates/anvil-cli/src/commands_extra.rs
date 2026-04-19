// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

//! Extra slash command handlers for `impl LiveCli`.
//!
//! Contains: `run_mcp_command`, `run_productivity_command`,
//! `run_history_archive_command`.
//!
//! Extracted from `main.rs` to reduce file size.  No behaviour is changed.

use std::fs;

use crate::{
    format_history_archive_list, extract_summary_from_archive, LiveCli,
};

impl LiveCli {
    /// `/mcp [list|status|tools <server>]` — MCP server management.
    pub(crate) fn run_mcp_command(&self, action: Option<&str>) -> String {
        // Read MCP server config from settings.json
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let settings_path = home.as_ref().map(|h| h.join(".anvil").join("settings.json"));
        let servers: Vec<String> = settings_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("mcpServers").cloned())
            .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
            .unwrap_or_default();
        let count = servers.len();

        match action.unwrap_or("list").split_whitespace().next().unwrap_or("list") {
            "list" | "status" => {
                if servers.is_empty() {
                    return "No MCP servers configured.\n\n\
                        Configure MCP servers in ~/.anvil/settings.json under \"mcpServers\".\n\
                        Example:\n  \"mcpServers\": {\n    \"filesystem\": {\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"@modelcontextprotocol/server-filesystem\", \"/path\"]\n    }\n  }"
                        .to_string();
                }
                let mut lines = vec![format!("🔌 MCP Servers ({count} configured):")];
                for name in &servers {
                    lines.push(format!("  🟢  {name}"));
                }
                lines.push(String::new());
                lines.push("MCP tools are auto-discovered at startup and available to the AI.".to_string());
                lines.push("Configure in ~/.anvil/settings.json under \"mcpServers\".".to_string());
                lines.join("\n")
            }
            other => format!("Unknown MCP action: {other}\nUsage: /mcp [list|status]"),
        }
    }

    /// `/productivity` — show session productivity stats.
    pub(crate) fn run_productivity_command(&self) -> String {
        use std::process::Command;

        let diff_output = Command::new("git")
            .args(["diff", "--shortstat"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        let mut ins: u32 = 0;
        let mut del: u32 = 0;
        for part in diff_output.split(',') {
            let part = part.trim();
            if part.contains("insertion") {
                if let Some(n) = part.split_whitespace().next() { ins = n.parse().unwrap_or(0); }
            } else if part.contains("deletion")
                && let Some(n) = part.split_whitespace().next() { del = n.parse().unwrap_or(0); }
        }

        let files_changed = Command::new("git")
            .args(["diff", "--name-only"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map_or(0, |s| s.lines().filter(|l| !l.is_empty()).count());

        let session = self.runtime.session();
        let total_tokens = session.messages.iter()
            .filter_map(|m| m.usage.as_ref())
            .map(|u| u.input_tokens + u.output_tokens)
            .sum::<u32>();

        let efficiency = if ins + del > 0 {
            format!("{:.0} tokens/line", f64::from(total_tokens) / f64::from(ins + del))
        } else {
            "—".to_string()
        };

        format!(
            "📊 Session Productivity\n\
             ─────────────────────────\n\
             📝 Lines added:    +{ins}\n\
             📝 Lines removed:  -{del}\n\
             📁 Files changed:  {files_changed}\n\
             🎯 Token efficiency: {efficiency}\n\
             💰 Total tokens:   {total_tokens}\n\
             ─────────────────────────\n\
             Tip: Use the 'academic' or 'dashboard' status line preset\n\
             to see live productivity in the status bar."
        )
    }

    /// Handle `/history-archive [search <q> | view <id>]` commands.
    pub(crate) fn run_history_archive_command(&self, action: Option<&str>) -> String {
        let archiver = &self.history_archiver;

        match action {
            None => format_history_archive_list(&archiver.list_archives()),

            Some("stats" | "summary") => {
                let entries = archiver.list_archives();
                let total = entries.len();
                let total_messages: usize = entries.iter().map(|e| e.message_count).sum();
                let models: std::collections::HashMap<String, usize> = {
                    let mut m = std::collections::HashMap::new();
                    for e in &entries {
                        *m.entry(e.model.clone()).or_insert(0) += 1;
                    }
                    m
                };
                let mut model_lines: Vec<String> = models.iter().map(|(k, v)| format!("  {k}: {v} sessions")).collect();
                model_lines.sort();
                format!(
                    "📊 Session History Stats\n\
                     ─────────────────────────\n\
                     📁 Total archived sessions: {total}\n\
                     💬 Total messages: {total_messages}\n\
                     🤖 Models used:\n{}\n\
                     ─────────────────────────\n\
                     Tip: /history-archive search <query> to search\n\
                     Tip: /history-archive view <id> to read a session",
                    model_lines.join("\n")
                )
            }

            Some(arg) if arg.starts_with("search ") => {
                let query = arg["search ".len()..].trim();
                if query.is_empty() {
                    return "Usage: /history-archive search <query>".to_string();
                }
                if !self.qmd.is_enabled() {
                    return "QMD is not available — install it at /opt/homebrew/bin/qmd or ensure it is on PATH.".to_string();
                }
                let results = self.qmd.search_collection("anvil-history", query, 5, 0.3);
                if results.is_empty() {
                    format!("No history results for: {query}")
                } else {
                    let mut lines = vec![format!("History search: {query}\n")];
                    for (i, r) in results.iter().enumerate() {
                        lines.push(format!("  {}. {} ({:.2})", i + 1, r.file, r.score));
                        if !r.snippet.is_empty() {
                            for line in r.snippet.lines().take(3) {
                                lines.push(format!("     {line}"));
                            }
                        }
                        lines.push(String::new());
                    }
                    lines.join("\n")
                }
            }

            Some(arg) if arg.starts_with("view ") => {
                let target = arg["view ".len()..].trim();
                if target.is_empty() {
                    return "Usage: /history-archive view <session-id>".to_string();
                }
                // Find the first archive whose filename or session_id contains the target.
                let entries = archiver.list_archives();
                let found = entries.iter().find(|e| {
                    e.session_id.contains(target)
                        || e.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.contains(target))
                });
                match found {
                    Some(entry) => match fs::read_to_string(&entry.path) {
                        Ok(content) => {
                            // Print a concise header + the summary section only.
                            let summary = extract_summary_from_archive(&content);
                            format!(
                                "Archive: {}\nModel:   {}\nMessages: {}\nPath:    {}\n\n{}",
                                entry.session_id,
                                entry.model,
                                entry.message_count,
                                entry.path.display(),
                                summary.unwrap_or_else(|| "(no summary)".to_string()),
                            )
                        }
                        Err(e) => format!("Could not read archive: {e}"),
                    },
                    None => format!("No archive found matching: {target}"),
                }
            }

            Some(unknown) => format!(
                "Unknown sub-command: {unknown}\nUsage: /history-archive [search <query> | view <session-id>]"
            ),
        }
    }

    /// `/knowledge [review|accept <N>|reject <N>|list]` — manage knowledge nominations.
    pub(crate) fn run_knowledge_command(&self, action: Option<&str>) -> String {
        let store = runtime::nominations::NominationStore::new();

        match action.map(str::trim).unwrap_or("review") {
            "review" | "list" | "" => store.format_pending(),

            arg if arg.starts_with("accept ") => {
                let n_str = arg["accept ".len()..].trim();
                let Ok(n) = n_str.parse::<usize>() else {
                    return "Usage: /knowledge accept <number>".to_string();
                };
                let pending = store.list(Some(runtime::nominations::NominationStatus::Pending));
                if n == 0 || n > pending.len() {
                    return format!("Invalid nomination number. {} pending.", pending.len());
                }
                let nom = &pending[n - 1];
                match store.accept(&nom.id, "CLAUDE.md") {
                    Ok(()) => {
                        // Append to CLAUDE.md in current directory
                        let claude_md = std::env::current_dir()
                            .unwrap_or_default()
                            .join("CLAUDE.md");
                        let entry = format!("\n- {}\n", nom.content);
                        let _ = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&claude_md)
                            .and_then(|mut f| {
                                use std::io::Write;
                                f.write_all(entry.as_bytes())
                            });
                        format!(
                            "✓ Accepted: {}\n  Promoted to CLAUDE.md",
                            nom.content
                        )
                    }
                    Err(e) => format!("Error accepting nomination: {e}"),
                }
            }

            arg if arg.starts_with("reject ") => {
                let n_str = arg["reject ".len()..].trim();
                let Ok(n) = n_str.parse::<usize>() else {
                    return "Usage: /knowledge reject <number>".to_string();
                };
                let pending = store.list(Some(runtime::nominations::NominationStatus::Pending));
                if n == 0 || n > pending.len() {
                    return format!("Invalid nomination number. {} pending.", pending.len());
                }
                let nom = &pending[n - 1];
                match store.reject(&nom.id) {
                    Ok(()) => format!("✗ Rejected: {}", nom.content),
                    Err(e) => format!("Error rejecting nomination: {e}"),
                }
            }

            other => format!(
                "Unknown action: {other}\nUsage: /knowledge [review|accept <N>|reject <N>]"
            ),
        }
    }

    /// `/daily [date]` — view daily summary.
    pub(crate) fn run_daily_command(&self, _date: Option<&str>) -> String {
        // Phase 4 will implement full daily summaries
        // For now, show nominations count and basic session info
        let store = runtime::nominations::NominationStore::new();
        let pending = store.pending_count();
        let session = self.runtime.session();
        let msg_count = session.messages.len();
        let total_tokens: u32 = session.messages.iter()
            .filter_map(|m| m.usage.as_ref())
            .map(|u| u.input_tokens + u.output_tokens)
            .sum();

        format!(
            "📊 Daily Summary\n\
             ─────────────────────────\n\
             📝 Messages this session: {msg_count}\n\
             💬 Tokens used: {total_tokens}\n\
             📋 Pending nominations: {pending}\n\
             ─────────────────────────\n\
             Full daily summaries with task reconciliation coming in v2.2.5.\n\
             Use /knowledge review to review pending nominations."
        )
    }
}
