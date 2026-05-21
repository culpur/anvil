/// Shared sub-renderers for the layout system.
///
/// These functions are used by multiple layout renderers (vertical_split,
/// three_pane, journal) to paint common UI elements so there is a single
/// canonical implementation for each widget.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use rust_i18n::t;

use runtime::Rgb;

use crate::tui::snapshot::LayoutSnapshot;

/// Convert a runtime `Rgb` triple into a ratatui `Color`.
#[inline]
pub(super) const fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Render the tab strip into `area` (a single-row rect).
///
/// Populates `tab_hits_out` with click-geometry entries so the input handler
/// can dispatch mouse Down events to switch/close tabs. Returns the tab bar row
/// (always `area.y`).
///
/// `can_close` — pass true when more than one tab is open (enables the × glyph).
pub(super) fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<crate::tui::TabHit>,
) {
    use crate::tui::TabHit;
    let theme = &snap.theme;
    let width = area.width as usize;

    let mut tab_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut cursor_col: u16 = area.x + 1;

    for (idx, (_tab_id, tab_name, is_active, has_unread, has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        // Base label without badges.
        let base = tab_name.clone();
        let label_start = cursor_col;

        // Build the tab label with inline badges:
        //   - Active tab: cyan + bold, with cyan underline indicator
        //   - Inactive: dim
        //   - has_unread: superscript digit "²" after name
        //   - has_perm: "⚠" glyph after name (and after unread if both)
        let name_style = if *is_active {
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(rgb(theme.text_secondary))
                .add_modifier(Modifier::DIM)
        };
        let badge_style = Style::default()
            .fg(rgb(theme.warning))
            .add_modifier(Modifier::BOLD);
        let perm_style = Style::default()
            .fg(rgb(theme.warning))
            .add_modifier(Modifier::BOLD);

        // Prefix space.
        tab_spans.push(Span::raw(" "));
        cursor_col += 1;

        // Active indicator: ● for active, ○ for inactive.
        let dot = if *is_active { "● " } else { "○ " };
        let dot_style = if *is_active { name_style } else {
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
        };
        tab_spans.push(Span::styled(dot.to_string(), dot_style));
        cursor_col += dot.chars().count() as u16;

        // Tab name.
        tab_spans.push(Span::styled(base.clone(), name_style));
        cursor_col += base.chars().count() as u16;

        // Unread superscript (use ² unicode superscript two as a compact badge).
        if *has_unread {
            tab_spans.push(Span::styled("²".to_string(), badge_style));
            cursor_col += 1;
        }

        // Pending permission glyph.
        if *has_perm {
            tab_spans.push(Span::raw(" "));
            tab_spans.push(Span::styled("⚠".to_string(), perm_style));
            cursor_col += 2;
        }

        let label_end = cursor_col;

        let close_col = if snap.can_close_tab {
            let col = cursor_col + 1;
            tab_spans.push(Span::raw(" "));
            tab_spans.push(Span::styled(
                "×",
                Style::default()
                    .fg(rgb(theme.border))
                    .add_modifier(Modifier::DIM),
            ));
            tab_spans.push(Span::raw(" "));
            cursor_col += 3;
            Some(col)
        } else {
            tab_spans.push(Span::raw(" "));
            cursor_col += 1;
            None
        };

        tab_hits_out.push(TabHit { idx, label_start, label_end, close_col });
    }

    let hint = Span::styled(
        t!("tui.tab.hint").to_string(),
        Style::default().fg(rgb(theme.border)),
    );
    let left_len: usize = tab_spans.iter().map(|s| s.content.chars().count()).sum();
    let hint_len = hint.content.chars().count();
    let pad = width.saturating_sub(left_len + hint_len);
    tab_spans.push(Span::raw(" ".repeat(pad)));
    tab_spans.push(hint);

    let widget = Paragraph::new(Line::from(tab_spans))
        .style(Style::default().bg(rgb(theme.bg_primary)));
    frame.render_widget(widget, area);
}

/// Task #574: rebuild `tab_hits_out` without repainting the tab bar.
///
/// Originally used by the region-gated render path when the TAB_STRIP
/// dirty bit was not set on this frame. After task #648 removed the
/// "skip paint on narrow dirty" approach (the ratatui frame-diff erased
/// regions we skipped — see `vertical_split.rs::render`), this helper
/// no longer has a production caller but stays in tree so the
/// `tab_hits_match_render_tab_bar` regression test below keeps
/// guarding the click-hit geometry in case a future render path needs
/// the shape again.
#[allow(dead_code)]
pub(super) fn rebuild_tab_hits(
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<crate::tui::TabHit>,
) {
    use crate::tui::TabHit;
    let mut cursor_col: u16 = area.x + 1;
    for (idx, (_tab_id, tab_name, _is_active, has_unread, has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        let label_start = cursor_col;
        // Prefix space.
        cursor_col += 1;
        // Dot prefix ("● " or "○ ", 2 chars).
        cursor_col += 2;
        // Tab name.
        cursor_col += tab_name.chars().count() as u16;
        // Unread superscript.
        if *has_unread {
            cursor_col += 1;
        }
        // Perm glyph (" ⚠" — space + glyph).
        if *has_perm {
            cursor_col += 2;
        }
        let label_end = cursor_col;
        let close_col = if snap.can_close_tab {
            let col = cursor_col + 1;
            // " × " takes 3 columns when closable.
            cursor_col += 3;
            Some(col)
        } else {
            // Single trailing space.
            cursor_col += 1;
            None
        };
        tab_hits_out.push(TabHit { idx, label_start, label_end, close_col });
    }
}

/// Render the model/session info bar into `area` (a single-row rect).
pub(super) fn render_model_bar(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let short_session = if snap.session_id.len() > 20 {
        format!("…{}", &snap.session_id[snap.session_id.len() - 18..])
    } else {
        snap.session_id.clone()
    };
    let bar_text = format!(
        " ⚒ Anvil v{}  │  {}  │  {}",
        env!("CARGO_PKG_VERSION"),
        snap.model,
        short_session,
    );
    let widget = Paragraph::new(bar_text).style(
        Style::default()
            .fg(rgb(theme.accent))
            .bg(rgb(theme.header_bg))
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(widget, area);
}

/// Render the slash-command completion popup, if visible, into an overlay above
/// `anchor_area`. This is the same popup used by all layouts.
pub(super) fn render_completion_popup(frame: &mut Frame, anchor_area: Rect, snap: &LayoutSnapshot) {
    if !snap.completion_visible || snap.completion_matches.is_empty() {
        return;
    }
    let theme = &snap.theme;
    let width = anchor_area.width as usize;
    const POPUP_BODY_HEIGHT: usize = 12;
    let visible = snap.completion_matches.len().min(POPUP_BODY_HEIGHT);
    let popup_height = visible as u16 + 2;
    let popup_width = (width as u16).min(60);
    let popup_y = anchor_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: anchor_area.x + 1,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    let start = snap.completion_view_offset.min(
        snap.completion_matches.len().saturating_sub(visible),
    );
    let end = (start + visible).min(snap.completion_matches.len());
    let items: Vec<Line<'static>> = snap.completion_matches[start..end]
        .iter()
        .enumerate()
        .map(|(rel_i, item)| {
            let i = start + rel_i;
            let insert: &str = item.0.as_str();
            let hint: &str = item.1.as_str();
            let is_header: bool = item.2;
            let is_free_text: bool = item.3;

            if is_header {
                return Line::from(Span::styled(
                    format!(" {insert}"),
                    Style::default()
                        .fg(rgb(theme.accent))
                        .add_modifier(Modifier::BOLD)
                        .bg(rgb(theme.bg_card)),
                ));
            }

            let is_selected = i == snap.completion_selected;
            let (fg, bg) = if is_selected {
                (rgb(theme.bg_primary), rgb(theme.accent))
            } else {
                (rgb(theme.text_primary), rgb(theme.bg_card))
            };
            let cmd_width = 24.min(popup_width as usize - 4);
            let padded_cmd = format!("{:<width$}", insert, width = cmd_width);
            let insert_style = if is_free_text {
                Style::default()
                    .fg(rgb(theme.text_secondary))
                    .bg(bg)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC)
            } else {
                Style::default().fg(fg).bg(bg)
            };

            Line::from(vec![
                Span::styled(format!(" {padded_cmd}"), insert_style),
                Span::styled(
                    format!(" {hint}"),
                    Style::default()
                        .fg(if is_selected {
                            rgb(theme.bg_primary)
                        } else {
                            rgb(theme.text_secondary)
                        })
                        .bg(bg)
                        .add_modifier(Modifier::DIM),
                ),
            ])
        })
        .collect();

    let popup_widget = Paragraph::new(Text::from(items)).block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(rgb(theme.border)))
            .style(Style::default().bg(rgb(theme.bg_card))),
    );
    frame.render_widget(ratatui::widgets::Clear, popup_area);
    frame.render_widget(popup_widget, popup_area);
}

