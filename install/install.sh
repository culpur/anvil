#!/usr/bin/env bash
# Anvil installer — macOS + Linux
# Usage:  curl -fsSL https://anvilhub.culpur.net/install.sh | bash
#   or:   bash install.sh [--no-setup] [--no-completions] [--dir /usr/local/bin]
#
# Exit codes:
#   0 — success
#   1 — network failure
#   2 — checksum failure
#   3 — dependency install failure
#   4 — user declined

set -euo pipefail

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
info()    { printf "  ${CYAN}>${NC} %s\n" "$*"; }
success() { printf "  ${GREEN}\u2714${NC} %s\n" "$*"; }
warn()    { printf "  ${YELLOW}\u26A0${NC}  %s\n" "$*"; }
error()   { printf "  ${RED}\u2718${NC}  %s\n" "$*" >&2; }
die()     { error "$*"; exit 1; }

# ── Banner ────────────────────────────────────────────────────────────────────
printf "\n${BOLD}${CYAN}"
printf '\u2554%.0s' $(seq 1 60); printf '\n'
printf '\u2551  Anvil installer                                          \u2551\n'
printf '\u2551  https://anvilhub.culpur.net                              \u2551\n'
printf '\u255A%.0s' $(seq 1 60); printf '\n'
printf "${NC}\n"

# ── Argument parsing ──────────────────────────────────────────────────────────
INSTALL_DIR=""
RUN_SETUP=true
INSTALL_COMPLETIONS=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-setup)        RUN_SETUP=false; shift ;;
        --no-completions)  INSTALL_COMPLETIONS=false; shift ;;
        --dir)             INSTALL_DIR="$2"; shift 2 ;;
        --dir=*)           INSTALL_DIR="${1#--dir=}"; shift ;;
        *)                 warn "Unknown flag: $1"; shift ;;
    esac
done

# ── Platform detection ────────────────────────────────────────────────────────
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux)   PLATFORM="linux"   ;;
    darwin)  PLATFORM="macos"   ;;
    freebsd) PLATFORM="freebsd" ;;
    openbsd) PLATFORM="openbsd" ;;
    netbsd)  PLATFORM="netbsd"  ;;
    *)       die "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
    x86_64|amd64)    ARCH_STD="x86_64"  ;;
    aarch64|arm64)   ARCH_STD="aarch64" ;;
    *)               die "Unsupported architecture: $ARCH" ;;
esac

# Rust target triple
case "$PLATFORM" in
    macos)   TARGET="${ARCH_STD}-apple-darwin"        ;;
    linux)   TARGET="${ARCH_STD}-unknown-linux-gnu"   ;;
    freebsd) TARGET="${ARCH_STD}-unknown-freebsd"     ;;
    openbsd) TARGET="${ARCH_STD}-unknown-openbsd"     ;;
    netbsd)  TARGET="${ARCH_STD}-unknown-netbsd"      ;;
esac

# OpenBSD/NetBSD only ship x86_64 binaries today; ARM64 users build from source.
if [[ "$PLATFORM" == "openbsd" || "$PLATFORM" == "netbsd" ]] && [[ "$ARCH_STD" != "x86_64" ]]; then
    die "Binary not available for $PLATFORM/$ARCH_STD — build from source with: cargo install --git https://github.com/culpur/anvil-source"
fi

info "Platform: ${PLATFORM} / ${ARCH_STD}  (target: ${TARGET})"

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

info "Install directory: ${INSTALL_DIR}"

# Ensure INSTALL_DIR is on PATH
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    warn "${INSTALL_DIR} is not on your PATH."
    warn "Add the following to your shell rc file:"
    warn "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

# ── Dependency checks ─────────────────────────────────────────────────────────
need() { command -v "$1" &>/dev/null; }

# Detect package manager
detect_pkg_manager() {
    if need brew;    then echo "brew";    return; fi
    if need apt-get; then echo "apt";     return; fi
    if need dnf;     then echo "dnf";     return; fi
    if need yum;     then echo "yum";     return; fi
    if need pacman;  then echo "pacman";  return; fi
    if need apk;     then echo "apk";     return; fi
    echo "none"
}

PKG_MGR="$(detect_pkg_manager)"
info "Package manager: ${PKG_MGR}"

