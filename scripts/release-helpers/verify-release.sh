#!/usr/bin/env bash
# Post-publish release verification gate.
#
# Sourced by scripts/release.sh AFTER `gh release upload` AND after the
# AnvilHub /api/version endpoint has been pointed at the new version.
#
# What it guards against:
#   v2.2.16 shipped a Windows binary as `*-pc-windows-gnu.exe` but the
#   AnvilHub /api/version endpoint advertised `*-pc-windows-msvc.exe` —
#   so every Windows upgrade returned HTTP 404 silently. The local
#   release.sh thought it succeeded.
#
# What this script does (when sourced into release.sh):
#   1. Sleep 5s for any pm2 / Cloudflare cache propagation.
#   2. Fetch https://anvilhub.culpur.net/api/version with no-cache header.
#   3. Assert .latest_version == the version we just published.
#   4. For each URL in .binaries{}, HEAD/GET it and assert HTTP 200.
#   5. On any failure: print a clear error naming the broken URL or
#      version mismatch + rollback guidance, and return non-zero so
#      release.sh exits the whole pipeline.
#
# Modular per feedback-anvil-main-rs-modularity (script size discipline
# applies to release.sh too — extract verify, don't inline).
#
# Usage (sourced):
#   . "$SCRIPT_DIR/release-helpers/verify-release.sh"
#   verify_release "$VERSION"   # returns 0 / non-zero
#
# Usage (standalone, e.g. for re-verifying after a fix):
#   bash scripts/release-helpers/verify-release.sh 2.2.17
#   bash scripts/release-helpers/verify-release.sh 2.2.17 --dry-run
#
# Self-test:
#   bash scripts/release-helpers/verify-release.sh --self-test
#   (uses mocked JSON fixtures, makes no network calls)

# Note: do NOT `set -euo pipefail` here when sourced — release.sh sets its
# own pipefail and we don't want to leak that into the parent. The function
# returns non-zero on failure; the caller decides what to do with it.

# ─── Configuration ──────────────────────────────────────────────────────────
ANVILHUB_VERSION_URL="${ANVILHUB_VERSION_URL:-https://anvilhub.culpur.net/api/version}"
VERIFY_SLEEP_SECS="${VERIFY_SLEEP_SECS:-5}"
VERIFY_CURL_TIMEOUT="${VERIFY_CURL_TIMEOUT:-15}"
VERIFY_RETRY_COUNT="${VERIFY_RETRY_COUNT:-1}"  # one retry on /api/version timeout

