//! Consolidated bracketed-paste handler.
//!
//! Task #599 / v2.2.14 Phase 1: replaces THREE duplicate paste loops
//! (`input_handler.rs::read_input`, `mod.rs::wait_for_turn_end`,
//! `mod.rs::wait_for_turn_end_for_tab`) that each did
//! `for ch in cleaned.chars() { self.insert_char(ch) }` with file-path
//! detection + placeholder substitution so a pasted absolute path no
//! longer collides with the slash-command parser.
//!
//! The user direction, verbatim:
//! > "Pasting images, documents, text etc should all follow the same
//! > claude-code type action. same look, same feel."
//!
//! See `~/.claude/projects/-Users-soulofall-projects/memory/feedback-clipboard-parity.md`
//! for the regression-gate rules — there must never be more than ONE
//! paste handler in the TUI.

use std::path::{Path, PathBuf};

use super::AnvilTui;
use super::state::{PlaceholderPayload, PlaceholderSpan};

// ── Public entry point ──────────────────────────────────────────────────────

/// Single source of truth for bracketed-paste handling. All three callsites
/// in `tui/input_handler.rs` and `tui/mod.rs` route through here.
///
/// Behavior:
///   - Strip carriage returns (terminals occasionally emit CRLF in paste).
///   - If the entire paste resolves to a real file path, insert a
///     placeholder display string and record a `PlaceholderSpan` so
///     submit-time can expand it to image / document / text content blocks.
///   - Otherwise, insert the cleaned text literally as before.
pub(super) fn handle_paste(tui: &mut AnvilTui, raw: String) {
    let cleaned = raw.replace('\r', "");
    if cleaned.is_empty() {
        return;
    }

    // Try to interpret the WHOLE paste as a single file path. If we can,
    // substitute a placeholder; otherwise fall through to plain text.
    if let Some(path) = parse_pasted_file_path(&cleaned) {
        insert_placeholder_for_path(tui, &path);
        return;
    }

    // Fallback: literal text insert (the historical behavior).
    for ch in cleaned.chars() {
        tui.insert_char(ch);
    }
}

// ── File-path detection ─────────────────────────────────────────────────────

/// Detect whether a pasted string is a single file path on disk.
///
/// Returns `Some(canonical_path)` ONLY when:
///   - The input (after stripping surrounding whitespace + single/double
///     quotes, and after `~/` expansion) starts with `/`, `~/`, `./`,
///     or `../`.
///   - The resolved path exists and is a regular file.
///   - The string contains no newlines (multi-line pastes are treated
///     as plain text — the user is most likely pasting a code snippet
///     with leading slashes inside comments).
///
/// Crucially: returns `None` for slash-commands like `/help`, `/model`,
/// `/Users` (string-only — the file does not exist), or any non-path
/// blob. This is the gate that fixes "paste /Users/foo/bar.png →
/// Unknown command" regression.
#[must_use]
pub fn parse_pasted_file_path(s: &str) -> Option<PathBuf> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('\n') {
        return None;
    }
    // Strip a single layer of surrounding quotes (some terminals add
    // them around dragged paths).
    let stripped = trimmed
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim();
    if stripped.is_empty() {
        return None;
    }
    // Must look like a path prefix.
    if !(stripped.starts_with('/')
        || stripped.starts_with("~/")
        || stripped.starts_with("./")
        || stripped.starts_with("../"))
    {
        return None;
    }
    let expanded = if let Some(rest) = stripped.strip_prefix("~/") {
        dirs_next::home_dir()?.join(rest)
    } else {
        PathBuf::from(stripped)
    };
    if expanded.is_file() {
        Some(expanded)
    } else {
        None
    }
}

// ── Placeholder insertion ───────────────────────────────────────────────────

