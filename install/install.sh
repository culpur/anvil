#!/usr/bin/env bash
# Anvil installer — macOS / Linux / BSD
# Usage:  curl -fsSL https://anvilhub.culpur.net/install.sh | bash
#   or:   bash install.sh [--no-setup] [--no-completions] [--dir /usr/local/bin] [--verbose] [--quiet]
#
# Design: silent by default. The installer is plumbing — Anvil's first-run
# wizard is the UX. We download, SHA256-verify, put the binary on PATH, and
# exec the wizard. No banners, no warnings about optional tools the wizard
# can guide the user to itself.
#
# Headless fallback (v2.2.18 #663 gap 14): the first-run wizard requires a
# TTY (it enters an alt-screen).  When stdin/stdout aren't both terminals
# (CI bootstrap, ssh-without-pty, container provisioning, pipe-fed
# shells), launching it would crash on `enter_alternate_screen()`.  We
# detect non-TTY and print a one-line "run anvil from a TTY" hint instead
# of exec'ing.
#
# Exit codes:
#   0 — success
#   1 — network failure
#   2 — checksum failure
#   3 — unsupported platform / permission error
#   4 — user declined

set -euo pipefail

# ── Locale detection + translation (task #751) ────────────────────────────────
# Detect the user's locale from $LC_ALL > $LC_MESSAGES > $LANG, normalize it
# (fr_FR.UTF-8 → fr-FR → fr if base is supported), and look up status strings
# via t() with English fallback. The installer stays silent-by-default (#660
# design); i18n only wraps the few user-visible lines (progress steps, die
# messages, headless hint, final summary). Cargo/curl/sudo tool output is
# never translated.
#
# Supported (Tier-1 priority): en, es, zh-CN, fr, pt-BR, ru, ja, de, ko, it.
SUPPORTED_LANGS="en es zh-CN fr pt-BR ru ja de ko it"

detect_lang() {
    local raw="" src=""
    if [[ -n "${LC_ALL:-}" ]]; then
        raw="$LC_ALL"; src="LC_ALL"
    elif [[ -n "${LC_MESSAGES:-}" ]]; then
        raw="$LC_MESSAGES"; src="LC_MESSAGES"
    elif [[ -n "${LANG:-}" ]]; then
        raw="$LANG"; src="LANG"
    else
        echo "en|default"; return
    fi
    local code="${raw%%.*}"     # strip .UTF-8
    code="${code%%@*}"          # strip @modifier
    code="${code//_/-}"         # fr_FR → fr-FR
    if [[ "$code" == "C" || "$code" == "POSIX" || -z "$code" ]]; then
        echo "en|$src"; return
    fi
    for cand in $SUPPORTED_LANGS; do
        [[ "$cand" == "$code" ]] && { echo "$code|$src"; return; }
    done
    local base="${code%%-*}"
    for cand in $SUPPORTED_LANGS; do
        [[ "$cand" == "$base" ]] && { echo "$base|$src"; return; }
    done
    # Unsupported — fall back to English but annotate the source for diagnostics.
    echo "en|$src(unsupported:$code)"
}

_LANG_DETECT_RAW="$(detect_lang)"
LANG_CODE="${_LANG_DETECT_RAW%%|*}"
LANG_SRC="${_LANG_DETECT_RAW##*|}"

