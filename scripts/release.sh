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

# ─── Phase 1.5: Regenerate sha256 manifests ─────────────────────────────────
# Each binary gets a paired `<binary>.sha256` manifest. These are uploaded
# alongside the binary to GitHub releases AND served from anvilhub.culpur.net
# so `anvil upgrade` can verify downloads. If we forget to regenerate them
# after rebuilding, every user upgrading hits a sha256 mismatch (incident
# 2026-05-06: stale v2.2.8 manifests paired with v2.2.9 binaries).

echo
echo "▸ Phase 1.5: Regenerate sha256 manifests..."
for f in "$OUTPUT_DIR"/anvil-aarch64-apple-darwin "$OUTPUT_DIR"/anvil-x86_64-apple-darwin \
         "$OUTPUT_DIR"/anvil-aarch64-unknown-linux-gnu "$OUTPUT_DIR"/anvil-x86_64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-x86_64-pc-windows-gnu.exe; do
    if [ ! -f "$f" ]; then continue; fi
    name=$(basename "$f")
    shasum -a 256 "$f" | awk -v n="$name" '{print $1"  "n}' > "$f.sha256"
    echo "  ✓ $name.sha256 → $(awk '{print $1}' "$f.sha256" | head -c 16)…"
done

# ─── Phase 2: Test ───────────────────────────────────────────────────────────

echo
echo "▸ Phase 2: Verify binaries..."
for f in "$OUTPUT_DIR"/anvil-*; do
    name=$(basename "$f")
    size=$(ls -lh "$f" | awk '{print $5}')
    filetype=$(file -b "$f" | head -c 60)
    echo "  ✓ $name ($size) — $filetype"
done

# ─── Phase 2.5: Sanity-check binary↔manifest pairing ────────────────────────
# Refuses to release if any binary's actual hash doesn't match what's in
# its companion .sha256 file. Catches the failure mode where someone hand-
# edits a manifest, or a copy step drops the wrong file.

