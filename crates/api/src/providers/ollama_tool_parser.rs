//! Multi-format Ollama tool-call parser.
//!
//! Ollama exposes an OpenAI-compatible endpoint but the underlying models emit
//! tool-call intent in several different formats depending on their training:
//!
//! | Format | Description |
//! |--------|-------------|
//! | 1 | Standard OpenAI `tool_calls` array in the response message |
//! | 2 | In-band `<tool_use>…</tool_use>` XML tags inside the text |
//! | 3 | JSON in a markdown code fence (` ```json … ``` `) |
//! | 4 | Natural-language prose describing a file write, followed by a code block |
//!
//! The parser here is applied **after** the primary OpenAI-format path has
//! already been tried.  If the primary path found real `tool_calls` this
//! module is not consulted.  If the primary path found none, we scan the raw
//! text content to see whether any of the alternate formats are present.
//!
//! The parser is deliberately conservative: it only extracts tool calls when
//! it can identify a tool name that matches one of the known Anvil built-in
//! tool names.  Unrecognised JSON blobs are ignored.

use serde_json::{json, Value};

/// A tool-call intent extracted from model text output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedToolCall {
    /// Tool name (e.g. `write_file`, `bash`).
    pub name: String,
    /// Parsed input arguments.  Always a JSON object.
    pub input: Value,
    /// How the call was found.
    pub source: ParseSource,
}

/// Records which parsing strategy found the tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseSource {
    /// `<tool_use>…</tool_use>` XML tag.
    XmlTag,
    /// JSON in a ` ```json … ``` ` code fence.
    JsonFence,
    /// Natural-language prose + code block heuristic.
    NaturalLanguage,
}

/// A prose-only model response that describes a file write without executing
/// one — the "silent failure" pattern we want to detect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilentWriteDetection {
    /// Short description of what the model claimed to do, for use in the
    /// warning message shown to the user.
    pub claimed_action: String,
}

/// Result of running the full parser over a model text response.
#[derive(Debug, Default)]
pub struct OllamaParseResult {
    /// Tool calls extracted from inline formats.  Empty when the primary
    /// OpenAI `tool_calls` path already handled everything.
    pub tool_calls: Vec<ExtractedToolCall>,
    /// Set when the text contains prose that _describes_ a file write but no
    /// tool call was actually found anywhere.
    pub silent_write: Option<SilentWriteDetection>,
}

/// Known Anvil tool names that we accept when scanning inline tool calls.
const KNOWN_TOOLS: &[&str] = &[
    "write_file",
    "read_file",
    "edit_file",
    "bash",
    "glob_search",
    "grep_search",
    "web_search",
    "task_manager",
];

// ─── Public entry point ───────────────────────────────────────────────────────

/// Attempt to extract tool calls from `text` using all supported alternate
/// formats.  The caller should pass `had_structured_tool_calls = true` when
/// the primary OpenAI `tool_calls` path already found at least one call, in
/// which case this function returns an empty result immediately.
#[must_use]
pub fn parse_ollama_text_for_tool_calls(
    text: &str,
    had_structured_tool_calls: bool,
) -> OllamaParseResult {
    if had_structured_tool_calls {
        return OllamaParseResult::default();
    }

    let mut result = OllamaParseResult::default();

    // Format 2: <tool_use>…</tool_use>
    result.tool_calls.extend(parse_xml_tool_use_tags(text));

    // Format 3: ```json … ``` fences
    if result.tool_calls.is_empty() {
        result.tool_calls.extend(parse_json_fences(text));
    }

    // Format 4: natural-language heuristic (only if still empty)
    if result.tool_calls.is_empty() {
        result.tool_calls.extend(parse_natural_language(text));
    }

    // Fail-loud detection: if we still have no tool calls but the text
    // contains phrases that suggest the model believed it was writing a file,
    // record a silent-write detection so the UI can warn the user.
    if result.tool_calls.is_empty() && text_describes_file_write(text) {
        result.silent_write = Some(SilentWriteDetection {
            claimed_action: extract_claimed_action(text),
        });
    }

    result
}

// ─── Format 2: XML <tool_use> tags ────────────────────────────────────────────