# Translation lookup. t <key> [printf args...]
# Format: LANG:key|MSG in a case statement, *:key) fallback to English.
t() {
    local key="$1"; shift
    local msg=""
    case "${LANG_CODE}:${key}" in
        # ─── English (default) ───────────────────────────────────────────
        en:downloading)         msg="Downloading Anvil for %s %s…" ;;
        en:verifying)           msg="Verifying signature…" ;;
        en:installing)          msg="Installing to %s…" ;;
        en:installed)           msg="Installed: %s" ;;
        en:run_anvil)           msg="Run: anvil" ;;
        en:installed_tty_hint)  msg="Installed: %s. Run \`anvil\` from a TTY to complete setup." ;;
        en:sudo_required)       msg="Installing to %s requires sudo:" ;;
        en:path_hint)           msg="%s is not on your PATH. Add this to your shell rc:" ;;
        en:err_bash_required)   msg="this installer requires bash." ;;
        en:err_unsupported_os)  msg="Unsupported OS: %s" ;;
        en:err_unsupported_arch) msg="Unsupported architecture: %s" ;;
        en:err_source_only)     msg="%s binary not available — build from source: cargo install --git https://github.com/culpur/anvil-source" ;;
        en:err_curl_required)   msg="curl is required but not installed. Install curl, then re-run." ;;
        en:err_download_failed) msg="Download failed: %s" ;;
        en:err_no_checksum)     msg="Could not fetch checksum. Refusing to install an unverified binary." ;;
        en:err_checksum_empty)  msg="Checksum file is empty or malformed." ;;
        en:err_no_sha_tool)     msg="No SHA256 tool found (need one of: sha256sum, shasum, sha256, cksum)." ;;
        en:err_sha_mismatch)    msg="SHA256 mismatch. expected=%s got=%s" ;;
        en:err_install_failed)  msg="Could not install to %s" ;;
        en:err_exec_failed)     msg="Installed anvil to %s but cannot execute it." ;;
        en:lang_detected)       msg="Language: %s (detected from %s)" ;;
        en:platform)            msg="Platform: %s / %s  (target: %s)" ;;
        en:install_dir)         msg="Install directory: %s" ;;
        en:dry_run_banner)      msg="DRY-RUN MODE — no files will be changed." ;;

        # ─── Español ─────────────────────────────────────────────────────
        es:downloading)         msg="Descargando Anvil para %s %s…" ;;
        es:verifying)           msg="Verificando firma…" ;;
        es:installing)          msg="Instalando en %s…" ;;
        es:installed)           msg="Instalado: %s" ;;
        es:run_anvil)           msg="Ejecute: anvil" ;;
        es:installed_tty_hint)  msg="Instalado: %s. Ejecute \`anvil\` desde una TTY para completar la configuración." ;;
        es:sudo_required)       msg="La instalación en %s requiere sudo:" ;;
        es:path_hint)           msg="%s no está en su PATH. Añada esto a su rc de shell:" ;;
        es:err_bash_required)   msg="este instalador requiere bash." ;;
        es:err_unsupported_os)  msg="Sistema operativo no compatible: %s" ;;
        es:err_unsupported_arch) msg="Arquitectura no compatible: %s" ;;
        es:err_source_only)     msg="Binario %s no disponible — compile desde el código fuente: cargo install --git https://github.com/culpur/anvil-source" ;;
        es:err_curl_required)   msg="curl es necesario pero no está instalado. Instale curl y vuelva a ejecutar." ;;
        es:err_download_failed) msg="Descarga fallida: %s" ;;
        es:err_no_checksum)     msg="No se pudo obtener la suma de verificación. Se niega a instalar un binario no verificado." ;;
        es:err_checksum_empty)  msg="El archivo de suma de verificación está vacío o mal formado." ;;
        es:err_no_sha_tool)     msg="Sin herramienta SHA256 (se necesita una de: sha256sum, shasum, sha256, cksum)." ;;
        es:err_sha_mismatch)    msg="Discrepancia de SHA256. esperado=%s obtenido=%s" ;;
        es:err_install_failed)  msg="No se pudo instalar en %s" ;;
        es:err_exec_failed)     msg="Anvil instalado en %s pero no se puede ejecutar." ;;
        es:lang_detected)       msg="Idioma: %s (detectado desde %s)" ;;
        es:platform)            msg="Plataforma: %s / %s  (destino: %s)" ;;
        es:install_dir)         msg="Directorio de instalación: %s" ;;
        es:dry_run_banner)      msg="MODO DE PRUEBA — no se modificarán archivos." ;;

        # ─── 简体中文 (zh-CN) ────────────────────────────────────────────
        zh-CN:downloading)      msg="正在下载 Anvil (%s %s)…" ;;
        zh-CN:verifying)        msg="正在验证签名…" ;;
        zh-CN:installing)       msg="正在安装到 %s…" ;;
        zh-CN:installed)        msg="已安装: %s" ;;
        zh-CN:run_anvil)        msg="运行: anvil" ;;
        zh-CN:installed_tty_hint) msg="已安装: %s。请从 TTY 运行 \`anvil\` 以完成设置。" ;;
        zh-CN:sudo_required)    msg="安装到 %s 需要 sudo:" ;;
        zh-CN:path_hint)        msg="%s 不在您的 PATH 中。请将此添加到您的 shell rc:" ;;
        zh-CN:err_bash_required) msg="此安装程序需要 bash。" ;;
        zh-CN:err_unsupported_os) msg="不支持的操作系统: %s" ;;
        zh-CN:err_unsupported_arch) msg="不支持的架构: %s" ;;
        zh-CN:err_source_only)  msg="%s 二进制不可用 — 请从源码编译: cargo install --git https://github.com/culpur/anvil-source" ;;
        zh-CN:err_curl_required) msg="需要 curl 但未安装。请安装 curl 后重新运行。" ;;
        zh-CN:err_download_failed) msg="下载失败: %s" ;;
        zh-CN:err_no_checksum)  msg="无法获取校验和。拒绝安装未验证的二进制文件。" ;;
        zh-CN:err_checksum_empty) msg="校验和文件为空或格式错误。" ;;
        zh-CN:err_no_sha_tool)  msg="未找到 SHA256 工具 (需要: sha256sum、shasum、sha256 或 cksum 之一)。" ;;
        zh-CN:err_sha_mismatch) msg="SHA256 不匹配。期望=%s 实际=%s" ;;
        zh-CN:err_install_failed) msg="无法安装到 %s" ;;
        zh-CN:err_exec_failed)  msg="Anvil 已安装到 %s 但无法执行。" ;;
        zh-CN:lang_detected)    msg="语言: %s (从 %s 检测)" ;;
        zh-CN:platform)         msg="平台: %s / %s  (目标: %s)" ;;
        zh-CN:install_dir)      msg="安装目录: %s" ;;
        zh-CN:dry_run_banner)   msg="演练模式 — 不会更改任何文件。" ;;

        # ─── Français ────────────────────────────────────────────────────
        fr:downloading)         msg="Téléchargement d'Anvil pour %s %s…" ;;
        fr:verifying)           msg="Vérification de la signature…" ;;
        fr:installing)          msg="Installation vers %s…" ;;
        fr:installed)           msg="Installé: %s" ;;
        fr:run_anvil)           msg="Exécutez: anvil" ;;
        fr:installed_tty_hint)  msg="Installé: %s. Lancez \`anvil\` depuis un TTY pour finaliser la configuration." ;;
        fr:sudo_required)       msg="L'installation vers %s nécessite sudo:" ;;
        fr:path_hint)           msg="%s n'est pas dans votre PATH. Ajoutez ceci à votre rc shell:" ;;
        fr:err_bash_required)   msg="cet installateur nécessite bash." ;;
        fr:err_unsupported_os)  msg="Système d'exploitation non pris en charge: %s" ;;
        fr:err_unsupported_arch) msg="Architecture non prise en charge: %s" ;;
        fr:err_source_only)     msg="Binaire %s non disponible — compilez depuis les sources: cargo install --git https://github.com/culpur/anvil-source" ;;
        fr:err_curl_required)   msg="curl est requis mais non installé. Installez curl, puis relancez." ;;
        fr:err_download_failed) msg="Échec du téléchargement: %s" ;;
        fr:err_no_checksum)     msg="Impossible de récupérer la somme de contrôle. Refus d'installer un binaire non vérifié." ;;
        fr:err_checksum_empty)  msg="Le fichier de somme de contrôle est vide ou mal formé." ;;
        fr:err_no_sha_tool)     msg="Aucun outil SHA256 trouvé (besoin de: sha256sum, shasum, sha256, ou cksum)." ;;
        fr:err_sha_mismatch)    msg="Incohérence SHA256. attendu=%s obtenu=%s" ;;
        fr:err_install_failed)  msg="Impossible d'installer vers %s" ;;
        fr:err_exec_failed)     msg="Anvil installé dans %s mais ne peut pas être exécuté." ;;
        fr:lang_detected)       msg="Langue: %s (détectée depuis %s)" ;;
        fr:platform)            msg="Plateforme: %s / %s  (cible: %s)" ;;
        fr:install_dir)         msg="Répertoire d'installation: %s" ;;
        fr:dry_run_banner)      msg="MODE SIMULATION — aucun fichier ne sera modifié." ;;

        # ─── Português (pt-BR) ───────────────────────────────────────────
        pt-BR:downloading)      msg="Baixando Anvil para %s %s…" ;;
        pt-BR:verifying)        msg="Verificando assinatura…" ;;
        pt-BR:installing)       msg="Instalando em %s…" ;;
        pt-BR:installed)        msg="Instalado: %s" ;;
        pt-BR:run_anvil)        msg="Execute: anvil" ;;
        pt-BR:installed_tty_hint) msg="Instalado: %s. Execute \`anvil\` em um TTY para concluir a configuração." ;;
        pt-BR:sudo_required)    msg="A instalação em %s requer sudo:" ;;
        pt-BR:path_hint)        msg="%s não está no seu PATH. Adicione isto ao rc do shell:" ;;
        pt-BR:err_bash_required) msg="este instalador requer bash." ;;
        pt-BR:err_unsupported_os) msg="Sistema operacional não suportado: %s" ;;
        pt-BR:err_unsupported_arch) msg="Arquitetura não suportada: %s" ;;
        pt-BR:err_source_only)  msg="Binário %s não disponível — compile do código-fonte: cargo install --git https://github.com/culpur/anvil-source" ;;
        pt-BR:err_curl_required) msg="curl é necessário mas não está instalado. Instale curl e tente novamente." ;;
        pt-BR:err_download_failed) msg="Falha no download: %s" ;;
        pt-BR:err_no_checksum)  msg="Não foi possível obter a soma de verificação. Recusando instalar binário não verificado." ;;
        pt-BR:err_checksum_empty) msg="Arquivo de soma de verificação vazio ou mal formado." ;;
        pt-BR:err_no_sha_tool)  msg="Nenhuma ferramenta SHA256 encontrada (precisa de: sha256sum, shasum, sha256 ou cksum)." ;;
        pt-BR:err_sha_mismatch) msg="Incompatibilidade SHA256. esperado=%s obtido=%s" ;;
        pt-BR:err_install_failed) msg="Não foi possível instalar em %s" ;;
        pt-BR:err_exec_failed)  msg="Anvil instalado em %s mas não pode ser executado." ;;

        # ─── Русский ─────────────────────────────────────────────────────
        ru:downloading)         msg="Загрузка Anvil для %s %s…" ;;
        ru:verifying)           msg="Проверка подписи…" ;;
        ru:installing)          msg="Установка в %s…" ;;
        ru:installed)           msg="Установлено: %s" ;;
        ru:run_anvil)           msg="Запустите: anvil" ;;
        ru:installed_tty_hint)  msg="Установлено: %s. Запустите \`anvil\` из TTY для завершения настройки." ;;
        ru:sudo_required)       msg="Установка в %s требует sudo:" ;;
        ru:path_hint)           msg="%s не находится в вашем PATH. Добавьте это в rc-файл оболочки:" ;;
        ru:err_bash_required)   msg="этому установщику требуется bash." ;;
        ru:err_unsupported_os)  msg="Неподдерживаемая ОС: %s" ;;
        ru:err_unsupported_arch) msg="Неподдерживаемая архитектура: %s" ;;
        ru:err_source_only)     msg="Бинарный файл %s недоступен — соберите из исходников: cargo install --git https://github.com/culpur/anvil-source" ;;
        ru:err_curl_required)   msg="требуется curl, но он не установлен. Установите curl и повторите." ;;
        ru:err_download_failed) msg="Сбой загрузки: %s" ;;
        ru:err_no_checksum)     msg="Не удалось получить контрольную сумму. Отказ от установки непроверенного бинарного файла." ;;
        ru:err_checksum_empty)  msg="Файл контрольной суммы пуст или повреждён." ;;
        ru:err_no_sha_tool)     msg="Инструмент SHA256 не найден (требуется: sha256sum, shasum, sha256 или cksum)." ;;
        ru:err_sha_mismatch)    msg="Несоответствие SHA256. ожидалось=%s получено=%s" ;;
        ru:err_install_failed)  msg="Не удалось установить в %s" ;;
        ru:err_exec_failed)     msg="Anvil установлен в %s, но не может быть запущен." ;;

        # ─── 日本語 (ja) ─────────────────────────────────────────────────
        ja:downloading)         msg="Anvil をダウンロード中 (%s %s)…" ;;
        ja:verifying)           msg="署名を検証中…" ;;
        ja:installing)          msg="%s にインストール中…" ;;
        ja:installed)           msg="インストール完了: %s" ;;
        ja:run_anvil)           msg="実行: anvil" ;;
        ja:installed_tty_hint)  msg="インストール完了: %s。セットアップを完了するには TTY から \`anvil\` を実行してください。" ;;
        ja:sudo_required)       msg="%s へのインストールには sudo が必要です:" ;;
        ja:path_hint)           msg="%s は PATH に含まれていません。シェル rc に以下を追加してください:" ;;
        ja:err_bash_required)   msg="このインストーラには bash が必要です。" ;;
        ja:err_unsupported_os)  msg="サポートされていない OS: %s" ;;
        ja:err_unsupported_arch) msg="サポートされていないアーキテクチャ: %s" ;;
        ja:err_source_only)     msg="%s バイナリはありません — ソースからビルド: cargo install --git https://github.com/culpur/anvil-source" ;;
        ja:err_curl_required)   msg="curl が必要ですがインストールされていません。curl をインストールして再実行してください。" ;;
        ja:err_download_failed) msg="ダウンロード失敗: %s" ;;
        ja:err_no_checksum)     msg="チェックサムを取得できませんでした。未検証のバイナリのインストールを拒否します。" ;;
        ja:err_checksum_empty)  msg="チェックサムファイルが空または不正です。" ;;
        ja:err_no_sha_tool)     msg="SHA256 ツールが見つかりません (sha256sum / shasum / sha256 / cksum のいずれかが必要)。" ;;
        ja:err_sha_mismatch)    msg="SHA256 が一致しません。期待値=%s 実値=%s" ;;
        ja:err_install_failed)  msg="%s にインストールできませんでした" ;;
        ja:err_exec_failed)     msg="Anvil を %s にインストールしましたが実行できません。" ;;
        ja:lang_detected)       msg="言語: %s (%s から検出)" ;;
        ja:platform)            msg="プラットフォーム: %s / %s  (ターゲット: %s)" ;;
        ja:install_dir)         msg="インストール先ディレクトリ: %s" ;;
        ja:dry_run_banner)      msg="ドライランモード — ファイルは変更されません。" ;;

        # ─── Deutsch ─────────────────────────────────────────────────────
        de:downloading)         msg="Lade Anvil für %s %s herunter…" ;;
        de:verifying)           msg="Überprüfe Signatur…" ;;
        de:installing)          msg="Installiere nach %s…" ;;
        de:installed)           msg="Installiert: %s" ;;
        de:run_anvil)           msg="Ausführen: anvil" ;;
        de:installed_tty_hint)  msg="Installiert: %s. Führen Sie \`anvil\` von einem TTY aus, um die Einrichtung abzuschließen." ;;
        de:sudo_required)       msg="Installation nach %s erfordert sudo:" ;;
        de:path_hint)           msg="%s ist nicht in Ihrem PATH. Fügen Sie dies zu Ihrer Shell-rc-Datei hinzu:" ;;
        de:err_bash_required)   msg="dieser Installer benötigt bash." ;;
        de:err_unsupported_os)  msg="Nicht unterstütztes Betriebssystem: %s" ;;
        de:err_unsupported_arch) msg="Nicht unterstützte Architektur: %s" ;;
        de:err_source_only)     msg="%s-Binary nicht verfügbar — aus dem Quellcode bauen: cargo install --git https://github.com/culpur/anvil-source" ;;
        de:err_curl_required)   msg="curl ist erforderlich, aber nicht installiert. Installieren Sie curl und führen Sie erneut aus." ;;
        de:err_download_failed) msg="Download fehlgeschlagen: %s" ;;
        de:err_no_checksum)     msg="Prüfsumme konnte nicht abgerufen werden. Verweigere Installation eines unverifizierten Binarys." ;;
        de:err_checksum_empty)  msg="Prüfsummendatei ist leer oder fehlerhaft." ;;
        de:err_no_sha_tool)     msg="Kein SHA256-Tool gefunden (benötigt eines von: sha256sum, shasum, sha256, cksum)." ;;
        de:err_sha_mismatch)    msg="SHA256-Konflikt. erwartet=%s erhalten=%s" ;;
        de:err_install_failed)  msg="Installation nach %s nicht möglich" ;;
        de:err_exec_failed)     msg="Anvil nach %s installiert, kann aber nicht ausgeführt werden." ;;
        de:lang_detected)       msg="Sprache: %s (erkannt aus %s)" ;;
        de:platform)            msg="Plattform: %s / %s  (Ziel: %s)" ;;
        de:install_dir)         msg="Installationsverzeichnis: %s" ;;
        de:dry_run_banner)      msg="TROCKENLAUF — keine Dateien werden geändert." ;;

        # ─── 한국어 (ko) ─────────────────────────────────────────────────
        ko:downloading)         msg="Anvil 다운로드 중 (%s %s)…" ;;
        ko:verifying)           msg="서명 검증 중…" ;;
        ko:installing)          msg="%s에 설치 중…" ;;
        ko:installed)           msg="설치됨: %s" ;;
        ko:run_anvil)           msg="실행: anvil" ;;
        ko:installed_tty_hint)  msg="설치됨: %s. 설정을 완료하려면 TTY에서 \`anvil\`을 실행하세요." ;;
        ko:sudo_required)       msg="%s에 설치하려면 sudo가 필요합니다:" ;;
        ko:path_hint)           msg="%s가 PATH에 없습니다. 셸 rc에 다음을 추가하세요:" ;;
        ko:err_bash_required)   msg="이 설치 프로그램에는 bash가 필요합니다." ;;
        ko:err_unsupported_os)  msg="지원되지 않는 OS: %s" ;;
        ko:err_unsupported_arch) msg="지원되지 않는 아키텍처: %s" ;;
        ko:err_source_only)     msg="%s 바이너리를 사용할 수 없습니다 — 소스에서 빌드: cargo install --git https://github.com/culpur/anvil-source" ;;
        ko:err_curl_required)   msg="curl이 필요하지만 설치되어 있지 않습니다. curl을 설치한 후 다시 실행하세요." ;;
        ko:err_download_failed) msg="다운로드 실패: %s" ;;
        ko:err_no_checksum)     msg="체크섬을 가져올 수 없습니다. 검증되지 않은 바이너리 설치를 거부합니다." ;;
        ko:err_checksum_empty)  msg="체크섬 파일이 비어 있거나 잘못되었습니다." ;;
        ko:err_no_sha_tool)     msg="SHA256 도구를 찾을 수 없습니다 (sha256sum, shasum, sha256, cksum 중 하나 필요)." ;;
        ko:err_sha_mismatch)    msg="SHA256 불일치. 예상=%s 실제=%s" ;;
        ko:err_install_failed)  msg="%s에 설치할 수 없습니다" ;;
        ko:err_exec_failed)     msg="Anvil이 %s에 설치되었지만 실행할 수 없습니다." ;;

        # ─── Italiano ────────────────────────────────────────────────────
        it:downloading)         msg="Download di Anvil per %s %s…" ;;
        it:verifying)           msg="Verifica della firma…" ;;
        it:installing)          msg="Installazione in %s…" ;;
        it:installed)           msg="Installato: %s" ;;
        it:run_anvil)           msg="Esegui: anvil" ;;
        it:installed_tty_hint)  msg="Installato: %s. Esegui \`anvil\` da un TTY per completare la configurazione." ;;
        it:sudo_required)       msg="L'installazione in %s richiede sudo:" ;;
        it:path_hint)           msg="%s non è nel tuo PATH. Aggiungi questo al rc della shell:" ;;
        it:err_bash_required)   msg="questo installer richiede bash." ;;
        it:err_unsupported_os)  msg="Sistema operativo non supportato: %s" ;;
        it:err_unsupported_arch) msg="Architettura non supportata: %s" ;;
        it:err_source_only)     msg="Binario %s non disponibile — compila dai sorgenti: cargo install --git https://github.com/culpur/anvil-source" ;;
        it:err_curl_required)   msg="curl è richiesto ma non installato. Installa curl e riprova." ;;
        it:err_download_failed) msg="Download fallito: %s" ;;
        it:err_no_checksum)     msg="Impossibile ottenere il checksum. Rifiuto di installare un binario non verificato." ;;
        it:err_checksum_empty)  msg="File di checksum vuoto o malformato." ;;
        it:err_no_sha_tool)     msg="Nessun tool SHA256 trovato (serve uno di: sha256sum, shasum, sha256, cksum)." ;;
        it:err_sha_mismatch)    msg="Mismatch SHA256. atteso=%s ottenuto=%s" ;;
        it:err_install_failed)  msg="Impossibile installare in %s" ;;
        it:err_exec_failed)     msg="Anvil installato in %s ma non può essere eseguito." ;;

        # ─── Fallback: unknown lang falls through to English ─────────────
        *:downloading)          msg="Downloading Anvil for %s %s…" ;;
        *:verifying)            msg="Verifying signature…" ;;
        *:installing)           msg="Installing to %s…" ;;
        *:installed)            msg="Installed: %s" ;;
        *:run_anvil)            msg="Run: anvil" ;;
        *:installed_tty_hint)   msg="Installed: %s. Run \`anvil\` from a TTY to complete setup." ;;
        *:sudo_required)        msg="Installing to %s requires sudo:" ;;
        *:path_hint)            msg="%s is not on your PATH. Add this to your shell rc:" ;;
        *:err_bash_required)    msg="this installer requires bash." ;;
        *:err_unsupported_os)   msg="Unsupported OS: %s" ;;
        *:err_unsupported_arch) msg="Unsupported architecture: %s" ;;
        *:err_source_only)      msg="%s binary not available — build from source: cargo install --git https://github.com/culpur/anvil-source" ;;
        *:err_curl_required)    msg="curl is required but not installed. Install curl, then re-run." ;;
        *:err_download_failed)  msg="Download failed: %s" ;;
        *:err_no_checksum)      msg="Could not fetch checksum. Refusing to install an unverified binary." ;;
        *:err_checksum_empty)   msg="Checksum file is empty or malformed." ;;
        *:err_no_sha_tool)      msg="No SHA256 tool found (need one of: sha256sum, shasum, sha256, cksum)." ;;
        *:err_sha_mismatch)     msg="SHA256 mismatch. expected=%s got=%s" ;;
        *:err_install_failed)   msg="Could not install to %s" ;;
        *:err_exec_failed)      msg="Installed anvil to %s but cannot execute it." ;;
        *:lang_detected)        msg="Language: %s (detected from %s)" ;;
        *:platform)             msg="Platform: %s / %s  (target: %s)" ;;
        *:install_dir)          msg="Install directory: %s" ;;
        *:dry_run_banner)       msg="DRY-RUN MODE — no files will be changed." ;;
    esac
    # shellcheck disable=SC2059
    printf "$msg" "$@"
}

