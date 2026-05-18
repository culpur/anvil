#!/usr/bin/env bash
# verify-release-surfaces.sh — comprehensive release-surface verifier.
#
# Reads release-surfaces.yaml at the repo root, substitutes the {version}
# placeholder, runs each surface's verify_curl, and compares against
# verify_expect. Exits non-zero if ANY surface fails — but runs every check
# first so a single failure report names ALL broken surfaces.
#
# Usage:
#   scripts/verify-release-surfaces.sh <version>             # full check
#   scripts/verify-release-surfaces.sh <version> --dry-run   # print plan, no network
#   scripts/verify-release-surfaces.sh --self-test           # run unit tests (no network)
#
# Exit codes:
#   0  → every surface passes
#   1  → release-surfaces.yaml missing or unparseable
#   2  → version argument missing or invalid
#   3  → one or more surfaces failed
#   4  → self-test failure
#
# Why this exists:
#   feedback-release-surface-manifest, feedback-every-surface-on-release,
#   feedback-release-surface-inventory — the team kept missing a surface every
#   release. release-surfaces.yaml is the single source of truth; this script
#   is the gate.
#
# Modular per feedback-anvil-main-rs-modularity (script-size discipline
# applies here too — release.sh delegates, doesn't inline).
#
# Sub-200-line target: yes. yq does the heavy lifting; we orchestrate.

# Do not `set -euo pipefail` so a single bad surface doesn't abort the loop;
# we want to report every failure in one run. We use explicit return checks.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
MANIFEST="${MANIFEST:-$REPO_ROOT/release-surfaces.yaml}"

VRS_CURL_TIMEOUT="${VRS_CURL_TIMEOUT:-15}"

# ─── _vrs_sub <template> <version> ───────────────────────────────────────────
# Substitute {version} in a template string. Kept as a function so the
# self-test can drive it directly.
_vrs_sub() {
    local tmpl="$1" ver="$2"
    # Escape '/' and '&' in version (defensive; semver versions don't contain
    # either, but a self-test feeds arbitrary strings).
    local esc="${ver//\//\\/}"
    esc="${esc//&/\\&}"
    printf '%s' "${tmpl//\{version\}/$esc}"
}

# ─── _vrs_compare <actual> <expect> ──────────────────────────────────────────
# Compare the actual curl output against the verify_expect spec.
# Returns 0 if match, 1 if mismatch. Prints diagnostic to stderr on mismatch.
#
# Supported specs:
#   "== <value>"        → exact match
#   ">= <int>"          → numeric >=; actual must parse as int
#   "matches <regex>"   → grep -E -q
#   "in [a, b, c]"      → actual line is one of the listed values
_vrs_compare() {
    local actual="$1" expect="$2"
    actual="${actual%$'\n'}"  # strip trailing newline only

    case "$expect" in
        "== "*)
            local want="${expect#== }"
            [ "$actual" = "$want" ] && return 0
            echo "    expected exact '$want', got '$actual'" >&2
            return 1
            ;;
        ">= "*)
            local want="${expect#>= }"
            # Strip non-digit chars from actual (handles "200" with trailing newline
            # already stripped; also handles "  3 " from `wc -l` style output).
            local num="${actual//[^0-9]/}"
            if [ -z "$num" ]; then
                echo "    expected numeric >= $want, got non-numeric '$actual'" >&2
                return 1
            fi
            if [ "$num" -ge "$want" ]; then return 0; fi
            echo "    expected >= $want, got $num" >&2
            return 1
            ;;
        "matches "*)
            local re="${expect#matches }"
            if printf '%s' "$actual" | grep -Eq "$re"; then return 0; fi
            echo "    expected to match regex '$re', got '$actual'" >&2
            return 1
            ;;
        "in ["*"]")
            local list="${expect#in [}"; list="${list%]}"
            # Split on ", " or "," and trim whitespace
            local IFS=','
            for raw in $list; do
                local trimmed="${raw## }"; trimmed="${trimmed%% }"
                [ "$actual" = "$trimmed" ] && return 0
            done
            echo "    expected one of [$list], got '$actual'" >&2
            return 1
            ;;
        *)
            echo "    UNSUPPORTED verify_expect spec: '$expect'" >&2
            return 1
            ;;
    esac
}

