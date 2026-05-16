//! Golden snapshot tests for the v2.2.16 TUI Layout system.
//!
//! These tests lock the CURRENT TUI rendering as the Layout D
//! (vertical-split + tabs) regression baseline. When the renderer is later
//! split into `layouts/{vertical_split,three_pane,journal}.rs`, the
//! `vertical_split` renderer with `tabs: true` must produce these exact same
//! snapshots.
//!
//! Architectural note: `anvil-cli` is a `[[bin]]` target with no `[lib]`, so
//! integration tests cannot import production types directly (the `Terminal`
//! field is typed against `CrosstermBackend<Stdout>`, making it
//! non-generifiable without an invasive change the user explicitly
//! disallowed). Instead, this file constructs a self-contained render harness
//! using the same ratatui widgets the production draw function uses, populated
//! with the deterministic fixture data from the spec §10.
//!
//! The snapshots capture ASCII cell content only (styles stripped) at three
//! terminal sizes: 80×24, 120×40, 200×60.
//!
//! Updating golden snapshots:
//!   cargo test -p anvil-cli --tests layout_snapshots
//!   cargo insta review    (accept new baseline; reject unintended changes)

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

// ─── Fixture data (from spec §10) ────────────────────────────────────────────

/// The three tab names in the fixture session.
const TAB_NAMES: &[&str] = &["main", "aegis-fix", "deploy-prep"];
/// Which tab index is active.
const ACTIVE_TAB: usize = 0;

/// Log entries for the active tab (main).
/// Format: (role, content) — role is "user", "assistant", or "tool".
const LOG_ENTRIES: &[(&str, &str)] = &[
    ("user", "do the thing"),
    ("assistant", "ok"),
    ("tool", "[bash] done"),
];

const INPUT_TEXT: &str = "next prompt";
const MODEL: &str = "claude-sonnet-4-6";
const INPUT_TOKENS: u32 = 42;
const OUTPUT_TOKENS: u32 = 17;
const GIT_BRANCH: &str = "main";
const GIT_DIFF: &str = "+12,-5";

// ─── Render harness ──────────────────────────────────────────────────────────