# ── Bash sanity (BSDs often don't ship bash in base) ──────────────────────────
# If someone pipes us into /bin/sh on FreeBSD/NetBSD by accident, fail loud
# with an actionable message instead of mystery `[[`-syntax errors.
if [ -z "${BASH_VERSION:-}" ]; then
    echo "error: this installer requires bash."
    echo "  FreeBSD:  pkg install bash, then re-run"
    echo "  NetBSD:   pkgin install bash, then re-run"
    echo "  Linux:    bash is in every distro's base — check your PATH"
    echo "  macOS:    bash is built in"
    exit 3
fi

# ── Argument parsing ──────────────────────────────────────────────────────────
INSTALL_DIR=""
RUN_SETUP=true
INSTALL_COMPLETIONS=true
VERBOSE=false
QUIET=false
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-setup)        RUN_SETUP=false; shift ;;
        --no-completions)  INSTALL_COMPLETIONS=false; shift ;;
        --dir)             INSTALL_DIR="$2"; shift 2 ;;
        --dir=*)           INSTALL_DIR="${1#--dir=}"; shift ;;
        --verbose|-v)      VERBOSE=true; shift ;;
        --quiet|-q)        QUIET=true; shift ;;
        --dry-run)         DRY_RUN=true; shift ;;
        *)                 shift ;;
    esac
