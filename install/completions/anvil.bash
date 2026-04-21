# Bash completion for Anvil
# Source this file or drop it in /etc/bash_completion.d/ or
# ~/.local/share/bash-completion/completions/anvil

_anvil_completions() {
    local cur prev words cword
    _init_completion 2>/dev/null || {
        COMPREPLY=()
        cur="${COMP_WORDS[COMP_CWORD]}"
        prev="${COMP_WORDS[COMP_CWORD-1]}"
    }

    # Top-level subcommands and flags
    local subcommands="check upgrade uninstall setup login logout init continue resume sessions agents skills model prompt"
    local flags="--version --help --model --update --check --uninstall --setup --first-run --allowed-tools --output-format --permission-mode --dangerously-skip-permissions --continue --no-respawn"

    # Flag-value completions
    case "${prev}" in
        --model|-m)
            local models="claude-opus-4-6 claude-sonnet-4-6 claude-haiku-4-5-20251213 gpt-4o gpt-4-turbo grok-3 grok-3-mini gemini-1.5-pro"
            COMPREPLY=($(compgen -W "${models}" -- "${cur}"))
            return 0
            ;;
        --output-format)
            COMPREPLY=($(compgen -W "text json" -- "${cur}"))
            return 0
            ;;
        --permission-mode)
            COMPREPLY=($(compgen -W "default auto-edit" -- "${cur}"))
            return 0
            ;;
        login)
            COMPREPLY=($(compgen -W "anthropic openai google xai ollama" -- "${cur}"))
            return 0
            ;;
    esac

    # If the current word starts with a flag, complete flags
    if [[ "${cur}" == --* ]]; then
        COMPREPLY=($(compgen -W "${flags}" -- "${cur}"))
        return 0
    fi

    # First positional argument — offer subcommands
    if [[ ${COMP_CWORD} -eq 1 ]]; then
        COMPREPLY=($(compgen -W "${subcommands}" -- "${cur}"))
        return 0
    fi

    # Default: file completion
    COMPREPLY=($(compgen -f -- "${cur}"))
}

complete -F _anvil_completions anvil
