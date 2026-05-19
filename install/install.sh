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

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-setup)        RUN_SETUP=false; shift ;;
        --no-completions)  INSTALL_COMPLETIONS=false; shift ;;
        --dir)             INSTALL_DIR="$2"; shift 2 ;;
        --dir=*)           INSTALL_DIR="${1#--dir=}"; shift ;;
        --verbose|-v)      VERBOSE=true; shift ;;
        --quiet|-q)        QUIET=true; shift ;;
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

# BSD support matrix:
#   - FreeBSD x86_64: shipped binary (Tier-2)
#   - FreeBSD ARM64:  source-only (no rust-std)
#   - NetBSD x86_64:  shipped binary (Tier-3)
#   - OpenBSD x86_64: source-only
#   - All other BSD arch combos: source-only.
if [[ "$PLATFORM" == "openbsd" ]]; then
    die "OpenBSD binary not available — build from source: cargo install --git https://github.com/culpur/anvil-source"
fi
if [[ "$PLATFORM" == "freebsd" && "$ARCH_STD" == "aarch64" ]]; then
    die "FreeBSD ARM64 binary not available — build from source: cargo install --git https://github.com/culpur/anvil-source"
fi
if [[ "$PLATFORM" == "netbsd" && "$ARCH_STD" != "x86_64" ]]; then
    die "Binary not available for netbsd/$ARCH_STD — build from source: cargo install --git https://github.com/culpur/anvil-source"
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

# ── Curl (required) ───────────────────────────────────────────────────────────
# curl is on every modern macOS and almost every Linux distro out of the box.
# If it's missing, we cannot proceed.
if ! command -v curl &>/dev/null; then
    die "curl is required but not installed. Install curl, then re-run."
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

step "Downloading Anvil for ${PLATFORM} ${ARCH_STD}…"
if ! curl -fSL --max-time 180 -o "$TMP_BINARY" "$BINARY_URL" >/dev/null 2>&1; then
    step_end
    die "Download failed: $BINARY_URL"
fi

# ── SHA256 verification (mandatory) ───────────────────────────────────────────
# Out-of-band primary source on anvilhub.culpur.net; GitHub mirror is fallback.
# We never trust the binary without a verified checksum.
step "Verifying signature…"
# 5s timeout on the primary so an unreachable anvilhub mirror fails fast
# to the GitHub fallback rather than hanging the install for 30+ seconds.
# Fallback gets 15s — by the time we're there we KNOW the primary is
# unreachable and the GitHub release endpoint is our last hope.
SHA256_SOURCE="primary"
if ! curl -fsSL --max-time 5 -o "$TMP_SHA256" "$SHA256_URL_PRIMARY" 2>/dev/null; then
    SHA256_SOURCE="fallback"
    if ! curl -fsSL --max-time 15 -o "$TMP_SHA256" "$SHA256_URL_FALLBACK" 2>/dev/null; then
        step_end
        die "Could not fetch checksum. Refusing to install an unverified binary."
    fi
fi

EXPECTED="$(awk '{print $1}' "$TMP_SHA256")"
if [[ -z "${EXPECTED}" ]]; then
    step_end
    die "Checksum file is empty or malformed."
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
    die "No SHA256 tool found (need one of: sha256sum, shasum, sha256, cksum)."
fi

if [[ "${ACTUAL}" != "${EXPECTED}" ]]; then
    step_end
    die "SHA256 mismatch. expected=${EXPECTED} got=${ACTUAL}"
fi

# ── Install binary ────────────────────────────────────────────────────────────
chmod +x "$TMP_BINARY"
INSTALL_PATH="${INSTALL_DIR}/anvil"

step "Installing to ${INSTALL_PATH}…"
if [[ -w "$INSTALL_DIR" ]]; then
    cp "$TMP_BINARY" "$INSTALL_PATH"
else
    # Need sudo. Tell the user once, in line, so the password prompt isn't
    # mystery-shell behavior.
    step_end
    printf "Installing to %s requires sudo:\n" "$INSTALL_PATH"
    sudo cp "$TMP_BINARY" "$INSTALL_PATH" || die "Could not install to $INSTALL_PATH"
fi

# ── PATH hint (only if actually needed, and only once) ────────────────────────
PATH_HINT=""
if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
    PATH_HINT="${INSTALL_DIR} is not on your PATH. Add this to your shell rc:
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
            printf "Installed: %s. Run \`anvil\` from a TTY to complete setup.\n" "$INSTALL_PATH"
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
        die "Installed anvil to $INSTALL_PATH but cannot execute it."
    fi
fi

# RUN_SETUP=false path — print a single line telling the user what to run.
# --quiet path: be even quieter (one line, just the install path).
if $QUIET; then
    printf "%s\n" "$INSTALL_PATH"
else
    printf "Installed: %s\nRun: anvil\n" "$INSTALL_PATH"
fi
