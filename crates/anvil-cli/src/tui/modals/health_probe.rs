//! `HealthProbeModal` — multi-issue repair checklist (task #666, v2.2.18).
//!
//! Drives A5's HealingModal. Renders a checklist of detected issues
//! with status icons (✓ ✗ ⚠) and lets the user toggle which ones to
//! repair via spacebar. Resolution returns
//! `ModalAnswer::HealthCheck { repair, quit }`.
//!
//! ## Sample render
//!
//! ```text
//! ┌─ Anvil setup needs attention ─────────────────────────────┐
//! │                                                            │
//! │  We checked your install and found:                       │
//! │                                                            │
//! │    [✓] Vault OK                                           │
//! │    [✓] Anthropic auth OK                                  │
//! │    [✗] Ollama daemon not running              [x] repair  │
//! │    [⚠] QMD index hasn't refreshed in 8 days   [x] repair  │
//! │    [⚠] Bash completions missing                [ ] repair  │
//! │                                                            │
//! │  [r] Repair selected   [a] Repair all   [c] Continue   [q] │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Keys
//!
//! - Up/Down            move highlight
//! - Space              toggle the highlighted row's repair flag
//! - r                  resolve with the current repair set
//! - a                  resolve with EVERY repairable row selected
//! - c / Enter          resolve with an EMPTY repair set (continue)
//! - q / Esc            resolve with `quit = true`
//!
//! OK rows (`HealthStatus::Ok`) are not toggleable — the spacebar
//! ignores them and `a` does not select them either.
//!
//! ## 8-axis capability contract (per `feedback-anvil-capability-contract.md`)
//!
//! 1. Definition       — `HealthProbeModal` + `HealthIssue` +
//!                       `HealthStatus` + `HealthProbeAction`.
//! 2. Registration     — `pub mod health_probe` in `tui/modals/mod.rs`.
//! 3. Completion       — N/A.
//! 4. Handler          — `handle_key` returns `HealthProbeAction`.
//! 5. Dispatch         — `WizardModalRunner::run_health_probe`.
//! 6. Rendering        — `render` paints a centered overlay.
//! 7. Gate             — only OK / non-OK rows differ on toggle; no
//!                       hidden state.
//! 8. OTel + tests     — unit tests below cover construction,
//!                       toggling, repair-all, continue, quit.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Status of one detected issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum HealthStatus {
    /// Healthy — renders with ✓.
    Ok,
    /// Hard failure — renders with ✗.
    Fail,
    /// Soft warning — renders with ⚠.
    Warn,
}

impl HealthStatus {
    /// Icon glyph for the row.
    pub(crate) fn icon(self) -> char {
        match self {
            Self::Ok => '✓',
            Self::Fail => '✗',
            Self::Warn => '⚠',
        }
    }

    /// Color used for the icon.
    pub(crate) fn color(self) -> Color {
        match self {
            Self::Ok => Color::Green,
            Self::Fail => Color::Red,
            Self::Warn => Color::Yellow,
        }
    }
}

/// One detected issue row.
#[derive(Debug, Clone)]
pub(crate) struct HealthIssue {
    pub(crate) status: HealthStatus,
    pub(crate) label: String,
    /// Whether this row is currently flagged for repair. Only
    /// meaningful when `status != Ok`. The default-on initial value
    /// for `Fail` is set by the caller via `HealthIssue::new_repair`.
    pub(crate) repair: bool,
}

impl HealthIssue {
    /// New row that is not flagged for repair by default.
    #[allow(dead_code)]
    pub(crate) fn new(status: HealthStatus, label: impl Into<String>) -> Self {
        Self {
            status,
            label: label.into(),
            repair: false,
        }
    }

    /// New row that IS flagged for repair by default. The contract
    /// in the design doc shows Fail and Warn rows pre-selected; the
    /// caller decides on a per-issue basis.
    #[allow(dead_code)]
    pub(crate) fn new_repair(status: HealthStatus, label: impl Into<String>) -> Self {
        Self {
            status,
            label: label.into(),
            repair: !matches!(status, HealthStatus::Ok),
        }
    }

    /// Is this row eligible for repair toggle? (`Ok` rows are not.)
    pub(crate) fn is_repairable(&self) -> bool {
        !matches!(self.status, HealthStatus::Ok)
    }
}

/// Outcome of `HealthProbeModal::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum HealthProbeAction {
    /// Stay open, redraw.
    Continue,
    /// User pressed `r` (or Enter when at least one is selected) —
    /// resolve with the current repair set.
    Repair(Vec<usize>),
    /// User pressed `c` (or Enter when nothing is selected) — resolve
    /// with an empty repair set.
    Continue_,
    /// User pressed `q` / Esc — resolve with `quit = true`.
    Quit,
}

