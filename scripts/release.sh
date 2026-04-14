#!/bin/bash
# Anvil Release Pipeline — Local CI/CD
# Builds all platform binaries and publishes to GitHub Releases
#
# Usage:
#   ./scripts/release.sh              # Build + release current version
#   ./scripts/release.sh --build-only # Build without pushing release
#   ./scripts/release.sh --skip-build # Skip build, upload existing binaries
#
# Requires: cargo, docker, gh (GitHub CLI), rustup
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Parse version from Cargo.toml
VERSION=$(grep -m1 'version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
TAG="v${VERSION}"

BUILD_ONLY=false
SKIP_BUILD=false
for arg in "$@"; do
    case "$arg" in
        --build-only) BUILD_ONLY=true ;;
        --skip-build) SKIP_BUILD=true ;;
    esac
done

echo "╔══════════════════════════════════════════════╗"
echo "║  Anvil Release Pipeline — v${VERSION}              ║"
echo "╚══════════════════════════════════════════════╝"
echo

TARGETS=(
    "aarch64-apple-darwin"
    "x86_64-apple-darwin"
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-pc-windows-gnu"
)
OUTPUT_DIR="$PROJECT_DIR/target/release-artifacts"
mkdir -p "$OUTPUT_DIR"

# ─── Phase 1: Build ──────────────────────────────────────────────────────────

if [ "$SKIP_BUILD" = false ]; then
    echo "▸ Phase 1: Building all targets..."
    echo

    # 1a. macOS ARM (native)
    echo "  [1/5] macOS ARM (aarch64-apple-darwin)..."
    cargo build --release --target aarch64-apple-darwin 2>&1 | tail -1
    cp target/aarch64-apple-darwin/release/anvil "$OUTPUT_DIR/anvil-aarch64-apple-darwin"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-aarch64-apple-darwin" | awk '{print $5}')"

    # 1b. macOS Intel (native cross)
    echo "  [2/5] macOS Intel (x86_64-apple-darwin)..."
    rustup target add x86_64-apple-darwin 2>/dev/null || true
    cargo build --release --target x86_64-apple-darwin 2>&1 | tail -1
    cp target/x86_64-apple-darwin/release/anvil "$OUTPUT_DIR/anvil-x86_64-apple-darwin"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-apple-darwin" | awk '{print $5}')"

    # 1c. Linux x86_64 (Docker)
    echo "  [3/5] Linux x86_64 (x86_64-unknown-linux-gnu)..."
    docker build --platform linux/amd64 -t anvil-builder-linux -f - . 2>/dev/null << 'DOCKERFILE'