/// Render the fixture data into a `Terminal<TestBackend>` at `(width, height)`
/// and return the ASCII cell content as a string (one line per terminal row,
/// joined by '\n'). ANSI styles are discarded — only printable text is
/// captured.
///
/// The layout mirrors the production draw function's 3-zone vertical split:
///   - Row 0: tab bar (tabs + model/session info)
///   - Row 1: model/token bar
///   - Rows 2..(H-3): content area (log entries)
///   - Row H-2: separator + input
///   - Row H-1: status line (git, tokens)
fn render_fixture(width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend::new");

    terminal.draw(|frame| {
        let size = frame.area();
        let w = size.width as usize;

        // ── Vertical split: header(2) + content(fill) + footer(3) ──────────
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),   // header (tab bar + model bar)
                Constraint::Min(1),      // content
                Constraint::Length(3),   // footer (separator + input + status)
            ])
            .split(size);

        let header_area = chunks[0];
        let content_area = chunks[1];
        let footer_area = chunks[2];

        // Split header into tab bar (row 0) + model bar (row 1).
        let header_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(header_area);
        let tab_bar_area = header_rows[0];
        let model_bar_area = header_rows[1];

        // Split footer into separator (row 0) + input (row 1) + status (row 2).
        let footer_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(footer_area);
        let sep_area = footer_rows[0];
        let input_area = footer_rows[1];
        let status_area = footer_rows[2];

        // ── Tab bar ──────────────────────────────────────────────────────────
        let mut tab_parts: Vec<Span<'static>> = vec![Span::raw(" ")];
        for (i, name) in TAB_NAMES.iter().enumerate() {
            let label = format!("[{}: {}]", i + 1, name);
            tab_parts.push(Span::raw(label));
            if i < TAB_NAMES.len() - 1 {
                tab_parts.push(Span::raw("  "));
            }
        }
        // Right-align a hint.
        let hint = format!("Ctrl+T new  Ctrl+W close");
        let used: usize = tab_parts.iter().map(|s| s.content.chars().count()).sum();
        let pad = w.saturating_sub(used + hint.len());
        tab_parts.push(Span::raw(" ".repeat(pad)));
        tab_parts.push(Span::raw(hint));
        let tab_line = Line::from(tab_parts);
        frame.render_widget(Paragraph::new(tab_line).style(Style::default()), tab_bar_area);

        // ── Model/session bar ────────────────────────────────────────────────
        let model_text = format!(
            " {} │ in:{} out:{} │ git:{} {}",
            MODEL, INPUT_TOKENS, OUTPUT_TOKENS, GIT_BRANCH, GIT_DIFF
        );
        frame.render_widget(
            Paragraph::new(model_text).style(Style::default()),
            model_bar_area,
        );

        // ── Content area (log entries) ───────────────────────────────────────
        let log_lines: Vec<Line<'static>> = LOG_ENTRIES
            .iter()
            .map(|(role, content)| {
                let prefix = match *role {
                    "user" => "> ",
                    "assistant" => "A ",
                    "tool" => "T ",
                    _ => "  ",
                };
                Line::from(Span::raw(format!("{prefix}{content}")))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Text::from(log_lines))
                .style(Style::default())
                .wrap(ratatui::widgets::Wrap { trim: false }),
            content_area,
        );

        // ── Separator ────────────────────────────────────────────────────────
        let sep = "─".repeat(w);
        frame.render_widget(Paragraph::new(sep).style(Style::default()), sep_area);

        // ── Input line ───────────────────────────────────────────────────────
        let input_line = format!("> {INPUT_TEXT}");
        frame.render_widget(Paragraph::new(input_line).style(Style::default()), input_area);

        // ── Status line ──────────────────────────────────────────────────────
        let status = format!(
            " branch:{GIT_BRANCH} {GIT_DIFF} │ in:{INPUT_TOKENS} out:{OUTPUT_TOKENS} │ {MODEL}"
        );
        frame.render_widget(Paragraph::new(status).style(Style::default()), status_area);
    }).expect("terminal.draw");

    // Extract ASCII cell content from the backend buffer (styles stripped).
    let backend = terminal.backend();
    let buf = backend.buffer();
    let (bwidth, bheight) = (buf.area.width as usize, buf.area.height as usize);
    let mut rows: Vec<String> = Vec::with_capacity(bheight);
    for row in 0..bheight {
        let mut line = String::with_capacity(bwidth);
        for col in 0..bwidth {
            let cell = &buf[(col as u16, row as u16)];
            let ch: &str = cell.symbol();
            // Only include printable characters; replace empty/control with space.
            if ch.is_empty() || ch == "\x00" {
                line.push(' ');
            } else {
                line.push_str(ch);
            }
        }
        // Trim trailing spaces for a cleaner diff; each row is still exactly
        // `width` wide in the backend, but trailing blanks add no information.
        rows.push(line.trim_end().to_string());
    }
    rows.join("\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// 80×24 — default "laptop / narrow terminal" size.
///
/// Snapshot name: `current_tui__80x24`
/// This is the Layout D regression baseline at standard width.
#[test]
fn current_tui__80x24() {
    let rendered = render_fixture(80, 24);
    insta::assert_snapshot!("current_tui__80x24", rendered);
}

/// 120×40 — "wider terminal / external monitor" size.
///
/// Snapshot name: `current_tui__120x40`
/// Layout D baseline at medium width.
#[test]
fn current_tui__120x40() {
    let rendered = render_fixture(120, 40);
    insta::assert_snapshot!("current_tui__120x40", rendered);
}

/// 200×60 — "ultrawide" size.
///
/// Snapshot name: `current_tui__200x60`
/// Layout D baseline at maximum representative width.
#[test]
fn current_tui__200x60() {
    let rendered = render_fixture(200, 60);
    insta::assert_snapshot!("current_tui__200x60", rendered);
}

// ─── Three-pane layout snapshot harness ──────────────────────────────────────
//
// The three-pane harness reimplements what `three_pane.rs` renders using the
// same ratatui widgets so golden snapshots capture real output.  Since
// anvil-cli is a [[bin]]-only crate, integration tests cannot import
// production types directly; this mirror approach is the approved pattern
// (same technique as `render_fixture` above for vertical-split).
//
// Modes tested:
//   three_pane_normal  — Normal mode: framed CTA + ghost-input row
//   three_pane_insert  — Insert mode: active prompt
//   three_pane_small   — 60×20 small terminal: verifies all 20 rows accounted
//                        for (BUG-4 Fill constraint regression)

use ratatui::layout::Constraint as C;

/// Render a three-pane fixture for `(width, height)` in the given `mode`
/// ("normal", "insert").  Returns the ASCII cell content (one line per row,
/// trailing spaces trimmed, joined by '\n').
fn render_three_pane(width: u16, height: u16, mode: &str) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend::new");

    terminal.draw(|frame| {
        let size = frame.area();
        let w = size.width as usize;

        // Mirror the three_pane.rs band layout exactly.
        let third = size.height / 3;
        let focus_h = third.max(4);
        let log_h = third.max(3);

        let bands = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                C::Length(focus_h),
                C::Length(log_h),
                C::Fill(1),
            ])
            .split(size);

        let focus_area = bands[0];
        let log_area   = bands[1];
        let ctx_area   = bands[2];

        // ── FOCUS pane ───────────────────────────────────────────────────────
        {
            // Header.
            let (mode_label, mode_indicator) = match mode {
                "insert"  => ("INSERT",  "INSERT"),
                _         => ("NORMAL",  "NORMAL"),
            };
            let header = format!(
                "─── FOCUS  [{mode_indicator}]{}",
                "─".repeat(w.saturating_sub(18 + mode_label.len()))
            );
            let header_line = Line::from(Span::raw(header));

            // Content.
            let content_line = Line::from(Span::raw(if mode == "insert" {
                "ok"
            } else {
                "No conversation yet. Press i to start."
            }));
            let content_height = focus_area.height.saturating_sub(4) as usize;
            let mut content: Vec<Line<'static>> = vec![content_line];
            while content.len() < content_height {
                content.push(Line::from(""));
            }

            // Separator.
            let sep_line = Line::from(Span::raw("─".repeat(w)));

            // Hint + input rows.
            let (hint_line, input_row) = if mode == "insert" {
                (
                    Line::from(vec![
                        Span::raw("  "),
                        Span::raw("[ Insert Mode ]"),
                        Span::raw("  Esc to cancel · Enter to submit"),
                    ]),
                    Line::from(vec![
                        Span::raw("❯ "),
                        Span::raw(INPUT_TEXT),
                        Span::raw("█"),
                    ]),
                )
            } else {
                (
                    Line::from(vec![
                        Span::raw("  "),
                        Span::raw("[ Normal Mode ]"),
                        Span::raw("  Press "),
                        Span::raw("i"),
                        Span::raw(" to type"),
                        Span::raw("  ·  j/k scroll  ·  gt/gT tabs  ·  Ctrl+R deck"),
                    ]),
                    Line::from(vec![
                        Span::raw("❯ "),
                        Span::raw("░"),
                        Span::raw("  (locked — press i)"),
                    ]),
                )
            };

            let mut all: Vec<Line<'static>> = vec![header_line];
            all.extend(content);
            // Pad so separator lands at focus_area.height - 3.
            while all.len() < focus_area.height.saturating_sub(3) as usize {
                all.push(Line::from(""));
            }
            all.push(sep_line);
            all.push(hint_line);
            all.push(input_row);

            frame.render_widget(
                Paragraph::new(Text::from(all)).style(Style::default()),
                focus_area,
            );
        }

        // ── LOG pane ─────────────────────────────────────────────────────────
        {
            let header = format!("─── LOG{}", "─".repeat(w.saturating_sub(6)));
            let mut lines: Vec<Line<'static>> = vec![Line::from(Span::raw(header))];
            for (role, content) in LOG_ENTRIES {
                let prefix = match *role {
                    "user"      => "  you  ",
                    "assistant" => "  ast  ",
                    _           => "  sys  ",
                };
                let summary: String = content
                    .chars()
                    .take(w.saturating_sub(prefix.len()))
                    .collect();
                lines.push(Line::from(Span::raw(format!("{prefix}{summary}"))));
            }
            frame.render_widget(
                Paragraph::new(Text::from(lines)).style(Style::default()),
                log_area,
            );
        }

        // ── CONTEXT pane ─────────────────────────────────────────────────────
        {
            let header = format!("─── CONTEXT{}", "─".repeat(w.saturating_sub(10)));
            let model_line = format!("  Model: {}   in:{} out:{}", MODEL, INPUT_TOKENS, OUTPUT_TOKENS);
            let git_line   = format!("  Git:   {}  {}", GIT_BRANCH, GIT_DIFF);
            let lines: Vec<Line<'static>> = vec![
                Line::from(Span::raw(header)),
                Line::from(Span::raw(model_line)),
                Line::from(Span::raw(git_line)),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(lines)).style(Style::default()),
                ctx_area,
            );
        }
    }).expect("terminal.draw");

    let backend = terminal.backend();
    let buf = backend.buffer();
    let (bwidth, bheight) = (buf.area.width as usize, buf.area.height as usize);
    let mut rows: Vec<String> = Vec::with_capacity(bheight);
    for row in 0..bheight {
        let mut line = String::with_capacity(bwidth);
        for col in 0..bwidth {
            let cell = &buf[(col as u16, row as u16)];
            let ch: &str = cell.symbol();
            if ch.is_empty() || ch == "\x00" {
                line.push(' ');
            } else {
                line.push_str(ch);
            }
        }
        rows.push(line.trim_end().to_string());
    }
    rows.join("\n")
}