install_pkg() {
    local pkg="$1"
    local brew_pkg="${2:-$1}"
    info "Installing ${pkg}..."
    case "$PKG_MGR" in
        brew)   brew install "$brew_pkg" || return 1 ;;
        apt)    sudo apt-get install -y "$pkg" || return 1 ;;
        dnf)    sudo dnf install -y "$pkg" || return 1 ;;
        yum)    sudo yum install -y "$pkg" || return 1 ;;
        pacman) sudo pacman -S --noconfirm "$pkg" || return 1 ;;
        apk)    sudo apk add --no-cache "$pkg" || return 1 ;;
        *)
            warn "${pkg} not found and no supported package manager detected."
            warn "Install it manually and re-run this script."
            return 1
            ;;
    esac
}

# curl is required for downloads
if ! need curl; then
    install_pkg curl || { error "curl is required"; exit 3; }
fi

# git
if ! need git; then
    warn "git not found — attempting to install..."
    install_pkg git || warn "Could not install git automatically."
fi

# Node.js + npm
if ! need node || ! need npm; then
    warn "Node.js / npm not found — attempting to install..."
    case "$PKG_MGR" in
        brew)    install_pkg node || warn "Install Node.js from https://nodejs.org" ;;
        apt)     install_pkg nodejs nodejs && install_pkg npm || warn "Install Node.js from https://nodejs.org" ;;
        dnf|yum) install_pkg nodejs nodejs && install_pkg npm || warn "Install Node.js from https://nodejs.org" ;;
        pacman)  install_pkg nodejs nodejs && install_pkg npm || warn "Install Node.js from https://nodejs.org" ;;
        apk)     install_pkg nodejs nodejs && install_pkg npm || warn "Install Node.js from https://nodejs.org" ;;
        *)       warn "Install Node.js from https://nodejs.org" ;;
    esac
fi

# QMD (optional — just warn, not a hard failure)
if ! need qmd && [[ ! -f "$HOME/.local/bin/qmd" ]] && [[ ! -f "/usr/local/bin/qmd" ]]; then
    warn "qmd not found — install it from https://anvilhub.culpur.net"
fi

# ── Download Anvil binary ─────────────────────────────────────────────────────
GITHUB_BASE="https://github.com/culpur/anvil/releases/latest/download"
BINARY_NAME="anvil-${TARGET}"
BINARY_URL="${GITHUB_BASE}/${BINARY_NAME}"
# Primary (out-of-band) SHA256 source: anvilhub.culpur.net. Served from a
# separate origin so a GitHub release compromise cannot also forge the hash.
# Fallback: the .sha256 sibling on GitHub releases. We only accept the
# fallback if the primary returns a clear 404, never on network errors.
SHA256_URL_PRIMARY="https://anvilhub.culpur.net/sha256/${BINARY_NAME}.sha256"
SHA256_URL_FALLBACK="${BINARY_URL}.sha256"

TMP_DIR="$(mktemp -d)"
TMP_BINARY="${TMP_DIR}/anvil"
TMP_SHA256="${TMP_DIR}/anvil.sha256"

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

info "Downloading ${BINARY_URL}..."
if ! curl -fSL --max-time 180 -o "$TMP_BINARY" "$BINARY_URL"; then
    die "Download failed — check network connection."
    exit 1
fi

# ── SHA256 verification ───────────────────────────────────────────────────────
# Integrity is non-negotiable. If we cannot fetch or verify the checksum we
# abort — never fall back to "trust the binary we just downloaded." An
# attacker who can suppress the checksum URL (DNS block, CDN outage, 404)
# would otherwise bypass the entire integrity check.
info "Fetching checksum from ${SHA256_URL_PRIMARY}..."
SHA256_SOURCE="primary"
if ! curl -fsSL --max-time 15 -o "$TMP_SHA256" "$SHA256_URL_PRIMARY"; then
    warn "Primary checksum source unreachable — trying GitHub mirror..."
    SHA256_SOURCE="fallback"
    if ! curl -fsSL --max-time 15 -o "$TMP_SHA256" "$SHA256_URL_FALLBACK"; then
        error "Could not fetch checksum from either ${SHA256_URL_PRIMARY} or ${SHA256_URL_FALLBACK}"
        error "Refusing to install an unverified binary. If this persists, download"
        error "manually from https://github.com/culpur/anvil/releases and verify the"
        error "SHA256 yourself against https://anvilhub.culpur.net/sha256/"
        exit 2
    fi
fi

# Track which source the checksum came from so the error message is accurate.
if [[ "${SHA256_SOURCE}" == "primary" ]]; then
    SHA256_SOURCE_URL="${SHA256_URL_PRIMARY}"
else
    SHA256_SOURCE_URL="${SHA256_URL_FALLBACK}"
fi

