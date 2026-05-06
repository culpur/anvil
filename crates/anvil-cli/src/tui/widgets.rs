/// Completion widget data: deep completion via suggest_completions, Ollama model cache, clipboard.
use super::state::{CompletionItem, CompletionPopup};
use super::completion::TuiCompletionContext;

// ─── Slash command completions (Phase 2 — deep hierarchical) ─────────────────

/// Convert a `commands::Completion` into a `CompletionItem` for the popup.
///
/// Free-text placeholder entries (`<hint>`) are passed through as-is — the
/// renderer will apply DIM styling when the text starts with `<`.
fn completion_to_item(c: commands::Completion) -> CompletionItem {
    let is_free_text = c.text.starts_with('<') && c.text.ends_with('>');
    CompletionItem {
        insert: c.text,
        hint: c.description,
        is_header: false,
        is_free_text,
    }
}

/// Update the completion popup for the given input string.
///
/// Delegates entirely to [`commands::suggest_completions`] with a
/// [`TuiCompletionContext`] that resolves dynamic enum sources from the live
/// environment (Ollama model cache, MCP config, plugin manager …).
pub(super) fn update_completions(input: &str) -> CompletionPopup {
    if input.is_empty() || !input.starts_with('/') {
        return CompletionPopup::default();
    }

    let ctx = TuiCompletionContext::new();
    let completions = commands::suggest_completions(input, &ctx);

    if completions.is_empty() {
        return CompletionPopup::default();
    }

    CompletionPopup {
        visible: true,
        matches: completions.into_iter().map(completion_to_item).collect(),
        selected: 0,
        view_offset: 0,
    }
}

/// Returns `true` if appending a space to `input` would produce further
/// completions.  Used by `tab_complete` to decide whether to add a trailing
/// space after accepting a completion (non-leaf = add space; leaf = don't).
pub(super) fn has_further_completions(input: &str) -> bool {
    if !input.ends_with(' ') {
        let with_space = format!("{input} ");
        !commands::suggest_completions(&with_space, &TuiCompletionContext::new()).is_empty()
    } else {
        !commands::suggest_completions(input, &TuiCompletionContext::new()).is_empty()
    }
}

// ─── Category grouping helper ─────────────────────────────────────────────────

/// Returns the category title for a root slash command, or `None` if the
/// command is not found in the spec registry.
///
/// Exposed for use by the popup renderer to inject category header lines.
#[allow(dead_code)] // helper for popup renderer — used by Phase 3 web completion
pub(super) fn category_for_command(name: &str) -> Option<&'static str> {
    let bare = name.trim_start_matches('/');
    commands::slash_command_specs()
        .iter()
        .find(|s| s.name == bare || s.aliases.contains(&bare))
        .map(|s| s.category.title())
}

// ─── Ollama model cache ───────────────────────────────────────────────────────

static OLLAMA_MODEL_CACHE: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

pub fn init_ollama_model_cache() {
    let _ = OLLAMA_MODEL_CACHE.get_or_init(|| {
        let ollama_url = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        let output = std::process::Command::new("curl")
            .args(["-s", "--max-time", "2", &format!("{ollama_url}/api/tags")])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&o.stdout)
                    && let Some(arr) = val.get("models").and_then(|m| m.as_array()) {
                        return arr.iter().filter_map(|m| {
                            let name = m.get("name").and_then(|n| n.as_str())?;
                            let size = m.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                            let gb = size / 1_000_000_000.0;
                            Some((name.to_string(), format!("{gb:.1}GB")))
                        }).collect();
                    }
                Vec::new()
            }
            _ => Vec::new(),
        }
    });
}

pub(super) fn cached_ollama_models() -> Vec<(String, String)> {
    OLLAMA_MODEL_CACHE.get().cloned().unwrap_or_default()
}

// ─── Clipboard image paste ────────────────────────────────────────────────────

/// Attempt to read a PNG image from the system clipboard.
pub fn check_clipboard_for_image() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        let script = r#"
try
    set imgData to the clipboard as «class PNGf»
    set hexStr to ""
    repeat with b in imgData
        set hexStr to hexStr & (do shell script "printf '%02x' " & (b as integer))
    end repeat
    return hexStr
on error
    return ""
end try
"#;
        let output = std::process::Command::new("osascript")
            .args(["-e", script])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let hex = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hex.is_empty() {
            return None;
        }
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();
        bytes.filter(|b| !b.is_empty())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let output = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png", "-o"])
            .output()
            .ok()?;
        if output.status.success() && !output.stdout.is_empty() {
            return Some(output.stdout);
        }
        None
    }
}
