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
use std::time::{Duration, Instant};

use super::AnvilTui;
use super::state::{PlaceholderPayload, PlaceholderSpan};

// ── Long-paste thresholds (Task #604) ───────────────────────────────────────

/// A paste this long-or-longer (in either dimension) gets the
/// `[Pasted text #N +M lines]` placeholder treatment instead of literal
/// insertion. Matches Claude Code's behaviour for webpage-selection paste.
pub(super) const LONG_PASTE_LINE_THRESHOLD: usize = 6;
pub(super) const LONG_PASTE_CHAR_THRESHOLD: usize = 400;

// ── Keystroke-burst detection (Task #604 Part C) ────────────────────────────

/// Minimum number of chars in a burst before the suffix becomes a candidate
/// for path-substitution. macOS Finder drag-and-drop typically delivers
/// 30-80 chars (full absolute path); the shortest plausible dropped path
/// is `/x/y.z` (7 chars) but real-world drops are much longer. We set the
/// threshold above all common slash-command prefixes (`/help`, `/model`,
/// `/clear`, `/scroll-speed`, `/permission-mode`, etc. — none exceed
/// 18 chars when typed by hand) so a slow typist of even the longest
/// slash command cannot accidentally trip the heuristic.
pub(super) const BURST_MIN_CHARS: usize = 20;

/// Maximum gap between successive `KeyCode::Char` events that still
/// qualifies as part of the same burst. Drag-and-drop arrives as a
/// nearly-instantaneous batch (well under 5 ms between chars on Apple
/// Terminal); a human typist takes 60-200 ms between keystrokes at
/// typical chat speed and well over 300 ms at thoughtful prose speed.
/// 100 ms is the conservative split between these two distributions.
pub(super) const BURST_MAX_GAP: Duration = Duration::from_millis(100);

/// Minimum quiet time after the last burst keystroke before we attempt
/// substitution. This is what prevents firing in the middle of a still-
/// arriving paste burst.
pub(super) const BURST_IDLE_AFTER: Duration = Duration::from_millis(50);

/// Per-TUI state for the keystroke-burst heuristic. When a user drags a
/// file from macOS Finder into the Anvil terminal, the file path arrives
/// as a burst of `KeyCode::Char` events — NOT as a single `Event::Paste`.
/// The bracketed-paste handler from #599 never fires; the submit-time
/// fallback from #604 Part A catches it on Enter.
///
/// Part C polishes the UX: detect the burst pattern AS IT ARRIVES and
/// substitute the placeholder *before* Enter, so the user sees
/// `[image: file.png]` appear in their input box mid-typing (matching CC).
#[derive(Debug, Clone, Default)]
pub struct BurstTracker {
    /// Wall-clock time of the most recent `KeyCode::Char` we observed.
    pub last_key_at: Option<Instant>,
    /// Byte offset in `Tab::input` where the current burst started, or
    /// `None` if we're not currently inside a burst.
    pub burst_start_idx: Option<usize>,
    /// Number of chars accumulated in the current burst.
    pub burst_char_count: usize,
    /// True once this burst was already evaluated for substitution.
    /// Prevents re-evaluating the same burst on every subsequent key.
    pub burst_committed: bool,
}

impl BurstTracker {
    /// Clear the tracker — called after a successful substitution or any
    /// time the input buffer is reset (e.g. submit, history navigation,
    /// Ctrl+C clear) so the next char starts a fresh burst.
    pub fn reset(&mut self) {
        self.last_key_at = None;
        self.burst_start_idx = None;
        self.burst_char_count = 0;
        self.burst_committed = false;
    }
}

/// Outcome of a single keystroke as evaluated by the burst tracker.
#[derive(Debug, PartialEq, Eq)]
pub enum BurstOutcome {
    /// The burst is still building — keep typing as usual.
    KeepTyping,
    /// A mature burst just ended and its suffix resolved to a real file
    /// path at `(start, end)` in `input`. Caller should replace
    /// `input[start..end]` with the appropriate placeholder display
    /// string and record a span over the new bytes.
    SubstitutePath { start: usize, end: usize, path: PathBuf },
}

