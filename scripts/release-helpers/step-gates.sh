#!/bin/bash
# release-helpers/step-gates.sh — per-step gate primitives for release.sh
#
# Added v2.2.19 Arc C1 (post-v2.2.18-silent-exit hardening).
#
# Background: v2.2.18 release silently exited after Phase 6 because the
# remote SSH heredoc dropped set-e on the remote side and the outer
# `&& ... || ...` chain masked the failure. There was no per-step ID/status
# output and no machine-readable status anywhere, so we didn't notice the
# truncated run until users reported stale data.
#
# These helpers make every phase emit explicit markers to BOTH stdout and
# stderr ("`>>> STEP P6 START — ...`", "`>>> STEP P6 OK`", "`>>> STEP P6
# FAIL — <reason>`"), write the result to JSON, and print a summary table.
#
# Public API:
#   init_step_gates <version> <tag>   set up state + EXIT trap
#   step <id> [title]                 mark step START, set CURRENT_STEP
#   ok [id]                           mark CURRENT_STEP (or named id) as OK
#   fail <reason>                     mark CURRENT_STEP as FAIL (does NOT exit)
#   fail <id> <reason>                explicit id form
#   warn [id] [reason]                non-blocking advisory (OK_WARN)
#   write_status_json                 persist current state to STATUS_FILE
#   print_summary_table               print final phase table (called by trap)
#   release_status_file               echo $STATUS_FILE so callers can locate it
#
# Status JSON path: /tmp/anvil-release-status-<version>.json
# Phase status values: RUNNING, OK, OK_WARN, FAIL, UNKNOWN

# Guard against double-sourcing.
if [ "${_ANVIL_STEP_GATES_LOADED:-}" = "1" ]; then
    return 0 2>/dev/null || exit 0
fi
_ANVIL_STEP_GATES_LOADED=1

# State (populated by init_step_gates):
STATUS_FILE=""
CURRENT_STEP=""
RELEASE_STARTED=""
declare -a PHASE_ORDER=()
declare -A PHASE_STATUS=()
declare -A PHASE_TITLE=()
declare -A PHASE_REASON=()

# Color only if the terminal advertises ≥8 colors. Fall back to plain otherwise
# (CI logs, file redirections, dumb terminals).
if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
    _C_GREEN=$(tput setaf 2)
    _C_RED=$(tput setaf 1)
    _C_YELLOW=$(tput setaf 3)
    _C_BOLD=$(tput bold)
    _C_RESET=$(tput sgr0)
else
    _C_GREEN=""
    _C_RED=""
    _C_YELLOW=""
    _C_BOLD=""
    _C_RESET=""
fi

init_step_gates() {
    local version="$1"
    local tag="$2"
    STATUS_FILE="/tmp/anvil-release-status-${version}.json"
    RELEASE_STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    : > "$STATUS_FILE"
    _ANVIL_RELEASE_VERSION="$version"
    _ANVIL_RELEASE_TAG="$tag"
    write_status_json
    # Ensure the summary table prints + status JSON is final even when set -e /
    # pipefail tears us down mid-run. Without this, a silent-exit failure mode
    # leaves no breadcrumb for the operator.
    trap '_anvil_step_gates_on_exit' EXIT
}

_anvil_step_gates_on_exit() {
    local rc=$?
    # If CURRENT_STEP is still in RUNNING state at exit, mark it as FAIL with
    # the exit code so the summary table doesn't lie about the truncation
    # point. This is the v2.2.18 silent-exit-detector.
    if [ -n "$CURRENT_STEP" ] && [ "${PHASE_STATUS[$CURRENT_STEP]:-}" = "RUNNING" ]; then
        PHASE_STATUS["$CURRENT_STEP"]="FAIL"
        PHASE_REASON["$CURRENT_STEP"]="script exited mid-phase (rc=$rc) — silent exit detector"
        local marker=">>> STEP ${CURRENT_STEP} FAIL — script exited mid-phase (rc=$rc)"
        echo "${_C_RED}${marker}${_C_RESET}" >&2
    fi
    write_status_json
    print_summary_table
    return $rc
}

