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

    # Slash commands (available inside a REPL turn as the first "word" of input)
    local slash="/help /status /compact /cost /memory /init /diff /version /undo /doctor /tokens /chat /vim /think /fast /changelog /sleep /focus /screenshot /model /permissions /clear /context /pin /unpin /export /loop /commit /commit-push-pr /pr /issue /ultraplan /teleport /debug-tool-call /bughunter /mcp /plugins /session /resume /productivity /knowledge /daily /hub /provider /login /failover /language /theme /agents /skills /branch /worktree /git /db /docker /test /refactor /security /api /docs /scaffold /perf /debug /voice /collab /env /lsp /notebook /k8s /iac /pipeline /review /deps /mono /browser /notify /migrate /regex /ssh /logs /markdown /snippets /finetune /webhook /plugin-sdk /remote-control /history-archive /review-pr /configure /vault /web /qmd /search /semantic-search /generate-image /tab /fork /share /audit /restart"

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

    # Slash-command completion (anvil /v<TAB> → /vault, /version, /vim, /voice, /v*)
    if [[ "${cur}" == /* ]]; then
        COMPREPLY=($(compgen -W "${slash}" -- "${cur}"))
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