// ─── Shared layout helpers (task #607) ────────────────────────────────────────
//
// `section_header_line`, `right_aligned_row`, and `truncate` were originally
// private to `vertical_split.rs` where the rail uses them to paint sections
// like MEMORY, MODEL, PERMISSIONS, etc. Task #607 (inline 7-layer MEMORY
// block in the classic layout) needs the same row geometry, so we lift them
// here as `pub(super)` and let both renderers call the same code. Keeping the
// helpers in one place also means future row-layout tweaks (padding, glyphs,
// truncation policy) don't drift between layouts.

/// Truncate a string to at most `max_chars` display characters. The last
/// character is replaced with `…` when truncation actually occurs.
pub(super) fn truncate(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Build a section header line with an optional parenthetical qualifier.
///
/// Example: `" SESSIONS"` — leading space, label.
/// Example: `" AGENTS (GLOBAL)"` — qualifier appended in parens.
/// The line is right-padded to `w` so the styled span fills the row.
pub(super) fn section_header_line(
    label: &'static str,
    qualifier: Option<&'static str>,
    w: usize,
    style: Style,
) -> Line<'static> {
    let q_owned = qualifier.map(str::to_owned);
    section_header_line_owned(label.to_owned(), q_owned, w, style)
}

/// Like `section_header_line`, but accepts owned `String`s so callers can pass
/// translated text from `rust_i18n::t!()` (which returns a `Cow<'_, str>`
/// rather than a `&'static str`). Task #746 / i18n end-to-end.
pub(super) fn section_header_line_owned(
    label: String,
    qualifier: Option<String>,
    w: usize,
    style: Style,
) -> Line<'static> {
    let full = if let Some(q) = qualifier {
        format!(" {label} ({q})")
    } else {
        format!(" {label}")
    };
    let padded = truncate(
        format!("{full}{}", " ".repeat(w.saturating_sub(full.chars().count()))),
        w,
    );
    Line::from(Span::styled(padded, style))
}

