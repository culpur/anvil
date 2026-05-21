#!/bin/bash
# scripts/test-release-gates.sh — regression test for v2.2.19 Arc C1
#
# Runs `release.sh --dry-run` and asserts that every expected phase fires
# exactly one START + exactly one terminal marker (OK / OK_WARN / FAIL).
# Does NOT actually build, tag, push, or upload anything.
#
# Exit codes:
#   0 — every expected phase emitted matching markers
#   1 — at least one phase missing or duplicated
#   2 — release.sh --dry-run itself exited non-zero
#
# The dry-run path skips the heavy phase bodies but still exercises:
#   - the step / ok / warn / fail helpers
#   - status JSON persistence
#   - the EXIT-trap summary table
#   - the silent-exit detector (RUNNING -> FAIL on early termination)
#
# This script is invoked manually + by any CI workflow that wants to confirm
# the release pipeline's gate plumbing didn't regress.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

EXPECTED_PHASES=( P0 P1 P1.5 P2 P2.5 P2.6 P3 P4 P5 P6 P7 )

OUT_FILE="$(mktemp -t anvil-release-dryrun.XXXXXX)"
STATUS_FILE=""

cleanup() { rm -f "$OUT_FILE"; }
trap cleanup EXIT

echo "▸ Running release.sh --dry-run ..."
# Capture both stdout + stderr so we see the markers (which go to both).
if ! bash "$SCRIPT_DIR/release.sh" --dry-run > "$OUT_FILE" 2>&1; then
    rc=$?
    echo "✗ release.sh --dry-run exited rc=$rc" >&2
    echo "─── tail of dry-run output ───" >&2
    tail -40 "$OUT_FILE" >&2
    exit 2
fi

# Locate the status JSON the dry-run wrote. release.sh logs its path at end.
VERSION=$(grep -m1 'version = ' "$PROJECT_DIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
STATUS_FILE="/tmp/anvil-release-status-${VERSION}.json"

if [ ! -s "$STATUS_FILE" ]; then
    echo "✗ status JSON missing or empty at $STATUS_FILE" >&2
    exit 1
fi

echo "  ✓ release.sh --dry-run completed successfully"
echo "  ✓ status JSON at $STATUS_FILE"
echo

# Assert each expected phase emitted START + (OK | OK_WARN | FAIL) exactly once.
failures=0
for phase in "${EXPECTED_PHASES[@]}"; do
    start_count=$(grep -c ">>> STEP ${phase} START" "$OUT_FILE" || true)
    ok_count=$(grep -c ">>> STEP ${phase} OK\b" "$OUT_FILE" || true)
    warn_count=$(grep -c ">>> STEP ${phase} WARN" "$OUT_FILE" || true)
    fail_count=$(grep -c ">>> STEP ${phase} FAIL" "$OUT_FILE" || true)
    terminal_count=$(( ok_count + warn_count + fail_count ))

    # Each START marker prints to both stdout and stderr, so the test file
    # (which collects 2>&1) sees TWO lines per START. Same for terminal
    # markers. Divide by 2 to get logical counts.
    start_logical=$(( start_count / 2 ))
    terminal_logical=$(( terminal_count / 2 ))

    if [ "$start_logical" -ne 1 ] || [ "$terminal_logical" -ne 1 ]; then
        echo "✗ ${phase}: expected 1 START + 1 terminal, got START=${start_logical} TERMINAL=${terminal_logical} (OK=${ok_count} WARN=${warn_count} FAIL=${fail_count}, raw counts)" >&2
        failures=$((failures + 1))
    else
        echo "  ✓ ${phase}: 1 START + 1 terminal (OK=${ok_count} WARN=${warn_count} FAIL=${fail_count})"
    fi
done

# Sanity: status JSON should list every expected phase.
if command -v jq >/dev/null 2>&1; then
    json_phases=$(jq -r '.phases[].id' "$STATUS_FILE" | sort -u)
    for phase in "${EXPECTED_PHASES[@]}"; do
        if ! grep -qx "$phase" <<< "$json_phases"; then
            echo "✗ ${phase}: missing from status JSON" >&2
            failures=$((failures + 1))
        fi
    done
fi

echo
if [ "$failures" -gt 0 ]; then
    echo "✗ FAILED — ${failures} phase(s) did not match gate contract" >&2
    echo
    echo "─── status JSON ───"
    cat "$STATUS_FILE"
    exit 1
fi

echo "✓ All ${#EXPECTED_PHASES[@]} phases emitted matching START + terminal markers"
echo "✓ Status JSON contains every expected phase id"
echo
echo "Phase summary from dry-run:"
sed -n '/════════════════════ Phase summary/,/═══════════════════════/p' "$OUT_FILE" | head -25