# ─── verify_release <expected_version> [--dry-run] ───────────────────────────
#
# Returns:
#   0  → all binary URLs return HTTP 200, version matches
#   1  → /api/version unreachable, or version mismatch
#   2  → one or more binary URLs return non-200
verify_release() {
    local expected="${1:-}"
    local dry_run="false"
    if [ "${2:-}" = "--dry-run" ]; then
        dry_run="true"
    fi

    if [ -z "$expected" ]; then
        echo "verify_release: missing expected version argument" >&2
        return 1
    fi

    echo
    echo "▸ Phase 8: Post-publish verification gate..."

    if [ "$dry_run" = "true" ]; then
        echo "  (--dry-run mode: would sleep ${VERIFY_SLEEP_SECS}s, then GET $ANVILHUB_VERSION_URL"
        echo "   and HEAD every binary URL, asserting latest_version == $expected and all HTTP 200.)"
        echo "  ✓ Dry-run complete — no network calls made."
        return 0
    fi

    echo "  Sleeping ${VERIFY_SLEEP_SECS}s for pm2 / cache propagation..."
    sleep "$VERIFY_SLEEP_SECS"

    # ─── 1. Fetch /api/version with one retry ────────────────────────────────
    local version_json=""
    local attempt=0
    local max_attempts=$((VERIFY_RETRY_COUNT + 1))
    while [ "$attempt" -lt "$max_attempts" ]; do
        attempt=$((attempt + 1))
        version_json=$(curl -sf \
            --max-time "$VERIFY_CURL_TIMEOUT" \
            -H "Cache-Control: no-cache" \
            "$ANVILHUB_VERSION_URL" 2>/dev/null) && break
        if [ "$attempt" -lt "$max_attempts" ]; then
            echo "  ⚠ /api/version timed out (attempt $attempt/$max_attempts) — retrying in 3s..."
            sleep 3
            version_json=""
        fi
    done

    if [ -z "$version_json" ]; then
        echo "  ✘ ERROR: $ANVILHUB_VERSION_URL unreachable after $max_attempts attempt(s)" >&2
        _verify_rollback_hint "$expected"
        return 1
    fi
    echo "  ✓ Fetched $ANVILHUB_VERSION_URL"

    # ─── 2. Assert latest_version matches ────────────────────────────────────
    local published
    published=$(echo "$version_json" | jq -r '.latest_version // empty')
    if [ -z "$published" ]; then
        echo "  ✘ ERROR: /api/version response is missing .latest_version" >&2
        echo "    response: $version_json" >&2
        _verify_rollback_hint "$expected"
        return 1
    fi

    if [ "$published" != "$expected" ]; then
        echo "  ✘ ERROR: /api/version reports latest_version=\"$published\"" >&2
        echo "    but we just published \"$expected\"." >&2
        _verify_rollback_hint "$expected"
        return 1
    fi
    echo "  ✓ /api/version.latest_version = $published (matches)"

    # ─── 3. HEAD/GET every binary URL ─────────────────────────────────────────
    # `.binaries` is an object (key → URL). `.binaries[]` yields each URL.
    local urls
    urls=$(echo "$version_json" | jq -r '.binaries[]? // empty')
    if [ -z "$urls" ]; then
        echo "  ✘ ERROR: /api/version response has no .binaries entries" >&2
        echo "    response: $version_json" >&2
        _verify_rollback_hint "$expected"
        return 1
    fi

    local broken=0
    local total=0
    local url code
    while IFS= read -r url; do
        [ -z "$url" ] && continue
        total=$((total + 1))
        # -L: follow redirects (GitHub Releases assets redirect to S3)
        # -o /dev/null: discard body
        # -w "%{http_code}": print final status code
        # --max-time: don't hang on slow CDN
        code=$(curl -sL \
            --max-time "$VERIFY_CURL_TIMEOUT" \
            -o /dev/null \
            -w "%{http_code}" \
            "$url" 2>/dev/null || echo "000")
        if [ "$code" = "200" ]; then
            echo "  ✓ $url → 200"
        else
            echo "  ✘ ERROR: $url returns HTTP $code" >&2
            broken=$((broken + 1))
        fi
    done <<EOF
$urls
EOF

    if [ "$broken" -gt 0 ]; then
        echo "  ✘ ERROR: $broken of $total binary URL(s) returned non-200. Release is INCOMPLETE." >&2
        _verify_rollback_hint "$expected"
        return 2
    fi

    echo "  ✓ All $total platform binaries return HTTP 200."
    echo "▸ Verification passed."
    return 0
}

