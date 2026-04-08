/// Shared utility functions used across tui submodules.

/// Remove ANSI escape codes from a string for plain text rendering.
pub(super) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if chars.peek().is_some() {
                chars.next();
            }
        } else if ch == '\r' {
            // skip bare carriage-return
        } else {
            out.push(ch);
        }
    }
    out
}

pub(super) fn truncate_str(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

pub(super) fn prev_char_boundary(s: &str, mut pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    pos -= 1;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

pub(super) fn next_char_boundary(s: &str, mut pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    pos += 1;
    while pos < s.len() && !s.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

/// Convert the internal permission mode string to a human-readable display string.
pub(super) fn permission_mode_display(mode: &str) -> String {
    match mode {
        "read-only" => "read-only mode on".to_string(),
        "workspace-write" => "workspace-write mode on".to_string(),
        "danger-full-access" => "bypass permissions on".to_string(),
        "prompt" => "prompt mode on".to_string(),
        "allow" => "allow mode on".to_string(),
        other if other.is_empty() => "bypass permissions on".to_string(),
        other => format!("{other} mode on"),
    }
}
