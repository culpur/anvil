// Edition 2024: env::set_var/remove_var require unsafe
#![allow(unsafe_code)]

//! Utility slash command handlers for `impl LiveCli`.
//!
//! Contains: `format_history`, `run_context`.
//!
//! Extracted from `main.rs` to reduce file size.  No behaviour is changed.

use std::fs;
use std::path::PathBuf;

use runtime::{ContentBlock, MessageRole};

use crate::{cmd_static, LiveCli};

impl LiveCli {
    /// `/history [all]` — display conversation messages.
    pub(crate) fn format_history(&self, show_all: bool) -> String {
        let messages = &self.runtime.session().messages;
        let limit = if show_all { messages.len() } else { 20 };
        let start = messages.len().saturating_sub(limit);
        let visible = &messages[start..];
        if visible.is_empty() {
            return "No conversation history yet.".to_string();
        }
        let mut lines = vec![format!(
            "Conversation history ({} of {} messages):",
            visible.len(),
            messages.len()
        )];
        for (i, msg) in visible.iter().enumerate() {
            let index = start + i;
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
                MessageRole::Tool => "tool",
            };
            // Render the first text block as a short snippet.
            let snippet: String = msg
                .blocks
                .iter()
                .find_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.chars().take(100).collect())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "<non-text content>".to_string());
            let ellipsis = if snippet.len() == 100 { "..." } else { "" };
            lines.push(format!("[{index}] {role}: \"{snippet}{ellipsis}\""));
        }
        lines.join("\n")
    }

    /// `/context [path]` — add a file to per-session context or list context files.
    pub(crate) fn run_context(&mut self, path: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
        let Some(path_str) = path else {
            if self.context_files.is_empty() {
                return Ok("No context files added this session.".to_string());
            }
            let mut lines = vec!["Context files:".to_string()];
            for p in &self.context_files {
                lines.push(format!("  {}", p.display()));
            }
            return Ok(lines.join("\n"));
        };

        let path_buf = PathBuf::from(path_str);
        let content = fs::read_to_string(&path_buf)
            .map_err(|e| format!("Failed to read {path_str}: {e}"))?;
        let injection = format!(
            "<system-reminder>File context: {}\n{}</system-reminder>",
            path_buf.display(),
            content
        );
        // Inject as a user message so the model sees it on the next turn.
        self.runtime.inject_user_message(&injection);
        self.context_files.push(path_buf.clone());
        Ok(format!("Added to context: {}", path_buf.display()))
    }

    /// `/undo` — show unstaged changes and offer to revert them.
    /// Returns the output text. Interactive confirmation is done inline.
    #[allow(clippy::unused_self)]
    pub(crate) fn run_undo(&self) -> Result<String, Box<dyn std::error::Error>> {
        cmd_static::run_undo()
    }
}