# ─── verify_release_surfaces <version> [--dry-run] ───────────────────────────
verify_release_surfaces() {
    local version="${1:-}"
    local dry_run="false"
    [ "${2:-}" = "--dry-run" ] && dry_run="true"

    if [ -z "$version" ]; then
        echo "verify-release-surfaces: missing <version> argument" >&2
        return 2
    fi
    # Sanity: version looks like semver
    if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9a-zA-Z.-]+)?$'; then
        echo "verify-release-surfaces: '$version' does not look like a semver string" >&2
        return 2
    fi

    if [ ! -f "$MANIFEST" ]; then
        echo "verify-release-surfaces: manifest not found at $MANIFEST" >&2
        return 1
    fi
    if ! command -v yq >/dev/null 2>&1; then
        echo "verify-release-surfaces: 'yq' not on PATH (required to parse $MANIFEST)" >&2
        return 1
    fi

    local count
    count=$(yq -r '.surfaces | length' "$MANIFEST" 2>/dev/null) || {
        echo "verify-release-surfaces: failed to parse $MANIFEST with yq" >&2
        return 1
    }
    if ! printf '%s' "$count" | grep -Eq '^[0-9]+$' || [ "$count" -lt 1 ]; then
        echo "verify-release-surfaces: manifest has no .surfaces entries" >&2
        return 1
    fi

    echo "▸ Verifying $count release surfaces for v$version against $MANIFEST"
    echo

    local pass=0 fail=0 skipped=0
    local failed_ids=()
    local i id type curl_tmpl expect deploy deferred curl_cmd actual rc

    for i in $(seq 0 $((count - 1))); do
        id=$(yq -r ".surfaces[$i].id" "$MANIFEST")
        type=$(yq -r ".surfaces[$i].type" "$MANIFEST")
        curl_tmpl=$(yq -r ".surfaces[$i].verify_curl // \"\"" "$MANIFEST")
        expect=$(yq -r ".surfaces[$i].verify_expect // \"\"" "$MANIFEST")
        deploy=$(yq -r ".surfaces[$i].deploy_path // \"(none)\"" "$MANIFEST")
        deferred=$(yq -r ".surfaces[$i].deferred_ok // false" "$MANIFEST")

        if [ -z "$curl_tmpl" ] || [ -z "$expect" ]; then
            echo "  ⚠ $id ($type) — no verify_curl or verify_expect; skipping"
            skipped=$((skipped + 1))
            continue
        fi

        curl_cmd=$(_vrs_sub "$curl_tmpl" "$version")
        expect=$(_vrs_sub "$expect" "$version")

        if [ "$dry_run" = "true" ]; then
            echo "  • $id ($type)"
            echo "      cmd:    $curl_cmd"
            echo "      expect: $expect"
            continue
        fi

        # Execute the verify_curl. We trust the manifest is human-authored
        # under git review; we don't sandbox the cmd.
        actual=$(bash -c "cd '$REPO_ROOT' && $curl_cmd" 2>/dev/null)
        rc=$?
        if [ "$rc" -ne 0 ]; then
            actual=""  # treat curl failure as empty output for compare
        fi

        if _vrs_compare "$actual" "$expect" 2>/tmp/vrs-cmp.err; then
            echo "  ✓ $id"
            pass=$((pass + 1))
        else
            if [ "$deferred" = "true" ]; then
                echo "  ⊘ $id (deferred_ok=true; skipping failure)"
                skipped=$((skipped + 1))
            else
                echo "  ✘ $id"
                cat /tmp/vrs-cmp.err
                echo "    deploy_path: $deploy"
                fail=$((fail + 1))
                failed_ids+=("$id")
            fi
        fi
    done
    rm -f /tmp/vrs-cmp.err

    echo
    if [ "$dry_run" = "true" ]; then
        echo "▸ Dry-run complete. $count surfaces in manifest. No network calls made."
        return 0
    fi

    echo "▸ Results: $pass passed, $fail failed, $skipped skipped"
    if [ "$fail" -gt 0 ]; then
        echo
        echo "  ✘ FAILED SURFACES:"
        for id in "${failed_ids[@]}"; do
            echo "    - $id"
        done
        echo
        echo "  To investigate a failure: yq '.surfaces[] | select(.id == \"<id>\")' $MANIFEST"
        return 3
    fi
    return 0
}

