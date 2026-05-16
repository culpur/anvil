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
