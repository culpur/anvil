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

    for (idx, (tab_id, tab_name, is_active, has_unread, has_perm)) in
        snap.tab_infos.iter().enumerate()
    {
        let label = if *has_unread && *has_perm {
            format!("[{tab_id}: {tab_name}*⚠]")
        } else if *has_unread {
            format!("[{tab_id}: {tab_name}*]")
        } else if *has_perm {
            format!("[{tab_id}: {tab_name}⚠]")
        } else {
            format!("[{tab_id}: {tab_name}]")
        };
        let label_len = label.chars().count() as u16;
        let label_start = cursor_col;
        let label_end = cursor_col + label_len;

        let style = if *is_active {
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(rgb(theme.text_secondary))
                .add_modifier(Modifier::DIM)
        };
        tab_spans.push(Span::styled(label, style));
        cursor_col = label_end;

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
        "F2/F3 switch  Ctrl+T new  Ctrl+W close  /help nav",
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