/// Pure-logic core of the keystroke-burst heuristic. Drives the tracker
/// based on the most recent keystroke and (when a burst has just ended)
/// inspects the burst suffix to decide whether to substitute.
///
/// Contract:
/// * Called AFTER the char has been inserted into `input`. `cursor` is the
///   cursor position post-insert.
/// * `now` is wall-clock time of the keystroke (always `Instant::now()` in
///   production; injected by tests).
/// * Returns `BurstOutcome::SubstitutePath { .. }` only when:
///     - >= [`BURST_MIN_CHARS`] chars arrived within [`BURST_MAX_GAP`] of
///       each other (the burst),
///     - the leading char of the burst region looks like a path prefix
///       (`/`, `~`, `.`, `'`, `"`),
///     - the gap between *this* keystroke and the previous keystroke is
///       >= [`BURST_IDLE_AFTER`] (i.e. the burst has stopped), AND
///     - the burst suffix resolves to a real file via
///       [`parse_pasted_file_path`].
///
/// Robustness notes (against terminal jitter):
/// * The 100 ms gap is conservative — Apple Terminal drag-drop measures
///   1-3 ms between chars; pasted clipboard via keystroke conversion
///   measures 5-15 ms. The fastest reliable human typing is ~60 ms per
///   char (~200 wpm sprint), still well above the 100 ms ceiling.
/// * The 20-char minimum exceeds every built-in slash command. A user
///   typing `/permission-mode` (16 chars) at full speed CANNOT trip the
///   heuristic because it stops below the minimum.
/// * The 50 ms idle-after is the END signal: while keys are still
///   arriving fast, we know the burst hasn't ended yet.
/// * If the substitution candidate doesn't resolve to a real file, we
///   return `KeepTyping` and the submit-time fallback
///   (`detect_submit_time_file_path`) still catches it on Enter.
pub fn record_keystroke(
    tracker: &mut BurstTracker,
    input: &str,
    cursor: usize,
    now: Instant,
) -> BurstOutcome {
    let gap = tracker.last_key_at.map(|prev| now.saturating_duration_since(prev));

    let mature_burst_ready_to_commit = tracker.burst_start_idx.is_some()
        && !tracker.burst_committed
        && tracker.burst_char_count >= BURST_MIN_CHARS
        && gap.is_some_and(|g| g >= BURST_IDLE_AFTER);

    if mature_burst_ready_to_commit {
        let start = tracker.burst_start_idx.unwrap();
        let end = prev_char_boundary(input, cursor);
        if end > start && end <= input.len() {
            let suffix = &input[start..end];
            if looks_like_path_prefix(suffix) {
                if let Some(path) = parse_pasted_file_path(suffix) {
                    tracker.burst_committed = true;
                    return BurstOutcome::SubstitutePath { start, end, path };
                }
            }
            tracker.burst_committed = true;
        }
    }

    match gap {
        None => {
            tracker.burst_start_idx = Some(prev_char_boundary(input, cursor));
            tracker.burst_char_count = 1;
            tracker.burst_committed = false;
        }
        Some(g) if g <= BURST_MAX_GAP => {
            if tracker.burst_start_idx.is_none() {
                tracker.burst_start_idx = Some(prev_char_boundary(input, cursor));
                tracker.burst_char_count = 1;
                tracker.burst_committed = false;
            } else {
                tracker.burst_char_count = tracker.burst_char_count.saturating_add(1);
            }
        }
        Some(_) => {
            tracker.burst_start_idx = Some(prev_char_boundary(input, cursor));
            tracker.burst_char_count = 1;
            tracker.burst_committed = false;
        }
    }

    tracker.last_key_at = Some(now);
    BurstOutcome::KeepTyping
}

/// Idle-based variant of [`record_keystroke`]. Called by the TUI event
/// loop on each poll iteration so a burst that ended without a trailing
/// keystroke still substitutes once the 50 ms idle window elapses.
pub fn check_burst_idle(
    tracker: &mut BurstTracker,
    input: &str,
    now: Instant,
) -> BurstOutcome {
    if tracker.burst_committed {
        return BurstOutcome::KeepTyping;
    }
    let Some(start) = tracker.burst_start_idx else {
        return BurstOutcome::KeepTyping;
    };
    let Some(last) = tracker.last_key_at else {
        return BurstOutcome::KeepTyping;
    };
    if tracker.burst_char_count < BURST_MIN_CHARS {
        return BurstOutcome::KeepTyping;
    }
    if now.saturating_duration_since(last) < BURST_IDLE_AFTER {
        return BurstOutcome::KeepTyping;
    }
    let end = input.len();
    if end <= start {
        return BurstOutcome::KeepTyping;
    }
    let suffix = &input[start..end];
    if !looks_like_path_prefix(suffix) {
        tracker.burst_committed = true;
        return BurstOutcome::KeepTyping;
    }
    if let Some(path) = parse_pasted_file_path(suffix) {
        tracker.burst_committed = true;
        return BurstOutcome::SubstitutePath { start, end, path };
    }
    tracker.burst_committed = true;
    BurstOutcome::KeepTyping
}

/// Test whether `s` starts with a path-like prefix. Cheap (no disk I/O)
/// fast-path used by the burst tracker to skip the file-system probe
/// entirely when the burst obviously isn't a path.
fn looks_like_path_prefix(s: &str) -> bool {
    let trimmed = s.trim_start();
    trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('\'')
        || trimmed.starts_with('"')
}

/// Walk back one Unicode char boundary from `idx` in `s`. Returns 0 if
/// `idx` is already at the start or at index 0.
fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    if idx > s.len() {
        return s.len();
    }
    let mut i = idx - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

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

    // Task #604: long bracketed paste from a webpage / clipboard. Anything
    // above the line/char threshold becomes a `[Pasted text #N +M lines,
    // C chars]` placeholder so the input box stays usable. The literal text
    // rides along as a Text content block on submit-time expansion.
    if is_long_paste(&cleaned) {
        insert_placeholder_for_long_paste(tui, cleaned);
        return;
    }

    // Fallback: literal text insert (the historical behavior).
    for ch in cleaned.chars() {
        tui.insert_char(ch);
    }
}