FROM --platform=linux/amd64 rust:1.94-slim-bookworm
RUN apt-get update && apt-get install -y pkg-config libssl-dev gcc-aarch64-linux-gnu g++-aarch64-linux-gnu && rm -rf /var/lib/apt/lists/*
RUN rustup target add aarch64-unknown-linux-gnu
WORKDIR /build
DOCKERFILE
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" anvil-builder-linux \
        cargo build --release --target x86_64-unknown-linux-gnu 2>&1 | tail -1
    cp target/x86_64-unknown-linux-gnu/release/anvil "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu" | awk '{print $5}')"

    # 1d. Linux ARM64 (Docker cross)
    echo "  [4/5] Linux ARM64 (aarch64-unknown-linux-gnu)..."
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" \
        -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
        -e CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
        -e CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
        anvil-builder-linux \
        cargo build --release --target aarch64-unknown-linux-gnu 2>&1 | tail -1
    cp target/aarch64-unknown-linux-gnu/release/anvil "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu" | awk '{print $5}')"

    # 1e. Windows x86_64 (Docker cross with mingw)
    echo "  [5/5] Windows x86_64 (x86_64-pc-windows-gnu)..."
    docker build --platform linux/amd64 -t anvil-builder-win -f - . 2>/dev/null << 'DOCKERFILE'
FROM --platform=linux/amd64 rust:1.94-slim-bookworm
RUN apt-get update && apt-get install -y gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-gnu
WORKDIR /build
DOCKERFILE
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" \
        -e CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
        anvil-builder-win \
        cargo build --release --target x86_64-pc-windows-gnu 2>&1 | tail -1
    cp target/x86_64-pc-windows-gnu/release/anvil.exe "$OUTPUT_DIR/anvil-x86_64-pc-windows-gnu.exe"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-pc-windows-gnu.exe" | awk '{print $5}')"

    echo
    echo "▸ Build complete:"
    ls -lh "$OUTPUT_DIR"/anvil-*
fi

# ─── Phase 2: Test ───────────────────────────────────────────────────────────

echo
echo "▸ Phase 2: Verify binaries..."
for f in "$OUTPUT_DIR"/anvil-*; do
    name=$(basename "$f")
    size=$(ls -lh "$f" | awk '{print $5}')
    filetype=$(file -b "$f" | head -c 60)
    echo "  ✓ $name ($size) — $filetype"
done

if [ "$BUILD_ONLY" = true ]; then
    echo
    echo "▸ Build-only mode — skipping release."
    echo "  Artifacts in: $OUTPUT_DIR/"
    exit 0
fi

# ─── Phase 3: Git Tag ────────────────────────────────────────────────────────

echo
echo "▸ Phase 3: Git tag..."
if git tag -l "$TAG" | grep -q "$TAG"; then
    echo "  Tag $TAG already exists"
else
    git tag -a "$TAG" -m "Anvil $TAG"
    git push origin "$TAG"
    echo "  Created and pushed tag $TAG"
fi

# ─── Phase 4: GitHub Release ─────────────────────────────────────────────────

echo
echo "▸ Phase 4: GitHub Release..."

NOTES="## Anvil $TAG

### Downloads
| Platform | Binary |
|----------|--------|
| macOS ARM (M1/M2/M3) | \`anvil-aarch64-apple-darwin\` |
| macOS Intel | \`anvil-x86_64-apple-darwin\` |
| Linux x86_64 | \`anvil-x86_64-unknown-linux-gnu\` |
| Linux ARM64 | \`anvil-aarch64-unknown-linux-gnu\` |
| Windows x86_64 | \`anvil-x86_64-pc-windows-gnu.exe\` |

### Installation
\`\`\`bash
# macOS/Linux
curl -LO https://github.com/culpur/anvil-source/releases/download/$TAG/anvil-\$(uname -m)-\$(uname -s | tr A-Z a-z)
chmod +x anvil-*
sudo mv anvil-* /usr/local/bin/anvil
\`\`\`

### Built locally via Culpur CI/CD pipeline (Docker cross-compilation)."

# Create or update release
if gh release view "$TAG" >/dev/null 2>&1; then
    echo "  Release $TAG exists — uploading assets..."
    gh release upload "$TAG" "$OUTPUT_DIR"/anvil-* --clobber
else
    gh release create "$TAG" \
        --title "Anvil $TAG" \
        --notes "$NOTES" \
        "$OUTPUT_DIR"/anvil-*
fi

echo
echo "▸ Phase 5: Update Homebrew formula..."
ARM_MAC=$(shasum -a 256 "$OUTPUT_DIR/anvil-aarch64-apple-darwin" | awk '{print $1}')
INTEL_MAC=$(shasum -a 256 "$OUTPUT_DIR/anvil-x86_64-apple-darwin" | awk '{print $1}')
ARM_LINUX=$(shasum -a 256 "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu" | awk '{print $1}')
X86_LINUX=$(shasum -a 256 "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu" | awk '{print $1}')

BREW_SHA=$(gh api repos/culpur/homebrew-anvil/contents/Formula/anvil.rb --jq '.sha' 2>/dev/null)
if [ -n "$BREW_SHA" ]; then
    cat > /tmp/anvil-brew.rb << BREWEOF
class Anvil < Formula
  desc "AI coding assistant with typed credential vault, live remote control, 5 providers"
  homepage "https://culpur.net/anvil"
  version "$VERSION"
  license "Proprietary"
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/culpur/anvil/releases/download/$TAG/anvil-aarch64-apple-darwin"
      sha256 "$ARM_MAC"
    else
      url "https://github.com/culpur/anvil/releases/download/$TAG/anvil-x86_64-apple-darwin"
      sha256 "$INTEL_MAC"
    end
  end
  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/culpur/anvil/releases/download/$TAG/anvil-aarch64-unknown-linux-gnu"
      sha256 "$ARM_LINUX"
    else
      url "https://github.com/culpur/anvil/releases/download/$TAG/anvil-x86_64-unknown-linux-gnu"
      sha256 "$X86_LINUX"
    end
  end
  def install
    downloaded = Dir["anvil-*"].first || "anvil"
    bin.install downloaded => "anvil"
  end
  test do
    assert_match "Anvil CLI", shell_output("#{bin}/anvil --version")
  end
end
BREWEOF
    BREW_CONTENT=$(base64 -i /tmp/anvil-brew.rb | tr -d '\n')
    gh api repos/culpur/homebrew-anvil/contents/Formula/anvil.rb \
        -X PUT -f message="formula: bump to $TAG" \
        -f content="$BREW_CONTENT" -f sha="$BREW_SHA" \
        --jq '.commit.sha' 2>/dev/null
    echo "  ✓ Homebrew formula updated to $TAG"
else
    echo "  ⚠ Could not update Homebrew (missing sha)"
fi

echo
echo "╔══════════════════════════════════════════════╗"
echo "║  ✓ Release complete: Anvil $TAG              ║"
echo "║  Binary: $OUTPUT_DIR/                        ║"
echo "║  GitHub: https://github.com/culpur/anvil/releases/tag/$TAG"
echo "║  Brew:   brew upgrade anvil                  ║"
echo "╚══════════════════════════════════════════════╝"
