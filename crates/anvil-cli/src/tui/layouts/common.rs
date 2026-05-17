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
}
