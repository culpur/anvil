//! Completion logic and popup rendering for slash commands.

use super::line_editor::{EditSession, EditorMode};

/// State for the tab-completion cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompletionState {
    pub(super) prefix: String,
    pub(super) matches: Vec<String>,
    pub(super) next_index: usize,
}

/// Advance tab completion for slash commands.
///
/// On first Tab press: find all completions for the current prefix and apply
/// the first one.  On subsequent Tab presses (while the text still matches a
/// completion candidate): cycle through the matches.
pub(super) fn complete_slash_command(
    completions: &[String],
    completion_state: &mut Option<CompletionState>,
    session: &mut EditSession,
) {
    if session.mode == EditorMode::Command {
        *completion_state = None;
        return;
    }

    // If there is an active cycle state and the cursor is still at the end of
    // a known candidate, advance to the next candidate.
    if let Some(state) = completion_state
        .as_mut()
        .filter(|_| session.cursor == session.text.len())
        .filter(|state| {
            state
                .matches
                .iter()
                .any(|candidate| candidate == &session.text)
        })
    {
        let candidate = state.matches[state.next_index % state.matches.len()].clone();
        state.next_index += 1;
        session.text.replace_range(..session.cursor, &candidate);
        session.cursor = candidate.len();
        return;
    }

    let Some(prefix) = slash_command_prefix(&session.text, session.cursor) else {
        *completion_state = None;
        return;
    };

    let matches = completions
        .iter()
        .filter(|candidate| candidate.starts_with(prefix) && candidate.as_str() != prefix)
        .cloned()
        .collect::<Vec<_>>();

    if matches.is_empty() {
        *completion_state = None;
        return;
    }

    let candidate = if let Some(state) = completion_state
        .as_mut()
        .filter(|state| state.prefix == prefix && state.matches == matches)
    {
        let index = state.next_index % state.matches.len();
        state.next_index += 1;
        state.matches[index].clone()
    } else {
        let candidate = matches[0].clone();
        *completion_state = Some(CompletionState {
            prefix: prefix.to_string(),
            matches,
            next_index: 1,
        });
        candidate
    };

    session.text.replace_range(..session.cursor, &candidate);
    session.cursor = candidate.len();
}

/// Return `Some(prefix)` when `line[..pos]` is a slash-command prefix
/// (starts with `/`, contains no whitespace, and `pos == line.len()`).
pub(super) fn slash_command_prefix(line: &str, pos: usize) -> Option<&str> {
    if pos != line.len() {
        return None;
    }

    let prefix = &line[..pos];
    if prefix.contains(char::is_whitespace) || !prefix.starts_with('/') {
        return None;
    }

    Some(prefix)
}
