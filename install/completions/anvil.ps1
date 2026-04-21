# PowerShell tab completion for Anvil
# Add this to your $PROFILE:
#   . "$env:LOCALAPPDATA\Programs\anvil\completions\anvil.ps1"
# Or source it directly:
#   . /path/to/anvil.ps1

Register-ArgumentCompleter -Native -CommandName 'anvil' -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)

    $subcommands = @(
        [System.Management.Automation.CompletionResult]::new('check',     'check',     'ParameterValue', 'Print installation health checklist')
        [System.Management.Automation.CompletionResult]::new('upgrade',   'upgrade',   'ParameterValue', 'Upgrade Anvil to the latest release')
        [System.Management.Automation.CompletionResult]::new('uninstall', 'uninstall', 'ParameterValue', 'Remove Anvil from this system')
        [System.Management.Automation.CompletionResult]::new('setup',     'setup',     'ParameterValue', 'Run the first-run setup wizard')
        [System.Management.Automation.CompletionResult]::new('login',     'login',     'ParameterValue', 'Authenticate with an AI provider')
        [System.Management.Automation.CompletionResult]::new('logout',    'logout',    'ParameterValue', 'Clear stored credentials')
        [System.Management.Automation.CompletionResult]::new('init',      'init',      'ParameterValue', 'Initialise CLAUDE.md in current project')
        [System.Management.Automation.CompletionResult]::new('continue',  'continue',  'ParameterValue', 'Resume the most recent session')
        [System.Management.Automation.CompletionResult]::new('resume',    'resume',    'ParameterValue', 'Resume a specific session')
        [System.Management.Automation.CompletionResult]::new('sessions',  'sessions',  'ParameterValue', 'List saved sessions')
        [System.Management.Automation.CompletionResult]::new('agents',    'agents',    'ParameterValue', 'List registered agents')
        [System.Management.Automation.CompletionResult]::new('skills',    'skills',    'ParameterValue', 'List available skills')
        [System.Management.Automation.CompletionResult]::new('model',     'model',     'ParameterValue', 'Start REPL with a specific model')
        [System.Management.Automation.CompletionResult]::new('prompt',    'prompt',    'ParameterValue', 'Run a one-shot prompt')
    )

    $flags = @(
        [System.Management.Automation.CompletionResult]::new('--version',                     '--version',                     'ParameterName', 'Print version and exit')
        [System.Management.Automation.CompletionResult]::new('--help',                        '--help',                        'ParameterName', 'Print help')
        [System.Management.Automation.CompletionResult]::new('--model',                       '--model',                       'ParameterName', 'Choose AI model')
        [System.Management.Automation.CompletionResult]::new('--update',                      '--update',                      'ParameterName', 'Self-update to the latest release')
        [System.Management.Automation.CompletionResult]::new('--check',                       '--check',                       'ParameterName', 'Print installation health checklist')
        [System.Management.Automation.CompletionResult]::new('--uninstall',                   '--uninstall',                   'ParameterName', 'Uninstall Anvil')
        [System.Management.Automation.CompletionResult]::new('--setup',                       '--setup',                       'ParameterName', 'Run first-run setup wizard')
        [System.Management.Automation.CompletionResult]::new('--continue',                    '--continue',                    'ParameterName', 'Resume most recent session')
        [System.Management.Automation.CompletionResult]::new('--no-respawn',                  '--no-respawn',                  'ParameterName', 'Disable in-place respawn')
        [System.Management.Automation.CompletionResult]::new('--output-format',               '--output-format',               'ParameterName', 'Output format (text|json)')
        [System.Management.Automation.CompletionResult]::new('--permission-mode',             '--permission-mode',             'ParameterName', 'Permission mode')
        [System.Management.Automation.CompletionResult]::new('--dangerously-skip-permissions','--dangerously-skip-permissions','ParameterName', 'Skip all permission prompts')
    )

    $models = @(
        'claude-opus-4-6', 'claude-sonnet-4-6', 'claude-haiku-4-5-20251213',
        'gpt-4o', 'gpt-4-turbo', 'grok-3', 'grok-3-mini', 'gemini-1.5-pro'
    )

    $providers = @('anthropic', 'openai', 'google', 'xai', 'ollama')

    # Parse words so far to find position
    $words = $commandAst.CommandElements | ForEach-Object { $_.ToString() }
    $prevWord = if ($words.Count -ge 2) { $words[$words.Count - 2] } else { '' }

    # Flag-value completions
    if ($prevWord -eq '--model') {
        return $models | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }

    if ($prevWord -eq '--output-format') {
        return @('text', 'json') | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }

    if ($prevWord -eq '--permission-mode') {
        return @('default', 'auto-edit') | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }

    if ($prevWord -eq 'login') {
        return $providers | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }

    if ($prevWord -eq 'model') {
        return $models | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    }

    # First word — offer flags and subcommands
    if ($wordToComplete.StartsWith('--')) {
        return $flags | Where-Object { $_.CompletionText -like "$wordToComplete*" }
    }

    # First positional — offer subcommands
    if ($words.Count -le 2) {
        return $subcommands | Where-Object { $_.CompletionText -like "$wordToComplete*" }
    }
}
