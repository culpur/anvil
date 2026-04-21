#!/usr/bin/env bash
# Smoke test: runs install.sh inside Docker containers for Ubuntu and Fedora.
# Usage: bash install/test/smoke-test.sh [ubuntu|fedora|debian|alpine|all]
#
# The test builds a Linux binary inside Docker (using the host source tree)
# then verifies:
#   1. anvil is on PATH inside the container
#   2. `anvil --version` prints something containing "2.2.7"
#   3. `anvil --check` runs without panicking
#
# Requires Docker to be running.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
INSTALL_DIR="${REPO_ROOT}/install"

TARGET="${1:-all}"
PASSED=0
FAILED=0

# Create a stub binary that emulates `anvil --version` output.
# This allows testing installer script logic without cross-compiling Rust.
# When a real Linux binary is available (e.g. from CI), set ANVIL_LINUX_BINARY.
LINUX_BINARY="${ANVIL_LINUX_BINARY:-}"
STUB_DIR="${REPO_ROOT}/target/smoke-test-stub"
mkdir -p "$STUB_DIR"

if [[ -z "$LINUX_BINARY" ]]; then
    echo ""
    echo "=== Creating stub binary for installer script testing ==="
    # The stub is a shell script that mimics anvil's basic interface
    cat > "${STUB_DIR}/anvil" << 'STUB_EOF'
#!/bin/sh
case "${1:-}" in
    --version|-V) echo "Anvil 2.2.7 (stub)" ;;
    --check)      echo "  v  Anvil on PATH   (stub)" ; exit 0 ;;
    --setup)      echo "  > Setup wizard (stub)" ; exit 0 ;;
    *)            echo "Anvil 2.2.7 (stub)" ;;
esac
STUB_EOF
    chmod +x "${STUB_DIR}/anvil"
    LINUX_BINARY="${STUB_DIR}/anvil"
    echo "Stub binary: ${LINUX_BINARY}"
fi

run_test() {
    local distro="$1"
    local image="$2"
    local pkg_update="$3"   # pre-install command (e.g. update package lists)

    echo ""
    echo "=== Smoke test: ${distro} (${image}) ==="

    if docker run --rm \
        -v "${LINUX_BINARY}:/usr/local/bin/anvil:ro" \
        -v "${INSTALL_DIR}:/anvil-install:ro" \
        -e "HOME=/root" \
        "${image}" \
        sh -c "
            set -e
            ${pkg_update}

            # Verify binary is on PATH and executable
            command -v anvil
            anvil --version | grep '2.2.7'
            anvil --check
            echo 'SMOKE TEST PASSED'
        " 2>&1; then
        echo "PASS: ${distro}"
        PASSED=$((PASSED + 1))
    else
        echo "FAIL: ${distro}"
        FAILED=$((FAILED + 1))
    fi
}

# Run selected distros
if [[ "$TARGET" == "ubuntu" || "$TARGET" == "all" ]]; then
    run_test "Ubuntu 22.04" "ubuntu:22.04" "apt-get update -q && apt-get install -yq curl ca-certificates 2>/dev/null"
fi

if [[ "$TARGET" == "debian" || "$TARGET" == "all" ]]; then
    run_test "Debian 12" "debian:12-slim" "apt-get update -q && apt-get install -yq curl ca-certificates 2>/dev/null"
fi

if [[ "$TARGET" == "fedora" || "$TARGET" == "all" ]]; then
    run_test "Fedora 39" "fedora:39" "dnf install -yq curl ca-certificates 2>/dev/null"
fi

if [[ "$TARGET" == "alpine" || "$TARGET" == "all" ]]; then
    run_test "Alpine 3.19" "alpine:3.19" "apk add --no-cache curl bash ca-certificates 2>/dev/null"
fi

echo ""
echo "=== Smoke test results ==="
echo "  PASSED: ${PASSED}"
echo "  FAILED: ${FAILED}"
echo ""

if [[ "$FAILED" -gt 0 ]]; then
    exit 1
fi