done

# --quiet wins over --verbose if someone passes both: package managers and
# CI bootstrap may set --quiet through a wrapper while a debug env var
# enables --verbose; in that combination the package manager intent (no
# noise) takes priority.
if $QUIET; then VERBOSE=false; fi

# ── Output helpers ────────────────────────────────────────────────────────────
# In default (quiet-ish) mode we print exactly one progress line and overwrite
# it in place. In verbose mode each step prints its own line. In --quiet mode
# we print nothing until the final summary (or an error).
if [[ -t 1 ]]; then
    DIM='\033[2m'; RESET='\033[0m'; CR='\r\033[K'
else
    DIM=''; RESET=''; CR='\n'
fi

step() {
    if $QUIET; then
        :  # eat the message
    elif $VERBOSE; then
        printf "%s\n" "$*"
    else
        printf "${CR}${DIM}%s${RESET}" "$*"
    fi
}

step_end() {
    if $QUIET || $VERBOSE; then
        :
    else
        # Erase the progress line — wizard will own the screen from here
        printf "${CR}"
    fi
}

die() {
    printf "\nerror: %s\n" "$*" >&2
    exit 1
}

# ── Platform detection ────────────────────────────────────────────────────────
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)   PLATFORM="linux"   ;;
    darwin)  PLATFORM="macos"   ;;
    freebsd) PLATFORM="freebsd" ;;
    openbsd) PLATFORM="openbsd" ;;
    netbsd)  PLATFORM="netbsd"  ;;
    *)       die "$(t err_unsupported_os "$OS")" ;;
