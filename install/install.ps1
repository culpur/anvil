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

# ── Locale detection + translation (task #751) ────────────────────────────────
# Detect the user's UI culture from $PSCulture, normalize it (fr-FR → fr if
# bare lang is supported), and look up status strings via T() with English
# fallback. Tier-1 priority: en, es, zh-CN, fr, pt-BR, ru, ja, de, ko, it.
# Mirrors install.sh: silent-by-default; i18n only wraps user-visible lines.
$SupportedLangs = @('en','es','zh-CN','fr','pt-BR','ru','ja','de','ko','it')

function Resolve-LangCode {
    $raw = $PSCulture
    if ([string]::IsNullOrWhiteSpace($raw)) { return 'en' }
    # PowerShell already gives us BCP-47 (e.g., fr-FR, zh-CN). Strip any
    # codeset/modifier just to be safe.
    $code = ($raw -replace '\..*$','') -replace '@.*$',''
    if ($SupportedLangs -contains $code) { return $code }
    $base = ($code -split '-')[0]
    if ($SupportedLangs -contains $base) { return $base }
    return 'en'
}
$LangCode = Resolve-LangCode

# Translation lookup. T <key> [arg1] [arg2] ...
# Returns the message formatted via -f. English is the fallback.
$Strings = @{
    'en' = @{
        'downloading'         = "Downloading Anvil for {0} {1}…"
        'verifying'           = "Verifying signature…"
        'installing'          = "Installing to {0}…"
        'installed'           = "Installed: {0}"
        'run_anvil'           = "Run: anvil"
        'installed_tty_hint'  = "Installed: {0}. Run ``anvil`` from a terminal to complete setup."
        'path_hint'           = "{0} added to user PATH. Restart your terminal for new shells to see it."
        'err_win_version'     = "Windows 10 or later is required (detected: {0}.{1})"
        'err_arch_unsupported' = "Only x86_64 (64-bit) Windows is supported (detected: {0})"
        'err_source_only'     = "Build from source: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Cannot create install directory: {0} ({1})"
        'err_download_failed' = "Download failed: {0}"
        'err_no_checksum'     = "Could not fetch checksum from either source."
        'err_unverified'      = "Refusing to install an unverified binary."
        'err_checksum_empty'  = "Checksum file is empty or malformed."
        'err_sha_mismatch'    = "SHA256 mismatch."
        'err_sha_expected'    = "  expected: {0}"
        'err_sha_got'         = "  got:      {0}"
        'err_install_failed'  = "Could not install to {0} ({1})"
    }
    'es' = @{
        'downloading'         = "Descargando Anvil para {0} {1}…"
        'verifying'           = "Verificando firma…"
        'installing'          = "Instalando en {0}…"
        'installed'           = "Instalado: {0}"
        'run_anvil'           = "Ejecute: anvil"
        'installed_tty_hint'  = "Instalado: {0}. Ejecute ``anvil`` desde una terminal para completar la configuración."
        'path_hint'           = "{0} añadido a la PATH del usuario. Reinicie su terminal para que las nuevas shells lo vean."
        'err_win_version'     = "Se requiere Windows 10 o posterior (detectado: {0}.{1})"
        'err_arch_unsupported' = "Solo se admite Windows x86_64 (64 bits) (detectado: {0})"
        'err_source_only'     = "Compile desde el código fuente: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "No se puede crear el directorio de instalación: {0} ({1})"
        'err_download_failed' = "Descarga fallida: {0}"
        'err_no_checksum'     = "No se pudo obtener la suma de verificación de ninguna fuente."
        'err_unverified'      = "Se niega a instalar un binario no verificado."
        'err_checksum_empty'  = "El archivo de suma de verificación está vacío o mal formado."
        'err_sha_mismatch'    = "¡Discrepancia de SHA256!"
        'err_sha_expected'    = "  esperado: {0}"
        'err_sha_got'         = "  obtenido: {0}"
        'err_install_failed'  = "No se pudo instalar en {0} ({1})"
    }
    'zh-CN' = @{
        'downloading'         = "正在下载 Anvil ({0} {1})…"
        'verifying'           = "正在验证签名…"
        'installing'          = "正在安装到 {0}…"
        'installed'           = "已安装: {0}"
        'run_anvil'           = "运行: anvil"
        'installed_tty_hint'  = "已安装: {0}。请从终端运行 ``anvil`` 以完成设置。"
        'path_hint'           = "{0} 已添加到用户 PATH。请重启终端以使新 shell 识别。"
        'err_win_version'     = "需要 Windows 10 或更高版本 (检测到: {0}.{1})"
        'err_arch_unsupported' = "仅支持 x86_64 (64 位) Windows (检测到: {0})"
        'err_source_only'     = "请从源码编译: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "无法创建安装目录: {0} ({1})"
        'err_download_failed' = "下载失败: {0}"
        'err_no_checksum'     = "无法从任何源获取校验和。"
        'err_unverified'      = "拒绝安装未经验证的二进制文件。"
        'err_checksum_empty'  = "校验和文件为空或格式错误。"
        'err_sha_mismatch'    = "SHA256 不匹配!"
        'err_sha_expected'    = "  期望: {0}"
        'err_sha_got'         = "  实际: {0}"
        'err_install_failed'  = "无法安装到 {0} ({1})"
    }
    'fr' = @{
        'downloading'         = "Téléchargement d'Anvil pour {0} {1}…"
        'verifying'           = "Vérification de la signature…"
        'installing'          = "Installation vers {0}…"
        'installed'           = "Installé: {0}"
        'run_anvil'           = "Exécutez: anvil"
        'installed_tty_hint'  = "Installé: {0}. Lancez ``anvil`` depuis un terminal pour finaliser la configuration."
        'path_hint'           = "{0} ajouté au PATH utilisateur. Redémarrez votre terminal pour que les nouveaux shells le voient."
        'err_win_version'     = "Windows 10 ou supérieur est requis (détecté: {0}.{1})"
        'err_arch_unsupported' = "Seul Windows x86_64 (64 bits) est pris en charge (détecté: {0})"
        'err_source_only'     = "Compilez depuis les sources: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Impossible de créer le répertoire d'installation: {0} ({1})"
        'err_download_failed' = "Échec du téléchargement: {0}"
        'err_no_checksum'     = "Impossible de récupérer la somme de contrôle depuis aucune source."
        'err_unverified'      = "Refus d'installer un binaire non vérifié."
        'err_checksum_empty'  = "Le fichier de somme de contrôle est vide ou mal formé."
        'err_sha_mismatch'    = "Incohérence SHA256!"
        'err_sha_expected'    = "  attendu: {0}"
        'err_sha_got'         = "  obtenu:  {0}"
        'err_install_failed'  = "Impossible d'installer vers {0} ({1})"
    }
    'pt-BR' = @{
        'downloading'         = "Baixando Anvil para {0} {1}…"
        'verifying'           = "Verificando assinatura…"
        'installing'          = "Instalando em {0}…"
        'installed'           = "Instalado: {0}"
        'run_anvil'           = "Execute: anvil"
        'installed_tty_hint'  = "Instalado: {0}. Execute ``anvil`` em um terminal para concluir a configuração."
        'path_hint'           = "{0} adicionado ao PATH do usuário. Reinicie seu terminal para que novos shells o vejam."
        'err_win_version'     = "Windows 10 ou posterior é necessário (detectado: {0}.{1})"
        'err_arch_unsupported' = "Apenas Windows x86_64 (64 bits) é suportado (detectado: {0})"
        'err_source_only'     = "Compile do código-fonte: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Não foi possível criar o diretório de instalação: {0} ({1})"
        'err_download_failed' = "Falha no download: {0}"
        'err_no_checksum'     = "Não foi possível obter a soma de verificação de nenhuma fonte."
        'err_unverified'      = "Recusando instalar um binário não verificado."
        'err_checksum_empty'  = "Arquivo de soma de verificação vazio ou mal formado."
        'err_sha_mismatch'    = "Incompatibilidade SHA256!"
        'err_sha_expected'    = "  esperado: {0}"
        'err_sha_got'         = "  obtido:   {0}"
        'err_install_failed'  = "Não foi possível instalar em {0} ({1})"
    }
    'ru' = @{
        'downloading'         = "Загрузка Anvil для {0} {1}…"
        'verifying'           = "Проверка подписи…"
        'installing'          = "Установка в {0}…"
        'installed'           = "Установлено: {0}"
        'run_anvil'           = "Запустите: anvil"
        'installed_tty_hint'  = "Установлено: {0}. Запустите ``anvil`` из терминала для завершения настройки."
        'path_hint'           = "{0} добавлено в пользовательский PATH. Перезапустите терминал, чтобы новые оболочки это увидели."
        'err_win_version'     = "Требуется Windows 10 или новее (обнаружено: {0}.{1})"
        'err_arch_unsupported' = "Поддерживается только x86_64 (64-битная) Windows (обнаружено: {0})"
        'err_source_only'     = "Соберите из исходников: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Не удалось создать каталог установки: {0} ({1})"
        'err_download_failed' = "Сбой загрузки: {0}"
        'err_no_checksum'     = "Не удалось получить контрольную сумму ни из одного источника."
        'err_unverified'      = "Отказ от установки непроверенного бинарного файла."
        'err_checksum_empty'  = "Файл контрольной суммы пуст или повреждён."
        'err_sha_mismatch'    = "Несоответствие SHA256!"
        'err_sha_expected'    = "  ожидалось: {0}"
        'err_sha_got'         = "  получено:  {0}"
        'err_install_failed'  = "Не удалось установить в {0} ({1})"
    }
    'ja' = @{
        'downloading'         = "Anvil をダウンロード中 ({0} {1})…"
        'verifying'           = "署名を検証中…"
        'installing'          = "{0} にインストール中…"
        'installed'           = "インストール完了: {0}"
        'run_anvil'           = "実行: anvil"
        'installed_tty_hint'  = "インストール完了: {0}。セットアップを完了するにはターミナルから ``anvil`` を実行してください。"
        'path_hint'           = "{0} をユーザー PATH に追加しました。新しいシェルで認識させるにはターミナルを再起動してください。"
        'err_win_version'     = "Windows 10 以降が必要です (検出: {0}.{1})"
        'err_arch_unsupported' = "x86_64 (64ビット) Windows のみサポートされています (検出: {0})"
        'err_source_only'     = "ソースからビルド: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "インストールディレクトリを作成できません: {0} ({1})"
        'err_download_failed' = "ダウンロード失敗: {0}"
        'err_no_checksum'     = "どちらのソースからもチェックサムを取得できませんでした。"
        'err_unverified'      = "未検証のバイナリのインストールを拒否します。"
        'err_checksum_empty'  = "チェックサムファイルが空または不正です。"
        'err_sha_mismatch'    = "SHA256 が一致しません!"
        'err_sha_expected'    = "  期待値: {0}"
        'err_sha_got'         = "  実値:   {0}"
        'err_install_failed'  = "{0} にインストールできませんでした ({1})"
    }
    'de' = @{
        'downloading'         = "Lade Anvil für {0} {1} herunter…"
        'verifying'           = "Überprüfe Signatur…"
        'installing'          = "Installiere nach {0}…"
        'installed'           = "Installiert: {0}"
        'run_anvil'           = "Ausführen: anvil"
        'installed_tty_hint'  = "Installiert: {0}. Führen Sie ``anvil`` von einem Terminal aus, um die Einrichtung abzuschließen."
        'path_hint'           = "{0} wurde zum Benutzer-PATH hinzugefügt. Starten Sie Ihr Terminal neu, damit neue Shells es erkennen."
        'err_win_version'     = "Windows 10 oder höher erforderlich (erkannt: {0}.{1})"
        'err_arch_unsupported' = "Nur x86_64 (64-Bit) Windows wird unterstützt (erkannt: {0})"
        'err_source_only'     = "Aus dem Quellcode bauen: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Installationsverzeichnis kann nicht erstellt werden: {0} ({1})"
        'err_download_failed' = "Download fehlgeschlagen: {0}"
        'err_no_checksum'     = "Prüfsumme konnte von keiner Quelle abgerufen werden."
        'err_unverified'      = "Verweigere die Installation eines unverifizierten Binarys."
        'err_checksum_empty'  = "Prüfsummendatei ist leer oder fehlerhaft."
        'err_sha_mismatch'    = "SHA256-Konflikt!"
        'err_sha_expected'    = "  erwartet: {0}"
        'err_sha_got'         = "  erhalten: {0}"
        'err_install_failed'  = "Installation nach {0} nicht möglich ({1})"
    }
    'ko' = @{
        'downloading'         = "Anvil 다운로드 중 ({0} {1})…"
        'verifying'           = "서명 검증 중…"
        'installing'          = "{0}에 설치 중…"
        'installed'           = "설치됨: {0}"
        'run_anvil'           = "실행: anvil"
        'installed_tty_hint'  = "설치됨: {0}. 설정을 완료하려면 터미널에서 ``anvil``을 실행하세요."
        'path_hint'           = "{0}이(가) 사용자 PATH에 추가되었습니다. 새 셸에서 인식하려면 터미널을 다시 시작하세요."
        'err_win_version'     = "Windows 10 이상이 필요합니다 (감지됨: {0}.{1})"
        'err_arch_unsupported' = "x86_64 (64비트) Windows만 지원됩니다 (감지됨: {0})"
        'err_source_only'     = "소스에서 빌드: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "설치 디렉터리를 만들 수 없습니다: {0} ({1})"
        'err_download_failed' = "다운로드 실패: {0}"
        'err_no_checksum'     = "어느 소스에서도 체크섬을 가져올 수 없습니다."
        'err_unverified'      = "검증되지 않은 바이너리 설치를 거부합니다."
        'err_checksum_empty'  = "체크섬 파일이 비어 있거나 잘못되었습니다."
        'err_sha_mismatch'    = "SHA256 불일치!"
        'err_sha_expected'    = "  예상: {0}"
        'err_sha_got'         = "  실제: {0}"
        'err_install_failed'  = "{0}에 설치할 수 없습니다 ({1})"
    }
    'it' = @{
        'downloading'         = "Download di Anvil per {0} {1}…"
        'verifying'           = "Verifica della firma…"
        'installing'          = "Installazione in {0}…"
        'installed'           = "Installato: {0}"
        'run_anvil'           = "Esegui: anvil"
        'installed_tty_hint'  = "Installato: {0}. Esegui ``anvil`` da un terminale per completare la configurazione."
        'path_hint'           = "{0} aggiunto al PATH utente. Riavvia il terminale per renderlo visibile alle nuove shell."
        'err_win_version'     = "Windows 10 o successivo è richiesto (rilevato: {0}.{1})"
        'err_arch_unsupported' = "Solo Windows x86_64 (64 bit) è supportato (rilevato: {0})"
        'err_source_only'     = "Compila dai sorgenti: cargo install --git https://github.com/culpur/anvil-source"
        'err_install_dir'     = "Impossibile creare la directory di installazione: {0} ({1})"
        'err_download_failed' = "Download fallito: {0}"
        'err_no_checksum'     = "Impossibile recuperare il checksum da nessuna fonte."
        'err_unverified'      = "Rifiuto di installare un binario non verificato."
        'err_checksum_empty'  = "File di checksum vuoto o malformato."
        'err_sha_mismatch'    = "Mismatch SHA256!"
        'err_sha_expected'    = "  atteso:    {0}"
        'err_sha_got'         = "  ottenuto:  {0}"
        'err_install_failed'  = "Impossibile installare in {0} ({1})"
    }
}