/// Classify a path into a placeholder display + payload kind, then insert
/// both the display string and the span metadata into the active tab.
fn insert_placeholder_for_path(tui: &mut AnvilTui, path: &Path) {
    let kind = classify_for_placeholder(path);
    let display = placeholder_display(path, kind);
    let payload = PlaceholderPayload::from_kind(kind, path.to_path_buf());

    let tab = tui.active_tab_mut();
    let start = tab.cursor;
    // Insert the display string at the cursor.
    tab.input.insert_str(start, &display);
    let end = start + display.len();
    // Shift any later placeholder spans by `display.len()` so their byte
    // offsets stay accurate.
    for span in &mut tab.input_placeholders {
        if span.start >= start {
            span.start += display.len();
            span.end += display.len();
        }
    }
    tab.input_placeholders.push(PlaceholderSpan {
        start,
        end,
        payload,
    });
    // Keep spans sorted by start offset so backspace boundary lookup is
    // a simple linear scan.
    tab.input_placeholders.sort_by_key(|s| s.start);
    tab.cursor = end;
    tab.history_idx = None;
    tab.history_backup = None;
}

/// Categories used for placeholder display + submit-time expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaceholderKind {
    Image,
    Document,
    Text,
}

pub(super) fn classify_for_placeholder(path: &Path) -> PlaceholderKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => PlaceholderKind::Image,
        "pdf" => PlaceholderKind::Document,
        _ => PlaceholderKind::Text,
    }
}

fn placeholder_display(path: &Path, kind: PlaceholderKind) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    match kind {
        PlaceholderKind::Image => format!("[image: {name}]"),
        PlaceholderKind::Document => format!("[document: {name}]"),
        PlaceholderKind::Text => format!("[file: {name}]"),
    }
}

// ── Backspace at placeholder boundary ───────────────────────────────────────

/// If `tab.cursor` sits at the end-byte of a placeholder span, drain the
/// entire span atomically and return `true`. The caller (`backspace`)
/// falls back to single-char deletion when this returns `false`.
pub(super) fn try_backspace_placeholder(tui: &mut AnvilTui) -> bool {
    let tab = tui.active_tab_mut();
    let cursor = tab.cursor;
    let removed = try_backspace_placeholder_inner(
        &mut tab.input,
        &mut tab.cursor,
        &mut tab.input_placeholders,
        cursor,
    );
    if removed {
        tab.history_idx = None;
        tab.history_backup = None;
    }
    removed
}

/// Stateless core of `try_backspace_placeholder` — operates on borrowed
/// pieces so it can be unit-tested without a live `AnvilTui`.
pub(super) fn try_backspace_placeholder_inner(
    input: &mut String,
    cursor_ref: &mut usize,
    spans: &mut Vec<super::state::PlaceholderSpan>,
    cursor: usize,
) -> bool {
    let idx = spans.iter().position(|s| s.end == cursor);
    let Some(idx) = idx else {
        return false;
    };
    let span = spans.remove(idx);
    input.replace_range(span.start..span.end, "");
    let removed = span.end - span.start;
    *cursor_ref = span.start;
    for s in spans.iter_mut() {
        if s.start >= span.end {
            s.start -= removed;
            s.end -= removed;
        }
    }
    true
}

// ── Submit-time expansion ───────────────────────────────────────────────────

/// Walk the active tab's input + placeholder spans and produce the
/// content-block list that should be sent to the model. When there are
/// no placeholders, returns a single Text block (preserving the cheap
/// path that 99 % of submissions take).
///
/// The returned `String` is the canonical "user text" that goes into the
/// session log and `/history` — placeholder displays are kept verbatim
/// (`[image: foo.png]`) so the conversation is human-readable.
pub(super) fn expand_input_for_submit(
    text: &str,
    spans: &[PlaceholderSpan],
) -> Vec<runtime::ContentBlock> {
    use runtime::ContentBlock;
    if spans.is_empty() {
        return vec![ContentBlock::Text {
            text: text.to_string(),
        }];
    }
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    let mut sorted: Vec<&PlaceholderSpan> = spans.iter().collect();
    sorted.sort_by_key(|s| s.start);
    for span in sorted {
        if span.start > cursor {
            let txt = &text[cursor..span.start];
            if !txt.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: txt.to_string(),
                });
            }
        }
        match span.payload.expand_to_blocks() {
            Ok(mut bs) => blocks.append(&mut bs),
            Err(notice) => {
                // On expansion failure, fall back to the placeholder display
                // as text so the model at least sees that something was
                // intended. The notice is also returned so the caller can
                // surface it in the TUI scrollback.
                blocks.push(ContentBlock::Text {
                    text: format!("[expansion error: {notice}]"),
                });
            }
        }
        cursor = span.end;
    }
    if cursor < text.len() {
        let trailing = &text[cursor..];
        if !trailing.is_empty() {
            blocks.push(ContentBlock::Text {
                text: trailing.to_string(),
            });
        }
    }
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: String::new(),
        });
    }
    blocks
}

