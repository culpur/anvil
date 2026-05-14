/// Phase 5.0.5 — unified slash-command dispatch scaffold.
///
/// # Design
///
/// `dispatch_slash_command` is the single canonical entry point for ALL
/// slash-command execution.  Every dispatch site — headless CLI arg parse,
/// `--resume` continuation, interactive TUI REPL, and batch (print) mode —
/// must call this function instead of forking its own `match SlashCommand { …
/// }` block.
///
/// The caller discriminates execution context via `DispatchContext`, which
/// carries the minimum state needed by each mode.  Handlers that need more
/// context (e.g. `&mut AnvilTui` for TUI-only commands) access it through the
/// appropriate context variant.
///
/// ## Why not in the commands crate today
///
/// The full handler implementations live in `crates/anvil-cli/src/main.rs`
/// because they call `LiveCli` methods directly.  Migrating them here without
/// introducing a circular dependency requires either:
///
/// 1. Defining a `CommandDispatcher` trait in this crate and implementing it
///    on `LiveCli` in `anvil-cli`, OR
/// 2. Moving the handler implementations themselves into this crate with a
///    thin abstraction over the session/runtime types.
///
/// Phase 5.0.5 lays the groundwork: it defines `DispatchContext` and
/// `dispatch_slash_command` as the public contract.  The concrete routing
/// still lives in `anvil-cli` for now, but every call site in `main.rs` is
/// expected to converge onto `dispatch_slash_command` in Phase 5.1.
///
/// ## Phase 5.0.5 proof-of-work
///
/// `/help`, `/compact`, `/memory`, `/skill`, `/cmd-cache`, `/file-cache`,
/// `/scroll-speed`, `/output-style`, `/effort`, and `/profile` are handled
/// entirely within this crate (via `handle_slash_command`) and therefore
/// satisfy the dispatch-axis contract without any `LiveCli` dependency.
///
/// The remaining variants are routed through the `RequiresLiveCli` outcome,
/// which is a signal to the caller that it must invoke its own per-variant
/// handler.  In Phase 5.1 each of those will be extracted into a trait method
/// and the `RequiresLiveCli` arm will disappear.

use runtime::{CompactionConfig, Session};

use crate::handlers::{handle_slash_command, SlashCommandResult};
use crate::SlashCommand;

/// Execution context passed to `dispatch_slash_command`.
///
/// Each variant carries the minimum state required by its mode.  Variants
/// without a live session (Headless, Resume) use the `Session` snapshot that
/// was loaded from disk; Repl and Batch variants borrow the session from the
/// live runtime.
///
/// The TUI handle (`AnvilTui`) and `LiveCli` references are intentionally
/// absent here — they live in `anvil-cli` and would create a circular
/// dependency.  When a command requires them the dispatch returns
/// `DispatchOutcome::RequiresLiveCli` so the caller can invoke its own
/// handler.
#[derive(Debug)]
pub enum DispatchContext<'a> {
    /// Top-level CLI arg parse for headless invocations
    /// (`anvil /memory show working`).  No live session.
    Headless {
        /// Parsed session or freshly-constructed `Session::new()`.
        session: &'a Session,
        /// Compaction configuration from settings.
        compaction: CompactionConfig,
    },

    /// `anvil --resume SESSION.json /command` continuation path.
    /// Has a saved session but no live runtime.
    Resume {
        /// Session loaded from the saved JSON file.
        session: &'a Session,
        compaction: CompactionConfig,
    },

    /// Interactive TUI REPL (`handle_repl_command_tui`).
    /// Requires `LiveCli` — commands routed here will return
    /// `DispatchOutcome::RequiresLiveCli` if `handle_slash_command` cannot
    /// satisfy them without a live runtime.
    Repl {
        session: &'a Session,
        compaction: CompactionConfig,
    },

    /// Non-TUI batch / print mode (`handle_repl_command`).
    Batch {
        session: &'a Session,
        compaction: CompactionConfig,
    },
}

impl<'a> DispatchContext<'a> {
    /// Borrow the session from any context variant.
    #[must_use]
    pub fn session(&self) -> &Session {
        match self {
            Self::Headless { session, .. }
            | Self::Resume { session, .. }
            | Self::Repl { session, .. }
            | Self::Batch { session, .. } => session,
        }
    }

    /// Borrow the compaction configuration from any context variant.
    #[must_use]
    pub fn compaction(&self) -> &CompactionConfig {
        match self {
            Self::Headless { compaction, .. }
            | Self::Resume { compaction, .. }
            | Self::Repl { compaction, .. }
            | Self::Batch { compaction, .. } => compaction,
        }
    }
}

/// Outcome of a `dispatch_slash_command` call.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// Command handled fully within the commands crate.
    /// The caller should display `result.message` and, if `result.session`
    /// differs from the input, persist the updated session.
    Handled(SlashCommandResult),

    /// Command requires a live `LiveCli` (and possibly `AnvilTui`) to execute.
    /// The caller must invoke its own per-variant handler for `cmd`.
    /// This outcome will be eliminated in Phase 5.1 as handlers are extracted
    /// from `anvil-cli` into this crate.
    RequiresLiveCli(SlashCommand),
}

/// Error type for dispatch failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The input string did not parse as a slash command.
    ParseError(String),
    /// The command is known but is not supported in the given context.
    ContextUnsupported {
        command: String,
        reason: String,
    },
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseError(msg) => write!(f, "parse error: {msg}"),
            Self::ContextUnsupported { command, reason } => {
                write!(f, "/{command}: not supported in this context: {reason}")
            }
        }
    }
}

impl std::error::Error for DispatchError {}