/// The checklist modal.
#[derive(Debug, Clone)]
pub(crate) struct HealthProbeModal {
    pub(crate) title: String,
    pub(crate) preamble: String,
    pub(crate) issues: Vec<HealthIssue>,
    pub(crate) cursor: usize,
}

impl HealthProbeModal {
    /// Construct a new modal. `issues` may be empty (will render
    /// only the preamble and offer Continue / Quit).
    #[allow(dead_code)]
    pub(crate) fn new(title: impl Into<String>, preamble: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            preamble: preamble.into(),
            issues: Vec::new(),
            cursor: 0,
        }
    }

    /// Builder: replace the issues list.
    #[allow(dead_code)]
    pub(crate) fn with_issues(mut self, issues: Vec<HealthIssue>) -> Self {
        self.issues = issues;
        // Land the initial cursor on the first repairable row so the
        // user is immediately at a useful cell.
        self.cursor = self
            .issues
            .iter()
            .position(HealthIssue::is_repairable)
            .unwrap_or(0);
        self
    }

    /// Indices of issues currently flagged for repair.
    pub(crate) fn selected_repair(&self) -> Vec<usize> {
        self.issues
            .iter()
            .enumerate()
            .filter(|(_, i)| i.repair && i.is_repairable())
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Mark every repairable row for repair.
    pub(crate) fn select_all_repairable(&mut self) {
        for issue in self.issues.iter_mut() {
            if issue.is_repairable() {
                issue.repair = true;
            }
        }
    }

    /// Process one keystroke.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> HealthProbeAction {
        let n = self.issues.len();
        match key.code {
            KeyCode::Up => {
                if n > 0 && self.cursor > 0 {
                    self.cursor -= 1;
                }
                HealthProbeAction::Continue
            }
            KeyCode::Down => {
                if n > 0 && self.cursor + 1 < n {
                    self.cursor += 1;
                }
                HealthProbeAction::Continue
            }
            KeyCode::Char(' ') => {
                if let Some(issue) = self.issues.get_mut(self.cursor) {
                    if issue.is_repairable() {
                        issue.repair = !issue.repair;
                    }
                }
                HealthProbeAction::Continue
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                HealthProbeAction::Repair(self.selected_repair())
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.select_all_repairable();
                HealthProbeAction::Repair(self.selected_repair())
            }
            KeyCode::Char('c') | KeyCode::Char('C') => HealthProbeAction::Continue_,
            KeyCode::Enter => {
                let sel = self.selected_repair();
                if sel.is_empty() {
                    HealthProbeAction::Continue_
                } else {
                    HealthProbeAction::Repair(sel)
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                HealthProbeAction::Quit
            }
            _ => HealthProbeAction::Continue,
        }
    }

    /// Render the overlay.
    pub(crate) fn render(&self, frame: &mut Frame, area: Rect, accent: Color) {
        let modal_w = area.width.saturating_sub(6).min(80).max(40);
        let rows = self.issues.len() as u16;
        let needed_h = rows + 9;
        let modal_h = needed_h.min(area.height.saturating_sub(2)).max(9);
        if area.width < 12 || area.height < 9 {
            return;
        }
        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_w,
            height: modal_h,
        };

        frame.render_widget(Clear, modal_area);

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.issues.len() + 6);
        lines.push(Line::from(""));
        if !self.preamble.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", self.preamble),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(""));
        }

        // Label-column width (right-aligned column for [x] repair).
        let label_col_w = inner.width.saturating_sub(18) as usize;

        for (idx, issue) in self.issues.iter().enumerate() {
            let highlight = idx == self.cursor;
            let label_style = if highlight {
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let label_visible: String = issue.label.chars().take(label_col_w).collect();
            let icon = issue.status.icon();
            let icon_color = issue.status.color();

            let mut spans: Vec<Span<'static>> = vec![
                Span::raw("    ["),
                Span::styled(
                    icon.to_string(),
                    Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("] "),
                Span::styled(
                    format!("{:<width$}", label_visible, width = label_col_w),
                    label_style,
                ),
            ];

            if issue.is_repairable() {
                let mark = if issue.repair { 'x' } else { ' ' };
                spans.push(Span::styled(
                    format!("  [{mark}] repair"),
                    Style::default().fg(super::modal_secondary_color()),
                ));
            } else {
                spans.push(Span::raw("          "));
            }

            lines.push(Line::from(spans));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  [r] Repair selected   [a] Repair all   [c] Continue   [q] Quit"
                .to_string(),
            Style::default().fg(super::modal_secondary_color()),
        )));

        frame.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_modal() -> HealthProbeModal {
        HealthProbeModal::new("Anvil setup needs attention", "We found:")
            .with_issues(vec![
                HealthIssue::new(HealthStatus::Ok, "Vault OK"),
                HealthIssue::new(HealthStatus::Ok, "Anthropic auth OK"),
                HealthIssue::new_repair(
                    HealthStatus::Fail,
                    "Ollama daemon not running",
                ),
                HealthIssue::new_repair(
                    HealthStatus::Warn,
                    "QMD index hasn't refreshed in 8 days",
                ),
                HealthIssue::new(HealthStatus::Warn, "Bash completions missing"),
            ])
    }

    #[test]
    fn new_repair_marks_non_ok_issues() {
        let m = sample_modal();
        assert_eq!(m.issues[0].repair, false, "Ok rows not pre-selected");
        assert_eq!(m.issues[2].repair, true, "Fail rows pre-selected");
        assert_eq!(m.issues[3].repair, true, "Warn pre-selected via new_repair");
        assert_eq!(m.issues[4].repair, false, "Warn NOT pre-selected via new");
    }

    #[test]
    fn cursor_starts_on_first_repairable() {
        let m = sample_modal();
        // First two are Ok — cursor should land on the third (the
        // first repairable row).
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn space_toggles_repair_on_highlighted_repairable_row() {
        let mut m = sample_modal();
        // Cursor at row 2 (Fail). Initially repair=true.
        assert_eq!(m.handle_key(key(KeyCode::Char(' '))), HealthProbeAction::Continue);
        assert_eq!(m.issues[2].repair, false, "spacebar toggled off");
        m.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(m.issues[2].repair, true, "spacebar toggled back on");
    }

    #[test]
    fn space_is_a_noop_on_ok_rows() {
        let mut m = sample_modal();
        m.cursor = 0; // an Ok row
        m.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(m.issues[0].repair, false, "Ok rows are not toggleable");
    }

    #[test]
    fn arrows_move_cursor_in_bounds() {
        let mut m = sample_modal();
        let start = m.cursor;
        m.handle_key(key(KeyCode::Down));
        assert_eq!(m.cursor, start + 1);
        m.handle_key(key(KeyCode::Up));
        assert_eq!(m.cursor, start);
        // Walk to the end.
        while m.handle_key(key(KeyCode::Down)) == HealthProbeAction::Continue
            && m.cursor + 1 < m.issues.len()
        {}
        assert_eq!(m.cursor, m.issues.len() - 1);
        // Down at the end stays.
        m.handle_key(key(KeyCode::Down));
        assert_eq!(m.cursor, m.issues.len() - 1);
    }

    #[test]
    fn r_resolves_with_currently_selected() {
        let mut m = sample_modal();
        // Initial selection: indices 2 (Fail) + 3 (Warn-pre-selected).
        let act = m.handle_key(key(KeyCode::Char('r')));
        assert_eq!(act, HealthProbeAction::Repair(vec![2, 3]));
    }

    #[test]
    fn a_selects_every_repairable_row() {
        let mut m = sample_modal();
        // Row 4 (Warn) is NOT pre-selected; pressing `a` must pick it.
        let act = m.handle_key(key(KeyCode::Char('a')));
        assert_eq!(act, HealthProbeAction::Repair(vec![2, 3, 4]));
        assert!(m.issues[4].repair);
    }

    #[test]
    fn c_returns_continue_with_empty_repair_set() {
        let mut m = sample_modal();
        let act = m.handle_key(key(KeyCode::Char('c')));
        assert_eq!(act, HealthProbeAction::Continue_);
    }

    #[test]
    fn q_and_esc_quit() {
        let mut m = sample_modal();
        assert_eq!(m.handle_key(key(KeyCode::Char('q'))), HealthProbeAction::Quit);
        assert_eq!(m.handle_key(key(KeyCode::Esc)), HealthProbeAction::Quit);
    }

    #[test]
    fn enter_falls_back_to_continue_when_nothing_selected() {
        let mut m = HealthProbeModal::new("t", "p").with_issues(vec![
            HealthIssue::new(HealthStatus::Ok, "OK"),
        ]);
        // Nothing repairable.
        let act = m.handle_key(key(KeyCode::Enter));
        assert_eq!(act, HealthProbeAction::Continue_);
    }

    #[test]
    fn render_smoke_test() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let m = sample_modal();
        term.draw(|f| {
            m.render(f, f.area(), Color::Cyan);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let dump: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            dump.contains("Anvil setup needs attention"),
            "title missing from render"
        );
        assert!(
            dump.contains("Vault OK"),
            "issue label missing from render"
        );
        assert!(
            dump.contains("Repair selected"),
            "footer hint missing from render"
        );
    }
}
