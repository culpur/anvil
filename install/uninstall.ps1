#Requires -Version 5.1
<#
.SYNOPSIS
    Anvil uninstaller for Windows 10/11
.DESCRIPTION
    Removes the Anvil binary from %LOCALAPPDATA%\Programs\anvil and
    optionally removes the %USERPROFILE%\.anvil data directory.

.PARAMETER InstallDir
    Override the default install directory.
.PARAMETER KeepData
    Do not prompt to remove ~/.anvil data directory.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\anvil",
    [switch]$KeepData
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

function Write-Info    { param([string]$Msg) Write-Host "  > $Msg" -ForegroundColor Cyan }
function Write-Success { param([string]$Msg) Write-Host "  v $Msg" -ForegroundColor Green }
function Write-Warn    { param([string]$Msg) Write-Host "  ! $Msg" -ForegroundColor Yellow }
function Write-Fail    { param([string]$Msg) Write-Host "  x $Msg" -ForegroundColor Red }

Write-Host ""
Write-Host "Anvil uninstaller" -ForegroundColor Bold
Write-Host ""

$AnvilExe  = Join-Path $InstallDir "anvil.exe"
$AnvilHome = if ($env:ANVIL_CONFIG_HOME) { $env:ANVIL_CONFIG_HOME } else { "$env:USERPROFILE\.anvil" }

Write-Host "  This will remove:"
if (Test-Path $AnvilExe)  { Write-Host "    Binary : $AnvilExe" }
if (-not $KeepData -and (Test-Path $AnvilHome)) {
    Write-Host "    Data   : $AnvilHome (optional)"
}
Write-Host ""

$confirm = Read-Host "  Proceed? [y/N]"
if ($confirm -notmatch '^[yY]') {
    Write-Host "  Cancelled."
    exit 0
}

$Errors = 0

# Remove binary
if (Test-Path $AnvilExe) {
    try {
        Remove-Item -Force $AnvilExe
        Write-Success "Removed $AnvilExe"
    } catch {
        Write-Fail "Could not remove ${AnvilExe}: $_"
        $Errors++
    }
} else {
    Write-Warn "anvil.exe not found at $AnvilExe"
}

# Remove completions
$CompletionDir = Join-Path $InstallDir "completions"
if (Test-Path $CompletionDir) {
    Remove-Item -Recurse -Force $CompletionDir -ErrorAction SilentlyContinue
    Write-Info "Removed completions directory."
}

# Remove install directory if now empty
if ((Test-Path $InstallDir) -and ((Get-ChildItem $InstallDir).Count -eq 0)) {
    Remove-Item -Recurse -Force $InstallDir -ErrorAction SilentlyContinue
    Write-Info "Removed install directory."
}

# Remove from user PATH
$CurrentPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($CurrentPath -like "*$InstallDir*") {
    $NewPath = ($CurrentPath -split ';' | Where-Object { $_ -ne $InstallDir }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
    Write-Info "Removed $InstallDir from user PATH."
}

# Remove $PROFILE hook
if (Test-Path $PROFILE) {
    $ProfileContent = Get-Content $PROFILE -Raw
    if ($ProfileContent -like "*anvil*completions*") {
        # Remove the two-line block we added
        $Clean = $ProfileContent -replace "(?m)^# Anvil tab completion\r?\n.*anvil.*completions.*\r?\n?", ""
        Set-Content $PROFILE $Clean.TrimEnd()
        Write-Info "Removed Anvil completions from $PROFILE"
    }
}

# Optionally remove data directory
if (-not $KeepData -and (Test-Path $AnvilHome)) {
    Write-Host ""
    $dataConfirm = Read-Host "  Remove $AnvilHome (vault + sessions)? [y/N]"
    if ($dataConfirm -match '^[yY]') {
        try {
            Remove-Item -Recurse -Force $AnvilHome
            Write-Success "Removed $AnvilHome"
        } catch {
            Write-Fail "Could not remove ${AnvilHome}: $_"
            $Errors++
        }
    } else {
        Write-Info "Keeping $AnvilHome."
    }
}

Write-Host ""
if ($Errors -eq 0) {
    Write-Success "Anvil uninstalled successfully."
} else {
    Write-Fail "Uninstall completed with $Errors error(s). Some files may require manual removal."
    exit 1
}
Write-Host ""