/// Build a right-aligned two-column row: label on the left, value on the right.
///
/// Example (w=24): `" running          3 tabs"`.
/// The leading space + label + pad + value packs to exactly `w` columns,
/// truncating with `…` on overflow.
pub(super) fn right_aligned_row(
    label: &str,
    value: &str,
    w: usize,
    style: Style,
) -> Line<'static> {
    let prefix = format!(" {label}");
    let total = prefix.len() + value.len();
    let pad = if total + 1 < w { w - total } else { 1 };
    let text = format!("{prefix}{}{value}", " ".repeat(pad));
    let truncated = truncate(text, w);
    Line::from(Span::styled(truncated, style))
}

// ─── Seven-layer memory + chrome helpers (task #594 / BUG-13) ────────────────
//
// These read on-disk caches under `~/.anvil/` to populate the
// `LayoutSnapshot.memory_*` and related fields. Every helper is best-effort
// — when a directory is missing or unreadable it returns 0. The functions
// are `pub(in crate::tui)` so `tui::collect_snapshot` can call them while
// keeping the implementation co-located with the renderer that reads it.

/// Resolve `~/.anvil/` (matches the rest of the codebase).
fn anvil_home() -> Option<std::path::PathBuf> {
    std::env::var_os("ANVIL_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".anvil"))
        })
}