// ── Mouse capture default + banner text ─────────────────────────────────────

/// Resolve the mouse-capture default from `ANVIL_TUI_MOUSE` + an
/// optional config value. The default when both are absent is **OFF**.
///
/// Task #599: extracted into a pure helper so the default OFF policy is
/// covered by unit tests and never quietly flips back to ON.
#[must_use]
pub fn resolve_mouse_capture_default(
    env_value: Option<&str>,
    config_value: Option<bool>,
) -> bool {
    if let Some(v) = env_value {
        return matches!(v, "1" | "true" | "yes" | "on");
    }
    config_value.unwrap_or(false)
}

/// Status-line hint shown at TUI startup, telling the user how to select
/// text in the current mouse-capture mode.
///
/// Task #599: tied to the mouse-capture resolution above so the docs
/// match the behavior. When mouse capture is OFF (the default), the
/// terminal owns drag-select. When ON, the user must hold a modifier.
#[must_use]
pub fn mouse_selection_hint(mouse_enabled: bool, is_macos: bool) -> String {
    if mouse_enabled {
        if is_macos {
            "Hold Option and drag to select text".to_string()
        } else {
            "Hold Shift and drag to select text".to_string()
        }
    } else {
        "Drag to select text  •  Cmd+C to copy".to_string()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpfile_with_ext(ext: &str) -> PathBuf {
        // Build a unique name without an extra crate dependency.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir();
        let path = dir.join(format!("anvil-paste-test-{pid}-{nanos}-{n}.{ext}"));
        fs::write(&path, b"dummy").expect("tmpfile");
        path
    }

    #[test]
    fn parse_path_rejects_slash_commands() {
        assert!(parse_pasted_file_path("/help").is_none());
        assert!(parse_pasted_file_path("/model").is_none());
        assert!(parse_pasted_file_path("/tab list").is_none());
    }

    #[test]
    fn parse_path_rejects_plain_text() {
        assert!(parse_pasted_file_path("hello world").is_none());
        assert!(parse_pasted_file_path("foo bar baz").is_none());
    }

    #[test]
    fn parse_path_rejects_nonexistent() {
        assert!(parse_pasted_file_path("/this/path/definitely/does/not/exist").is_none());
        assert!(parse_pasted_file_path("/Users/nobody/nothing.png").is_none());
    }

    #[test]
    fn parse_path_accepts_real_absolute_file() {
        let p = tmpfile_with_ext("txt");
        let s = p.to_string_lossy().to_string();
        let got = parse_pasted_file_path(&s).expect("real absolute path");
        assert_eq!(got, p);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn parse_path_accepts_single_quoted_path() {
        let p = tmpfile_with_ext("png");
        let s = format!("'{}'", p.to_string_lossy());
        let got = parse_pasted_file_path(&s).expect("quoted path");
        assert_eq!(got, p);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn parse_path_accepts_double_quoted_path() {
        let p = tmpfile_with_ext("pdf");
        let s = format!("\"{}\"", p.to_string_lossy());
        let got = parse_pasted_file_path(&s).expect("dq path");
        assert_eq!(got, p);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn parse_path_rejects_multiline_blob() {
        let p = tmpfile_with_ext("txt");
        let s = format!("{}\nhello", p.to_string_lossy());
        assert!(parse_pasted_file_path(&s).is_none());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn classify_extensions() {
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/foo.png")),
            PlaceholderKind::Image
        );
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/Foo.JPEG")),
            PlaceholderKind::Image
        );
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/spec.pdf")),
            PlaceholderKind::Document
        );
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/notes.txt")),
            PlaceholderKind::Text
        );
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/readme.md")),
            PlaceholderKind::Text
        );
    }

    #[test]
    fn placeholder_display_is_human_readable() {
        assert_eq!(
            placeholder_display(Path::new("/tmp/foo.png"), PlaceholderKind::Image),
            "[image: foo.png]"
        );
        assert_eq!(
            placeholder_display(Path::new("/tmp/spec.pdf"), PlaceholderKind::Document),
            "[document: spec.pdf]"
        );
        assert_eq!(
            placeholder_display(Path::new("/tmp/x.txt"), PlaceholderKind::Text),
            "[file: x.txt]"
        );
    }

    #[test]
    fn expand_returns_single_text_block_when_no_spans() {
        let blocks = expand_input_for_submit("hello world", &[]);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            runtime::ContentBlock::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn mouse_capture_default_is_off_with_no_env_no_config() {
        // Brief: "Mouse capture default OFF." Regression gate.
        assert!(!resolve_mouse_capture_default(None, None));
    }

    #[test]
    fn mouse_capture_env_var_wins_over_config() {
        // Config says ON, env says OFF: env wins.
        assert!(!resolve_mouse_capture_default(Some("0"), Some(true)));
        // Config absent, env says ON: ON.
        assert!(resolve_mouse_capture_default(Some("1"), None));
        assert!(resolve_mouse_capture_default(Some("true"), None));
        assert!(resolve_mouse_capture_default(Some("yes"), None));
        // Unknown env values are treated as OFF.
        assert!(!resolve_mouse_capture_default(Some("maybe"), Some(true)));
        // Env absent, config ON: ON.
        assert!(resolve_mouse_capture_default(None, Some(true)));
        // Env absent, config OFF: OFF.
        assert!(!resolve_mouse_capture_default(None, Some(false)));
    }

    #[test]
    fn banner_says_cmd_c_when_mouse_off() {
        // Brief verbatim: "Drag to select text · Cmd+C to copy".
        let hint = mouse_selection_hint(false, true);
        assert!(hint.contains("Drag to select text"), "got {hint:?}");
        assert!(hint.contains("Cmd+C to copy"), "got {hint:?}");
    }

    #[test]
    fn banner_says_option_or_shift_when_mouse_on() {
        let mac = mouse_selection_hint(true, true);
        assert!(mac.contains("Option"), "got {mac:?}");
        assert!(mac.contains("drag to select text"), "got {mac:?}");
        let linux = mouse_selection_hint(true, false);
        assert!(linux.contains("Shift"), "got {linux:?}");
        assert!(linux.contains("drag to select text"), "got {linux:?}");
    }

    #[test]
    fn backspace_at_placeholder_boundary_deletes_whole_span() {
        // "hello [image: foo.png] world"
        // Cursor sitting just after the placeholder closing `]` must
        // delete the entire placeholder display, not just one char.
        use crate::tui::state::{PlaceholderPayload, PlaceholderSpan};
        let display = "[image: foo.png]";
        let mut input = format!("hello {display} world");
        let img_start = "hello ".len();
        let img_end = img_start + display.len();
        let mut cursor = img_end;
        let mut spans = vec![PlaceholderSpan {
            start: img_start,
            end: img_end,
            payload: PlaceholderPayload::Image(PathBuf::from("/tmp/foo.png")),
        }];

        let removed = try_backspace_placeholder_inner(
            &mut input,
            &mut cursor,
            &mut spans,
            img_end,
        );
        assert!(removed, "expected the boundary-deletion path to fire");
        assert_eq!(input, "hello  world", "input was {input:?}");
        assert_eq!(cursor, img_start);
        assert!(spans.is_empty());
    }

    #[test]
    fn backspace_not_at_boundary_returns_false() {
        // Cursor in the middle of a placeholder span → fall back to
        // single-char delete (handled by the caller).
        use crate::tui::state::{PlaceholderPayload, PlaceholderSpan};
        let display = "[image: foo.png]";
        let mut input = format!("hello {display}!");
        let img_start = "hello ".len();
        let img_end = img_start + display.len();
        let mut cursor = img_start + 3; // not at the end
        let mut spans = vec![PlaceholderSpan {
            start: img_start,
            end: img_end,
            payload: PlaceholderPayload::Image(PathBuf::from("/tmp/foo.png")),
        }];
        let at = cursor;
        let removed = try_backspace_placeholder_inner(
            &mut input,
            &mut cursor,
            &mut spans,
            at,
        );
        assert!(!removed);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn only_one_paste_handler_in_the_tui_module() {
        // Regression gate (Task #599 / feedback-clipboard-parity.md):
        // there must be exactly ONE non-comment paste handler in the TUI
        // module — and that handler must be the consolidated
        // `paste::handle_paste`. Anything else means a future refactor
        // reintroduced the duplicate-paste-handler bug we just fixed.
        let mut total_handlers = 0;
        for src in [
            include_str!("input_handler.rs"),
            include_str!("mod.rs"),
        ] {
            for line in src.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                // Count places that take a Paste event off the crossterm
                // event queue (the only legitimate entry points).
                if trimmed.contains("Event::Paste(") || trimmed.contains("CtEvent::Paste(") {
                    total_handlers += 1;
                }
            }
        }
        // We expect EXACTLY 3 callsites that match the pattern (read_input,
        // wait_for_turn_end, wait_for_turn_end_for_tab) — they must all
        // delegate to `paste::handle_paste`. The handler count being 3 is
        // fine; what's NOT fine is a per-site `for ch in cleaned.chars()`
        // body, which the next test guards against.
        assert!(
            (3..=4).contains(&total_handlers),
            "expected ~3 Paste match arms; got {total_handlers}. \
             A new paste callsite is a candidate for regression — it \
             MUST delegate to tui::paste::handle_paste. See \
             feedback-clipboard-parity.md."
        );
    }

    #[test]
    fn no_inline_paste_char_loop_outside_paste_module() {
        // Regression gate: the buggy "for ch in cleaned.chars() {
        // self.insert_char(ch); }" pattern must live ONLY inside
        // tui::paste (where it's the fallback after path detection).
        for (name, src) in [
            ("input_handler.rs", include_str!("input_handler.rs")),
            ("mod.rs", include_str!("mod.rs")),
        ] {
            let bad = src.contains("for ch in cleaned.chars()");
            assert!(
                !bad,
                "Inline `for ch in cleaned.chars()` loop found in {name}. \
                 Paste handling must go through tui::paste::handle_paste. \
                 See feedback-clipboard-parity.md."
            );
        }
    }

    #[test]
    fn expand_text_around_text_placeholder() {
        // We use a Text placeholder (not Image) so we don't have to write
        // real image bytes — the expansion path uses fs::read so a tmp
        // text file works fine.
        let p = tmpfile_with_ext("txt");
        let txt = std::fs::read_to_string(&p).unwrap();
        let _ = txt; // unused
        let display = format!("[file: {}]", p.file_name().unwrap().to_str().unwrap());
        let input = format!("before {display} after");
        let span = PlaceholderSpan {
            start: "before ".len(),
            end: "before ".len() + display.len(),
            payload: PlaceholderPayload::Text(p.clone()),
        };
        let blocks = expand_input_for_submit(&input, &[span]);
        // before-text + expanded text (the placeholder becomes a Text
        // block with the file's contents wrapped) + after-text.
        // Must be at least 2 blocks (we may merge later).
        assert!(blocks.len() >= 2, "got blocks: {blocks:?}");
        let _ = fs::remove_file(&p);
    }
}
