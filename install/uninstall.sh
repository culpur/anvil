#!/usr/bin/env bash
# Anvil uninstaller — macOS + Linux
# Usage:  bash uninstall.sh [--keep-data]
#
# Exit codes:
#   0 — success
#   1 — error

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
info()    { printf "  ${CYAN}>${NC} %s\n" "$*"; }
success() { printf "  ${GREEN}\u2714${NC} %s\n" "$*"; }
warn()    { printf "  ${YELLOW}\u26A0${NC}  %s\n" "$*"; }
error()   { printf "  ${RED}\u2718${NC}  %s\n" "$*" >&2; }

KEEP_DATA=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --keep-data) KEEP_DATA=true; shift ;;
        *) warn "Unknown flag: $1"; shift ;;
    esac
done

printf "\n${BOLD}Anvil uninstaller${NC}\n\n"

# Find the binary
ANVIL_BIN="$(command -v anvil 2>/dev/null || true)"
if [[ -z "$ANVIL_BIN" ]]; then
    # Check common locations
    for candidate in /usr/local/bin/anvil "$HOME/.local/bin/anvil"; do
        if [[ -f "$candidate" ]]; then
            ANVIL_BIN="$candidate"
            break
        fi
    done
fi

ANVIL_HOME="${ANVIL_CONFIG_HOME:-$HOME/.anvil}"

echo "  This will remove:"
[[ -n "$ANVIL_BIN" ]] && echo "    Binary : ${ANVIL_BIN}"
if [[ "$KEEP_DATA" == "false" && -d "$ANVIL_HOME" ]]; then
    echo "    Data   : ${ANVIL_HOME} (vault, config, sessions)"
fi
echo ""

read -r -p "  Proceed? [y/N] " answer
case "$answer" in
    y|Y|yes|YES) ;;
    *) echo "  Cancelled."; exit 0 ;;
esac

ERRORS=0

# Remove binary
if [[ -n "$ANVIL_BIN" ]]; then
    if rm -f "$ANVIL_BIN" 2>/dev/null; then
        success "Removed ${ANVIL_BIN}"
    elif sudo rm -f "$ANVIL_BIN" 2>/dev/null; then
        success "Removed ${ANVIL_BIN} (via sudo)"
    else
        error "Could not remove ${ANVIL_BIN}"
        ERRORS=$((ERRORS + 1))
    fi
else
    warn "anvil binary not found on PATH"
fi

# Remove shell completions
for dir in \
    "$HOME/.local/share/bash-completion/completions/anvil" \
    "$HOME/.zfunc/_anvil" \
    "$HOME/.zsh/completions/_anvil" \
    "$HOME/.config/fish/completions/anvil.fish" \
    "/usr/local/share/anvil" \
    "/opt/homebrew/share/anvil"; do
    if [[ -e "$dir" ]]; then
        rm -rf "$dir" 2>/dev/null && info "Removed ${dir}" || true
    fi
done

# Remove data directory
if [[ "$KEEP_DATA" == "false" ]]; then
    if [[ -d "$ANVIL_HOME" ]]; then
        echo ""
        read -r -p "  Remove ${ANVIL_HOME} (vault + sessions)? [y/N] " data_answer
        case "$data_answer" in
            y|Y|yes|YES)
                if rm -rf "$ANVIL_HOME"; then
                    success "Removed ${ANVIL_HOME}"
                else
                    error "Could not remove ${ANVIL_HOME}"
                    ERRORS=$((ERRORS + 1))
                fi
                ;;
            *)
                info "Keeping ${ANVIL_HOME}."
                ;;
        esac
    fi
fi

echo ""
if [[ "$ERRORS" -eq 0 ]]; then
    success "Anvil uninstalled successfully."
else
    error "Uninstall completed with ${ERRORS} error(s). Some files may require manual removal."
    exit 1
fi
echo ""