esac

case "$ARCH" in
    x86_64|amd64)    ARCH_STD="x86_64"  ;;
    aarch64|arm64)   ARCH_STD="aarch64" ;;
    *)               die "$(t err_unsupported_arch "$ARCH")" ;;
esac

# Rust target triple
case "$PLATFORM" in
    macos)   TARGET="${ARCH_STD}-apple-darwin"        ;;
    linux)   TARGET="${ARCH_STD}-unknown-linux-gnu"   ;;
    freebsd) TARGET="${ARCH_STD}-unknown-freebsd"     ;;
    openbsd) TARGET="${ARCH_STD}-unknown-openbsd"     ;;
    netbsd)  TARGET="${ARCH_STD}-unknown-netbsd"      ;;
esac

# BSD support matrix:
#   - FreeBSD x86_64: shipped binary (Tier-2)
#   - FreeBSD ARM64:  source-only (no rust-std)
#   - NetBSD x86_64:  shipped binary (Tier-3)
#   - OpenBSD x86_64: source-only
#   - All other BSD arch combos: source-only.
if [[ "$PLATFORM" == "openbsd" ]]; then
    die "$(t err_source_only "OpenBSD")"
fi
if [[ "$PLATFORM" == "freebsd" && "$ARCH_STD" == "aarch64" ]]; then
    die "$(t err_source_only "FreeBSD ARM64")"
