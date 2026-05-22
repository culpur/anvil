#!/usr/bin/env bash
# Locale-aware installer test (task #751).
#
# Verifies that install/install.sh detects the user's locale from $LANG and
# prints the platform / installation / completion banner in the matching
# Tier-1 priority language. Runs install.sh in --dry-run mode so no files
# are touched.
#
# Usage:  bash install/test_install_sh.sh
#
# Exit codes:
#   0 — all assertions passed
#   1 — at least one assertion failed
#
# Locales tested: de, fr, zh-CN, ja, es, en (via LANG=C).
# Translation contract: each locale must include AT LEAST ONE word/glyph
# that is unique to that language and NOT in English. These checks intentionally
# use string matches against the t() table in install.sh.

set -uo pipefail   # NOT -e: we want to count failures, not bail on first one.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="${SCRIPT_DIR}/install.sh"

if [[ ! -x "$INSTALL_SH" ]]; then
    echo "FAIL: $INSTALL_SH not executable" >&2
    exit 1
fi

PASS=0
FAIL=0

# assert_contains <lang> <LANG-env> <needle> <description>
assert_contains() {
    local lang="$1" lang_env="$2" needle="$3" desc="$4"
    local out
    # Run under the requested locale. Clear LC_ALL/LC_MESSAGES so $LANG wins.
    out="$(LANG="$lang_env" LC_ALL= LC_MESSAGES= "$INSTALL_SH" --dry-run 2>&1 || true)"
    if echo "$out" | grep -qF -- "$needle"; then
        echo "  PASS [$lang] $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL [$lang] $desc"
        echo "         expected to find: $needle"
        echo "         in output:"
        echo "$out" | sed 's/^/         | /' | head -20
        FAIL=$((FAIL + 1))
    fi
}

echo "=== install.sh locale detection tests ==="
echo

# German — "Plattform" and "Trockenlauf" are German-only.
assert_contains "de" "de_DE.UTF-8" "Plattform" "German: 'Plattform' appears"
assert_contains "de" "de_DE.UTF-8" "TROCKENLAUF" "German: dry-run banner"

# Chinese (zh-CN) — "平台" and "演练模式" are zh-CN-only.
assert_contains "zh-CN" "zh_CN.UTF-8" "平台" "Chinese: '平台' appears"
assert_contains "zh-CN" "zh_CN.UTF-8" "演练模式" "Chinese: dry-run banner"

# French — "Plateforme" and "MODE SIMULATION" are fr-specific.
assert_contains "fr" "fr_FR.UTF-8" "Plateforme" "French: 'Plateforme' appears"
assert_contains "fr" "fr_FR.UTF-8" "MODE SIMULATION" "French: dry-run banner"

# Japanese — "プラットフォーム" is ja-only.
assert_contains "ja" "ja_JP.UTF-8" "プラットフォーム" "Japanese: 'プラットフォーム' appears"

# Spanish — "Plataforma" + "MODO DE PRUEBA"
assert_contains "es" "es_ES.UTF-8" "Plataforma" "Spanish: 'Plataforma' appears"

# C / POSIX — must print English.
assert_contains "C" "C" "Platform:" "POSIX: English platform line"
assert_contains "C" "C" "DRY-RUN MODE" "POSIX: English dry-run banner"

# Unsupported locale falls back to English but reports the source.
assert_contains "swahili-fallback" "sw_KE.UTF-8" "Platform:" "Unsupported locale falls back to English"
assert_contains "swahili-fallback" "sw_KE.UTF-8" "unsupported:sw-KE" "Unsupported locale reports source"

# Region-only normalization: fr_FR → fr
assert_contains "fr-FR-normalization" "fr_FR.UTF-8" "Langue: fr (détectée depuis LANG)" "fr_FR normalized to base fr"

echo
echo "=== Results ==="
echo "  PASS: $PASS"
echo "  FAIL: $FAIL"

if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
exit 0
