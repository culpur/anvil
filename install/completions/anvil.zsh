#compdef anvil
# Zsh completion for Anvil
# Drop in a directory on your $fpath (e.g. ~/.zfunc/) then run:
#   autoload -Uz compinit && compinit

_anvil() {
    local -a subcommands flags models providers output_formats permission_modes

    subcommands=(
        'check:Print installation health checklist'
        'upgrade:Upgrade Anvil to the latest release'
        'uninstall:Remove Anvil from this system'
        'setup:Run the first-run setup wizard'
        'login:Authenticate with an AI provider'
        'logout:Clear stored credentials'
        'init:Initialise CLAUDE.md in the current project'
        'continue:Resume the most recent session'
        'resume:Resume a specific session'
        'sessions:List saved sessions'
        'agents:List registered agents'
        'skills:List available skills'
        'model:Start REPL with a specific model'
        'prompt:Run a one-shot prompt'
    )

    flags=(
        '(--version -V)'{--version,-V}'[Print version and exit]'
        '(--help -h)'{--help,-h}'[Print help]'
        '--model[Choose AI model]:model:(claude-opus-4-6 claude-sonnet-4-6 gpt-4o grok-3 gemini-1.5-pro)'
        '--update[Self-update to the latest release]'
        '--check[Print installation health checklist]'
        '--uninstall[Uninstall Anvil]'
        '--setup[Run first-run setup wizard]'
        '--first-run[Run first-run setup wizard]'
        '--allowed-tools[Comma-separated list of allowed tools]:tools:'
        '--output-format[Output format]:format:(text json)'
        '--permission-mode[Permission mode]:mode:(default auto-edit)'
        '--dangerously-skip-permissions[Skip all permission prompts]'
        '(--continue -c)'{--continue,-c}'[Resume most recent session]'
        '--no-respawn[Disable in-place respawn after upgrade]'
    )

    models=(claude-opus-4-6 claude-sonnet-4-6 claude-haiku-4-5-20251213 gpt-4o gpt-4-turbo grok-3 grok-3-mini gemini-1.5-pro)
    providers=(anthropic openai google xai ollama)
    output_formats=(text json)
    permission_modes=(default auto-edit)

    _arguments -C \
        "${flags[@]}" \
        '1: :->cmd' \
        '*:: :->args' \
        && return 0

    case "$state" in
        cmd)
            _describe 'subcommand' subcommands
            ;;
        args)
            case "${words[1]}" in
                login)
                    _arguments "1: :(${providers[*]})"
                    ;;
                model)
                    _arguments "1: :(${models[*]})"
                    ;;
                resume)
                    _files -g '*.json'
                    ;;
                *)
                    _default
                    ;;
            esac
            ;;
    esac
}

_anvil "$@"