fi
if [[ "$PLATFORM" == "netbsd" && "$ARCH_STD" != "x86_64" ]]; then
    die "$(t err_source_only "netbsd/$ARCH_STD")"
fi

# ── Install directory selection ───────────────────────────────────────────────
if [[ -z "$INSTALL_DIR" ]]; then
    if [[ -w "/usr/local/bin" ]]; then
        INSTALL_DIR="/usr/local/bin"
    elif [[ "$(id -u)" == "0" ]]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="$HOME/.local/bin"
        mkdir -p "$INSTALL_DIR"
    fi
fi

# ── Dry-run preview (task #751 — used by test_install_sh.sh) ──────────────────
# Print a locale-aware banner showing what *would* happen, then exit without
# touching the network or filesystem. The banner is the test contract surface
# for verifying t() lookups work across locales. Production code paths
# (non-dry-run) stay silent-by-default per #660 design.
if $DRY_RUN; then
    printf "  %s\n" "$(t lang_detected "$LANG_CODE" "$LANG_SRC")"
    printf "  %s\n" "$(t dry_run_banner)"
    printf "  %s\n" "$(t platform "$PLATFORM" "$ARCH_STD" "$TARGET")"
    printf "  %s\n" "$(t install_dir "$INSTALL_DIR")"
    exit 0