# ─── Rollback / remediation guidance ─────────────────────────────────────────
_verify_rollback_hint() {
    local expected="$1"
    cat >&2 <<HINT

  ── Rollback / remediation ──────────────────────────────────────────────────
  The release is published on GitHub but a downstream surface is broken.
  The binaries on GitHub are intact — only the AnvilHub-advertised URLs or
  version string are wrong. To fix without re-uploading binaries:

    1. SSH to dev0001 and edit:
         /opt/projects/anvilhub/packages/web/src/lib/anvil-config.ts
       Confirm the version matches \"$expected\" and every entry in the
       \`binaries\` map matches the actual filenames uploaded to the
       v$expected GitHub Release (check on github.com/culpur/anvil).

    2. Rebuild + restart AnvilHub on dev0001:
         cd /opt/projects/anvilhub && npx next build && pm2 restart anvilhub-web

    3. Re-run verification only (no rebuild, no re-upload):
         bash scripts/release-helpers/verify-release.sh $expected

    4. Or re-run the full release with builds skipped:
         scripts/release.sh --skip-build

  ────────────────────────────────────────────────────────────────────────────
HINT
}

# ─── Self-test (mock fixtures, no network) ──────────────────────────────────
#
# Run: bash scripts/release-helpers/verify-release.sh --self-test
#
# Covers the four cases from task #611:
#   - verify_release_passes_when_all_urls_200
#   - verify_release_fails_when_one_url_404
#   - verify_release_fails_when_version_mismatch
#   - verify_release_handles_endpoint_timeout_with_one_retry
#
# Implementation: we shadow `curl` with a bash function fed by a fixture
# selector ($VERIFY_TEST_CASE). Each case returns curl-shaped output.
_verify_self_test() {
    local pass=0 fail=0

    # Save and override curl with a mock for the duration of the self-test.
    # The mock reads $VERIFY_TEST_CASE to decide what to return.
    curl() {
        local case="${VERIFY_TEST_CASE:-}"
        # Detect which kind of call this is by scanning args for "/api/version"
        # vs anything else (binary URL probe).
        local is_version_call="false"
        for a in "$@"; do
            case "$a" in
                *"/api/version"*) is_version_call="true" ;;
            esac
        done

        if [ "$is_version_call" = "true" ]; then
            case "$case" in
                all_good|one_404|version_mismatch)
                    local ver="2.2.17"
                    [ "$case" = "version_mismatch" ] && ver="2.2.99"
                    local good_url="https://example.test/good-bin"
                    local bad_url="https://example.test/bad-bin"
                    local b_url2="$good_url"
                    [ "$case" = "one_404" ] && b_url2="$bad_url"
                    cat <<JSON
{"latest_version":"$ver","binaries":{"a":"$good_url","b":"$b_url2"}}
JSON
                    return 0
                    ;;
                timeout_then_recover)
                    # First call: fail. Second call: succeed.
                    # Use a file-based marker because curl is invoked via
                    # `$(curl ...)` which runs in a subshell, so shell vars
                    # can't persist between mock invocations.
                    local marker="${VERIFY_TEST_MARKER:-/tmp/anvil-verify-test-marker}"
                    if [ ! -f "$marker" ]; then
                        touch "$marker"
                        return 28  # curl timeout exit code
                    fi
                    cat <<JSON
{"latest_version":"2.2.17","binaries":{"a":"https://example.test/good-bin"}}
JSON
                    return 0
                    ;;
            esac
        else
            # Binary URL probe. Reads -w "%{http_code}" → we need to emit a code.
            local url="${@: -1}"  # last arg is the URL
            case "$url" in
                *bad-bin*) echo "404"; return 0 ;;
                *)         echo "200"; return 0 ;;
            esac
        fi
    }

    # Also stub sleep so the self-test runs in <1s.
    sleep() { :; }

    local saved_sleep="$VERIFY_SLEEP_SECS"
    local saved_retry="$VERIFY_RETRY_COUNT"
    VERIFY_SLEEP_SECS=0
    VERIFY_RETRY_COUNT=1

    _self_test_case() {
        local name="$1" expected_rc="$2" case_id="$3" version="$4"
        # Clear any leftover marker from a prior case.
        rm -f "${VERIFY_TEST_MARKER:-/tmp/anvil-verify-test-marker}"
        VERIFY_TEST_CASE="$case_id"
        export VERIFY_TEST_CASE
        local out rc
        out=$(verify_release "$version" 2>&1)
        rc=$?
        if [ "$rc" -eq "$expected_rc" ]; then
            echo "  ✓ $name (rc=$rc)"
            pass=$((pass + 1))
        else
            echo "  ✘ $name (expected rc=$expected_rc, got $rc)"
            echo "    --- output ---"
            echo "$out" | sed 's/^/    /'
            echo "    --- end ---"
            fail=$((fail + 1))
        fi
    }

    echo "▸ Running verify-release.sh self-test (no network)..."
    _self_test_case "verify_release_passes_when_all_urls_200"        0 "all_good"             "2.2.17"
    _self_test_case "verify_release_fails_when_one_url_404"          2 "one_404"              "2.2.17"
    _self_test_case "verify_release_fails_when_version_mismatch"     1 "version_mismatch"     "2.2.17"
    _self_test_case "verify_release_handles_endpoint_timeout_with_one_retry" 0 "timeout_then_recover" "2.2.17"

    # Restore configuration
    VERIFY_SLEEP_SECS="$saved_sleep"
    VERIFY_RETRY_COUNT="$saved_retry"
    unset -f curl sleep
    unset VERIFY_TEST_CASE
    rm -f "${VERIFY_TEST_MARKER:-/tmp/anvil-verify-test-marker}"

    echo
    echo "  Self-test results: $pass passed, $fail failed"
    [ "$fail" -eq 0 ]
}

# ─── CLI entry point (when invoked directly, not sourced) ────────────────────
# BASH_SOURCE[0] == $0 iff this file is being executed, not sourced.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -uo pipefail
    case "${1:-}" in
        --self-test)
            _verify_self_test
            exit $?
            ;;
        --dry-run)
            verify_release "${2:-0.0.0}" --dry-run
            exit $?
            ;;
        "")
            echo "Usage: $0 <version> [--dry-run]" >&2
            echo "       $0 --dry-run <version>" >&2
            echo "       $0 --self-test" >&2
            exit 64
            ;;
        *)
            verify_release "$1" "${2:-}"
            exit $?
            ;;
    esac
fi