/// Count immediate children of a directory that pass `predicate`.
/// Returns 0 if the directory is missing or unreadable.
fn count_in<F>(dir: std::path::PathBuf, predicate: F) -> usize
where
    F: Fn(&std::fs::DirEntry) -> bool,
{
    match std::fs::read_dir(&dir) {
        Ok(iter) => iter
            .filter_map(|e| e.ok())
            .filter(|e| predicate(e))
            .count(),
        Err(_) => 0,
    }
}

/// Count session JSON files under `~/.anvil/sessions/`.
pub(in crate::tui) fn count_episodic_sessions() -> usize {
    let Some(home) = anvil_home() else { return 0; };
    count_in(home.join("sessions"), |e| {
        e.path().extension().and_then(|s| s.to_str()) == Some("json")
    })
}

/// Return `(collections, archives)` for the QMD semantic store.
/// Collections is 1 when `~/.anvil/semantic/` exists, 0 otherwise.
/// Archives is the count of `.md`/`.txt` files inside it.
pub(in crate::tui) fn semantic_counts() -> (usize, usize) {
    let Some(home) = anvil_home() else { return (0, 0); };
    let dir = home.join("semantic");
    if !dir.exists() {
        return (0, 0);
    }
    let archives = count_in(dir, |e| {
        let path = e.path();
        let ext = path.extension().and_then(|s| s.to_str());
        matches!(ext, Some("md" | "txt" | "json"))
    });
    (1, archives)
}

/// Count skill `.md` files under `~/.anvil/skills/` (non-recursive).
pub(in crate::tui) fn count_skills() -> usize {
    let Some(home) = anvil_home() else { return 0; };
    count_in(home.join("skills"), |e| {
        e.path().extension().and_then(|s| s.to_str()) == Some("md")
    })
}

/// Count plugin subdirectories under `~/.anvil/plugins/`.
pub(in crate::tui) fn count_plugins() -> usize {
    let Some(home) = anvil_home() else { return 0; };
    count_in(home.join("plugins"), |e| {
        e.file_type().map(|t| t.is_dir()).unwrap_or(false)
    })
}

/// Count daily summary JSON files under `~/.anvil/daily/`.
pub(in crate::tui) fn count_daily_summaries() -> usize {
    let Some(home) = anvil_home() else { return 0; };
    count_in(home.join("daily"), |e| {
        e.path().extension().and_then(|s| s.to_str()) == Some("json")
    })
}

/// Count persisted permission decisions for the current project.
/// Best-effort: uses `std::env::current_dir()` to scope the lookup. Returns 0
/// when the project_dir cannot be resolved or no permission memory exists.
pub(in crate::tui) fn count_permission_decisions() -> usize {
    let Ok(cwd) = std::env::current_dir() else {
        return 0;
    };
    let pm = runtime::PermissionMemory::load(&cwd);
    pm.all_entries().count()
}