fi

# ── Curl (required) ───────────────────────────────────────────────────────────
# curl is on every modern macOS and almost every Linux distro out of the box.
# If it's missing, we cannot proceed.
if ! command -v curl &>/dev/null; then
    die "$(t err_curl_required)"
fi

# ── Download Anvil binary ─────────────────────────────────────────────────────
GITHUB_BASE="https://github.com/culpur/anvil/releases/latest/download"
BINARY_NAME="anvil-${TARGET}"
BINARY_URL="${GITHUB_BASE}/${BINARY_NAME}"
SHA256_URL_PRIMARY="https://anvilhub.culpur.net/sha256/${BINARY_NAME}.sha256"
SHA256_URL_FALLBACK="${BINARY_URL}.sha256"

TMP_DIR="$(mktemp -d)"
TMP_BINARY="${TMP_DIR}/anvil"
TMP_SHA256="${TMP_DIR}/anvil.sha256"

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

step "$(t downloading "$PLATFORM" "$ARCH_STD")"
if ! curl -fSL --max-time 180 -o "$TMP_BINARY" "$BINARY_URL" >/dev/null 2>&1; then
    step_end
    die "$(t err_download_failed "$BINARY_URL")"
fi

# ── SHA256 verification (mandatory) ───────────────────────────────────────────
# Out-of-band primary source on anvilhub.culpur.net; GitHub mirror is fallback.
# We never trust the binary without a verified checksum.
step "$(t verifying)"
# 5s timeout on the primary so an unreachable anvilhub mirror fails fast
# to the GitHub fallback rather than hanging the install for 30+ seconds.
# Fallback gets 15s — by the time we're there we KNOW the primary is
# unreachable and the GitHub release endpoint is our last hope.
SHA256_SOURCE="primary"
if ! curl -fsSL --max-time 5 -o "$TMP_SHA256" "$SHA256_URL_PRIMARY" 2>/dev/null; then
    SHA256_SOURCE="fallback"
    if ! curl -fsSL --max-time 15 -o "$TMP_SHA256" "$SHA256_URL_FALLBACK" 2>/dev/null; then
        step_end
        die "$(t err_no_checksum)"
    fi
fi

EXPECTED="$(awk '{print $1}' "$TMP_SHA256")"
if [[ -z "${EXPECTED}" ]]; then
    step_end
    die "$(t err_checksum_empty)"
fi

# SHA256 tooling differs by platform:
#   macOS:        shasum -a 256
#   Linux:        sha256sum (coreutils)
#   FreeBSD:      sha256 -q   (base system; sha256sum exists if coreutils pkg installed)
#   NetBSD:       cksum -a sha256 -n   (base system)
# Probe in order of preference so we pick whatever the host actually has.
sha256_of() {
    if command -v sha256sum &>/dev/null; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum &>/dev/null; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v sha256 &>/dev/null; then
        # FreeBSD: -q prints just the hash
        sha256 -q "$1"
    elif command -v cksum &>/dev/null && cksum -a sha256 /dev/null &>/dev/null; then
        # NetBSD: cksum -a sha256 file → "SHA256 (file) = hash"
        cksum -a sha256 -n "$1" | awk '{print $NF}'
    else
        return 1
    fi
}
if ! ACTUAL="$(sha256_of "$TMP_BINARY")" || [[ -z "$ACTUAL" ]]; then
    step_end
    die "$(t err_no_sha_tool)"
fi

if [[ "${ACTUAL}" != "${EXPECTED}" ]]; then
    step_end
    die "$(t err_sha_mismatch "$EXPECTED" "$ACTUAL")"
fi

# ── Install binary ────────────────────────────────────────────────────────────
chmod +x "$TMP_BINARY"
INSTALL_PATH="${INSTALL_DIR}/anvil"