function T {
    param([string]$Key)
    $args2 = $args
    $table = $Strings[$LangCode]
    if (-not $table -or -not $table.ContainsKey($Key)) {
        # Fall back to English
        $table = $Strings['en']
    }
    $tpl = $table[$Key]
    if ($args2.Count -eq 0) { return $tpl }
    return $tpl -f $args2
}

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
    Write-Fail (T 'err_win_version' $WinVer.Major $WinVer.Minor)
    exit 3
}

# Only x86_64 binaries are produced for Windows today (matches install.sh
# arch matrix: aarch64-pc-windows-msvc is source-only).
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($Arch -ne 'X64') {
    Write-Fail (T 'err_arch_unsupported' $Arch)
    Write-Fail (T 'err_source_only')
    exit 3
}
$Target = 'x86_64-pc-windows-msvc'

# ── Install directory ─────────────────────────────────────────────────────────
if (-not (Test-Path $InstallDir)) {
    try {
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    } catch {
        Write-Fail (T 'err_install_dir' $InstallDir $_)
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

Write-Step (T 'downloading' 'windows' 'x86_64')
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
    Write-Fail (T 'err_download_failed' $_)
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 1
}

# ── SHA256 verification (mandatory) ───────────────────────────────────────────
# Integrity is non-negotiable.  If checksum fetch fails on BOTH sources
# we abort — we never install an unverified binary.  Previously a
# network error in the checksum fetch silently skipped verification,
# which meant an attacker blocking the .sha256 URL would bypass the
# whole integrity check (regression closed for parity with install.sh).
Write-Step (T 'verifying')
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
        Write-Fail (T 'err_no_checksum')
        Write-Fail (T 'err_unverified')
        Write-Fail "primary:  $Sha256UrlPrimary"
        Write-Fail "fallback: $Sha256UrlFallback"
        Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
        exit 2
    }
}