step() {
    local id="$1"
    local title="${2:-$1}"
    CURRENT_STEP="$id"
    PHASE_ORDER+=("$id")
    PHASE_STATUS["$id"]="RUNNING"
    PHASE_TITLE["$id"]="$title"
    PHASE_REASON["$id"]=""
    local marker=">>> STEP ${id} START — ${title}"
    echo "${_C_BOLD}${marker}${_C_RESET}"
    echo "${marker}" >&2
    write_status_json
}

ok() {
    local id="${1:-$CURRENT_STEP}"
    PHASE_STATUS["$id"]="OK"
    local marker=">>> STEP ${id} OK"
    echo "${_C_GREEN}${marker}${_C_RESET}"
    echo "${marker}" >&2
    write_status_json
}

fail() {
    local id="${CURRENT_STEP}"
    local reason="${1:-unspecified}"
    if [ $# -ge 2 ]; then
        id="$1"
        reason="$2"
    fi
    PHASE_STATUS["$id"]="FAIL"
    PHASE_REASON["$id"]="$reason"
    local marker=">>> STEP ${id} FAIL — ${reason}"
    echo "${_C_RED}${marker}${_C_RESET}"
    echo "${marker}" >&2
    write_status_json
}

warn() {
    local id="${1:-$CURRENT_STEP}"
    local reason="${2:-unspecified}"
    PHASE_STATUS["$id"]="OK_WARN"
    PHASE_REASON["$id"]="$reason"
    local marker=">>> STEP ${id} WARN — ${reason}"
    echo "${_C_YELLOW}${marker}${_C_RESET}"
    echo "${marker}" >&2
    write_status_json
}

# JSON-escape backslash + double-quote (sufficient for our values, which are
# IDs / titles / one-line reasons — no embedded newlines).
_json_escape() {
    local s="${1//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '%s' "$s"
}

write_status_json() {
    [ -n "$STATUS_FILE" ] || return 0
    {
        printf '{\n  "version": "%s",\n  "tag": "%s",\n  "started": "%s",\n  "phases": [\n' \
            "${_ANVIL_RELEASE_VERSION:-}" "${_ANVIL_RELEASE_TAG:-}" "${RELEASE_STARTED:-}"
        local first=1
        for id in "${PHASE_ORDER[@]}"; do
            local st="${PHASE_STATUS[$id]:-UNKNOWN}"
            local title
            local reason
            title="$(_json_escape "${PHASE_TITLE[$id]:-}")"
            reason="$(_json_escape "${PHASE_REASON[$id]:-}")"
            if [ "$first" -eq 1 ]; then
                first=0
            else
                printf ',\n'
            fi
            printf '    { "id": "%s", "title": "%s", "status": "%s", "reason": "%s" }' \
                "$id" "$title" "$st" "$reason"
        done
        printf '\n  ]\n}\n'
    } > "$STATUS_FILE"
}

print_summary_table() {
    echo
    echo "════════════════════ Phase summary ════════════════════"
    printf '  %-8s  %-7s  %s\n' "STEP" "STATUS" "TITLE"
    printf '  %-8s  %-7s  %s\n' "----" "------" "-----"
    for id in "${PHASE_ORDER[@]}"; do
        local st="${PHASE_STATUS[$id]:-UNKNOWN}"
        local color=""
        case "$st" in
            OK)      color="$_C_GREEN" ;;
            OK_WARN) color="$_C_YELLOW" ;;
            FAIL)    color="$_C_RED" ;;
            RUNNING) color="$_C_YELLOW" ;;
        esac
        printf '  %-8s  %s%-7s%s  %s\n' "$id" "$color" "$st" "$_C_RESET" "${PHASE_TITLE[$id]:-}"
    done
    echo "═══════════════════════════════════════════════════════"
    echo "  Status JSON: $STATUS_FILE"
    echo
}

release_status_file() {
    printf '%s\n' "$STATUS_FILE"
}