echo
echo "▸ Phase 2.5: Verify each binary's hash matches its manifest..."
for f in "$OUTPUT_DIR"/anvil-aarch64-apple-darwin "$OUTPUT_DIR"/anvil-x86_64-apple-darwin \
         "$OUTPUT_DIR"/anvil-aarch64-unknown-linux-gnu "$OUTPUT_DIR"/anvil-x86_64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-x86_64-pc-windows-gnu.exe; do
    if [ ! -f "$f" ]; then continue; fi
    actual=$(shasum -a 256 "$f" | awk '{print $1}')
    expected=$(awk '{print $1}' "$f.sha256")
    if [ "$actual" != "$expected" ]; then
        echo "  ✗ $(basename "$f"): actual=$actual manifest=$expected — ABORTING release"
        exit 1
    fi
    echo "  ✓ $(basename "$f")"
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
curl -LO https://github.com/culpur/anvil/releases/download/$TAG/anvil-\$(uname -m)-\$(uname -s | tr A-Z a-z)
chmod +x anvil-*
sudo mv anvil-* /usr/local/bin/anvil
\`\`\`

### Built locally via Culpur CI/CD pipeline (Docker cross-compilation)."

# Create or update release on the PUBLIC repo (culpur/anvil) — that's where
# users download from. The private culpur/anvil-source repo only holds source
# code; binaries don't go there. Always pass --repo explicitly so this never
# silently follows whichever remote the cwd happens to track.
PUBLIC_REPO="culpur/anvil"
if gh release view "$TAG" --repo "$PUBLIC_REPO" >/dev/null 2>&1; then
    echo "  Release $TAG exists on $PUBLIC_REPO — uploading assets..."
    gh release upload "$TAG" --repo "$PUBLIC_REPO" "$OUTPUT_DIR"/anvil-* --clobber
else
    echo "  Creating release $TAG on $PUBLIC_REPO..."
    gh release create "$TAG" \
        --repo "$PUBLIC_REPO" \
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
echo "▸ Phase 6: Update version configs..."
# AnvilHub config.ts — update version (if accessible via SSH)
ssh -p 30022 -i ~/.ssh/id_ed25519_guard soulofall@guard.armored.ninja \
    "ssh dev0001 'sed -i \"s/version: \\\"[0-9.]*\\\"/version: \\\"$VERSION\\\"/\" /opt/projects/anvilhub/packages/web/src/lib/anvil-config.ts'" 2>/dev/null \
    && echo "  ✓ AnvilHub config.ts updated" \
    || echo "  ⚠ AnvilHub config.ts update skipped (SSH not available)"

# WordPress shortcodes — update ANVIL_VERSION constant
ssh -p 30022 -i ~/.ssh/id_ed25519_guard soulofall@guard.armored.ninja \
    "ssh 10.0.70.6 'sudo docker exec wordpress-wordpress-1 sed -i \"s/ANVIL_VERSION.*,.*/ANVIL_VERSION\\\", \\\"$VERSION\\\");/\" /var/www/html/wp-content/mu-plugins/culpur-hardening.php && sudo docker exec wordpress-wordpress-1 rm -rf /var/www/html/wp-content/wphb-cache/'" 2>/dev/null \
    && echo "  ✓ WordPress shortcodes updated + cache cleared" \
    || echo "  ⚠ WordPress update skipped (SSH not available)"

# GitHub README — update ONLY the version badge.
#
# Past bug: the previous version of this step ran a second sed that
# globally replaced every vX.Y.Z token in the README with the new version,
# silently rewriting every changelog entry to the latest version on every
# release. The changelog must be edited by humans (or an explicit
# changelog-prepend tool) — never by find/replace.
README_SHA=$(gh api repos/culpur/anvil/contents/README.md --jq '.sha' 2>/dev/null)
if [ -n "$README_SHA" ]; then
    README_CONTENT=$(gh api repos/culpur/anvil/contents/README.md --jq '.content' | base64 -d)
    # Badge looks like: version-2.2.10-0FBCFF — only that token gets rewritten.
    UPDATED_README=$(echo "$README_CONTENT" | sed "s/version-[0-9.]*-/version-$VERSION-/g")

    # Sanity: refuse to push if more than the badge changed.
    DIFF_LINES=$(diff <(echo "$README_CONTENT") <(echo "$UPDATED_README") | grep -c '^[<>]' || true)
    if [ "$DIFF_LINES" -gt 2 ]; then
        echo "  ⚠ README badge update would change $DIFF_LINES lines — refusing to push (would mangle changelog)" >&2
    elif [ "$UPDATED_README" = "$README_CONTENT" ]; then
        echo "  ✓ GitHub README badge already at $TAG (no change)"
    else
        ENCODED=$(echo "$UPDATED_README" | base64 | tr -d '\n')
        gh api repos/culpur/anvil/contents/README.md \
            -X PUT -f message="docs: bump version badge to $TAG" \
            -f content="$ENCODED" -f sha="$README_SHA" --jq '.commit.sha' 2>/dev/null
        echo "  ✓ GitHub README badge updated to $TAG"
    fi
fi

echo
echo "▸ Phase 7: Generate release notes draft..."
PREV_TAG=$(git describe --tags --abbrev=0 "$TAG^" 2>/dev/null || echo "")
if [ -n "$PREV_TAG" ]; then
    echo "  Changes since $PREV_TAG:"
    git log --oneline "$PREV_TAG..$TAG" | head -20
    echo
    echo "  ── Draft changelog entry ──"
    echo "  v${VERSION} — $(date +%B\ %d,\ %Y)"
    git log --oneline "$PREV_TAG..$TAG" | sed 's/^[a-f0-9]* /  - /'
    echo
fi

echo "╔══════════════════════════════════════════════╗"
echo "║  ✓ Release complete: Anvil $TAG              ║"
echo "║                                              ║"
echo "║  Binaries:  $OUTPUT_DIR/                     ║"
echo "║  GitHub:    https://github.com/culpur/anvil/releases/tag/$TAG"
echo "║  Brew:      brew upgrade anvil               ║"
echo "║                                              ║"
echo "║  MANUAL STEPS REMAINING:                     ║"
echo "║  1. Review + edit changelog on AnvilHub about ║"
echo "║  2. Update feature descriptions if needed     ║"
echo "║  3. Update docs/usage page if commands added  ║"
echo "║  4. Post to marketing channels if major       ║"
echo "╚══════════════════════════════════════════════╝"
