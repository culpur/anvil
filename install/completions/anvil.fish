# Fish shell completions for Anvil
# Drop in ~/.config/fish/completions/anvil.fish

# Disable file completion by default
complete -c anvil -f

# ── Subcommands ───────────────────────────────────────────────────────────────
complete -c anvil -n '__fish_use_subcommand' -a check      -d 'Print installation health checklist'
complete -c anvil -n '__fish_use_subcommand' -a upgrade    -d 'Upgrade Anvil to the latest release'
complete -c anvil -n '__fish_use_subcommand' -a uninstall  -d 'Remove Anvil from this system'
complete -c anvil -n '__fish_use_subcommand' -a setup      -d 'Run the first-run setup wizard'
complete -c anvil -n '__fish_use_subcommand' -a login      -d 'Authenticate with an AI provider'
complete -c anvil -n '__fish_use_subcommand' -a logout     -d 'Clear stored credentials'
complete -c anvil -n '__fish_use_subcommand' -a init       -d 'Initialise CLAUDE.md in current project'
complete -c anvil -n '__fish_use_subcommand' -a continue   -d 'Resume the most recent session'
complete -c anvil -n '__fish_use_subcommand' -a resume     -d 'Resume a specific session'
complete -c anvil -n '__fish_use_subcommand' -a sessions   -d 'List saved sessions'
complete -c anvil -n '__fish_use_subcommand' -a agents     -d 'List registered agents'
complete -c anvil -n '__fish_use_subcommand' -a skills     -d 'List available skills'
complete -c anvil -n '__fish_use_subcommand' -a model      -d 'Start REPL with a specific model'
complete -c anvil -n '__fish_use_subcommand' -a prompt     -d 'Run a one-shot prompt'

# ── Global flags ──────────────────────────────────────────────────────────────
complete -c anvil -l version  -s V -d 'Print version and exit'
complete -c anvil -l help     -s h -d 'Print help'
complete -c anvil -l update          -d 'Self-update to the latest release'
complete -c anvil -l check           -d 'Print installation health checklist'
complete -c anvil -l uninstall       -d 'Uninstall Anvil'
complete -c anvil -l setup           -d 'Run first-run setup wizard'
complete -c anvil -l first-run       -d 'Run first-run setup wizard'
complete -c anvil -l continue -s c   -d 'Resume most recent session'
complete -c anvil -l no-respawn      -d 'Disable in-place respawn'
complete -c anvil -l dangerously-skip-permissions -d 'Skip all permission prompts'

# --model
complete -c anvil -l model -d 'Choose AI model' -r -a '
    claude-opus-4-6\t"Anthropic Claude Opus"
    claude-sonnet-4-6\t"Anthropic Claude Sonnet"
    claude-haiku-4-5-20251213\t"Anthropic Claude Haiku"
    gpt-4o\t"OpenAI GPT-4o"
    gpt-4-turbo\t"OpenAI GPT-4 Turbo"
    grok-3\t"xAI Grok 3"
    grok-3-mini\t"xAI Grok 3 Mini"
    gemini-1.5-pro\t"Google Gemini 1.5 Pro"
'

# --output-format
complete -c anvil -l output-format -d 'Output format' -r -a 'text json'

# --permission-mode
complete -c anvil -l permission-mode -d 'Permission mode' -r -a 'default auto-edit'

# ── Subcommand: login — provider names ────────────────────────────────────────
complete -c anvil -n '__fish_seen_subcommand_from login' -a 'anthropic openai google xai ollama' -d 'Provider'

# ── Subcommand: model — model names ──────────────────────────────────────────
complete -c anvil -n '__fish_seen_subcommand_from model' -a '
    claude-opus-4-6
    claude-sonnet-4-6
    claude-haiku-4-5-20251213
    gpt-4o
    grok-3
    grok-3-mini
    gemini-1.5-pro
'

# ── Subcommand: resume — JSON session files ───────────────────────────────────
complete -c anvil -n '__fish_seen_subcommand_from resume' -F -a '*.json'