fn parse_xml_tool_use_tags(text: &str) -> Vec<ExtractedToolCall> {
    let mut calls = Vec::new();
    let mut search_from = 0;

    while let Some(open_start) = text[search_from..].find("<tool_use>") {
        let open_start = search_from + open_start;
        let content_start = open_start + "<tool_use>".len();
        if let Some(close_offset) = text[content_start..].find("</tool_use>") {
            let content = text[content_start..content_start + close_offset].trim();
            search_from = content_start + close_offset + "</tool_use>".len();
            if let Some(call) = try_parse_json_blob(content, ParseSource::XmlTag) {
                calls.push(call);
            }
        } else {
            break;
        }
    }

    calls
}

// ─── Format 3: JSON in markdown code fences ───────────────────────────────────

fn parse_json_fences(text: &str) -> Vec<ExtractedToolCall> {
    let mut calls = Vec::new();
    let mut search_from = 0;

    // Match both ```json and plain ```
    while search_from < text.len() {
        // Find the next opening fence.
        let fence_pos = find_opening_fence(text, search_from);
        let Some((fence_start, content_start)) = fence_pos else {
            break;
        };

        // Find matching closing fence.
        let Some(close_offset) = text[content_start..].find("\n```") else {
            search_from = fence_start + 3;
            continue;
        };

        let content = text[content_start..content_start + close_offset].trim();
        search_from = content_start + close_offset + 4; // skip \n```

        if let Some(call) = try_parse_json_blob(content, ParseSource::JsonFence) {
            calls.push(call);
        }
    }

    calls
}

/// Find the next ` ```json ` or ` ``` ` opening fence starting from `from`.
/// Returns `(fence_start, content_start)` where `content_start` is immediately
/// after the opening fence line.
fn find_opening_fence(text: &str, from: usize) -> Option<(usize, usize)> {
    let slice = &text[from..];
    // Try ```json first, then plain ```
    for prefix in &["```json\n", "```\n"] {
        if let Some(pos) = slice.find(prefix) {
            return Some((from + pos, from + pos + prefix.len()));
        }
    }
    None
}

// ─── Format 4: natural-language + code block ──────────────────────────────────

/// Patterns that suggest the model is describing a file-write operation.
const WRITE_INTENT_PHRASES: &[&str] = &[
    "i'll save this",
    "i will save this",
    "saving the file",
    "i'll write this",
    "i will write this",
    "writing the file",
    "creating the file",
    "i'll create",
    "i will create",
    "here is the file",
    "here's the file",
    "i've created the file",
    "i've written the file",
    "i've saved the file",
    "file has been created",
    "file has been written",
    "file has been saved",
];

/// Backtick-quoted path patterns: `path/to/file` in text.
fn extract_backtick_path(text: &str) -> Option<String> {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '`' {
            let mut path = String::new();
            for inner in chars.by_ref() {
                if inner == '`' {
                    break;
                }
                path.push(inner);
            }
            // A plausible file path has a '.' or '/' somewhere.
            if path.contains('.') || path.contains('/') {
                return Some(path);
            }
        }
    }
    None
}

fn parse_natural_language(text: &str) -> Vec<ExtractedToolCall> {
    let lower = text.to_ascii_lowercase();

    // Check for write intent phrase.
    let has_write_intent = WRITE_INTENT_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase));

    if !has_write_intent {
        return Vec::new();
    }

    // Try to extract a path from a backtick-quoted span.
    let Some(path) = extract_backtick_path(text) else {
        return Vec::new();
    };

    // Try to extract content from the first code block after the path mention.
    let content = extract_first_code_block_content(text).unwrap_or_default();
    if content.is_empty() {
        return Vec::new();
    }

    vec![ExtractedToolCall {
        name: "write_file".to_string(),
        input: json!({
            "path": path,
            "content": content,
        }),
        source: ParseSource::NaturalLanguage,
    }]
}

fn extract_first_code_block_content(text: &str) -> Option<String> {
    // Find the first ``` that starts a block.
    let fence_start = text.find("```")?;
    let after_fence = &text[fence_start + 3..];
    // Skip optional language tag on the opening line.
    let content_start = after_fence.find('\n').map_or(0, |pos| pos + 1);
    let after_tag = &after_fence[content_start..];
    // Find closing ```.
    let close = after_tag.find("```")?;
    Some(after_tag[..close].trim_end_matches('\n').to_string())
}

// ─── JSON blob normalization ───────────────────────────────────────────────────

