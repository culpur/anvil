/// Layout C/F — Journal renderer.
///
/// Single-column timestamped journal view with Ctrl-K command palette.
///
/// Layout F (tabs: true): thread-switcher anchor line at row 0 with
///   `anvil-dev · aegis-fix · deploy-prep · +`
/// Layout C (tabs: false): anchor line absent, multi-thread state still
///   accessible via Ctrl+K palette "Switch thread: <name>" command.
///
/// Input: always-visible bottom row with `▌ ctrl-k for command palette`.
///
/// Ctrl-K palette (locked: full VS Code scope):
///   Fuzzy search over: slash commands, thread names, recent prompts, files.
///   Ctrl+J/Ctrl+K navigate inside palette (Emacs muscle memory).
///   Enter executes. Esc closes.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout as RLayout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use super::common::{render_completion_popup, rgb};
use super::{LayoutLocalState, TuiLayoutRenderer};
use crate::tui::helpers::strip_ansi;
use crate::tui::redraw::DirtyRegions;
use crate::tui::snapshot::LayoutSnapshot;
use crate::tui::state::LogEntry;
use crate::tui::TabHit;

pub(super) struct Renderer {
    pub tabs: bool,
}

impl TuiLayoutRenderer for Renderer {
    fn render(
        &self,
        frame: &mut Frame,
        snap: &LayoutSnapshot,
        local: &mut LayoutLocalState,
        tab_hits_out: &mut Vec<TabHit>,
    ) {
        let size = frame.area();
        let theme = &snap.theme;

        // BUG-3 fix (Option B): clear the entire frame so cells from a prior
        // layout do not survive ratatui's frame-diff into this layout's first
        // frame.
        //
        // Task #622 (CRITICAL accessibility fix): gate the full-screen Clear
        // on structural dirty (DirtyRegions::ALL) only — unconditional Clear
        // flashes on Gnome Terminal during streaming. See classic.rs for the
        // full rationale.
        let force_full_clear = snap.dirty_regions.contains(DirtyRegions::ALL)
            || std::env::var("ANVIL_TUI_FORCE_CLEAR")
                .map(|v| v == "1")
                .unwrap_or(false);
        if force_full_clear {
            frame.render_widget(ratatui::widgets::Clear, size);
        }

        // Extract local state.
        let (palette_open, palette_query, palette_selected) = match local {
            LayoutLocalState::Journal {
                palette_open,
                palette_query,
                palette_selected,
            } => (*palette_open, palette_query.clone(), *palette_selected),
            _ => (false, String::new(), 0),
        };

        // Task #648 (release-blocker fix): paint every band every frame.
        // See vertical_split.rs / classic.rs for the full ratatui API
        // rationale — skipping a region erases it on terminal via the
        // back-buffer diff.

        // ── Row allocation ────────────────────────────────────────────────────
        // Layout F: row 0 = thread switcher; rest = header + body + input row.
        // Layout C: no thread switcher row.
        let mut y_offset: u16 = 0;
        if self.tabs {
            let anchor_area = Rect {
                x: size.x,
                y: size.y,
                width: size.width,
                height: 1,
            };
            render_thread_switcher(frame, anchor_area, snap, tab_hits_out);
            y_offset = 1;
        }

        let remaining = Rect {
            x: size.x,
            y: size.y + y_offset,
            width: size.width,
            height: size.height.saturating_sub(y_offset),
        };

        // remaining = header(1) + body(fill) + input_row(1).
        let bands = RLayout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(remaining);

        let header_area = bands[0];
        let body_area = bands[1];
        let input_area = bands[2];

        // ── Header ────────────────────────────────────────────────────────────
        {
            let version = env!("CARGO_PKG_VERSION");
            let header_text = format!(
                " {} · v{} · {}",
                if snap.git_branch.is_empty() { "anvil".to_string() } else { snap.git_branch.clone() },
                version,
                snap.model,
            );
            frame.render_widget(ratatui::widgets::Clear, header_area);
            frame.render_widget(
                Paragraph::new(header_text).style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .bg(rgb(theme.bg_primary))
                        .add_modifier(Modifier::ITALIC),
                ),
                header_area,
            );
        }