$Expected = (Get-Content $TmpSha256 -Raw).Trim().Split()[0].ToLower()
if ([string]::IsNullOrWhiteSpace($Expected)) {
    Write-Fail (T 'err_checksum_empty')
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 2
}

$Actual = (Get-FileHash -Path $TmpBinary -Algorithm SHA256).Hash.ToLower()
if ($Actual -ne $Expected) {
    Write-Fail (T 'err_sha_mismatch')
    Write-Fail (T 'err_sha_expected' $Expected)
    Write-Fail (T 'err_sha_got' $Actual)
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 2
}

# ── Install binary ────────────────────────────────────────────────────────────
$DestExe = Join-Path $InstallDir "anvil.exe"
Write-Step (T 'installing' $DestExe)
try {
    Copy-Item -Path $TmpBinary -Destination $DestExe -Force
} catch {
    Write-Fail (T 'err_install_failed' $DestExe $_)
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    exit 3
}
Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue

# ── anvild sibling (task #766) ────────────────────────────────────────────────
# Hardlink anvild.exe next to anvil.exe so Task Manager / tasklist show the
# daemon under a distinct process name. The Scheduled Task XML written by
# `anvil daemon install-service` references the anvild path; Task Scheduler
# execs it, argv[0] propagates, and the daemon naturally identifies as anvild.
# Hardlink (no admin needed for same-volume NTFS), fallback to Copy-Item.
$AnvildExe = Join-Path $InstallDir "anvild.exe"
try {
    if (Test-Path $AnvildExe) { Remove-Item -Force $AnvildExe -ErrorAction SilentlyContinue }
    New-Item -ItemType HardLink -Path $AnvildExe -Target $DestExe -Force -ErrorAction Stop | Out-Null
} catch {
    try { Copy-Item -Path $DestExe -Destination $AnvildExe -Force } catch {
        # Best-effort. If we can't create anvild.exe, the daemon will still
        # run via anvil.exe — just with a less distinct process name.
        if ($IsVerbose) { Write-Host "  anvild link skipped: $_" -ForegroundColor DarkGray }
    }
}

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
    $PathHint = (T 'path_hint' $InstallDir)
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
            Write-Host (T 'installed_tty_hint' $DestExe)
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
    Write-Host (T 'installed' $DestExe)
    Write-Host (T 'run_anvil')
}
