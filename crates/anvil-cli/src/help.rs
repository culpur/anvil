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