        // ── Body (timestamped journal entries) ────────────────────────────────
        render_journal_body(frame, body_area, snap);

        // ── Input row ─────────────────────────────────────────────────────────
        {
            let input_line = if snap.input_text.is_empty() {
                Line::from(Span::styled(
                    " ▌ ctrl-k for command palette · ↑ history · enter to send",
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(vec![
                    Span::styled(" ▌ ", Style::default().fg(rgb(theme.accent))),
                    Span::raw(snap.input_text.clone()),
                    Span::styled("█", Style::default().fg(Color::White)),
                ])
            };
            frame.render_widget(ratatui::widgets::Clear, input_area);
            frame.render_widget(
                Paragraph::new(input_line).style(Style::default().bg(rgb(theme.bg_primary))),
                input_area,
            );
        }

        // Cursor in the input row — always set, since it's positional.
        let col = snap.cursor_pos.min(input_area.width.saturating_sub(4) as usize);
        frame.set_cursor_position(Position {
            x: input_area.x + 3 + col as u16,
            y: input_area.y,
        });

        // ── Ctrl-K palette overlay ─────────────────────────────────────────────
        if palette_open {
            render_palette(frame, size, snap, &palette_query, palette_selected);
        }

        // BUG-6: render the slash-command completion popup anchored to the
        // input row so `/` completions appear in journal layout the same way
        // they do in vertical-split (line 263) and three-pane (line 103).
        // Only renders when `snap.completion_visible` is true (gated inside
        // `render_completion_popup`), so it is a no-op when the palette is
        // also open.
        render_completion_popup(frame, input_area, snap);
    }
}

// ─── Thread switcher (Layout F) ───────────────────────────────────────────────

/// Task #574: rebuild journal thread-switcher click hit-test geometry
/// without repainting the band. Walks the same `tab_infos` sequence with
/// identical column arithmetic as `render_thread_switcher`.  After
/// task #648 the production path always paints the thread switcher; this
/// helper is retained for any future render variant that wants to
/// rebuild hits without painting.
#[allow(dead_code)]
fn rebuild_thread_switcher_hits(
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<TabHit>,
) {
    let mut cursor_col: u16 = area.x + 1;
    for (idx, (_tab_id, tab_name, _is_active, _has_unread, _has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        let label_len = tab_name.chars().count() as u16;
        let label_start = cursor_col;
        let label_end = cursor_col + label_len;
        tab_hits_out.push(TabHit { idx, label_start, label_end, close_col: None });
        // Separator between tabs (" · " = 3 cols) or final " · +" (4 cols).
        if idx + 1 < snap.tab_infos.len() {
            cursor_col = label_end + 3;
        } else {
            cursor_col = label_end + 4;
        }
    }
}

fn render_thread_switcher(
    frame: &mut Frame,
    area: Rect,
    snap: &LayoutSnapshot,
    tab_hits_out: &mut Vec<TabHit>,
) {
    use crate::tui::TabHit;
    let theme = &snap.theme;
    let width = area.width as usize;

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut cursor_col: u16 = area.x + 1;

    for (idx, (tab_id, tab_name, is_active, _has_unread, _has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        let label = tab_name.clone();
        let label_len = label.chars().count() as u16;
        let label_start = cursor_col;
        let label_end = cursor_col + label_len;

        let style = if *is_active {
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(rgb(theme.text_secondary))
        };
        spans.push(Span::styled(label, style));
        tab_hits_out.push(TabHit { idx, label_start, label_end, close_col: None });

        // Separator between tabs, " · " or " + " for the new-tab button.
        if idx + 1 < snap.tab_infos.len() {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
            cursor_col = label_end + 3;
        } else {
            spans.push(Span::styled(" · +", Style::default().fg(Color::DarkGray)));
            cursor_col = label_end + 4;
        }
        let _ = tab_id;
    }

    // Right-align a small hint.
    let hint = "Ctrl+Tab cycle";
    let left_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = width.saturating_sub(left_len + hint.len());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(rgb(theme.bg_primary))),
        area,
    );
}

// ─── Journal body ─────────────────────────────────────────────────────────────

fn render_journal_body(frame: &mut Frame, area: Rect, snap: &LayoutSnapshot) {
    let theme = &snap.theme;
    let width = area.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in &snap.log_snapshot {
        match entry {
            LogEntry::User(text) => {
                let summary: String = text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(width.saturating_sub(14))
                    .collect();
                lines.push(Line::from(vec![
                    Span::styled("         you  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(summary, Style::default().fg(rgb(theme.accent))),
                ]));
            }
            LogEntry::Assistant(text) => {
                let clean = strip_ansi(text);
                for raw_line in clean.lines().take(5) {
                    let trimmed: String = raw_line.chars().take(width.saturating_sub(14)).collect();
                    lines.push(Line::from(vec![
                        Span::raw("              "),
                        Span::styled(trimmed, Style::default().fg(Color::White)),
                    ]));
                }
            }
            LogEntry::ToolCall { name, detail, full_result, done, .. } => {
                let op = if name.contains("read") || name.contains("Read") {
                    "read"
                } else if name.contains("edit") || name.contains("Edit") || name.contains("write") || name.contains("Write") {
                    "edit"
                } else if name.contains("bash") || name.contains("Bash") || name.contains("exec") {
                    "exec"
                } else {
                    name.as_str()
                };

                let args_str: String = detail
                    .chars()
                    .take(width.saturating_sub(op.len() + 22))
                    .collect();

                let result_indicator = if *done { "✓" } else { "…" };
                let result_color = if full_result.is_some() { Color::Green } else { Color::DarkGray };
                lines.push(Line::from(vec![
                    Span::styled("  •  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{op:<6}"),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {args_str}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("  {result_indicator}"),
                        Style::default().fg(result_color),
                    ),
                ]));
            }
            LogEntry::System(text) => {
                lines.push(Line::from(Span::styled(
                    format!("  ◆  {text}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            // Inline images are not renderable in text mode; skip.
            LogEntry::Image { .. } => {}
        }
    }

    // Streaming pending text (current assistant turn).
    if !snap.pending.is_empty() {
        let clean = strip_ansi(&snap.pending);
        for raw_line in clean.lines().take(3) {
            let trimmed: String = raw_line.chars().take(width.saturating_sub(14)).collect();
            lines.push(Line::from(vec![
                Span::raw("              "),
                Span::styled(
                    trimmed,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
    }

    // Scroll to bottom: show the last N lines that fit.
    let visible_rows = area.height as usize;
    let start = lines.len().saturating_sub(visible_rows);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(start).collect();

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(visible))
            .style(Style::default().bg(rgb(theme.bg_primary)))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

// ─── Ctrl-K palette overlay ───────────────────────────────────────────────────

/// Render the command palette.
///
/// Sources (in order): slash commands, thread names, recent prompts from
/// `Tab.history` (via `queued_preview` for now — full `file_cache` fuzzy index
/// deferred to step 4 since it requires accessing the runtime file cache not yet
/// part of `LayoutSnapshot`).
fn render_palette(
    frame: &mut Frame,
    size: Rect,
    snap: &LayoutSnapshot,
    query: &str,
    selected: usize,
) {
    let theme = &snap.theme;

    // Build items.
    let mut items: Vec<(String, String)> = Vec::new();

    // Slash commands that fuzzy-match the query.
    let slash_cmds: &[(&str, &str)] = &[
        ("/help", "show help"),
        ("/clear", "clear conversation"),
        ("/model", "switch model"),
        ("/layout", "switch TUI layout"),
        ("/status", "session status"),
        ("/config", "configuration"),
        ("/cost", "token cost summary"),
        ("/exit", "exit Anvil"),
    ];
    for (cmd, hint) in slash_cmds {
        if query.is_empty() || cmd.contains(query) || hint.contains(query) {
            items.push((cmd.to_string(), hint.to_string()));
        }
    }

    // Thread names.
    for (_id, name, is_active, _unread, _perm) in &snap.tab_infos {
        let label = format!("Switch thread: {name}");
        if query.is_empty() || name.contains(&query[..]) {
            let hint = if *is_active { "active" } else { "background" };
            items.push((label, hint.to_string()));
        }
    }

    // Recent prompts from queued_preview (step 4 will add full history).
    for preview in &snap.queued_preview {
        if query.is_empty() || preview.contains(query) {
            items.push((preview.clone(), "recent prompt".to_string()));
        }
    }

    let max_visible = 10usize;
    let visible_count = items.len().min(max_visible);
    let palette_h = (visible_count as u16 + 4).min(size.height.saturating_sub(4));
    let palette_w = (size.width / 2).min(70).max(30);
    let palette_x = (size.width.saturating_sub(palette_w)) / 2;
    let palette_y = size.height / 4;

    let palette_area = Rect {
        x: palette_x,
        y: palette_y,
        width: palette_w,
        height: palette_h,
    };

    frame.render_widget(ratatui::widgets::Clear, palette_area);

    // Query line.
    let query_line = Line::from(vec![
        Span::styled(" ❯ ", Style::default().fg(rgb(theme.accent))),
        Span::raw(query.to_string()),
        Span::styled("█", Style::default().fg(Color::White)),
    ]);

    let hint_line = Line::from(Span::styled(
        " ↑↓ navigate  Enter execute  Esc close",
        Style::default().fg(Color::DarkGray),
    ));

    let inner_w = palette_w.saturating_sub(2) as usize;
    let mut palette_lines: Vec<Line<'static>> = vec![query_line];

    for (i, (cmd, hint)) in items.iter().take(max_visible).enumerate() {
        let is_sel = i == selected;
        let (fg, bg) = if is_sel {
            (rgb(theme.bg_primary), rgb(theme.accent))
        } else {
            (rgb(theme.text_primary), rgb(theme.bg_card))
        };
        let cmd_w = (inner_w / 2).min(cmd.len() + 4);
        let padded = format!(" {cmd:<width$}", width = cmd_w);
        let hint_w = inner_w.saturating_sub(cmd_w + 1);
        let padded_hint: String = format!("{hint}").chars().take(hint_w).collect();
        palette_lines.push(Line::from(vec![
            Span::styled(padded, Style::default().fg(fg).bg(bg)),
            Span::styled(
                format!("  {padded_hint}"),
                Style::default().fg(if is_sel { rgb(theme.bg_primary) } else { rgb(theme.text_secondary) }).bg(bg)
                    .add_modifier(Modifier::DIM),
            ),
        ]));
    }
    palette_lines.push(hint_line);

    frame.render_widget(
        Paragraph::new(Text::from(palette_lines))
            .block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(rgb(theme.border)))
                    .style(Style::default().bg(rgb(theme.bg_card))),
            ),
        palette_area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::tui::layouts::{LayoutLocalState, TuiLayoutRenderer};
    use crate::tui::redraw::DirtyRegions;
    use crate::tui::snapshot::LayoutSnapshot;

    /// Build a snapshot tailored for the journal layout. Populates the model,
    /// git branch, and tab list so the header has real content to render, and
    /// pre-seeds the input buffer so the input row carries non-default text.
    fn journal_snap() -> LayoutSnapshot {
        let mut snap = LayoutSnapshot::test_default();
        snap.model = "claude-sonnet-4-6".to_string();
        snap.git_branch = "v2.2.14-phase1".to_string();
        snap.tab_infos = vec![(1, "main".to_string(), true, false, false)];
        snap.input_text = "hello world".to_string();
        snap.cursor_pos = 11;
        snap
    }

    fn render_real_journal(
        snap: &LayoutSnapshot,
        width: u16,
        height: u16,
        tabs: bool,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend");
        let mut local = LayoutLocalState::Journal {
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
        };
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                super::Renderer { tabs }.render(frame, snap, &mut local, &mut hits);
            })
            .expect("draw");
        let buf = terminal.backend().buffer();
        let (bw, bh) = (buf.area.width as usize, buf.area.height as usize);
        let mut rows = Vec::with_capacity(bh);
        for row in 0..bh {
            let mut line = String::with_capacity(bw);
            for col in 0..bw {
                let ch = buf[(col as u16, row as u16)].symbol();
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

    /// Task #648 (v2.2.17 release-blocker fix): every band paints every
    /// frame. The previous task-#574 "skip if not dirty" behavior left
    /// blank cells in ratatui's back-buffer; the frame diff against the
    /// previous-frame buffer then emitted writes that ERASED on-terminal
    /// content for the skipped regions. New contract — paint everything,
    /// rely on ratatui's cell diff for efficiency.
    #[test]
    fn journal_paints_every_band_on_scrollback_only_dirty() {
        let mut snap = journal_snap();
        snap.dirty_regions = DirtyRegions::SCROLLBACK;
        snap.pending = "Streaming token".to_string();
        let rendered = render_real_journal(&snap, 100, 20, false);
        assert!(
            rendered.contains("claude-sonnet-4-6"),
            "header model MUST render every frame (task #648); got:\n{rendered}"
        );
        // journal_snap has input_text="hello world" so the input row
        // renders the typed text, not the placeholder. Either form
        // proves the input row is painted.
        assert!(
            rendered.contains("hello world"),
            "input row MUST render every frame (task #648); got:\n{rendered}"
        );
        assert!(
            rendered.contains("Streaming token"),
            "streaming pending text must still render; got:\n{rendered}"
        );
    }

    /// Task #648: HEADER-only dirty still paints input row.
    #[test]
    fn journal_paints_every_band_on_header_dirty() {
        let mut snap = journal_snap();
        snap.dirty_regions = DirtyRegions::HEADER;
        let rendered = render_real_journal(&snap, 100, 20, false);
        assert!(
            rendered.contains("claude-sonnet-4-6"),
            "header model must render; got:\n{rendered}"
        );
        assert!(
            rendered.contains("hello world"),
            "input row text must ALSO render (task #648); got:\n{rendered}"
        );
    }

    /// Task #574: first-frame `DirtyRegions::ALL` paints every band.
    #[test]
    fn journal_first_frame_renders_everything_on_dirty_all() {
        let mut snap = journal_snap();
        snap.dirty_regions = DirtyRegions::ALL;
        let rendered = render_real_journal(&snap, 100, 20, false);
        assert!(rendered.contains("claude-sonnet-4-6"));
        assert!(rendered.contains("v2.2.14-phase1"));
        assert!(rendered.contains("hello world"));
    }

    /// Task #648 streaming contract: a streaming `pending` text frame
    /// (SCROLLBACK-only dirty) paints the new tokens AND keeps the
    /// header + input bands visible (no terminal-side erasure).
    #[test]
    fn journal_streaming_pending_keeps_every_band_visible() {
        let mut snap = journal_snap();
        snap.dirty_regions = DirtyRegions::SCROLLBACK;
        snap.pending = "Streaming response token".to_string();
        let rendered = render_real_journal(&snap, 100, 20, false);
        assert!(
            rendered.contains("Streaming response token"),
            "streaming pending must render; got:\n{rendered}"
        );
        assert!(
            rendered.contains("claude-sonnet-4-6"),
            "header must STAY visible on streaming SCROLLBACK frame (task #648); got:\n{rendered}"
        );
    }

    /// Task #574 cross-task contract: a Confirm modal overlay renders
    /// cleanly above the layout regardless of layout dirty bits.
    #[test]
    fn confirm_modal_renders_cleanly_under_header_only_dirty() {
        use crate::tui::modals::ConfirmModal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        use ratatui::Terminal;

        let mut snap = journal_snap();
        snap.dirty_regions = DirtyRegions::HEADER;
        let modal = ConfirmModal::new("Restart Anvil?", "This will respawn the current binary in-place.");

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("TestBackend");
        let mut local = LayoutLocalState::Journal {
            palette_open: false,
            palette_query: String::new(),
            palette_selected: 0,
        };
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                super::Renderer { tabs: false }.render(frame, &snap, &mut local, &mut hits);
                let size = frame.area();
                modal.render(frame, size, Color::Cyan);
            })
            .expect("draw");
        let buf = terminal.backend().buffer();
        let (bw, bh) = (buf.area.width as usize, buf.area.height as usize);
        let mut rows = Vec::with_capacity(bh);
        for row in 0..bh {
            let mut line = String::with_capacity(bw);
            for col in 0..bw {
                let ch = buf[(col as u16, row as u16)].symbol();
                if ch.is_empty() || ch == "\x00" {
                    line.push(' ');
                } else {
                    line.push_str(ch);
                }
            }
            rows.push(line.trim_end().to_string());
        }
        let rendered = rows.join("\n");
        assert!(
            rendered.contains("Restart Anvil?"),
            "confirm modal title must render over HEADER-only dirty layout; got:\n{rendered}"
        );
        assert!(
            rendered.contains("respawn the current binary"),
            "confirm modal body must render over HEADER-only dirty layout; got:\n{rendered}"
        );
    }

    // ── Task #573: golden baseline snapshots ──────────────────────────────
    //
    // Step 1 of the TUI Layout work spec: each layout (classic,
    // vertical-split, three-pane, journal) gets a deterministic golden
    // snapshot at known sizes. The vertical-split + three-pane snapshots
    // already exist alongside this directory; these add journal C (no tabs)
    // and journal F (tabs) so a stylistic regression to either is caught
    // at `cargo test -p anvil-cli` time.
    //
    // Snapshots use `insta::assert_snapshot!` to keep the workflow uniform
    // with three_pane.rs / vertical_split.rs. Refresh with
    // `cargo insta review` from the workspace root.

    /// Deterministic fixture for golden snapshots: no live timestamps, no
    /// dynamic git branch, no input echo, no streaming pending text. We pin
    /// every field that varies between runs.
    fn journal_golden_snap() -> LayoutSnapshot {
        let mut snap = LayoutSnapshot::test_default();
        snap.model = "claude-sonnet-4-6".to_string();
        snap.git_branch = "main".to_string();
        snap.tab_infos = vec![
            (1, "anvil-dev".to_string(), true, false, false),
            (2, "aegis-fix".to_string(), false, false, false),
        ];
        snap.input_text = String::new();
        snap.cursor_pos = 0;
        snap.pending = String::new();
        snap.session_id = "session-pinned".to_string();
        // ALL so the snapshot reflects a "clean" structural paint.
        snap.dirty_regions = DirtyRegions::ALL;
        snap
    }

    #[test]
    fn journal_no_tabs__80x24() {
        let snap = journal_golden_snap();
        let rendered = render_real_journal(&snap, 80, 24, false);
        insta::assert_snapshot!("journal_no_tabs__80x24", rendered);
    }

    #[test]
    fn journal_no_tabs__120x40() {
        let snap = journal_golden_snap();
        let rendered = render_real_journal(&snap, 120, 40, false);
        insta::assert_snapshot!("journal_no_tabs__120x40", rendered);
    }

    #[test]
    fn journal_tabs__80x24() {
        let snap = journal_golden_snap();
        let rendered = render_real_journal(&snap, 80, 24, true);
        insta::assert_snapshot!("journal_tabs__80x24", rendered);
    }

    #[test]
    fn journal_tabs__120x40() {
        let snap = journal_golden_snap();
        let rendered = render_real_journal(&snap, 120, 40, true);
        insta::assert_snapshot!("journal_tabs__120x40", rendered);
    }
}
