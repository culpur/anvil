// Declarative description of the `anvil` CLI surface for manpage generation.
//
// This file is consumed by `build.rs` and used only at build time to feed
// `clap_mangen`.  At runtime the CLI is parsed by the hand-rolled
// `parse_args` in `src/main.rs`; this `clap::Command` is NOT used for
// parsing.  When you add or rename a flag/subcommand, edit BOTH this file
// AND the runtime parser.  A regression test (`crates/anvil-cli/src/tests.rs::
// generated_manpage_matches_committed_copy`) diffs the build-time output
// against `man/anvil.1` and fails the build if they drift.
//
// Why a separate file (not a module under src/): build.rs runs before the
// crate is compiled, so it cannot depend on `src/`.  Keeping the spec here
// as a free-standing snippet lets `build.rs` `include!` it directly.
//
// This file is `include!`d from build.rs, so attributes are outer-only and
// imports are at the top level — do not introduce `mod` or inner `//!`
// doc comments here.

use clap::{Arg, ArgAction, Command as ClapCommand};

/// Build the `clap::Command` describing the anvil CLI surface.  The result is
/// fed to `clap_mangen::Man::new(...)` and rendered into a roff document.
fn build_cli() -> ClapCommand {
    ClapCommand::new("anvil")
        .version(env!("CARGO_PKG_VERSION"))
        .about("AI coding assistant (interactive REPL, one-shot prompts, MCP server).")
        .long_about(
            "anvil is an interactive AI coding assistant. Run it inside a workspace \
             to chat with an LLM that can read, edit, and run code in that workspace; \
             route prompts through providers like Anthropic, OpenAI, xAI, Ollama, and \
             more; switch models mid-session; persist sessions across restarts; SSH to \
             remote hosts from inside the same TUI; manage credentials in an encrypted \
             vault; install plugins (agents, skills, MCP servers); and stream results \
             to a remote web viewer. Anvil is shipped by Culpur Defense — source at \
             https://github.com/culpur/anvil, releases at \
             https://anvilhub.culpur.net/anvil/releases.",
        )
        .author("Culpur Defense <https://culpur.net>")
        // No subcommand structure here — anvil's real subcommands are
        // dispatched by the hand-rolled parser in src/main.rs.  We list them
        // here as positional hints for `--help`/manpage output only.
        .arg(
            Arg::new("prompt")
                .help(
                    "Prompt to run as a one-shot turn, or a known subcommand \
                     (prompt, login, logout, init, setup, check, upgrade, uninstall, \
                     sessions, continue, resume, system-prompt, agents, skills, \
                     skill-eval, model, project, mcp-server, emit-schema).",
                )
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true),
        )
        .arg(
            Arg::new("model")
                .long("model")
                .value_name("MODEL")
                .help(
                    "Override the active model. Accepts provider-prefixed forms like \
                     anthropic:claude-sonnet-4-6, openai:gpt-5, xai:grok-4, \
                     ollama:qwen3-coder, or a bare model name.",
                ),
        )
        .arg(
            Arg::new("output-format")
                .long("output-format")
                .value_name("FORMAT")
                .value_parser(["text", "json"])
                .help(
                    "For non-interactive use. `text` (default) prints plain output; \
                     `json` emits a stream of JSON events suitable for scripts.",
                ),
        )
        .arg(
            Arg::new("permission-mode")
                .long("permission-mode")
                .value_name("MODE")
                .value_parser(["ask", "workspace-write", "danger-full-access"])
                .help(
                    "Set the default permission posture. ask = confirm each tool call \
                     (safest); workspace-write = auto-allow edits inside the workspace; \
                     danger-full-access = no prompts. Toggle at runtime with /permissions.",
                ),
        )
        .arg(
            Arg::new("dangerously-skip-permissions")
                .long("dangerously-skip-permissions")
                .action(ArgAction::SetTrue)
                .help(
                    "Skip every permission check for this run. Equivalent to \
                     --permission-mode=danger-full-access. Use only when you fully \
                     trust both the model and the prompt.",
                ),
        )
        .arg(
            Arg::new("allowedTools")
                .long("allowedTools")
                .visible_alias("allowed-tools")
                .value_name("TOOLS")
                .action(ArgAction::Append)
                .help(
                    "Restrict which built-in tools the assistant may invoke. \
                     Repeatable or comma-separated. Aliases like Read, Edit, Bash \
                     work alongside fully-qualified names.",
                ),
        )
        .arg(
            Arg::new("resume")
                .long("resume")
                .value_name("PATH_OR_SESSION")
                .num_args(0..)
                .help(
                    "Load and continue an existing session. A path opens that JSON \
                     file; a bare token resolves against ~/.anvil/sessions/. With no \
                     argument, resumes the most-recent session.",
                ),
        )
        .arg(
            Arg::new("continue")
                .long("continue")
                .short('c')
                .action(ArgAction::SetTrue)
                .help("Resume the most-recently-saved session."),
        )
        .arg(
            Arg::new("first-run")
                .long("first-run")
                .action(ArgAction::SetTrue)
                .help(
                    "Force the first-run setup wizard, even if ~/.anvil/config.json \
                     already exists.",
                ),
        )
        .arg(
            Arg::new("setup")
                .long("setup")
                .action(ArgAction::SetTrue)
                .help(
                    "Run the post-install setup wizard (same as the `anvil setup` \
                     subcommand). Used by installer scripts.",
                ),
        )
        .arg(
            Arg::new("check")
                .long("check")
                .action(ArgAction::SetTrue)
                .help("Run preflight self-checks (same as `anvil doctor` / `anvil check`)."),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .action(ArgAction::SetTrue)
                .help(
                    "Self-update to the latest GitHub release. Verifies SHA256 before \
                     replacing the binary.",
                ),
        )
        .arg(
            Arg::new("emit-schema")
                .long("emit-schema")
                .action(ArgAction::SetTrue)
                .help(
                    "Print the JSON Schema for ~/.anvil/config.json to stdout, then exit.",
                ),
        )
        .arg(
            Arg::new("uninstall")
                .long("uninstall")
                .action(ArgAction::SetTrue)
                .help("Uninstall the anvil binary and optionally remove ~/.anvil/."),
        )
        .arg(
            Arg::new("profile")
                .long("profile")
                .value_name("NAME")
                .help(
                    "Activate a configuration profile for this session. Lower precedence \
                     than the ANVIL_PROFILE env var when both are set.",
                ),
        )
        .arg(
            Arg::new("plugin-dir")
                .long("plugin-dir")
                .value_name("PATH")
                .action(ArgAction::Append)
                .help(
                    "Load a plugin from a local directory or .zip archive for the \
                     current session only.",
                ),
        )
        .arg(
            Arg::new("plugin-url")
                .long("plugin-url")
                .value_name("URL")
                .action(ArgAction::Append)
                .help(
                    "Fetch a plugin .zip from an https URL for the current session. \
                     Pair with --plugin-sha256 to verify the download.",
                ),
        )
        .arg(
            Arg::new("plugin-sha256")
                .long("plugin-sha256")
                .value_name("HEX")
                .action(ArgAction::Append)
                .help(
                    "SHA-256 checksum for the most recent (or next) --plugin-url. \
                     Required when the installed requirements.toml policy gates URL \
                     plugin loads.",
                ),
        )
        .arg(
            Arg::new("print")
                .long("print")
                .action(ArgAction::SetTrue)
                .help("Force non-interactive output (Claude-CLI compatibility flag)."),
        )
        .arg(
            Arg::new("p")
                .short('p')
                .value_name("PROMPT")
                .num_args(1..)
                .help(
                    "Run a one-shot prompt and exit (Claude-CLI compatibility flag). \
                     The remainder of the command line is treated as the prompt.",
                ),
        )
        .arg(
            Arg::new("gen-man")
                .long("gen-man")
                .action(ArgAction::SetTrue)
                .hide(true)
                .help(
                    "Render this manpage to stdout and exit. Used by scripts/release.sh \
                     to regenerate man/anvil.1 on every release.",
                ),
        )
    // Note: clap auto-generates --version/-V and --help/-h.  The manpage
    // gets one-liner help for each; anvil's actual --version output (semver,
    // git SHA, target triple, build date) is described in the DESCRIPTION
    // and AUTHORS sections above and below via the tail.
}