/// Try to parse `blob` as JSON and map it to a `write_file` or other known
/// tool call.  Returns `None` if the blob does not look like a valid tool call.
fn try_parse_json_blob(blob: &str, source: ParseSource) -> Option<ExtractedToolCall> {
    let value: Value = serde_json::from_str(blob).ok()?;
    let obj = value.as_object()?;

    // Canonical form: {"name": "...", "args": {...}} or {"name": "...", "input": {...}}
    // Also accept: {"tool": "...", "args": {...}}
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .and_then(Value::as_str)?
        .to_string();

    // Reject names that are not in our known-tools list to avoid false positives.
    if !KNOWN_TOOLS.iter().any(|known| *known == name) {
        return None;
    }

    let input = obj
        .get("args")
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("arguments"))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // arguments may arrive as a JSON string (OpenAI function-call encoding).
    let input = if let Value::String(s) = &input {
        serde_json::from_str(s).unwrap_or_else(|_| json!({ "raw": s }))
    } else {
        input
    };

    Some(ExtractedToolCall { name, input, source })
}

// ─── Fail-loud prose detection ────────────────────────────────────────────────

/// Phrases that strongly suggest the model "believes" it wrote a file.
const CLAIMED_WRITE_PHRASES: &[&str] = &[
    "i wrote",
    "i have written",
    "i saved",
    "i have saved",
    "i created the file",
    "i have created the file",
    "the file was written",
    "the file was saved",
    "the file was created",
    "file created at",
    "file written to",
    "file saved to",
    "i've created the file",
    "i've written the file",
    "i've saved the file",
];

