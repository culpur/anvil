#!/bin/bash
# Quick local build + install for the current platform
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 'version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
echo "Building Anvil v${VERSION} (local)..."
cargo build --release 2>&1 | tail -3
cp target/release/anvil /opt/homebrew/bin/anvil 2>/dev/null || \
    cp target/release/anvil /usr/local/bin/anvil 2>/dev/null || \
    cp target/release/anvil ~/bin/anvil
echo "Installed: $(anvil --version 2>&1 | head -2 | tail -1)"
