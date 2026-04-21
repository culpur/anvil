#Requires -Version 5.1
<#
.SYNOPSIS
    Anvil installer for Windows 10/11
.DESCRIPTION
    Downloads and installs the Anvil AI coding assistant, checks for
    Node.js / npm / Git / QMD, adds Anvil to the user PATH, installs
    PowerShell tab completion, and runs the first-run setup wizard.

    Run with:
      Set-ExecutionPolicy Bypass -Scope Process -Force
      iwr -useb https://anvilhub.culpur.net/install.ps1 | iex
    or:
      .\install.ps1 [-NoSetup] [-NoCompletions] [-InstallDir <path>]

.PARAMETER NoSetup
    Skip the first-run setup wizard.
.PARAMETER NoCompletions
    Skip PowerShell tab completion installation.
.PARAMETER InstallDir
    Override the default install directory (%LOCALAPPDATA%\Programs\anvil).
#>
[CmdletBinding()]
param(
    [switch]$NoSetup,
    [switch]$NoCompletions,
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\anvil"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Helpers ───────────────────────────────────────────────────────────────────
function Write-Info    { param([string]$Msg) Write-Host "  > $Msg" -ForegroundColor Cyan }
function Write-Success { param([string]$Msg) Write-Host "  v $Msg" -ForegroundColor Green }
function Write-Warn    { param([string]$Msg) Write-Host "  ! $Msg" -ForegroundColor Yellow }
function Write-Fail    { param([string]$Msg) Write-Host "  x $Msg" -ForegroundColor Red }

function Test-Command {
    param([string]$Name)
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# ── Windows version check ─────────────────────────────────────────────────────
$WinVer = [System.Environment]::OSVersion.Version
if ($WinVer.Major -lt 10) {
    Write-Fail "Windows 10 or later is required (detected: $($WinVer.Major).$($WinVer.Minor))"
    exit 1
}

# ── Banner ────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host "  Anvil installer — Windows                                    " -ForegroundColor Cyan
Write-Host "  https://anvilhub.culpur.net                                  " -ForegroundColor Cyan
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host ""

# ── Architecture ──────────────────────────────────────────────────────────────
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($Arch -ne 'X64') {
    Write-Fail "Only x86_64 (64-bit) Windows is supported (detected: $Arch)"
    exit 1
}
$Target = 'x86_64-pc-windows-msvc'
Write-Info "Platform: Windows x64 (target: $Target)"

# ── Install directory ─────────────────────────────────────────────────────────
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}
Write-Info "Install directory: $InstallDir"

# ── Dependency checks + winget installs ──────────────────────────────────────
function Install-WithWinget {
    param([string]$Name, [string]$WingetId, [string]$ManualUrl)
    if (-not (Test-Command $Name)) {
        Write-Warn "$Name not found."
        if (Test-Command 'winget') {
            Write-Info "Installing $Name via winget..."
            try {
                winget install --id $WingetId --silent --accept-package-agreements --accept-source-agreements
                Write-Success "$Name installed."
            } catch {
                Write-Warn "winget install failed. Install manually: $ManualUrl"
            }
        } else {
            Write-Warn "winget not available. Install $Name from: $ManualUrl"
        }
    } else {
        $ver = (& $Name --version 2>$null) -join '' | Select-String '\d[\d.]+' | ForEach-Object { $_.Matches[0].Value }
        Write-Success "$Name $ver already installed."
    }
}

Install-WithWinget -Name 'git'  -WingetId 'Git.Git'   -ManualUrl 'https://git-scm.com'
Install-WithWinget -Name 'node' -WingetId 'OpenJS.NodeJS.LTS' -ManualUrl 'https://nodejs.org'

if (-not (Test-Command 'npm')) {
    Write-Warn "npm not found — it is bundled with Node.js. Install Node.js from https://nodejs.org"
}

if (-not (Test-Command 'qmd')) {
    Write-Warn "qmd not found — install from https://anvilhub.culpur.net"
}

# ── Download Anvil ────────────────────────────────────────────────────────────
$GithubBase = "https://github.com/culpur/anvil/releases/latest/download"
$BinaryName = "anvil-$Target.exe"
$BinaryUrl  = "$GithubBase/$BinaryName"
$Sha256Url  = "$BinaryUrl.sha256"

$TmpDir     = Join-Path $env:TEMP "anvil-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null
$TmpBinary  = Join-Path $TmpDir "anvil.exe"
$TmpSha256  = Join-Path $TmpDir "anvil.sha256"

Write-Info "Downloading $BinaryUrl ..."
try {
    Invoke-WebRequest -Uri $BinaryUrl -OutFile $TmpBinary -UseBasicParsing
} catch {
    Write-Fail "Download failed: $_"
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 1
}

# ── SHA256 verification ───────────────────────────────────────────────────────
Write-Info "Verifying checksum..."
try {
    Invoke-WebRequest -Uri $Sha256Url -OutFile $TmpSha256 -UseBasicParsing
    $Expected = (Get-Content $TmpSha256 -Raw).Trim().Split()[0].ToLower()
    $Actual   = (Get-FileHash -Path $TmpBinary -Algorithm SHA256).Hash.ToLower()

    if ($Actual -ne $Expected) {
        Write-Fail "SHA256 mismatch!"
        Write-Fail "  expected: $Expected"
        Write-Fail "  got:      $Actual"
        Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
        exit 2
    }
    Write-Success "Checksum verified."
} catch {
    Write-Warn "No checksum file found — skipping verification."
}

# ── Install binary ────────────────────────────────────────────────────────────
$DestExe = Join-Path $InstallDir "anvil.exe"
Copy-Item -Path $TmpBinary -Destination $DestExe -Force
Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
Write-Success "Anvil installed to $DestExe"

# ── Add to user PATH ──────────────────────────────────────────────────────────
$CurrentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($CurrentPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable(
        'Path',
        "$CurrentPath;$InstallDir",
        'User'
    )
    Write-Success "Added $InstallDir to user PATH."
    Write-Info "Restart your terminal for PATH changes to take effect."
} else {
    Write-Info "$InstallDir already on user PATH."
}

# Also add to current session PATH so we can invoke anvil immediately
$env:PATH = "$env:PATH;$InstallDir"

# ── Shell completions ─────────────────────────────────────────────────────────
if (-not $NoCompletions) {
    $CompletionDir  = Join-Path $InstallDir "completions"
    New-Item -ItemType Directory -Force -Path $CompletionDir | Out-Null

    # Write the PowerShell completion script bundled in this installer
    $PsCompletionSrc = Join-Path (Split-Path $MyInvocation.MyCommand.Path) "completions\anvil.ps1"
    $PsCompletionDst = Join-Path $CompletionDir "anvil.ps1"

    if (Test-Path $PsCompletionSrc) {
        Copy-Item $PsCompletionSrc $PsCompletionDst -Force
        Write-Success "PowerShell completions: $PsCompletionDst"
    } else {
        # Embed a minimal inline completion registration
        $InlineCompletion = @'
# Auto-generated by Anvil installer
. "$env:LOCALAPPDATA\Programs\anvil\completions\anvil.ps1" -ErrorAction SilentlyContinue
'@
        Set-Content -Path $PsCompletionDst -Value $InlineCompletion
    }

    # Hook into $PROFILE
    if (Test-Path $PROFILE) {
        $ProfileContent = Get-Content $PROFILE -Raw -ErrorAction SilentlyContinue
    } else {
        $ProfileContent = ""
    }
    $CompletionLine = ". `"$PsCompletionDst`" -ErrorAction SilentlyContinue"
    if ($ProfileContent -notlike "*anvil*completions*") {
        Add-Content -Path $PROFILE -Value "`n# Anvil tab completion`n$CompletionLine"
        Write-Success "Completions hooked into $PROFILE"
    } else {
        Write-Info "Completions already referenced in $PROFILE"
    }
}

# ── First-run wizard ──────────────────────────────────────────────────────────
if (-not $NoSetup) {
    Write-Host ""
    Write-Info "Launching first-run setup wizard..."
    Write-Host ""
    try {
        & "$DestExe" --setup
    } catch {
        Write-Warn "Setup wizard failed: $_"
        Write-Warn "Run 'anvil --setup' later to configure."
    }
}

# ── Done ──────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "================================================================" -ForegroundColor Green
Write-Host "  Installation complete!                                        " -ForegroundColor Green
Write-Host "  Run: anvil                                                    " -ForegroundColor Green
Write-Host "  Docs: https://anvilhub.culpur.net/docs                       " -ForegroundColor Green
Write-Host "================================================================" -ForegroundColor Green
Write-Host ""