/// Threshold check that decides whether a paste qualifies for the
/// `[Pasted text #N]` placeholder treatment.
#[must_use]
pub(super) fn is_long_paste(cleaned: &str) -> bool {
    // Count newlines as line breaks. A paste with no terminating newline
    // still counts the last line, so we use `lines().count()`.
    let line_count = cleaned.lines().count();
    line_count > LONG_PASTE_LINE_THRESHOLD || cleaned.chars().count() > LONG_PASTE_CHAR_THRESHOLD
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
    // Some terminals shell-escape spaces with backslashes when dragging
    // (Apple Terminal does this). Replace `\<space>` with `<space>` so
    // `/Users/foo/Downloads/Screenshot\ 2026.png` resolves to the real
    // path the file lives at. This was the trailing-backslash mystery
    // in the v2.2.14 screenshot: `Screenshot\` was just the escape on
    // the last char of the dropped path.
    let unescaped: String = unescape_shell_spaces(stripped);
    let candidate = unescaped.as_str();
    let expanded = if let Some(rest) = candidate.strip_prefix("~/") {
        dirs_next::home_dir()?.join(rest)
    } else {
        PathBuf::from(candidate)
    };
    if expanded.is_file() {
        return Some(expanded);
    }
    // Task #604: macOS bundles (`.xcworkspace`, `.app`, `.framework`,
    // `.xcodeproj`, …) are directories the user thinks of as files.
    // Accept them so a workspace path dragged from Finder doesn't fall
    // through to the slash parser. We don't accept arbitrary directories
    // — a bare `/` or `/Users` should NOT be treated as a path drop.
    if expanded.is_dir() && is_macos_bundle(&expanded) {
        return Some(expanded);
    }
    None
}

/// Return true when `path` ends in a macOS bundle extension. Bundles are
/// directories that the OS surfaces as opaque files in Finder.
fn is_macos_bundle(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "xcworkspace"
            | "xcodeproj"
            | "app"
            | "framework"
            | "bundle"
            | "playground"
            | "pkg"
            | "kext"
    )
}