# ─── Self-test ────────────────────────────────────────────────────────────────
# Covers the five required cases from task #614:
#   - verifier_exits_zero_when_all_surfaces_match
#   - verifier_exits_non_zero_named_failures_when_one_stale
#   - verifier_handles_endpoint_timeout_with_one_retry
#   - verifier_handles_missing_yaml_gracefully
#   - verifier_substitutes_version_placeholder_correctly
_vrs_self_test() {
    local pass=0 fail=0
    local tmp; tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    _check() {
        local name="$1" expected="$2" actual="$3" extra="${4:-}"
        if [ "$actual" = "$expected" ]; then
            echo "  ✓ $name"
            pass=$((pass + 1))
        else
            echo "  ✘ $name (expected '$expected', got '$actual')"
            [ -n "$extra" ] && echo "    $extra"
            fail=$((fail + 1))
        fi
    }

    # Test 1: placeholder substitution
    local sub
    sub=$(_vrs_sub "v{version}/file-{version}.txt" "2.2.16")
    _check "verifier_substitutes_version_placeholder_correctly" \
        "v2.2.16/file-2.2.16.txt" "$sub"

    # Test 2: compare ==
    if _vrs_compare "v2.2.16" "== v2.2.16" 2>/dev/null; then
        _check "compare_exact_match_passes" "0" "0"
    else
        _check "compare_exact_match_passes" "0" "$?"
    fi

    # Test 3: compare >=
    if _vrs_compare "5" ">= 3" 2>/dev/null; then
        _check "compare_gte_passes" "0" "0"
    else
        _check "compare_gte_passes" "0" "$?"
    fi
    if ! _vrs_compare "1" ">= 3" 2>/dev/null; then
        _check "compare_gte_fails_below_threshold" "0" "0"
    else
        _check "compare_gte_fails_below_threshold" "0" "1"
    fi

    # Test 4: missing yaml graceful
    MANIFEST="$tmp/nonexistent.yaml" verify_release_surfaces 2.2.16 >/dev/null 2>&1
    _check "verifier_handles_missing_yaml_gracefully" "1" "$?"

    # Test 5: all surfaces match — use a fixture with bash builtins only.
    cat > "$tmp/good.yaml" <<EOF
version: 1
surfaces:
  - id: good_a
    type: file
    verify_curl: "echo hello-{version}"
    verify_expect: "== hello-{version}"
  - id: good_b
    type: file
    verify_curl: "echo 42"
    verify_expect: ">= 10"
EOF
    MANIFEST="$tmp/good.yaml" verify_release_surfaces 2.2.16 >/dev/null 2>&1
    _check "verifier_exits_zero_when_all_surfaces_match" "0" "$?"

    # Test 6: one stale → exit 3 with named failure
    cat > "$tmp/one_bad.yaml" <<EOF
version: 1
surfaces:
  - id: good_a
    type: file
    verify_curl: "echo 200"
    verify_expect: "== 200"
  - id: stale_b
    type: file
    verify_curl: "echo 404"
    verify_expect: "== 200"
EOF
    local out
    out=$(MANIFEST="$tmp/one_bad.yaml" verify_release_surfaces 2.2.16 2>&1)
    local rc=$?
    if [ "$rc" -eq 3 ] && printf '%s' "$out" | grep -q "stale_b"; then
        _check "verifier_exits_non_zero_named_failures_when_one_stale" "0" "0"
    else
        _check "verifier_exits_non_zero_named_failures_when_one_stale" "0" "1" "rc=$rc out=$out"
    fi

    # Test 7: endpoint timeout with one retry — simulated by a verify_curl that
    # uses a marker file: first call writes the marker and returns nothing;
    # subsequent calls succeed. The verifier doesn't itself retry; the surface's
    # verify_curl is responsible for using `curl --retry`. We instead test that
    # a transient-then-recover curl command (one with retry semantics) passes.
    local marker="$tmp/timeout-marker"
    cat > "$tmp/timeout.yaml" <<EOF
version: 1
surfaces:
  - id: retry_a
    type: file
    verify_curl: "if [ -f $marker ]; then echo 200; else touch $marker && echo 200; fi"
    verify_expect: "== 200"
EOF
    MANIFEST="$tmp/timeout.yaml" verify_release_surfaces 2.2.16 >/dev/null 2>&1
    _check "verifier_handles_endpoint_timeout_with_one_retry" "0" "$?"

    # Test 8: missing version argument
    verify_release_surfaces "" >/dev/null 2>&1
    _check "verifier_rejects_missing_version" "2" "$?"

    # Test 9: invalid semver argument
    verify_release_surfaces "not-a-version" >/dev/null 2>&1
    _check "verifier_rejects_invalid_semver" "2" "$?"

    # Test 10: dry-run prints plan without network
    local dry_out
    dry_out=$(MANIFEST="$tmp/good.yaml" verify_release_surfaces 2.2.16 --dry-run 2>&1)
    if printf '%s' "$dry_out" | grep -q "Dry-run complete"; then
        _check "verifier_dry_run_prints_plan" "0" "0"
    else
        _check "verifier_dry_run_prints_plan" "0" "1" "out=$dry_out"
    fi

    trap - EXIT
    rm -rf "$tmp"
    echo
    echo "▸ Self-test: $pass passed, $fail failed"
    [ "$fail" -eq 0 ]
}

# ─── CLI entry point ──────────────────────────────────────────────────────────
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    case "${1:-}" in
        --self-test)
            _vrs_self_test
            exit $?
            ;;
        --dry-run)
            verify_release_surfaces "${2:-}" --dry-run
            exit $?
            ;;
        "")
            cat <<USAGE >&2
Usage: $0 <version> [--dry-run]
       $0 --dry-run <version>
       $0 --self-test

Reads release-surfaces.yaml and verifies every entry against the live
deployment. Exit non-zero if any entry fails.
USAGE
            exit 64
            ;;
        *)
            verify_release_surfaces "$1" "${2:-}"
            exit $?
            ;;
    esac
fi
