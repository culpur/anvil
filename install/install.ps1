#Requires -Version 5.1
<#
.SYNOPSIS
    Anvil installer for Windows 10/11.
.DESCRIPTION
    Silent by default. The installer is plumbing — Anvil's first-run
    wizard is the UX. We download the binary, SHA256-verify it (primary
    source: anvilhub.culpur.net; fallback: GitHub release sibling),
    write it to %LOCALAPPDATA%\Programs\anvil, add that directory to
    user PATH, and launch the alt-screen wizard.  No banners, no
    warnings about optional dependencies (the wizard handles
    QMD / Node.js / Ollama detection itself).

    Cross-platform parity with install/install.sh (v2.2.18 #661):
    - Silent by default (one-line progress on supported terminals, or
      a single "Verifying…" line on classic consoles).
    - SHA256 verify is non-negotiable; abort on mismatch or missing
      checksum.
    - Ends with `& "$DestExe"` (no `--setup` flag), so the first-run
      wizard fires from the first-run-no-config gate.  The `--setup`
      flag now routes to the same alt-screen wizard regardless, but
      we converge installers on the bare-launch path so there is one
      code path for both fresh installs and first-launch.
    - `-Verbose` switch prints each step on its own line for debugging.
    - `-NoSetup` skips wizard launch and prints "Installed. Run anvil."
    - `-Quiet` suppresses progress messages entirely (for use in
      package-manager wrappers and CI bootstrap).

    Usage:
      Set-ExecutionPolicy Bypass -Scope Process -Force
      iwr -useb https://anvilhub.culpur.net/install.ps1 | iex
    or:
      .\install.ps1 [-NoSetup] [-NoCompletions] [-InstallDir <path>]
                    [-Verbose] [-Quiet]

.PARAMETER NoSetup
    Skip the first-run wizard launch.  Useful for CI / image baking.
.PARAMETER NoCompletions
    Skip PowerShell tab completion installation.
.PARAMETER InstallDir
    Override the default install directory (%LOCALAPPDATA%\Programs\anvil).
.PARAMETER Quiet
    Suppress per-step progress messages.  Errors still print.

.NOTES
    Exit codes mirror install.sh:
      0 — success
      1 — network failure
      2 — checksum failure
      3 — unsupported platform / permission error
#>
[CmdletBinding()]
param(
    [switch]$NoSetup,
    [switch]$NoCompletions,
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\anvil",
    [switch]$Quiet
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Output helpers ────────────────────────────────────────────────────────────
# `-Verbose` is an automatic CmdletBinding switch — $VerbosePreference is
# 'Continue' when the caller passed -Verbose, 'SilentlyContinue' otherwise.
# We treat that as our debug-print mode.
$IsVerbose = ($VerbosePreference -eq 'Continue')

# When the script is run via `iwr ... | iex` the host is interactive but
# we cannot do carriage-return overwrites the way bash does on a real
# terminal.  Best-effort: print one-line status per step in default mode,
# nothing at all in -Quiet mode, each step on its own line in -Verbose.
function Write-Step {
    param([string]$Msg)
    if ($Quiet) { return }
    if ($IsVerbose) {
        Write-Host "  $Msg"
    } else {
        Write-Host "  $Msg" -ForegroundColor DarkGray
    }
}

function Write-Fail {
    param([string]$Msg)
    Write-Host "error: $Msg" -ForegroundColor Red
}

function Test-Command {
    param([string]$Name)
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# ── Windows version + architecture sanity ────────────────────────────────────
$WinVer = [System.Environment]::OSVersion.Version
if ($WinVer.Major -lt 10) {
    Write-Fail "Windows 10 or later is required (detected: $($WinVer.Major).$($WinVer.Minor))"
    exit 3
}

# Only x86_64 binaries are produced for Windows today (matches install.sh
# arch matrix: aarch64-pc-windows-msvc is source-only).
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($Arch -ne 'X64') {
    Write-Fail "Only x86_64 (64-bit) Windows is supported (detected: $Arch)"
    Write-Fail "Build from source: cargo install --git https://github.com/culpur/anvil-source"
    exit 3
}
$Target = 'x86_64-pc-windows-msvc'

# ── Install directory ─────────────────────────────────────────────────────────
if (-not (Test-Path $InstallDir)) {
    try {
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    } catch {
        Write-Fail "Cannot create install directory: $InstallDir ($_)"
        exit 3
    }
}

# ── Download Anvil ────────────────────────────────────────────────────────────
$GithubBase = "https://github.com/culpur/anvil/releases/latest/download"
$BinaryName = "anvil-$Target.exe"
$BinaryUrl  = "$GithubBase/$BinaryName"

# Primary (out-of-band) SHA256 source: anvilhub.culpur.net.  Served from
# a separate origin so a GitHub release compromise cannot also forge the
# hash.  Fallback: the .sha256 sibling on the GitHub release.
$Sha256UrlPrimary  = "https://anvilhub.culpur.net/sha256/$BinaryName.sha256"
$Sha256UrlFallback = "$BinaryUrl.sha256"

$TmpDir    = Join-Path $env:TEMP "anvil-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null
$TmpBinary = Join-Path $TmpDir "anvil.exe"
$TmpSha256 = Join-Path $TmpDir "anvil.sha256"

# Best-effort cleanup on exit.  PowerShell finally-blocks at script scope
# are awkward under `iex`, so we register an event hook on the engine
# exit which fires even on uncaught errors and Ctrl+C.
$null = Register-EngineEvent -SourceIdentifier PowerShell.Exiting -Action {
    Remove-Item -Recurse -Force $using:TmpDir -ErrorAction SilentlyContinue
}

Write-Step "Downloading Anvil for windows x86_64…"
try {
    # `-UseBasicParsing` keeps us off the IE-engine dependency that's
    # missing on Server Core; `-MaximumRedirection 5` follows the
    # GitHub release redirect chain.
    Invoke-WebRequest -Uri $BinaryUrl `
        -OutFile $TmpBinary `
        -UseBasicParsing `
        -MaximumRedirection 5 `
        -TimeoutSec 180 `
        -ErrorAction Stop | Out-Null
} catch {
    Write-Fail "Download failed: $_"
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 1
}

# ── SHA256 verification (mandatory) ───────────────────────────────────────────
# Integrity is non-negotiable.  If checksum fetch fails on BOTH sources
# we abort — we never install an unverified binary.  Previously a
# network error in the checksum fetch silently skipped verification,
# which meant an attacker blocking the .sha256 URL would bypass the
# whole integrity check (regression closed for parity with install.sh).
Write-Step "Verifying signature…"
$Sha256Source = "primary"
try {
    # 5s timeout on the primary — fail fast to the fallback.
    Invoke-WebRequest -Uri $Sha256UrlPrimary `
        -OutFile $TmpSha256 `
        -UseBasicParsing `
        -MaximumRedirection 5 `
        -TimeoutSec 5 `
        -ErrorAction Stop | Out-Null
} catch {
    $Sha256Source = "fallback"
    try {
        Invoke-WebRequest -Uri $Sha256UrlFallback `
            -OutFile $TmpSha256 `
            -UseBasicParsing `
            -MaximumRedirection 5 `
            -TimeoutSec 15 `
            -ErrorAction Stop | Out-Null
    } catch {
        Write-Fail "Could not fetch checksum from either source."
        Write-Fail "Refusing to install an unverified binary."
        Write-Fail "primary:  $Sha256UrlPrimary"
        Write-Fail "fallback: $Sha256UrlFallback"
        Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
        exit 2
    }
}

$Expected = (Get-Content $TmpSha256 -Raw).Trim().Split()[0].ToLower()
if ([string]::IsNullOrWhiteSpace($Expected)) {
    Write-Fail "Checksum file is empty or malformed."
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 2
}

$Actual = (Get-FileHash -Path $TmpBinary -Algorithm SHA256).Hash.ToLower()
if ($Actual -ne $Expected) {
    Write-Fail "SHA256 mismatch."
    Write-Fail "  expected: $Expected"
    Write-Fail "  got:      $Actual"
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 2
}

# ── Install binary ────────────────────────────────────────────────────────────
$DestExe = Join-Path $InstallDir "anvil.exe"
Write-Step "Installing to $DestExe…"
try {
    Copy-Item -Path $TmpBinary -Destination $DestExe -Force
} catch {
    Write-Fail "Could not install to $DestExe ($_)"
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 3
}
Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue

# ── Add to user PATH ──────────────────────────────────────────────────────────
$CurrentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$PathHint = $null
if ($CurrentPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable(
        'Path',
        "$CurrentPath;$InstallDir",
        'User'
    )
    # We can't refresh the current shell's PATH without a re-launch, but
    # we CAN extend the process env so the `& $DestExe` invocation below
    # finds it.  Surface the hint AFTER install so it lives in scrollback
    # above the alt-screen wizard.
    $env:PATH = "$env:PATH;$InstallDir"
    $PathHint = "$InstallDir added to user PATH. Restart your terminal for new shells to see it."
}

# ── Shell completions (silent, best-effort) ───────────────────────────────────
# Mirrors install.sh's best-effort behaviour: copy bundled completions
# into a known share location, then add an `Invoke` line to $PROFILE so
# new shells pick them up.  We never fail the install over a completion
# hiccup.
if (-not $NoCompletions) {
    try {
        $CompletionDir = Join-Path $InstallDir "completions"
        New-Item -ItemType Directory -Force -Path $CompletionDir | Out-Null

        # `$MyInvocation.MyCommand.Path` is `$null` when run via `iex`
        # (the script came from a pipeline, not a file).  Guard for that.
        $ScriptDir = if ($MyInvocation.MyCommand.Path) {
            Split-Path $MyInvocation.MyCommand.Path
        } else { $null }

        $PsCompletionSrc = if ($ScriptDir) {
            Join-Path $ScriptDir "completions\anvil.ps1"
        } else { $null }
        $PsCompletionDst = Join-Path $CompletionDir "anvil.ps1"

        if ($PsCompletionSrc -and (Test-Path $PsCompletionSrc)) {
            Copy-Item $PsCompletionSrc $PsCompletionDst -Force
        } else {
            # Inline stub: when running via iwr|iex we don't have the
            # bundled file.  The wizard's `anvil --completions powershell`
            # path will materialise it later; for now leave a marker so
            # repeat installs don't keep re-emitting.
            Set-Content -Path $PsCompletionDst `
                -Value "# Anvil completions: run ``anvil completions powershell > $PsCompletionDst`` to populate."
        }

        # Hook into $PROFILE if it isn't already pointing at us.
        $ProfileContent = if (Test-Path $PROFILE) {
            Get-Content $PROFILE -Raw -ErrorAction SilentlyContinue
        } else { "" }
        if ($ProfileContent -notlike "*anvil*completions*") {
            $CompletionLine = ". `"$PsCompletionDst`" -ErrorAction SilentlyContinue"
            $ProfileDir = Split-Path $PROFILE
            if (-not (Test-Path $ProfileDir)) {
                New-Item -ItemType Directory -Force -Path $ProfileDir | Out-Null
            }
            Add-Content -Path $PROFILE -Value "`n# Anvil tab completion`n$CompletionLine"
        }
    } catch {
        # Best-effort.  Completions are a nice-to-have; never block install.
        if ($IsVerbose) { Write-Host "  Completion install skipped: $_" -ForegroundColor DarkGray }
    }
}

# ── Hand off to the wizard ────────────────────────────────────────────────────
# Surface the PATH hint BEFORE the wizard so it stays in scrollback above
# the alt-screen.  The wizard's welcome card will be the first thing on
# the new screen.
if ($PathHint -and -not $Quiet) {
    Write-Host ""
    Write-Host $PathHint -ForegroundColor DarkGray
    Write-Host ""
}

if (-not $NoSetup) {
    # Headless detection.  Windows hosts can run install.ps1 in two
    # non-interactive modes:
    #   * `powershell.exe -NonInteractive -Command "iwr ... | iex"`
    #     (azure-pipelines, github-actions, packer)
    #   * Service contexts (winrm bootstrap, scheduled task) where
    #     `$Host.UI.RawUI` is not a console.
    # In either case the alt-screen wizard would render escape sequences
    # into the surrounding pipe and never reach a user keyboard.  Bail
    # out the same way install.sh does and let the user run anvil from
    # a real terminal session.
    $IsHeadless = $false
    try {
        # `$Host.Name` is 'ConsoleHost' under a real console, 'ServerRemoteHost'
        # under winrm, 'Default Host' under -NonInteractive.  We only treat
        # ConsoleHost as a TTY-equivalent.
        if ($Host.Name -ne 'ConsoleHost') { $IsHeadless = $true }
        # `[Console]::IsInputRedirected` covers `cmd | powershell` and
        # `iex` invocation patterns where stdin is not the keyboard.
        if ([Console]::IsInputRedirected -or [Console]::IsOutputRedirected) {
            $IsHeadless = $true
        }
    } catch {
        # If we can't tell, err on the side of "don't crash the wizard".
        $IsHeadless = $true
    }

    if ($IsHeadless) {
        if ($Quiet) {
            Write-Host $DestExe
        } else {
            Write-Host "Installed: $DestExe. Run ``anvil`` from a terminal to complete setup."
        }
        exit 0
    }

    # IMPORTANT: do NOT pass `--setup` even though as of v2.2.18 #661
    # the flag now routes to the same alt-screen wizard.  Single code
    # path: invoke the binary bare and let the first-run-no-config gate
    # in `wizard.rs::anvil_config_json_exists` fire the wizard for us.
    & $DestExe
    exit $LASTEXITCODE
}

# -NoSetup path — single line summary.
if ($Quiet) {
    Write-Host $DestExe
} else {
    Write-Host "Installed: $DestExe"
    Write-Host "Run: anvil"
}