/// Single canonical dispatch entry point for all slash commands.
///
/// ## Contract
///
/// - If the command can be fully handled within the `commands` crate (e.g.
///   `/help`, `/compact`, `/memory`, `/skill`, `/scroll-speed`), returns
///   `Ok(DispatchOutcome::Handled(_))`.
///
/// - If the command requires a live CLI session (anything that calls
///   `LiveCli::run_*` methods in `anvil-cli`), returns
///   `Ok(DispatchOutcome::RequiresLiveCli(cmd))`.  The caller is responsible
///   for handling it.  In Phase 5.1 this set shrinks to zero.
///
/// - If the input is not a recognisable slash command at all, returns
///   `Err(DispatchError::ParseError)`.
///
/// ## Phase 5.0.5 note
///
/// This function currently routes every command through `handle_slash_command`
/// (the existing fallback registry).  Commands for which `handle_slash_command`
/// returns `Some` are returned as `Handled`; all others are returned as
/// `RequiresLiveCli`.  This means that in Phase 5.0.5 the "all five sites
/// become single-line calls to dispatch_slash_command" target is achieved for
/// the commands-crate-resident handlers, and the remaining sites converge in
/// Phase 5.1.
#[must_use]
pub fn dispatch_slash_command(
    input: &str,
    ctx: &DispatchContext<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    let cmd = SlashCommand::parse(input)
        .ok_or_else(|| DispatchError::ParseError(format!("not a slash command: {input}")))?;

    let session = ctx.session();
    let compaction = ctx.compaction().clone();

    // Fast path: handle_slash_command covers all commands that can run
    // without a live CLI (help, compact, memory, skill, cmd-cache, file-cache,
    // scroll-speed, output-style, effort, profile, ollama, plus all the
    // "requires active session" informational responses for TUI-only commands).
    //
    // Specifically: every arm in handle_slash_command now returns Some(_) for
    // ALL slash command variants (no variant is unmatched), so this always
    // returns Handled.  The RequiresLiveCli path below is reserved for the
    // Phase 5.1 migration when individual handler arms are extracted from
    // handle_slash_command into dedicated modules with trait-based LiveCli
    // access.
    if let Some(result) = handle_slash_command(input, session, compaction) {
        return Ok(DispatchOutcome::Handled(result));
    }

    // Fallback: command not handled by the crate-resident dispatcher.
    // Return the command back to the caller for LiveCli dispatch.
    Ok(DispatchOutcome::RequiresLiveCli(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that dispatch_slash_command returns Handled for all spec-registered
    /// commands when invoked with a Headless context.  This is the Phase 5.0.5
    /// proof-of-work: every command has a resolved handler callable from all
    /// entry points.
    #[test]
    fn dispatch_handles_all_spec_commands_headlessly() {
        use crate::specs::slash_command_specs;

        let session = runtime::Session::default();
        let compaction = runtime::CompactionConfig::default();
        let ctx = DispatchContext::Headless {
            session: &session,
            compaction: compaction.clone(),
        };

        let mut unhandled: Vec<String> = Vec::new();

        for spec in slash_command_specs() {
            let input = format!("/{}", spec.name);
            match dispatch_slash_command(&input, &ctx) {
                Ok(DispatchOutcome::Handled(result)) => {
                    // Verify the result has a non-empty message.
                    if result.message.is_empty() {
                        unhandled.push(format!(
                            "{}: dispatch returned Handled but message is empty",
                            spec.name
                        ));
                    }
                }
                Ok(DispatchOutcome::RequiresLiveCli(_)) => {
                    // A command that falls through to RequiresLiveCli means
                    // handle_slash_command returned None — this should not
                    // happen post-5.0.5.
                    unhandled.push(format!(
                        "{}: dispatch returned RequiresLiveCli (handle_slash_command returned None)",
                        spec.name
                    ));
                }
                Err(e) => {
                    unhandled.push(format!("{}: dispatch error: {e}", spec.name));
                }
            }
        }

        assert!(
            unhandled.is_empty(),
            "Phase 5.0.5 dispatch-axis failures:\n  {}\n\n\
             Every spec-registered command must return DispatchOutcome::Handled \
             when called headlessly.  Add a match arm to handle_slash_command for \
             each failing command.",
            unhandled.join("\n  ")
        );
    }

    /// Verify that Headless context exposes session and compaction correctly.
    #[test]
    fn dispatch_context_accessors_work() {
        let session = runtime::Session::default();
        let compaction = runtime::CompactionConfig::default();

        let ctx = DispatchContext::Headless {
            session: &session,
            compaction: compaction.clone(),
        };
        assert_eq!(ctx.session().messages.len(), session.messages.len());

        let ctx2 = DispatchContext::Batch {
            session: &session,
            compaction: compaction.clone(),
        };
        assert_eq!(ctx2.session().messages.len(), session.messages.len());
    }

    /// Verify that /help dispatches as Handled from all four context variants.
    #[test]
    fn help_dispatches_as_handled_from_all_contexts() {
        let session = runtime::Session::default();
        let compaction = runtime::CompactionConfig::default();

        let contexts: Vec<DispatchContext<'_>> = vec![
            DispatchContext::Headless { session: &session, compaction: compaction.clone() },
            DispatchContext::Resume { session: &session, compaction: compaction.clone() },
            DispatchContext::Repl { session: &session, compaction: compaction.clone() },
            DispatchContext::Batch { session: &session, compaction: compaction.clone() },
        ];

        for ctx in &contexts {
            match dispatch_slash_command("/help", ctx) {
                Ok(DispatchOutcome::Handled(result)) => {
                    assert!(
                        !result.message.is_empty(),
                        "/help must return a non-empty message"
                    );
                }
                other => panic!("/help from {:?} context returned unexpected outcome: {other:?}",
                    std::mem::discriminant(ctx)),
            }
        }
    }
}