fn text_describes_file_write(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    CLAIMED_WRITE_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

fn extract_claimed_action(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for phrase in CLAIMED_WRITE_PHRASES {
        if let Some(pos) = lower.find(phrase) {
            // Return the original-case snippet starting at that position.
            let snippet = &text[pos..];
            let end = snippet.find('\n').unwrap_or(snippet.len()).min(120);
            return snippet[..end].to_string();
        }
    }
    "described a file write".to_string()
}

/// Build the warning message shown in the UI when a silent write is detected.
#[must_use]
pub fn silent_write_warning(detection: &SilentWriteDetection) -> String {
    format!(
        "WARNING: The model described writing a file but no tool call was executed.\n\
         Claimed action: \"{}\"\n\
         This often happens with Ollama models that do not fully support\n\
         structured tool calls. The file was NOT saved to disk.",
        detection.claimed_action
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Format 2: XML <tool_use> tags ──────────────────────────────────────────

    #[test]
    fn format2_xml_tool_use_tag_write_file() {
        let text = r#"Sure, I'll write the file for you.
<tool_use>
{"name": "write_file", "args": {"path": "/tmp/hello.py", "content": "print('hello')"}}
</tool_use>
Done."#;

        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "write_file");
        assert_eq!(result.tool_calls[0].source, ParseSource::XmlTag);
        assert_eq!(result.tool_calls[0].input["path"], "/tmp/hello.py");
        assert_eq!(result.tool_calls[0].input["content"], "print('hello')");
        assert!(result.silent_write.is_none());
    }

    #[test]
    fn format2_xml_tool_use_tag_bash() {
        let text = r#"Running the command:
<tool_use>
{"name": "bash", "args": {"command": "echo hello"}}
</tool_use>"#;

        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "bash");
        assert_eq!(result.tool_calls[0].input["command"], "echo hello");
    }

    #[test]
    fn format2_xml_tool_use_with_input_key_instead_of_args() {
        let text = r#"<tool_use>{"name": "write_file", "input": {"path": "a.txt", "content": "hi"}}</tool_use>"#;
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].input["path"], "a.txt");
    }

    #[test]
    fn format2_xml_tool_use_unknown_tool_is_ignored() {
        // "delete_everything" is not in KNOWN_TOOLS — must not produce a call.
        let text = r#"<tool_use>{"name": "delete_everything", "args": {}}</tool_use>"#;
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.tool_calls.is_empty());
    }

    // ── Format 3: JSON in markdown code fences ─────────────────────────────────

    #[test]
    fn format3_json_fence_write_file() {
        let text = "I'll use the write_file tool:\n```json\n{\"name\": \"write_file\", \"args\": {\"path\": \"script.sh\", \"content\": \"#!/bin/bash\"}}\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "write_file");
        assert_eq!(result.tool_calls[0].source, ParseSource::JsonFence);
        assert_eq!(result.tool_calls[0].input["path"], "script.sh");
    }

    #[test]
    fn format3_json_fence_tool_key() {
        // Some models use "tool" instead of "name".
        let text = "```json\n{\"tool\": \"bash\", \"args\": {\"command\": \"ls -la\"}}\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "bash");
    }

    #[test]
    fn format3_plain_fence_write_file() {
        let text = "```\n{\"name\": \"write_file\", \"args\": {\"path\": \"out.txt\", \"content\": \"data\"}}\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].source, ParseSource::JsonFence);
    }

    #[test]
    fn format3_fence_non_json_content_is_ignored() {
        // A plain code fence with Python code — must not produce a call.
        let text = "```python\ndef hello():\n    print('hi')\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.tool_calls.is_empty());
    }

    // ── Format 4: natural-language heuristic ───────────────────────────────────

    #[test]
    fn format4_natural_language_write_intent() {
        let text = "I'll save this to `hello.py`:\n```\nprint('hello world')\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "write_file");
        assert_eq!(result.tool_calls[0].source, ParseSource::NaturalLanguage);
        assert_eq!(result.tool_calls[0].input["path"], "hello.py");
        assert_eq!(result.tool_calls[0].input["content"], "print('hello world')");
    }

    #[test]
    fn format4_will_write_variant() {
        let text = "I will write this to `scripts/deploy.sh`:\n```bash\n#!/bin/bash\necho done\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].input["path"], "scripts/deploy.sh");
    }

    #[test]
    fn format4_no_code_block_does_not_produce_call() {
        let text = "I'll save this to `output.txt` shortly.";
        let result = parse_ollama_text_for_tool_calls(text, false);
        // No code block to extract content from — no call.
        assert!(result.tool_calls.is_empty());
    }

    // ── Fail-loud: silent write detection ─────────────────────────────────────

    #[test]
    fn fail_loud_detects_claimed_file_creation() {
        let text = "I've created the file at `/tmp/result.txt` with the requested content.";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.tool_calls.is_empty());
        assert!(result.silent_write.is_some());
        let warning = silent_write_warning(result.silent_write.as_ref().unwrap());
        assert!(warning.contains("WARNING"));
        assert!(warning.contains("NOT saved to disk"));
    }

    #[test]
    fn fail_loud_detects_i_wrote_phrase() {
        let text = "I wrote the file successfully. The content has been saved.";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.silent_write.is_some());
    }

    #[test]
    fn fail_loud_detects_file_saved_to_phrase() {
        let text = "The configuration has been file saved to /etc/app/config.json.";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.silent_write.is_some());
    }

    #[test]
    fn no_false_positive_on_unrelated_text() {
        let text = "Here is how to use the write_file tool. Call it with a path and content.";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert!(result.tool_calls.is_empty());
        assert!(result.silent_write.is_none());
    }

    // ── had_structured_tool_calls guard ───────────────────────────────────────

    #[test]
    fn skips_all_parsing_when_structured_tool_calls_already_found() {
        // Even if the text has an XML tag, we skip when structured calls exist.
        let text = "<tool_use>{\"name\": \"write_file\", \"args\": {\"path\": \"x\", \"content\": \"y\"}}</tool_use>";
        let result = parse_ollama_text_for_tool_calls(text, true);
        assert!(result.tool_calls.is_empty());
        assert!(result.silent_write.is_none());
    }

    // ── Multiple calls ─────────────────────────────────────────────────────────

    #[test]
    fn format2_multiple_xml_tags_produce_multiple_calls() {
        let text = r#"<tool_use>{"name": "write_file", "args": {"path": "a.txt", "content": "a"}}</tool_use>
<tool_use>{"name": "write_file", "args": {"path": "b.txt", "content": "b"}}</tool_use>"#;
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].input["path"], "a.txt");
        assert_eq!(result.tool_calls[1].input["path"], "b.txt");
    }

    // ── arguments as JSON string (OpenAI function-call encoding) ──────────────

    #[test]
    fn format3_arguments_as_json_string() {
        let text = "```json\n{\"name\": \"bash\", \"arguments\": \"{\\\"command\\\": \\\"ls\\\"}\"}\n```\n";
        let result = parse_ollama_text_for_tool_calls(text, false);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].input["command"], "ls");
    }
}