/// Best-effort archive count parsed from the live `qmd_status` string.
/// The status line is set by main.rs as `"QMD: <docs> docs, <vectors> vectors"`
/// — we pull the leading integer.
pub(in crate::tui) fn qmd_archive_count(qmd_status: &str) -> u32 {
    // Token after "QMD:" before " docs". Fall back to first integer found.
    qmd_status
        .split_whitespace()
        .find_map(|tok| tok.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Map a model name to a short cost-source label that matches the
/// `compute_cost_usd()` heuristic in the renderer:
///   - "local"   for Ollama/local models (contains `:` but not `:cloud`)
///   - "cloud"   for hosted ollama-cloud (contains `:cloud`)
///   - "OAuth"   for `claude-*` / `gpt-*` proper API names (heuristic)
///   - "metered" for anything else (priced via pricing_for_model)
pub(in crate::tui) fn cost_provider_label(model: &str) -> String {
    if model.contains(":cloud") {
        "cloud".to_string()
    } else if model.contains(':') {
        "local".to_string()
    } else if model.starts_with("claude-") {
        "OAuth".to_string()
    } else if runtime::pricing_for_model(model).is_some() {
        "metered".to_string()
    } else {
        "—".to_string()
    }
}

// ─── Assistant markdown rendering (#592) ─────────────────────────────────────
//
// `vertical_split`'s assistant-message renderer used to push every line as a
// `Span::raw`, so prose with `## headings`, `**bold**`, table-pipes, or
// `` `inline code` `` rendered with the literal markdown syntax visible.
// Classic.rs had the same problem (state.rs `LogEntry::Assistant` arm only
// `strip_ansi`s the body). The fix is a single shared helper both surfaces
// call so the deck-conversation column always shows styled prose.
//
// The helper takes the raw assistant text (NOT pre-rendered ANSI — that
// arrives as part of the streaming `pending_text` path which is also
// channeled through here) and emits styled `Line`s. We don't go through
// `markdown_to_ansi` + an ANSI parser because (a) we'd then need a separate
// SGR-to-Span layer, and (b) the inline rendering is faster + simpler for
// the narrow set of markdown the assistant produces in chat.
//
// Supported:
//   - ATX headings `# ` … `###### ` → bold, accent-coloured
//   - Bold `**text**` → bold span
//   - Italic `*text*` / `_text_` (single-character markers, word-bounded)
//   - Inline code `` `text` `` → magenta/256-colour
//   - Tables: `|` pipes coloured dim so the structure reads, cell text plain
//   - Fenced code-blocks `` ``` `` … `` ``` `` → entire body in code style
//   - Bullet markers `- `, `* `, `+ ` and numbered `1. ` left alone (already
//     readable)

#[allow(clippy::cast_possible_truncation)]
pub(in crate::tui) fn assistant_text_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_fence = false;
    let mut fence_lang: String = String::new();

    let code_block_style = Style::default().fg(Color::Rgb(0xc0, 0xc0, 0xc0));
    let code_block_border = Style::default().fg(Color::DarkGray);
    let table_pipe_style = Style::default().fg(Color::DarkGray);
    let heading_style = Style::default()
        .fg(Color::Rgb(0x44, 0xaa, 0xaa))
        .add_modifier(Modifier::BOLD);

    for raw in text.lines() {
        // Fence start/end (``` or ~~~).
        if raw.trim_start().starts_with("```") || raw.trim_start().starts_with("~~~") {
            if in_fence {
                in_fence = false;
                fence_lang.clear();
                lines.push(Line::from(Span::styled(
                    raw.to_string(),
                    code_block_border,
                )));
            } else {
                in_fence = true;
                fence_lang = raw.trim_start().trim_start_matches(['`', '~']).to_string();
                lines.push(Line::from(Span::styled(
                    raw.to_string(),
                    code_block_border,
                )));
            }
            continue;
        }
        if in_fence {
            lines.push(Line::from(Span::styled(
                raw.to_string(),
                code_block_style,
            )));
            continue;
        }

        // ATX heading: strip the `#`s + space, render bold + accent.
        let trimmed = raw.trim_start();
        if let Some(after_hashes) = strip_atx_heading(trimmed) {
            lines.push(Line::from(Span::styled(
                after_hashes.to_string(),
                heading_style,
            )));
            continue;
        }

        // Table row: contains `|` and is not pure prose. Treat as a row when
        // the line has at least 2 pipes or starts with `|`.
        let pipe_count = raw.bytes().filter(|b| *b == b'|').count();
        if pipe_count >= 2 || (raw.starts_with('|') && pipe_count >= 1) {
            let spans = split_on_char_styled(raw, '|', table_pipe_style);
            lines.push(Line::from(spans));
            continue;
        }

        // Inline markdown: split on **bold** / `code` / *italic* / _italic_.
        lines.push(Line::from(inline_markdown_spans(raw)));
    }

    lines
}

/// Return the text after the leading `#`s for an ATX heading, or `None`.
fn strip_atx_heading(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.is_empty() || bytes[0] != b'#' {
        return None;
    }
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    if i == 0 || i > 6 {
        return None;
    }
    // Require a space after the run of `#`s for it to be a heading.
    if i >= bytes.len() || bytes[i] != b' ' {
        return None;
    }
    Some(&line[i + 1..])
}

/// Split `s` on every occurrence of `delim`, returning a vec of styled spans
/// where the delimiter glyphs get `delim_style` and the runs between them
/// stay default.
fn split_on_char_styled(s: &str, delim: char, delim_style: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    for ch in s.chars() {
        if ch == delim {
            if !buf.is_empty() {
                out.push(Span::raw(std::mem::take(&mut buf)));
            }
            out.push(Span::styled(ch.to_string(), delim_style));
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        out.push(Span::raw(buf));
    }
    out
}

/// Render one prose line, applying inline-markdown styling.
///
/// Scanner-driven so we don't have to allocate intermediate strings per
/// rule: we walk the chars once and split spans as we go.
#[allow(clippy::cognitive_complexity)]
fn inline_markdown_spans(line: &str) -> Vec<Span<'static>> {
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let italic_style = Style::default().add_modifier(Modifier::ITALIC);
    let code_style = Style::default().fg(Color::Rgb(0xc8, 0x7c, 0xff));

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    let flush = |buf: &mut String, out: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            out.push(Span::raw(std::mem::take(buf)));
        }
    };

    while i < chars.len() {
        let c = chars[i];

        // **bold**
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_close_marker(&chars, i + 2, "**") {
                flush(&mut buf, &mut out);
                let body: String = chars[i + 2..end].iter().collect();
                out.push(Span::styled(body, bold_style));
                i = end + 2;
                continue;
            }
        }
        // `inline code`
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&ch| ch == '`') {
                let end = i + 1 + end;
                flush(&mut buf, &mut out);
                let body: String = chars[i + 1..end].iter().collect();
                out.push(Span::styled(body, code_style));
                i = end + 1;
                continue;
            }
        }
        // *italic* / _italic_ — single-char marker, word-bounded so URLs
        // and bullet markers don't get accidentally italicised.
        if (c == '*' || c == '_') && is_word_bounded_marker(&chars, i) {
            if let Some(end) = find_single_close_marker(&chars, i + 1, c) {
                flush(&mut buf, &mut out);
                let body: String = chars[i + 1..end].iter().collect();
                out.push(Span::styled(body, italic_style));
                i = end + 1;
                continue;
            }
        }

        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    if out.is_empty() {
        // Empty line: emit a single empty span so the Line still renders
        // a blank row of the correct height.
        out.push(Span::raw(String::new()));
    }
    out
}

