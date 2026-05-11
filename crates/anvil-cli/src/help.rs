//! CLI and REPL help text rendering.

use std::io::{self, Write};

use commands::{render_slash_command_help, resume_supported_slash_commands};

use crate::VERSION;

/// Write the full `anvil --help` output to the given writer.
pub(crate) fn print_help_to(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "Anvil CLI v{VERSION}")?;
    writeln!(
        out,
        "  Interactive coding assistant for the current workspace."
    )?;
    writeln!(out)?;
    writeln!(out, "Quick start")?;
    writeln!(
        out,
        "  anvil                                  Start the interactive REPL"
    )?;
    writeln!(
        out,
        "  anvil \"summarize this repo\"            Run one prompt and exit"
    )?;
    writeln!(
        out,
        "  anvil prompt \"explain src/main.rs\"     Explicit one-shot prompt"
    )?;
    writeln!(
        out,
        "  anvil --resume SESSION.json /status    Inspect a saved session"
    )?;
    writeln!(out)?;
    writeln!(out, "Interactive essentials")?;
    writeln!(
        out,
        "  /help                                 Browse the full slash command map"
    )?;
    writeln!(
        out,
        "  /status                               Inspect session + workspace state"
    )?;
    writeln!(
        out,
        "  /model <name>                         Switch models mid-session"
    )?;
    writeln!(
        out,
        "  /permissions <mode>                   Adjust tool access"
    )?;
    writeln!(
        out,
        "  Tab                                   Complete slash commands"
    )?;
    writeln!(
        out,
        "  /vim                                  Toggle modal editing"
    )?;
    writeln!(
        out,
        "  Shift+Enter / Ctrl+J                  Insert a newline"
    )?;
    writeln!(out)?;
    writeln!(out, "Navigation (TUI)")?;
    writeln!(
        out,
        "  Tabs"
    )?;
    writeln!(
        out,
        "    F2 / F3                             Previous / next tab  (works in every terminal)"
    )?;
    writeln!(
        out,
        "    Ctrl+T  / Ctrl+W                    New tab / close active tab"
    )?;
    writeln!(
        out,
        "    Ctrl+← / Ctrl+→                     Previous / next tab  (most terminals)"
    )?;
    writeln!(
        out,
        "    Ctrl+1 … Ctrl+9                     Jump to tab N (iTerm2 / Ghostty; not Apple Terminal)"
    )?;
    writeln!(
        out,
        "    Alt+1 … Alt+9                       Jump to tab N (works in Apple Terminal too)"
    )?;
    writeln!(
        out,
        "    Click a tab label                   Switch to that tab"
    )?;
    writeln!(
        out,
        "    Click ×  next to a tab              Close that tab"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  Parallel inference"
    )?;
    writeln!(
        out,
        "    Each tab runs its own conversation thread independently.  You can"
    )?;
    writeln!(
        out,
        "    send a prompt in Tab 1, switch to Tab 2, send another, and both"
    )?;
    writeln!(
        out,
        "    turns stream concurrently.  A tab label with * has unread output;"
    )?;
    writeln!(
        out,
        "    ⚠ means that tab is waiting for your permission approval."
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  Permission modal"
    )?;
    writeln!(
        out,
        "    When a tool requires elevated permissions, a modal overlay appears"
    )?;
    writeln!(
        out,
        "    for the active tab.  Background tabs queue the request and show ⚠."
    )?;
    writeln!(
        out,
        "    Switch to the tab to see the modal, then answer:"
    )?;
    writeln!(
        out,
        "      y / Enter   Allow once"
    )?;
    writeln!(
        out,
        "      a           Allow always (for this session)"
    )?;
    writeln!(
        out,
        "      n / Esc     Deny"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  Scrolling"
    )?;
    writeln!(
        out,
        "    Up / Down (empty input)             Walk command history / scroll chat"
    )?;
    writeln!(
        out,
        "    PageUp / PageDown                   Page through scrollback"
    )?;
    writeln!(
        out,
        "    Mouse wheel                         Scroll chat (also scrolls dialogs / pickers)"
    )?;
    writeln!(
        out,
        "    End                                 Snap back to live bottom"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  In an /ssh tab"
    )?;
    writeln!(
        out,
        "    Ctrl+B  then key                    SSH-mode escape prefix (like tmux): a digit jumps tabs, q closes the SSH tab"
    )?;
    writeln!(
        out,
        "    All other keys forwarded to the remote shell"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  Other"
    )?;
    writeln!(
        out,
        "    Ctrl+C (empty input)                Press twice within 1s to exit"
    )?;
    writeln!(
        out,
        "    Ctrl+C (with input)                 Clear the input line"
    )?;
    writeln!(
        out,
        "    Ctrl+O                              Toggle focus view (hide side panels)"
    )?;
    writeln!(
        out,
        "    Esc (during a turn)                 Cancel the in-flight assistant response"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  Mouse capture is ON by default. Set ANVIL_TUI_MOUSE=0 to disable"
    )?;
    writeln!(
        out,
        "  (e.g. when your terminal swallows drag-to-select)."
    )?;
    writeln!(out)?;
    writeln!(out, "Commands")?;
    writeln!(
        out,
        "  anvil dump-manifests                   Read upstream TS sources and print extracted counts"
    )?;
    writeln!(
        out,
        "  anvil bootstrap-plan                   Print the bootstrap phase skeleton"
    )?;
    writeln!(
        out,
        "  anvil agents                           List configured agents"
    )?;
    writeln!(
        out,
        "  anvil skills                           List installed skills"
    )?;
    writeln!(
        out,
        "  anvil model [name]                     Start REPL with a specific model"
    )?;
    writeln!(out, "  anvil system-prompt [--cwd PATH] [--date YYYY-MM-DD]")?;
    writeln!(
        out,
        "  anvil login [provider]                 Login to a provider (anthropic, openai, ollama) — interactive if omitted"
    )?;
    writeln!(
        out,
        "  anvil logout                           Clear saved OAuth credentials"
    )?;
    writeln!(
        out,
        "  anvil init                             Scaffold ANVIL.md + local files"
    )?;
    writeln!(
        out,
        "  anvil setup                            Run the interactive first-run setup wizard"
    )?;
    writeln!(out)?;
    writeln!(out, "Flags")?;
    writeln!(
        out,
        "  --model MODEL                         Override the active model"
    )?;
    writeln!(
        out,
        "  --output-format FORMAT                Non-interactive output: text or json"
    )?;
    writeln!(
        out,
        "  --permission-mode MODE                Set read-only, workspace-write, or danger-full-access"
    )?;
    writeln!(
        out,
        "  --dangerously-skip-permissions        Skip all permission checks"
    )?;
    writeln!(
        out,
        "  --allowedTools TOOLS                  Restrict enabled tools (repeatable; comma-separated aliases supported)"
    )?;
    writeln!(
        out,
        "  --version, -V                         Print version and build information"
    )?;
    writeln!(
        out,
        "  --update                              Self-update to the latest release"
    )?;
    writeln!(
        out,
        "  --first-run, --setup                  Run the interactive first-run setup wizard"
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "For the full reference (env vars, file locations, vault, hooks, MCP,"
    )?;
    writeln!(
        out,
        "skills, agents, examples), run `man anvil` after a Homebrew install,"
    )?;
    writeln!(
        out,
        "or read man/anvil.1 in the source tree."
    )?;
    writeln!(out)?;
    writeln!(out, "Slash command reference")?;
    writeln!(out, "{}", render_slash_command_help())?;
    writeln!(out)?;
    let resume_commands = resume_supported_slash_commands()
        .into_iter()
        .map(|spec| match spec.argument_hint {
            Some(argument_hint) => format!("/{} {}", spec.name, argument_hint),
            None => format!("/{}", spec.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "Resume-safe commands: {resume_commands}")?;
    writeln!(out, "Examples")?;
    writeln!(out, "  anvil --model opus \"summarize this repo\"")?;
    writeln!(
        out,
        "  anvil --output-format json prompt \"explain src/main.rs\""
    )?;
    writeln!(
        out,
        "  anvil --allowedTools read,glob \"summarize Cargo.toml\""
    )?;
    writeln!(
        out,
        "  anvil --resume session.json /status /diff /export notes.txt"
    )?;
    writeln!(out, "  anvil agents")?;
    writeln!(out, "  anvil /skills")?;
    writeln!(
        out,
        "  anvil login                              # Interactive provider setup"
    )?;
    writeln!(
        out,
        "  anvil login openai                       # Setup OpenAI API key"
    )?;
    writeln!(
        out,
        "  anvil login ollama                       # Configure Ollama endpoint"
    )?;
    writeln!(
        out,
        "  anvil model llama3.2                     # Start with Ollama model"
    )?;
    writeln!(
        out,
        "  anvil model gpt-4o                       # Start with OpenAI model"
    )?;
    writeln!(out, "  anvil init")?;
    Ok(())
}

/// Print `--help` output to stdout.
pub(crate) fn print_help() {
    let _ = print_help_to(&mut io::stdout());
}