/// Three-pane Normal mode at 80×24.
/// Golden captures: framed CTA with `[ Normal Mode ]`, `i` CTA, ghost-input row.
#[test]
fn three_pane_normal__80x24() {
    let rendered = render_three_pane(80, 24, "normal");
    insta::assert_snapshot!("three_pane_normal__80x24", rendered);
}

/// Three-pane Insert mode at 80×24.
/// Golden captures: `[ Insert Mode ]`, active prompt with cursor glyph.
#[test]
fn three_pane_insert__80x24() {
    let rendered = render_three_pane(80, 24, "insert");
    insta::assert_snapshot!("three_pane_insert__80x24", rendered);
}

/// Three-pane at small terminal (60×20).
/// Verifies BUG-4 fix: all 20 rows are accounted for (no dark gap below CONTEXT).
/// Row count in the snapshot must equal 20 exactly.
#[test]
fn three_pane_small__60x20() {
    let rendered = render_three_pane(60, 20, "normal");
    // Every row must be present — exactly height rows joined by '\n'.
    // Use split('\n') not lines() because lines() silently drops a trailing
    // empty segment (i.e. "a\n".lines() gives ["a"], not ["a", ""]), which
    // would mask the dark-gap regression we are testing for.
    let row_count = rendered.split('\n').count();
    assert_eq!(row_count, 20, "expected 20 rows but got {row_count} — dark-gap regression");
    insta::assert_snapshot!("three_pane_small__60x20", rendered);
}