fn find_close_marker(chars: &[char], start: usize, marker: &str) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    let mut j = start;
    while j + m.len() <= chars.len() {
        if chars[j..j + m.len()] == m[..] {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn find_single_close_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    let mut j = start;
    while j < chars.len() {
        if chars[j] == marker {
            // Require the closing marker NOT to be immediately preceded
            // by a space (CommonMark-ish: emphasis can't close after a
            // space).
            if j > start && !chars[j - 1].is_whitespace() {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

/// `*` and `_` only count as italic markers when their left side is the
/// start-of-line or a non-alphanumeric, and the next char is not whitespace.
fn is_word_bounded_marker(chars: &[char], i: usize) -> bool {
    let left_ok = i == 0 || !chars[i - 1].is_alphanumeric();
    let right_ok = i + 1 < chars.len() && !chars[i + 1].is_whitespace();
    left_ok && right_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_provider_label_local_for_ollama() {
        assert_eq!(cost_provider_label("qwen3.5:latest"), "local");
        assert_eq!(cost_provider_label("llama3:8b"), "local");
    }

    #[test]
    fn cost_provider_label_cloud_for_ollama_cloud() {
        assert_eq!(cost_provider_label("kimi-k2.6:cloud"), "cloud");
    }

    #[test]
    fn cost_provider_label_oauth_for_claude() {
        assert_eq!(cost_provider_label("claude-opus-4-6"), "OAuth");
        assert_eq!(cost_provider_label("claude-sonnet-4-6"), "OAuth");
    }

    #[test]
    fn cost_provider_label_dash_for_unknown() {
        assert_eq!(cost_provider_label("unknown-mystery-model"), "—");
    }

    #[test]
    fn qmd_archive_count_parses_status_string() {
        assert_eq!(qmd_archive_count("QMD: 42 docs, 100 vectors"), 42);
        assert_eq!(qmd_archive_count("QMD: active"), 0);
        assert_eq!(qmd_archive_count(""), 0);
        assert_eq!(qmd_archive_count("QMD: 8 docs"), 8);
    }

    // ── #592 assistant markdown rendering ─────────────────────────────

    /// Concatenate the contents of every span on a line.
    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Returns the `Modifier` flags applied to the span that contains
    /// `needle` inside `line`. Helper for the #592 contract that
    /// `**bold**` actually produces a BOLD span (not a raw `**…**`).
    fn modifiers_for(line: &Line<'static>, needle: &str) -> Option<Modifier> {
        line.spans
            .iter()
            .find(|s| s.content.contains(needle))
            .map(|s| s.style.add_modifier)
    }

    /// #592: assistant-message renderer in `vertical_split` used to emit
    /// raw markdown syntax (`##`, `**bold**`, table pipes) because each
    /// line was a single `Span::raw`. The fix routes assistant prose
    /// through `assistant_text_to_lines`, which strips heading markers,
    /// applies BOLD modifier on `**…**`, ITALIC on `*…*` / `_…_`,
    /// foreground colour on `` `code` ``, and dims table `|` glyphs.
    ///
    /// This test pins all of the above on a single representative input.
    #[test]
    fn vertical_split_renders_assistant_markdown_styled_not_raw() {
        let body = "\
## Heading line
This paragraph contains **bold** and *italic* and `code`.
| col1 | col2 |
|------|------|
| a    | b    |
";
        let lines = assistant_text_to_lines(body);
        assert!(lines.len() >= 5, "expected at least 5 lines, got {}", lines.len());

        // Heading: `##` markers stripped, content rendered.
        let heading = line_text(&lines[0]);
        assert!(
            !heading.contains("##"),
            "heading markers `##` must be stripped, got {heading:?}",
        );
        assert!(
            heading.contains("Heading line"),
            "heading text must survive, got {heading:?}",
        );
        // Heading should be BOLD (the renderer applies bold + accent).
        let h_mods = lines[0]
            .spans
            .iter()
            .next()
            .map(|s| s.style.add_modifier)
            .unwrap_or(Modifier::empty());
        assert!(
            h_mods.contains(Modifier::BOLD),
            "heading line must be BOLD-styled, got {h_mods:?}",
        );

        // Bold span: literal `**` must NOT appear in the rendered text.
        let prose = line_text(&lines[1]);
        assert!(
            !prose.contains("**"),
            "rendered prose must not contain literal `**`, got {prose:?}",
        );
        // The word "bold" must live in a BOLD-modified span.
        let bold_mods = modifiers_for(&lines[1], "bold").unwrap_or(Modifier::empty());
        assert!(
            bold_mods.contains(Modifier::BOLD),
            "the word inside **…** must be BOLD-styled, got {bold_mods:?}",
        );

        // Italic span: the word "italic" must live in an ITALIC-modified
        // span, and no literal `*italic*` should leak through.
        let it_mods = modifiers_for(&lines[1], "italic").unwrap_or(Modifier::empty());
        assert!(
            it_mods.contains(Modifier::ITALIC),
            "the word inside *…* must be ITALIC-styled, got {it_mods:?}",
        );

        // Inline code: rendered without the backticks.
        let code_text: String = lines[1]
            .spans
            .iter()
            .filter(|s| s.style.fg.is_some()) // code span has a colour set
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !prose.contains("`code`"),
            "literal backticks must not appear in rendered prose, got {prose:?}",
        );
        assert!(
            code_text.contains("code"),
            "inline code body `code` must end up in a styled span, got {code_text:?}",
        );

        // Table row: `|` glyphs styled (DarkGray), cell text default.
        let pipe_count_line = line_text(&lines[2]);
        assert!(
            pipe_count_line.matches('|').count() >= 2,
            "table row pipes must still render visibly, got {pipe_count_line:?}",
        );
        let pipe_dim_count = lines[2]
            .spans
            .iter()
            .filter(|s| s.content == "|" && s.style.fg == Some(Color::DarkGray))
            .count();
        assert!(
            pipe_dim_count >= 2,
            "every `|` in a table row must render with DarkGray, dim styling, got {pipe_dim_count}",
        );
    }
}