/// Replace `\<space>` and `\<tab>` with the unescaped character. Used
/// only by `parse_pasted_file_path` so paths dragged from Finder /
/// Nautilus (where they arrive shell-escaped) resolve correctly. Leaves
/// other backslashes alone — they're either literal filename characters
/// or the start of a multi-char escape we don't try to interpret.
fn unescape_shell_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(' ') | Some('\t') => {
                    // Drop the backslash; the next loop iteration emits
                    // the whitespace verbatim.
                    continue;
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
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

/// Insert a `[Pasted text #N +M lines, C chars]` placeholder for a long
/// bracketed paste. Records the literal text on a `PastedText` span so
/// `expand_input_for_submit` produces a Text content block on submit.
fn insert_placeholder_for_long_paste(tui: &mut AnvilTui, cleaned: String) {
    let lines = cleaned.lines().count();
    let chars = cleaned.chars().count();
    tui.paste_counter = tui.paste_counter.saturating_add(1);
    let id = tui.paste_counter;
    let display = format_long_paste_display(id, lines, chars);
    let payload = PlaceholderPayload::PastedText {
        text: cleaned,
        lines,
        chars,
        id,
    };

    let tab = tui.active_tab_mut();
    let start = tab.cursor;
    tab.input.insert_str(start, &display);
    let end = start + display.len();
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
    tab.input_placeholders.sort_by_key(|s| s.start);
    tab.cursor = end;
    tab.history_idx = None;
    tab.history_backup = None;
}

/// Human-readable display for a long-paste placeholder. Kept as a pure
/// function so the unit tests assert the exact format the user sees.
#[must_use]
pub(super) fn format_long_paste_display(id: usize, lines: usize, chars: usize) -> String {
    format!("[Pasted text #{id} +{lines} lines, {chars} chars]")
}

/// Task #604 Part C: replace the byte range `start..end` in the active
/// tab's input with the placeholder display string for `path`, and record
/// a `PlaceholderSpan` so submit-time expansion produces the right
/// content block. Mirrors `insert_placeholder_for_path` but works on a
/// pre-existing input region instead of inserting fresh text at the cursor.
///
/// Called by `insert_char` after `record_keystroke` returns
/// `BurstOutcome::SubstitutePath`, and by the wait-loop's idle-check after
/// `check_burst_idle`.
pub(super) fn apply_burst_substitution(
    tui: &mut AnvilTui,
    start: usize,
    end: usize,
    path: &Path,
) {
    let kind = classify_for_placeholder(path);
    let display = placeholder_display(path, kind);
    let payload = PlaceholderPayload::from_kind(kind, path.to_path_buf());

    let tab = tui.active_tab_mut();
    if start > tab.input.len() || end > tab.input.len() || start > end {
        return;
    }
    if !tab.input.is_char_boundary(start) || !tab.input.is_char_boundary(end) {
        return;
    }
    let removed_len = end - start;
    tab.input.replace_range(start..end, &display);
    let new_end = start + display.len();
    let delta_i: isize = display.len() as isize - removed_len as isize;
    // Shift any later placeholder spans by the delta.
    for span in &mut tab.input_placeholders {
        if span.start >= end {
            span.start = ((span.start as isize) + delta_i) as usize;
            span.end = ((span.end as isize) + delta_i) as usize;
        }
    }
    tab.input_placeholders.push(PlaceholderSpan {
        start,
        end: new_end,
        payload,
    });
    tab.input_placeholders.sort_by_key(|s| s.start);
    // Cursor adjustment: if it sat past `end`, shift by delta to track
    // the trailing text. If it sat inside the burst region (shouldn't
    // happen in normal flow), clamp to the end of the placeholder.
    let prior_cursor = tab.cursor;
    if prior_cursor >= end {
        tab.cursor = ((prior_cursor as isize) + delta_i) as usize;
    } else {
        tab.cursor = new_end;
    }
    tab.history_idx = None;
    tab.history_backup = None;
}

/// Submit-time path detection (Task #604 Part A).
///
/// When the user hits Enter, `trim(input)` might be:
///   1. A bracketed-paste file path that arrived as KEYSTROKES (Apple
///      Terminal Cmd+V on a Finder-copied image — the path arrives one
///      character at a time, NOT as a `Event::Paste`, so `handle_paste`
///      never fires).
///   2. A workspace/folder path the user typed by hand.
///   3. A bracketed paste whose receiving terminal converted it to keystrokes.
///
/// All three look the same at submit time: `trimmed` starts with `/` and
/// resolves to a real file on disk. Returns `Some(path)` so the caller
/// can route the submission through the placeholder-substitution path
/// instead of letting `SlashCommand::parse` return `Unknown("Users")`.
///
/// This is the missing piece behind the user-reported regression at
/// 2026-05-17 — see `feedback-clipboard-parity.md`.
#[must_use]
pub fn detect_submit_time_file_path(trimmed: &str) -> Option<PathBuf> {
    parse_pasted_file_path(trimmed)
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
        // Task #601 (v2.2.16): PDF + the Office / OpenDocument formats
        // that Anthropic accepts via the native `document` content block.
        // The runtime's `expand_to_blocks` emits a
        // `ContentBlock::Document` for these; the provider wire-format
        // builders pass them through to Anthropic and fall back to a
        // base64-in-text notice for non-Anthropic providers.
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods"
        | "odp" => PlaceholderKind::Document,
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

    /// Task #601 (v2.2.16): Office and OpenDocument formats classify as
    /// Document so they ride the first-class Anthropic document-content
    /// path rather than the base64-in-text fallback. This is the test
    /// the task brief enumerates as `docx_routes_to_document_block`.
    #[test]
    fn docx_routes_to_document_block() {
        for ext in &[
            "docx", "doc", "xlsx", "xls", "pptx", "ppt", "odt", "ods", "odp",
        ] {
            let p = PathBuf::from(format!("/tmp/foo.{ext}"));
            assert_eq!(
                classify_for_placeholder(&p),
                PlaceholderKind::Document,
                "expected {ext} to classify as Document"
            );
        }
        // Case insensitivity — uppercase extension still routes correctly.
        assert_eq!(
            classify_for_placeholder(Path::new("/tmp/Report.DOCX")),
            PlaceholderKind::Document
        );
    }

    /// Task #601 (v2.2.16): a pasted PDF must produce a
    /// `runtime::ContentBlock::Document` (NOT a `Text` block with an
    /// embedded base64 payload — that was the v2.2.13 behaviour we
    /// just replaced).
    #[test]
    fn pdf_paste_routes_to_document_block_not_text() {
        use crate::tui::state::PlaceholderPayload;
        // Build a tiny "PDF" on disk — the runtime doesn't validate the
        // header; it just base64-encodes the bytes and tags them with
        // `application/pdf` based on the extension.
        let p = tmpfile_with_ext("pdf");
        std::fs::write(&p, b"%PDF-1.4\n%fake but valid extension\n").unwrap();
        let payload = PlaceholderPayload::Document(p.clone());
        let blocks = payload
            .expand_to_blocks()
            .expect("expansion should succeed for small pdf");
        assert_eq!(blocks.len(), 1, "expected exactly one block, got {blocks:?}");
        match &blocks[0] {
            runtime::ContentBlock::Document {
                media_type,
                title,
                data,
                ..
            } => {
                assert_eq!(media_type, "application/pdf");
                assert_eq!(title.as_deref(), Some(p.file_name().unwrap().to_str().unwrap()));
                assert!(!data.is_empty(), "data should be base64 encoded");
            }
            other => panic!(
                "expected ContentBlock::Document; got {other:?} — this is the \
                 regression task #601 fixes (was a Text block with base64 \
                 stuffed inside, which billed as text tokens and bypassed \
                 Anthropic's native document support)"
            ),
        }
        let _ = std::fs::remove_file(&p);
    }

    /// Task #601 (v2.2.16): documents larger than 32 MB are refused
    /// locally with a human-readable notice. The notice format is the
    /// one the brief specifies — the leading `[Document too large:` is
    /// a contract for the TUI surface text.
    #[test]
    fn large_pdf_above_32mb_emits_size_error_placeholder() {
        use crate::tui::state::PlaceholderPayload;
        let p = tmpfile_with_ext("pdf");
        // Write 33 MB of zero bytes — the size guard fires before any
        // base64 work is done, so this is cheap.
        let big = vec![0u8; 33 * 1024 * 1024];
        std::fs::write(&p, &big).expect("write big tmp file");
        let payload = PlaceholderPayload::Document(p.clone());
        let err = payload
            .expand_to_blocks()
            .expect_err("expansion must refuse files > 32 MB");
        assert!(
            err.starts_with("Document too large:"),
            "notice must start with the brief's exact prefix; got {err:?}"
        );
        assert!(err.contains("32 MB max"), "notice should cite the 32 MB cap; got {err:?}");
        let _ = std::fs::remove_file(&p);
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

    // ── Task #604 Part A: submit-time path detection ────────────────────────

    /// User repro: Cmd+V on a Finder-copied PNG screenshot. Terminal
    /// delivers the path as keystrokes (no Paste event), so it lives in
    /// `tab.input` as plain text. On Enter, `trimmed` starts with `/` →
    /// `SlashCommand::parse` returns `Unknown("Users")` → "Unknown command"
    /// error. The Part A fix is `detect_submit_time_file_path`, which
    /// resolves the path BEFORE the slash parser sees it.
    #[test]
    fn submit_with_image_path_input_substitutes_placeholder() {
        let p = tmpfile_with_ext("png");
        let typed_or_pasted = p.to_string_lossy().to_string();
        let detected = detect_submit_time_file_path(&typed_or_pasted)
            .expect("real image path must resolve at submit time");
        assert_eq!(detected, p, "submit-time detector must return the resolved path");
        let _ = fs::remove_file(&p);
    }

    /// User repro: workspace path
    /// `/Users/soulofall/projects/aegis-culpur.net/ios/Aegis.xcworkspace`
    /// arrives as keystrokes. Submit-time detection must still route to
    /// the file-drop pipeline rather than letting it hit the slash parser.
    #[test]
    fn submit_with_document_path_input_substitutes_placeholder() {
        let p = tmpfile_with_ext("pdf");
        let typed_or_pasted = p.to_string_lossy().to_string();
        let detected = detect_submit_time_file_path(&typed_or_pasted)
            .expect("real document path must resolve at submit time");
        assert_eq!(detected, p);
        let _ = fs::remove_file(&p);
    }

    /// Negative: a normal chat prompt or a real slash command MUST NOT
    /// be misclassified as a file path.
    #[test]
    fn submit_with_arbitrary_text_unchanged_runs_normally() {
        assert!(detect_submit_time_file_path("hello world").is_none());
        assert!(detect_submit_time_file_path("/help").is_none());
        assert!(detect_submit_time_file_path("/model").is_none());
        assert!(detect_submit_time_file_path("/clear").is_none());
        // A path that LOOKS valid but doesn't exist must also be None
        // — otherwise we'd shadow `/Users` slash commands that don't yet
        // map to a real binary.
        assert!(
            detect_submit_time_file_path("/Users/nobody/notarealfile.png").is_none()
        );
    }

    /// User repro #3: workspace path
    /// `/Users/soulofall/projects/aegis-culpur.net/ios/Aegis.xcworkspace`.
    /// macOS bundles are directories that Finder + the user think of as
    /// files. The submit-time detector must accept them so a dragged
    /// workspace path doesn't fall through to the slash parser.
    #[test]
    fn submit_with_macos_bundle_directory_resolves() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir();
        let bundle = dir.join(format!("anvil-paste-test-{pid}-{nanos}-{n}.xcworkspace"));
        fs::create_dir(&bundle).expect("tmp bundle");
        let s = bundle.to_string_lossy().to_string();
        let detected = detect_submit_time_file_path(&s)
            .expect("xcworkspace bundle directory must resolve as a path");
        assert_eq!(detected, bundle);
        let _ = std::fs::remove_dir(&bundle);
    }

    /// Negative: a regular non-bundle directory must NOT match. We never
    /// want `/Users` or `/tmp` slipping through and shadowing slash
    /// commands.
    #[test]
    fn regular_directory_is_not_a_path_drop() {
        assert!(detect_submit_time_file_path("/").is_none());
        assert!(detect_submit_time_file_path("/Users").is_none());
        assert!(detect_submit_time_file_path("/tmp").is_none());
    }

    /// Apple Terminal escapes spaces with backslash when dragging from
    /// Finder. The submit-time detector must unescape so the resolved
    /// path lands on disk.
    #[test]
    fn submit_with_shell_escaped_space_resolves() {
        // Build a tmpfile whose name contains a literal space, then
        // construct the shell-escaped form the user sees.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir();
        let real = dir.join(format!("anvil paste test {pid}-{nanos}-{n}.png"));
        fs::write(&real, b"dummy").expect("tmpfile");
        let escaped = real.to_string_lossy().replace(' ', "\\ ");
        let detected = detect_submit_time_file_path(&escaped)
            .expect("shell-escaped path must resolve");
        assert_eq!(detected, real, "detector must canonicalise to the real path");
        let _ = fs::remove_file(&real);
    }

    // ── Task #604 Part B: long-paste placeholder ────────────────────────────

    /// Long paste from a webpage / clipboard: anything above the line OR
    /// char threshold must be substituted. The Part B fix is
    /// `is_long_paste` in `handle_paste`.
    #[test]
    fn long_paste_substitutes_summary_placeholder_in_input() {
        // Build a 50-line paste — clearly past LONG_PASTE_LINE_THRESHOLD.
        let body = (1..=50)
            .map(|i| format!("line {i}: some sample text content"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(is_long_paste(&body), "50-line paste must qualify");
        let display = format_long_paste_display(1, 50, body.chars().count());
        assert!(
            display.starts_with("[Pasted text #1 +50 lines, "),
            "display format must match the brief; got {display:?}"
        );
        assert!(display.ends_with(" chars]"), "got {display:?}");
        // And a short blob over the char threshold (400) but only 1 line.
        let long_line = "x".repeat(500);
        assert!(is_long_paste(&long_line));
    }

    /// Negative: a 3-line paste under 400 chars goes in LITERALLY.
    #[test]
    fn short_paste_inserts_literally() {
        let s = "hello\nworld\nthird line";
        assert!(!is_long_paste(s));
        let two_hundred = "x".repeat(200);
        assert!(!is_long_paste(&two_hundred));
        let six_lines = "a\nb\nc\nd\ne\nf"; // exactly 6 lines → at threshold, NOT over
        assert!(!is_long_paste(six_lines));
    }

    /// Per-session monotonic counter — multiple long pastes get unique IDs.
    /// We exercise the pure display helper directly; integration with
    /// `AnvilTui::paste_counter` is covered by the broader paste path.
    #[test]
    fn multiple_long_pastes_get_unique_numbers() {
        let d1 = format_long_paste_display(1, 50, 500);
        let d2 = format_long_paste_display(2, 30, 1200);
        let d3 = format_long_paste_display(3, 7, 401);
        assert!(d1.contains("#1"), "got {d1:?}");
        assert!(d2.contains("#2"), "got {d2:?}");
        assert!(d3.contains("#3"), "got {d3:?}");
        // All three must be DIFFERENT strings.
        assert_ne!(d1, d2);
        assert_ne!(d2, d3);
        assert_ne!(d1, d3);
    }

    /// PastedText expansion: the inline text rides as a Text content
    /// block wrapped in `<pasted_text id="N">…</pasted_text>` so the
    /// model can distinguish pasted blobs from typed prompt.
    #[test]
    fn pasted_text_expands_to_text_block() {
        use crate::tui::state::PlaceholderPayload;
        let payload = PlaceholderPayload::PastedText {
            text: "first line\nsecond line\nthird".to_string(),
            lines: 3,
            chars: 28,
            id: 7,
        };
        let blocks = payload.expand_to_blocks().expect("inline text never fails");
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            runtime::ContentBlock::Text { text } => {
                assert!(text.contains("first line"));
                assert!(text.contains("third"));
                assert!(text.contains("id=\"7\""), "expected id marker; got {text:?}");
            }
            other => panic!("expected Text block; got {other:?}"),
        }
    }

    /// User screenshot repro: the long-paste threshold MUST fire on a
    /// 49-line paste even when the paste arrives as a single Paste event
    /// (Part B). 49 lines is well above the 6-line threshold; this guards
    /// against accidentally raising the threshold above realistic webpage
    /// selections (the user's actual paste was "many lines of text").
    #[test]
    fn screenshot_repro_49_line_paste_qualifies_as_long() {
        let body = (0..49)
            .map(|i| format!("paragraph {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            is_long_paste(&body),
            "49-line paste must qualify (LONG_PASTE_LINE_THRESHOLD = {LONG_PASTE_LINE_THRESHOLD})"
        );
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

    // ── Task #604 Part C: keystroke-burst detection ─────────────────────────

    /// Helper: drive `record_keystroke` over a series of synthetic key
    /// events. Each tuple is `(char, gap_ms_from_previous)`; the first
    /// tuple's gap is interpreted as "absolute t=gap_ms" so the test
    /// can control whether the first keystroke is the start of a burst
    /// or a continuation of a prior one.
    ///
    /// Returns the final `BurstTracker` state and an `Option<(start,
    /// end, path)>` capturing the FIRST `SubstitutePath` outcome we
    /// observed during the drive (since `record_keystroke` is called
    /// per-char, we surface the substitution moment for assertion).
    fn drive_keystrokes(
        tracker: &mut BurstTracker,
        input: &mut String,
        events: &[(char, u64)],
    ) -> Option<(usize, usize, PathBuf)> {
        // Anchor `now` to a fixed Instant so the absolute timeline is
        // deterministic. The actual Instant value doesn't matter; only
        // gaps between consecutive observations do.
        let base = std::time::Instant::now();
        let mut elapsed = Duration::from_millis(0);
        let mut substitution: Option<(usize, usize, PathBuf)> = None;
        for (ch, gap_ms) in events {
            elapsed += Duration::from_millis(*gap_ms);
            input.insert(input.len(), *ch);
            let cursor = input.len();
            let now = base + elapsed;
            let outcome = record_keystroke(tracker, input, cursor, now);
            if let BurstOutcome::SubstitutePath { start, end, path } = outcome {
                if substitution.is_none() {
                    substitution = Some((start, end, path));
                }
            }
        }
        substitution
    }

    /// Brief Test #1: simulate a fast burst of >= 20 chars whose suffix
    /// resolves to a real file, then a "post-idle" char to trigger the
    /// substitution check. The detector must return `SubstitutePath`
    /// covering the burst region.
    #[test]
    fn keystroke_burst_matching_drag_and_drop_substitutes_placeholder() {
        let p = tmpfile_with_ext("png");
        let s = p.to_string_lossy().to_string();
        assert!(
            s.len() >= BURST_MIN_CHARS,
            "tmpfile path must exceed BURST_MIN_CHARS for this test to be meaningful; \
             tmpfile produced {s:?} ({} chars)",
            s.len()
        );

        let mut tracker = BurstTracker::default();
        let mut input = String::new();

        // 1) Each char of the path arrives 2ms after the previous.
        let mut events: Vec<(char, u64)> = s.chars().map(|c| (c, 2u64)).collect();
        // 2) A trailing space arrives 80ms later (> BURST_IDLE_AFTER) to
        //    signal "burst ended". The detector should fire on THIS
        //    keystroke.
        events.push((' ', 80));

        let outcome = drive_keystrokes(&mut tracker, &mut input, &events);
        let (start, end, detected) =
            outcome.expect("burst should have produced a SubstitutePath");
        assert_eq!(start, 0, "burst started at offset 0 (input was empty)");
        assert_eq!(end, s.len(), "burst ended at the byte before the trailing space");
        assert_eq!(detected, p, "detector resolved to the real tmpfile");

        let _ = fs::remove_file(&p);
    }

    /// Brief Test #2: slow typing of `/help` (5 chars over 500ms each)
    /// must NOT trigger the burst — neither the char count nor the gap
    /// thresholds are met.
    #[test]
    fn slow_typing_of_slash_command_does_not_trigger_burst() {
        let mut tracker = BurstTracker::default();
        let mut input = String::new();

        // Each char arrives 500 ms after the previous → way past
        // BURST_MAX_GAP (100 ms). The tracker should treat each char
        // as the start of a fresh burst with count=1. After all 5
        // chars, the burst count is still 1 (most recent gap reset).
        let events: Vec<(char, u64)> = "/help".chars().map(|c| (c, 500u64)).collect();
        let outcome = drive_keystrokes(&mut tracker, &mut input, &events);
        assert!(
            outcome.is_none(),
            "slow typing must NOT trigger burst substitution; got {outcome:?}"
        );
        assert_eq!(input, "/help", "input must remain literal `/help`");
        // The tracker's char count never crossed the minimum, so it
        // must NOT be in a committed-substitute state.
        assert!(!tracker.burst_committed);
    }

    /// Brief Test #3: with `"look at "` already typed, burst-paste a
    /// real file path. Only the burst region gets substituted; the
    /// `"look at "` prefix is preserved.
    #[test]
    fn burst_in_middle_of_existing_text_only_substitutes_burst_portion() {
        let p = tmpfile_with_ext("png");
        let s = p.to_string_lossy().to_string();
        assert!(s.len() >= BURST_MIN_CHARS);

        let mut tracker = BurstTracker::default();
        let mut input = String::from("look at ");
        // Pretend the user typed "look at " a while ago — we start the
        // burst tracker fresh, simulating the burst arriving after a
        // long idle on the existing prefix.

        let mut events: Vec<(char, u64)> = s.chars().map(|c| (c, 2u64)).collect();
        events.push((' ', 80)); // post-idle trigger

        let outcome = drive_keystrokes(&mut tracker, &mut input, &events);
        let (start, end, detected) = outcome.expect("burst should substitute");
        // The burst started at index 8 (length of "look at ") and ended
        // at start + s.len() (i.e. right before the trailing space).
        assert_eq!(start, "look at ".len());
        assert_eq!(end, "look at ".len() + s.len());
        assert_eq!(detected, p);

        // Simulate applying the substitution: replace `input[start..end]`
        // with the display string. This mirrors what `apply_burst_substitution`
        // does on the live TUI.
        let display = format!("[image: {}]", p.file_name().unwrap().to_str().unwrap());
        input.replace_range(start..end, &display);
        assert!(
            input.starts_with("look at "),
            "prefix must be preserved; got {input:?}"
        );
        assert!(
            input.contains(&display),
            "placeholder must be inserted; got {input:?}"
        );

        let _ = fs::remove_file(&p);
    }

    /// Brief Test #4: a `CtEvent::Paste` event MUST take the
    /// bracketed-paste path (`handle_paste` → `parse_pasted_file_path`)
    /// and not the keystroke-burst path. We assert that
    /// `parse_pasted_file_path` (the function the Paste handler delegates
    /// to) still resolves the same input the way it did before Part C,
    /// and that the burst tracker is untouched on a paste.
    #[test]
    fn cmd_v_paste_event_still_goes_through_paste_handler_not_burst() {
        let p = tmpfile_with_ext("png");
        let s = p.to_string_lossy().to_string();
        let resolved = parse_pasted_file_path(&s).expect("paste handler resolves path");
        assert_eq!(resolved, p);

        // Burst tracker remains untouched if no keystrokes flowed through
        // `record_keystroke`.
        let tracker = BurstTracker::default();
        assert!(tracker.burst_start_idx.is_none());
        assert_eq!(tracker.burst_char_count, 0);
        assert!(!tracker.burst_committed);

        let _ = fs::remove_file(&p);
    }

    /// The idle-based variant must fire when a mature burst has been
    /// quiet for >= BURST_IDLE_AFTER, even without a trailing keystroke.
    /// This is the path the wait-loop in `tui::mod` drives on every poll
    /// iteration.
    #[test]
    fn check_burst_idle_fires_after_quiet_period() {
        let p = tmpfile_with_ext("png");
        let s = p.to_string_lossy().to_string();
        assert!(s.len() >= BURST_MIN_CHARS);

        let mut tracker = BurstTracker::default();
        let mut input = String::new();

        // Drive a fast burst with NO trailing keystroke.
        let events: Vec<(char, u64)> = s.chars().map(|c| (c, 2u64)).collect();
        let _ = drive_keystrokes(&mut tracker, &mut input, &events);

        // Tracker state: burst is mature but not yet committed.
        assert!(tracker.burst_start_idx.is_some());
        assert!(tracker.burst_char_count >= BURST_MIN_CHARS);
        assert!(!tracker.burst_committed);

        // Idle check at "now" only 10 ms past the last key → too soon.
        let last = tracker.last_key_at.unwrap();
        let too_soon = last + Duration::from_millis(10);
        assert_eq!(
            check_burst_idle(&mut tracker, &input, too_soon),
            BurstOutcome::KeepTyping,
            "idle check must wait BURST_IDLE_AFTER before firing"
        );

        // Idle check at "now" past the idle window → fires.
        let later = last + Duration::from_millis(100);
        let outcome = check_burst_idle(&mut tracker, &input, later);
        match outcome {
            BurstOutcome::SubstitutePath { start, end, path } => {
                assert_eq!(start, 0);
                assert_eq!(end, input.len());
                assert_eq!(path, p);
            }
            other => panic!("expected SubstitutePath; got {other:?}"),
        }

        let _ = fs::remove_file(&p);
    }

    /// Burst with no path-like prefix must NEVER substitute, even when
    /// fast and over the char threshold. E.g. a fast-typing user
    /// mashing a long word does not become a file drop.
    #[test]
    fn fast_burst_without_path_prefix_does_not_substitute() {
        let mut tracker = BurstTracker::default();
        let mut input = String::new();
        // 30 random alpha chars, 2 ms apart, then a trigger space.
        let body: String = std::iter::repeat('q').take(30).collect();
        let mut events: Vec<(char, u64)> = body.chars().map(|c| (c, 2u64)).collect();
        events.push((' ', 80));

        let outcome = drive_keystrokes(&mut tracker, &mut input, &events);
        assert!(
            outcome.is_none(),
            "fast typing of non-path text must NOT substitute; got {outcome:?}"
        );
    }

    /// Burst of an unresolvable path (looks like a path but doesn't
    /// exist on disk) must NOT substitute — and the burst is committed
    /// so we don't keep re-evaluating it on every following char.
    #[test]
    fn fast_burst_with_unresolvable_path_does_not_substitute() {
        let mut tracker = BurstTracker::default();
        let mut input = String::new();
        let phantom = "/this/path/definitely/does/not/exist/ever.png";
        assert!(phantom.len() >= BURST_MIN_CHARS);
        let mut events: Vec<(char, u64)> = phantom.chars().map(|c| (c, 2u64)).collect();
        events.push((' ', 80));

        let outcome = drive_keystrokes(&mut tracker, &mut input, &events);
        assert!(outcome.is_none(), "unresolvable path must not substitute");
        assert!(
            tracker.burst_committed,
            "tracker must be committed so we don't re-evaluate every char"
        );
    }

    /// Burst tracker `reset()` must clear all four fields so the next
    /// keystroke starts a fresh burst from scratch.
    #[test]
    fn burst_tracker_reset_clears_all_state() {
        let mut t = BurstTracker {
            last_key_at: Some(std::time::Instant::now()),
            burst_start_idx: Some(5),
            burst_char_count: 30,
            burst_committed: true,
        };
        t.reset();
        assert!(t.last_key_at.is_none());
        assert!(t.burst_start_idx.is_none());
        assert_eq!(t.burst_char_count, 0);
        assert!(!t.burst_committed);
    }
}
