//! Command history management and navigation.

use super::line_editor::{EditSession, EditorMode};

/// Navigate backward through history (up arrow / k in normal mode).
pub(super) fn history_up(
    history: &[String],
    _vim_enabled: bool,
    session: &mut EditSession,
) {
    if session.mode == EditorMode::Command || history.is_empty() {
        return;
    }

    let next_index = if let Some(index) = session.history_index {
        index.saturating_sub(1)
    } else {
        session.history_backup = Some(session.text.clone());
        history.len() - 1
    };

    session.history_index = Some(next_index);
    session.set_text_from_history(history[next_index].clone());
}

/// Navigate forward through history (down arrow).
pub(super) fn history_down(
    history: &[String],
    vim_enabled: bool,
    session: &mut EditSession,
) where {
    if session.mode == EditorMode::Command {
        return;
    }

    let Some(index) = session.history_index else {
        return;
    };

    if index + 1 < history.len() {
        let next_index = index + 1;
        session.history_index = Some(next_index);
        session.set_text_from_history(history[next_index].clone());
        return;
    }

    session.history_index = None;
    let restored = session.history_backup.take().unwrap_or_default();
    session.set_text_from_history(restored);
    if vim_enabled {
        session.enter_insert_mode();
    } else {
        session.mode = EditorMode::Plain;
    }
}