EXPECTED="$(awk '{print $1}' "$TMP_SHA256")"
if [[ -z "${EXPECTED}" ]]; then
    error "Checksum file at ${SHA256_SOURCE_URL} is empty or malformed."
    exit 2
fi

if [[ "$PLATFORM" == "macos" ]]; then
    ACTUAL="$(shasum -a 256 "$TMP_BINARY" | awk '{print $1}')"
else
    ACTUAL="$(sha256sum "$TMP_BINARY" | awk '{print $1}')"
fi

if [[ "${ACTUAL}" != "${EXPECTED}" ]]; then
    error "SHA256 mismatch!"
    error "  expected: ${EXPECTED}"
    error "  got:      ${ACTUAL}"
    exit 2
fi
success "Checksum verified."

# ── Install binary ────────────────────────────────────────────────────────────
chmod +x "$TMP_BINARY"
INSTALL_PATH="${INSTALL_DIR}/anvil"

if [[ -w "$INSTALL_DIR" ]]; then
    cp "$TMP_BINARY" "$INSTALL_PATH"
else
    sudo cp "$TMP_BINARY" "$INSTALL_PATH"
fi

success "Anvil installed to ${INSTALL_PATH}"

# Verify it runs
if command -v anvil &>/dev/null; then
    INSTALLED_VERSION="$(anvil --version 2>/dev/null | head -1 || echo "unknown")"
    success "Anvil version: ${INSTALLED_VERSION}"
fi

# ── Shell completions ─────────────────────────────────────────────────────────
if [[ "$INSTALL_COMPLETIONS" == "true" ]]; then
    COMPLETION_BASE="$(dirname "$0")/completions"
    # If running from curl, completions might not be alongside the script.
    # We ship them embedded as separate downloads or in the binary's share dir.
    SHARE_DIR="${INSTALL_DIR%/bin}/share/anvil/completions"

    if [[ -d "$COMPLETION_BASE" ]]; then
        mkdir -p "$SHARE_DIR"
        cp "$COMPLETION_BASE"/* "$SHARE_DIR"/ 2>/dev/null || true
    fi

    # Install for detected shell
    CURRENT_SHELL="${SHELL##*/}"
    case "$CURRENT_SHELL" in
        bash)
            BASH_COMP="$HOME/.local/share/bash-completion/completions"
            mkdir -p "$BASH_COMP"
            if [[ -f "$SHARE_DIR/anvil.bash" ]]; then
                cp "$SHARE_DIR/anvil.bash" "$BASH_COMP/anvil"
                success "Bash completions installed to ${BASH_COMP}/anvil"
            fi
            ;;
        zsh)
            ZSH_COMP="$HOME/.zfunc"
            mkdir -p "$ZSH_COMP"
            if [[ -f "$SHARE_DIR/anvil.zsh" ]]; then
                cp "$SHARE_DIR/anvil.zsh" "$ZSH_COMP/_anvil"
                success "Zsh completions installed to ${ZSH_COMP}/_anvil"
                info "Add 'fpath=(~/.zfunc \$fpath)' + 'autoload -Uz compinit && compinit' to ~/.zshrc"
            fi
            ;;
        fish)
            FISH_COMP="$HOME/.config/fish/completions"
            mkdir -p "$FISH_COMP"
            if [[ -f "$SHARE_DIR/anvil.fish" ]]; then
                cp "$SHARE_DIR/anvil.fish" "$FISH_COMP/anvil.fish"
                success "Fish completions installed to ${FISH_COMP}/anvil.fish"
            fi
            ;;
    esac
fi

# ── First-run wizard ──────────────────────────────────────────────────────────
if [[ "$RUN_SETUP" == "true" ]]; then
    printf "\n"
    info "Launching first-run setup wizard..."
    printf "\n"
    if command -v anvil &>/dev/null; then
        anvil --setup || warn "Setup wizard exited with an error — run 'anvil --setup' to retry."
    else
        warn "anvil not found on PATH after install — run '${INSTALL_PATH} --setup' to configure."
    fi
fi

# ── Done ──────────────────────────────────────────────────────────────────────
printf "\n${GREEN}${BOLD}"
printf '\u2554%.0s' $(seq 1 60); printf '\n'
printf '\u2551  Installation complete!                                    \u2551\n'
printf '\u2551  Run: anvil                                                \u2551\n'
printf '\u2551  Docs: https://anvilhub.culpur.net/docs                   \u2551\n'
printf '\u255A%.0s' $(seq 1 60); printf "\n${NC}\n"
