# Locale-aware installer test for install.ps1 (task #751).
#
# Verifies that install/install.ps1 detects $PSCulture and renders the
# platform / install / completion lines in the matching Tier-1 priority
# language. Runs install.ps1 in -DryRun mode so no files are touched.
#
# Usage (Windows / pwsh):
#   pwsh -File install/test_install_ps1.ps1
#
# Exit codes:
#   0 — all assertions passed
#   1 — at least one assertion failed
#
# NOTE: this test requires pwsh / PowerShell 5.1+. macOS / Linux CI without
# pwsh installed must SKIP this test; the shell test (test_install_sh.sh)
# is the gate for CI. This file is the manual gate for Windows reviewers.

$ErrorActionPreference = 'Continue'
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$InstallPs1 = Join-Path $ScriptDir "install.ps1"

if (-not (Test-Path $InstallPs1)) {
    Write-Host "FAIL: $InstallPs1 not found" -ForegroundColor Red
    exit 1
}

$Pass = 0
$Fail = 0

function Invoke-WithCulture {
    param([string]$Culture, [string]$Script)
    # Run install.ps1 in a child PowerShell with the requested culture.
    $code = @"
[System.Threading.Thread]::CurrentThread.CurrentCulture = [System.Globalization.CultureInfo]::new('$Culture')
[System.Threading.Thread]::CurrentThread.CurrentUICulture = [System.Globalization.CultureInfo]::new('$Culture')
& '$Script' -DryRun -NoSetup -NoCompletions
"@
    return (pwsh -NoProfile -Command $code 2>&1 | Out-String)
}

function Assert-Contains {
    param([string]$Lang, [string]$Culture, [string]$Needle, [string]$Desc)
    $out = Invoke-WithCulture -Culture $Culture -Script $InstallPs1
    if ($out -match [regex]::Escape($Needle)) {
        Write-Host "  PASS [$Lang] $Desc" -ForegroundColor Green
        $script:Pass++
    } else {
        Write-Host "  FAIL [$Lang] $Desc" -ForegroundColor Red
        Write-Host "         expected to find: $Needle"
        Write-Host "         got:"
        ($out -split "`n") | Select-Object -First 20 | ForEach-Object { Write-Host "         | $_" }
        $script:Fail++
    }
}

Write-Host "=== install.ps1 locale detection tests ==="
Write-Host

Assert-Contains -Lang 'de'    -Culture 'de-DE'  -Needle 'Plattform'        -Desc "German: 'Plattform' appears"
Assert-Contains -Lang 'de'    -Culture 'de-DE'  -Needle 'TROCKENLAUF'      -Desc "German: dry-run banner"
Assert-Contains -Lang 'fr'    -Culture 'fr-FR'  -Needle 'Plateforme'       -Desc "French: 'Plateforme' appears"
Assert-Contains -Lang 'fr'    -Culture 'fr-FR'  -Needle 'MODE SIMULATION'  -Desc "French: dry-run banner"
Assert-Contains -Lang 'zh-CN' -Culture 'zh-CN'  -Needle '平台'              -Desc "Chinese: '平台' appears"
Assert-Contains -Lang 'zh-CN' -Culture 'zh-CN'  -Needle '演练模式'           -Desc "Chinese: dry-run banner"
Assert-Contains -Lang 'ja'    -Culture 'ja-JP'  -Needle 'プラットフォーム'    -Desc "Japanese: 'プラットフォーム' appears"
Assert-Contains -Lang 'es'    -Culture 'es-ES'  -Needle 'Plataforma'       -Desc "Spanish: 'Plataforma' appears"
Assert-Contains -Lang 'en'    -Culture 'en-US'  -Needle 'Platform:'        -Desc "en-US: English platform line"
Assert-Contains -Lang 'en'    -Culture 'en-US'  -Needle 'DRY-RUN MODE'     -Desc "en-US: English dry-run banner"

# Unsupported culture falls back to English but reports source.
Assert-Contains -Lang 'sw-KE-fallback' -Culture 'sw-KE' -Needle 'Platform:'       -Desc "Unsupported locale -> English"
Assert-Contains -Lang 'sw-KE-fallback' -Culture 'sw-KE' -Needle 'unsupported:sw'  -Desc "Unsupported locale reports source"

Write-Host
Write-Host "=== Results ==="
Write-Host "  PASS: $Pass" -ForegroundColor Green
Write-Host "  FAIL: $Fail" -ForegroundColor Red

if ($Fail -gt 0) {
    exit 1
}
exit 0