step "$(t installing "$INSTALL_PATH")"
if [[ -w "$INSTALL_DIR" ]]; then
    cp "$TMP_BINARY" "$INSTALL_PATH"
else
    # Need sudo. Tell the user once, in line, so the password prompt isn't
    # mystery-shell behavior.
    step_end
    printf "%s\n" "$(t sudo_required "$INSTALL_PATH")"
    sudo cp "$TMP_BINARY" "$INSTALL_PATH" || die "$(t err_install_failed "$INSTALL_PATH")"
fi

# ── PATH hint (only if actually needed, and only once) ────────────────────────
PATH_HINT=""
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    PATH_HINT="$(t path_hint "$INSTALL_DIR")
  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

# ── Shell completions (silent, best-effort) ───────────────────────────────────
if [[ "$INSTALL_COMPLETIONS" == "true" ]]; then
    COMPLETION_BASE="$(dirname "$0")/completions"
    SHARE_DIR="${INSTALL_DIR%/bin}/share/anvil/completions"
    if [[ -d "$COMPLETION_BASE" ]]; then
        mkdir -p "$SHARE_DIR" 2>/dev/null || true
        cp "$COMPLETION_BASE"/* "$SHARE_DIR"/ 2>/dev/null || true
    fi
    CURRENT_SHELL="${SHELL##*/}"
    case "$CURRENT_SHELL" in
        bash)
            BASH_COMP="$HOME/.local/share/bash-completion/completions"
            mkdir -p "$BASH_COMP" 2>/dev/null || true
            [[ -f "$SHARE_DIR/anvil.bash" ]] && cp "$SHARE_DIR/anvil.bash" "$BASH_COMP/anvil" 2>/dev/null || true
            ;;
        zsh)
            ZSH_COMP="$HOME/.zfunc"
            mkdir -p "$ZSH_COMP" 2>/dev/null || true
            [[ -f "$SHARE_DIR/anvil.zsh" ]] && cp "$SHARE_DIR/anvil.zsh" "$ZSH_COMP/_anvil" 2>/dev/null || true
            ;;
        fish)
            FISH_COMP="$HOME/.config/fish/completions"
            mkdir -p "$FISH_COMP" 2>/dev/null || true
            [[ -f "$SHARE_DIR/anvil.fish" ]] && cp "$SHARE_DIR/anvil.fish" "$FISH_COMP/anvil.fish" 2>/dev/null || true
            ;;
    esac
fi

# ── Hand off to the wizard ────────────────────────────────────────────────────
# Erase the progress line so the wizard's welcome card is the first thing on
# screen. The wizard handles QMD discovery, MEMORY setup, OAuth, vault — none
# of which the installer needs to mention.
step_end

# If we have a PATH hint to give, print it BEFORE launching the wizard so it
# stays in the user's scrollback above the alt-screen.
if [[ -n "$PATH_HINT" ]] && ! $QUIET; then
    printf "%s\n\n" "$PATH_HINT"
fi

if [[ "$RUN_SETUP" == "true" ]]; then
    # Headless detection (v2.2.18 #663 gap 14). The wizard enters an
    # alt-screen via crossterm `EnterAlternateScreen`; without a TTY on
    # both stdin AND stdout the alt-screen entry would either crash
    # (`Inappropriate ioctl`) or print escape sequences into the
    # surrounding pipe.  Common non-TTY contexts:
    #
    #   * CI bootstrap   — github-actions, gitlab, jenkins
    #   * ssh-no-pty     — `ssh host 'curl … | bash'`
    #   * provisioner    — packer, ansible, terraform user-data
    #   * dockerfile     — `RUN curl … | bash`
    #
    # In all of these the user (or their automation) wanted the binary
    # installed; they did NOT want a wizard.  Print the install path and
    # a one-line hint, exit clean.  `[ -t 0 ] && [ -t 1 ]` covers stdin
    # AND stdout — either one being a pipe means alt-screen is unsafe.
    if [[ ! -t 0 ]] || [[ ! -t 1 ]]; then
        if $QUIET; then
            printf "%s\n" "$INSTALL_PATH"
        else
            printf "%s\n" "$(t installed_tty_hint "$INSTALL_PATH")"
        fi
        exit 0
    fi

    # IMPORTANT: do NOT pass --setup at the historical level.  As of
    # v2.2.18 task #661, `--setup` correctly routes to the alt-screen
    # wizard (was previously wired to legacy setup.rs), but we still
    # prefer the bare `exec anvil` form so the first-run-no-config gate
    # in `wizard.rs::anvil_config_json_exists` is what triggers the
    # wizard — which is the same path users hit on every fresh install
    # whether the installer ran or not.  Single code path.
    if command -v anvil &>/dev/null; then
        exec anvil
    elif [[ -x "$INSTALL_PATH" ]]; then
        exec "$INSTALL_PATH"
    else
        die "$(t err_exec_failed "$INSTALL_PATH")"
    fi
fi

# RUN_SETUP=false path — print a single line telling the user what to run.
# --quiet path: be even quieter (one line, just the install path).
if $QUIET; then
    printf "%s\n" "$INSTALL_PATH"
else
    printf "%s\n%s\n" "$(t installed "$INSTALL_PATH")" "$(t run_anvil)"
fi
