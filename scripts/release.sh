#!/bin/bash
# Anvil Release Pipeline — Local CI/CD
# Builds all platform binaries and publishes to GitHub Releases
#
# Usage:
#   ./scripts/release.sh                  # Build + release current version
#   ./scripts/release.sh --build-only     # Build without pushing release
#   ./scripts/release.sh --skip-build     # Skip build, upload existing binaries
#   ./scripts/release.sh --skip-verify    # Skip post-publish verification gate
#   ./scripts/release.sh --dry-run-verify # Print verification plan, no network
#
# Requires: cargo, docker, gh (GitHub CLI), rustup, jq
set -euo pipefail

# Ensure rustup's rustc takes precedence over any system Rust (brew, distro pkgs).
# Without this, multi-target cross-compile fails with "can't find crate for 'core'"
# because brew's rustc has no cross-compile std libs. (v2.2.15 incident.)
export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Parse version from Cargo.toml
VERSION=$(grep -m1 'version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
TAG="v${VERSION}"

BUILD_ONLY=false
SKIP_BUILD=false
SKIP_VERIFY=false
DRY_RUN_VERIFY=false
for arg in "$@"; do
    case "$arg" in
        --build-only)     BUILD_ONLY=true ;;
        --skip-build)     SKIP_BUILD=true ;;
        --skip-verify)    SKIP_VERIFY=true ;;
        --dry-run-verify) DRY_RUN_VERIFY=true ;;
    esac
done

# Source the post-publish verification helper (modular per
# feedback-anvil-main-rs-modularity — release.sh itself stays focused on
# orchestration, the verify logic lives in its own file).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/release-helpers/verify-release.sh"
# Source the AnvilHub sha256-manifest publisher. release.sh used to leave
# /sha256/<version>.txt unpublished, which broke /api/version's advertised
# sha256_url (#620, v2.2.16 incident). The helper assembles + uploads the
# combined manifest after Phase 4.
# shellcheck disable=SC1091
. "$SCRIPT_DIR/release-helpers/upload-sha256-manifest.sh"

echo "╔══════════════════════════════════════════════╗"
echo "║  Anvil Release Pipeline — v${VERSION}              ║"
echo "╚══════════════════════════════════════════════╝"
echo

# ─── Phase 0: Pre-flight (T1-A) ──────────────────────────────────────────────
#
# Past bug: v2.2.11 was tagged at 6e9d518, then a build.rs fix landed at
# 9617d07 — but the tag was never moved, so the released binaries reported
# the wrong SHA. A pre-flight check would have caught this in seconds.
#
# This phase aborts release if ANY of these are true:
#   1. Working tree has uncommitted changes (release.sh would build from a
#      different state than what's tagged).
#   2. A local tag $TAG already exists at a commit OTHER than HEAD.
#   3. The remote tag $TAG exists at a commit OTHER than the local tag.
#   4. We're not on a branch tip — release.sh expects to tag HEAD.
echo "▸ Phase 0: Pre-flight checks..."

# 0.1 — uncommitted changes
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "  ✘ Working tree has uncommitted changes."
    echo "    Run 'git status' to see what's pending. Either commit or stash."
    exit 1
fi
echo "  ✓ Working tree is clean"

# 0.2 — fetch remote tags so the remote-tag check sees the truth
git fetch --tags --quiet || {
    echo "  ⚠ Could not fetch tags (offline?). Continuing with local view only."
}

CURRENT_HEAD=$(git rev-parse HEAD)

# 0.3 — local tag must point at HEAD if it exists
if git tag -l "$TAG" | grep -q "^${TAG}$"; then
    LOCAL_TAG_SHA=$(git rev-list -n1 "$TAG")
    if [ "$LOCAL_TAG_SHA" != "$CURRENT_HEAD" ]; then
        echo "  ✘ Local tag $TAG points at $LOCAL_TAG_SHA"
        echo "    but HEAD is $CURRENT_HEAD."
        echo "    Either move HEAD to the tagged commit, or delete + retag:"
        echo "        git tag -d $TAG && git push origin :refs/tags/$TAG"
        exit 1
    fi
    echo "  ✓ Local tag $TAG points at HEAD"
fi

# 0.4 — remote tag (if any) must agree with local tag
REMOTE_TAG_SHA=$(git ls-remote --tags origin "refs/tags/${TAG}" 2>/dev/null | awk '{print $1}' | head -1 || true)
if [ -n "$REMOTE_TAG_SHA" ]; then
    LOCAL_TAG_SHA=$(git rev-list -n1 "$TAG" 2>/dev/null || echo "")
    if [ -z "$LOCAL_TAG_SHA" ]; then
        echo "  ✘ Remote tag $TAG exists ($REMOTE_TAG_SHA) but no local tag."
        echo "    Run: git fetch --tags"
        exit 1
    fi
    if [ "$LOCAL_TAG_SHA" != "$REMOTE_TAG_SHA" ]; then
        echo "  ✘ Local tag $TAG ($LOCAL_TAG_SHA)"
        echo "    disagrees with remote tag ($REMOTE_TAG_SHA)."
        echo "    Resolve before releasing — usually means an aborted prior"
        echo "    release left the remote tag in a stale state."
        exit 1
    fi
    echo "  ✓ Remote tag $TAG matches local"
fi
echo

TARGETS=(
    "aarch64-apple-darwin"
    "x86_64-apple-darwin"
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-pc-windows-gnu"
    "x86_64-unknown-freebsd"
    "x86_64-unknown-netbsd"
)
# Builder images for cross-compile BSD targets — built locally from
# dist/builders/*.Dockerfile, tagged with the local docker daemon.
# Image names match `docker images` output; release.sh resolves them locally.
BUILDER_FREEBSD_X86_64="${BUILDER_FREEBSD_X86_64:-culpur/anvil-builder-freebsd-x86_64:test}"
BUILDER_NETBSD_X86_64="${BUILDER_NETBSD_X86_64:-culpur/anvil-builder-netbsd-x86_64:test}"
OUTPUT_DIR="$PROJECT_DIR/target/release-artifacts"
mkdir -p "$OUTPUT_DIR"

# ─── Phase 1: Build ──────────────────────────────────────────────────────────

if [ "$SKIP_BUILD" = false ]; then
    echo "▸ Phase 1: Building all targets..."
    echo

    # v2.2.17 / task #570: each platform also ships the sandbox-runner binary
    # next to `anvil`.  Cargo builds both binaries when we run
    # `cargo build --release --target <triple>` (workspace default-members
    # picks up the new `anvil-sandbox-runner` crate), so the only change
    # below is the extra `cp` of the second binary into the artifacts dir.

    # 1a. macOS ARM (native)
    echo "  [1/7] macOS ARM (aarch64-apple-darwin)..."
    cargo build --release --target aarch64-apple-darwin
    cp target/aarch64-apple-darwin/release/anvil "$OUTPUT_DIR/anvil-aarch64-apple-darwin"
    cp target/aarch64-apple-darwin/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-aarch64-apple-darwin"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-aarch64-apple-darwin" | awk '{print $5}')"

    # 1b. macOS Intel (native cross)
    echo "  [2/7] macOS Intel (x86_64-apple-darwin)..."
    rustup target add x86_64-apple-darwin 2>/dev/null || true
    cargo build --release --target x86_64-apple-darwin
    cp target/x86_64-apple-darwin/release/anvil "$OUTPUT_DIR/anvil-x86_64-apple-darwin"
    cp target/x86_64-apple-darwin/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-x86_64-apple-darwin"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-apple-darwin" | awk '{print $5}')"

    # 1c. Linux x86_64 (Docker)
    echo "  [3/7] Linux x86_64 (x86_64-unknown-linux-gnu)..."
    docker build --platform linux/amd64 -t anvil-builder-linux -f - . 2>/dev/null << 'DOCKERFILE'
FROM --platform=linux/amd64 rust:1.94-slim-bookworm
RUN apt-get update && apt-get install -y pkg-config libssl-dev gcc-aarch64-linux-gnu g++-aarch64-linux-gnu && rm -rf /var/lib/apt/lists/*
RUN rustup target add aarch64-unknown-linux-gnu
WORKDIR /build
DOCKERFILE
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" anvil-builder-linux \
        cargo build --release --target x86_64-unknown-linux-gnu
    cp target/x86_64-unknown-linux-gnu/release/anvil "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu"
    cp target/x86_64-unknown-linux-gnu/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-x86_64-unknown-linux-gnu"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu" | awk '{print $5}')"

    # 1d. Linux ARM64 (Docker cross)
    echo "  [4/7] Linux ARM64 (aarch64-unknown-linux-gnu)..."
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" \
        -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
        -e CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
        -e CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
        anvil-builder-linux \
        cargo build --release --target aarch64-unknown-linux-gnu
    cp target/aarch64-unknown-linux-gnu/release/anvil "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu"
    cp target/aarch64-unknown-linux-gnu/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-aarch64-unknown-linux-gnu"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu" | awk '{print $5}')"

    # 1e. Windows x86_64 (Docker cross with mingw)
    echo "  [5/7] Windows x86_64 (x86_64-pc-windows-gnu)..."
    docker build --platform linux/amd64 -t anvil-builder-win -f - . 2>/dev/null << 'DOCKERFILE'
FROM --platform=linux/amd64 rust:1.94-slim-bookworm
RUN apt-get update && apt-get install -y gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-gnu
WORKDIR /build
DOCKERFILE
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" \
        -e CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
        anvil-builder-win \
        cargo build --release --target x86_64-pc-windows-gnu
    cp target/x86_64-pc-windows-gnu/release/anvil.exe "$OUTPUT_DIR/anvil-x86_64-pc-windows-gnu.exe"
    cp target/x86_64-pc-windows-gnu/release/anvil-sandbox-runner.exe "$OUTPUT_DIR/anvil-sandbox-runner-x86_64-pc-windows-gnu.exe"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-pc-windows-gnu.exe" | awk '{print $5}')"

    # 1f. FreeBSD x86_64 (local builder image — Tier-2, hard fail on error)
    # Image source: dist/builders/freebsd-x86_64.Dockerfile (rust + FreeBSD 14.3 sysroot).
    # Built via:  cd dist/builders && docker buildx build --platform linux/amd64 \
    #                 -t culpur/anvil-builder-freebsd-x86_64:test \
    #                 -f freebsd-x86_64.Dockerfile --load .
    echo "  [6/7] FreeBSD x86_64 (x86_64-unknown-freebsd)..."
    docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" -w /build \
        "${BUILDER_FREEBSD_X86_64}" \
        cargo build --release --target x86_64-unknown-freebsd
    cp target/x86_64-unknown-freebsd/release/anvil "$OUTPUT_DIR/anvil-x86_64-unknown-freebsd"
    cp target/x86_64-unknown-freebsd/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-x86_64-unknown-freebsd"
    echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-unknown-freebsd" | awk '{print $5}')"

    # 1g. NetBSD x86_64 (local builder image — Tier-3, soft fail on error)
    # Image source: dist/builders/netbsd-x86_64.Dockerfile (rust + NetBSD 9.3 sysroot).
    # NetBSD is Rust Tier-3; if the build breaks on a future toolchain bump,
    # users can build from source via cargo install.
    echo "  [7/7] NetBSD x86_64 (x86_64-unknown-netbsd)..."
    if docker run --platform linux/amd64 --rm -v "$PROJECT_DIR:/build" -w /build \
        "${BUILDER_NETBSD_X86_64}" \
        cargo build --release --target x86_64-unknown-netbsd; then
        cp target/x86_64-unknown-netbsd/release/anvil "$OUTPUT_DIR/anvil-x86_64-unknown-netbsd"
        cp target/x86_64-unknown-netbsd/release/anvil-sandbox-runner "$OUTPUT_DIR/anvil-sandbox-runner-x86_64-unknown-netbsd"
        echo "        ✓ $(ls -lh "$OUTPUT_DIR/anvil-x86_64-unknown-netbsd" | awk '{print $5}')"
    else
        echo "        ⚠ NetBSD build skipped (Tier-3 target — toolchain may have drifted)"
        echo "        Users on NetBSD can build from source:"
        echo "        cargo install --git https://github.com/culpur/anvil-source"
    fi

    # FreeBSD ARM64 and OpenBSD x86_64 are not built:
    # - FreeBSD ARM64: Rust ships no aarch64-unknown-freebsd rust-std (build-std needed)
    # - OpenBSD x86_64: Rust Tier-3 with no usable cross sysroot today
    # Both targets are documented as source-build in install/install.sh.

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
# v2.2.17 / #570: sandbox-runner binaries get the same per-platform manifest
# treatment as `anvil` itself so `anvil upgrade` can verify them too.
for f in "$OUTPUT_DIR"/anvil-aarch64-apple-darwin "$OUTPUT_DIR"/anvil-x86_64-apple-darwin \
         "$OUTPUT_DIR"/anvil-aarch64-unknown-linux-gnu "$OUTPUT_DIR"/anvil-x86_64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-x86_64-pc-windows-gnu.exe \
         "$OUTPUT_DIR"/anvil-x86_64-unknown-freebsd \
         "$OUTPUT_DIR"/anvil-x86_64-unknown-netbsd \
         "$OUTPUT_DIR"/anvil-sandbox-runner-aarch64-apple-darwin \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-apple-darwin \
         "$OUTPUT_DIR"/anvil-sandbox-runner-aarch64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-pc-windows-gnu.exe \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-freebsd \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-netbsd; do
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
         "$OUTPUT_DIR"/anvil-x86_64-pc-windows-gnu.exe \
         "$OUTPUT_DIR"/anvil-x86_64-unknown-freebsd \
         "$OUTPUT_DIR"/anvil-x86_64-unknown-netbsd \
         "$OUTPUT_DIR"/anvil-sandbox-runner-aarch64-apple-darwin \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-apple-darwin \
         "$OUTPUT_DIR"/anvil-sandbox-runner-aarch64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-linux-gnu \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-pc-windows-gnu.exe \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-freebsd \
         "$OUTPUT_DIR"/anvil-sandbox-runner-x86_64-unknown-netbsd; do
    if [ ! -f "$f" ]; then continue; fi
    actual=$(shasum -a 256 "$f" | awk '{print $1}')
    expected=$(awk '{print $1}' "$f.sha256")
    if [ "$actual" != "$expected" ]; then
        echo "  ✗ $(basename "$f"): actual=$actual manifest=$expected — ABORTING release"
        exit 1
    fi
    echo "  ✓ $(basename "$f")"
done

# ─── Phase 2.6: Embedded GIT_SHA must match HEAD (T1-B) ────────────────────
#
# Past bug: v2.2.11 release shipped binaries that reported a different
# GIT_SHA than the tag pointed at. Root cause was build.rs caching a stale
# rev because cargo:rerun-if-changed=.git/HEAD doesn't fire on commits to
# the current branch (only on branch switches). build.rs is now fixed, but
# this gate is the belt-and-suspenders: even if build.rs caches incorrectly
# in the future, this check refuses to release a binary whose embedded SHA
# disagrees with the tag.
#
# We only run the macOS-native binary (the others are cross-compiled and
# can't execute here), but they all build from the same workspace so a
# match here implies a match everywhere.
echo
echo "▸ Phase 2.6: Embedded GIT_SHA must match HEAD..."
EXPECTED_SHA=$(git rev-parse --short HEAD)
NATIVE_BIN="$OUTPUT_DIR/anvil-aarch64-apple-darwin"
if [ -f "$NATIVE_BIN" ] && [ -x "$NATIVE_BIN" ]; then
    EMBEDDED_SHA=$("$NATIVE_BIN" --version 2>/dev/null | awk '/Git SHA/ {print $3}' | head -1)
    if [ -z "$EMBEDDED_SHA" ]; then
        echo "  ⚠ Could not extract Git SHA from $NATIVE_BIN — skipping check"
    elif [ "$EMBEDDED_SHA" != "$EXPECTED_SHA" ]; then
        echo "  ✘ Native binary reports Git SHA $EMBEDDED_SHA"
        echo "    but HEAD is $EXPECTED_SHA."
        echo "    This usually means build.rs cached a stale rev. Try:"
        echo "        cargo clean -p anvil-cli && bash scripts/release.sh"
        exit 1
    else
        echo "  ✓ Native binary embeds Git SHA $EMBEDDED_SHA (matches HEAD)"
    fi
else
    echo "  ⚠ Native binary not present or not executable — skipping check"
fi

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

# Release notes are sourced from RELEASE-NOTES-<TAG>.md at the repo root.
# This file is hand-written for each release per memory feedback-release-notes
# ("Release notes must be written, never auto-generated from commit subject").
# Missing the file is a HARD FAIL — we will not ship a release with an empty
# body. v2.2.10 and v2.2.11 shipped with no narrative because release.sh
# previously ignored these files; never again.
RELEASE_NOTES_FILE="$PROJECT_DIR/RELEASE-NOTES-$TAG.md"
if [ ! -f "$RELEASE_NOTES_FILE" ]; then
    echo "✗ FAIL: release notes not found at $RELEASE_NOTES_FILE" >&2
    echo "  Every release MUST have a hand-written RELEASE-NOTES-<TAG>.md file" >&2
    echo "  at the repo root. Create one before re-running this script." >&2
    echo "  (See RELEASE-NOTES-v2.2.11.md for the expected format.)" >&2
    exit 1
fi

# Compose the body: hand-written notes + the standard Downloads/Install
# block appended at the bottom so users always see the install instructions.
NOTES="$(cat "$RELEASE_NOTES_FILE")

---

### Downloads
| Platform | Binary |
|----------|--------|
| macOS ARM (M1/M2/M3) | \`anvil-aarch64-apple-darwin\` |
| macOS Intel | \`anvil-x86_64-apple-darwin\` |
| Linux x86_64 | \`anvil-x86_64-unknown-linux-gnu\` |
| Linux ARM64 | \`anvil-aarch64-unknown-linux-gnu\` |
| Windows x86_64 | \`anvil-x86_64-pc-windows-gnu.exe\` |
| FreeBSD x86_64 | \`anvil-x86_64-unknown-freebsd\` |
| NetBSD x86_64 | \`anvil-x86_64-unknown-netbsd\` |

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
# Stage the manpage alongside the binaries so it ships as a release asset.
# The Homebrew formula's `resource "manpage"` block downloads this URL.
if [ -f "$PROJECT_DIR/man/anvil.1" ]; then
    cp "$PROJECT_DIR/man/anvil.1" "$OUTPUT_DIR/anvil.1"
fi
if gh release view "$TAG" --repo "$PUBLIC_REPO" >/dev/null 2>&1; then
    echo "  Release $TAG exists on $PUBLIC_REPO — uploading assets..."
    gh release upload "$TAG" --repo "$PUBLIC_REPO" "$OUTPUT_DIR"/anvil-* --clobber
    [ -f "$OUTPUT_DIR/anvil.1" ] && \
        gh release upload "$TAG" --repo "$PUBLIC_REPO" "$OUTPUT_DIR/anvil.1" --clobber
else
    echo "  Creating release $TAG on $PUBLIC_REPO..."
    if [ -f "$OUTPUT_DIR/anvil.1" ]; then
        gh release create "$TAG" \
            --repo "$PUBLIC_REPO" \
            --title "Anvil $TAG" \
            --notes "$NOTES" \
            "$OUTPUT_DIR"/anvil-* "$OUTPUT_DIR/anvil.1"
    else
        gh release create "$TAG" \
            --repo "$PUBLIC_REPO" \
            --title "Anvil $TAG" \
            --notes "$NOTES" \
            "$OUTPUT_DIR"/anvil-*
    fi
fi


# ─── Phase 4.5: Publish AnvilHub sha256 manifest ────────────────────────────
#
# /api/version advertises sha256_url=https://anvilhub.culpur.net/sha256/<ver>.txt
# but pre-#620 release.sh never wrote that file — v2.2.16 shipped with the URL
# returning 404. Users running `sha256sum -c` after `curl … binary` hit the
# 404 and reasonably assumed the project was broken.
#
# Skipped on --skip-build because it depends on Phase 1.5 having written the
# .sha256 sidecars to $OUTPUT_DIR; --skip-build callers should backfill via
#   bash scripts/release-helpers/upload-sha256-manifest.sh <ver> <output_dir>
echo
echo "▸ Phase 4.5: Publish AnvilHub sha256 manifest..."
if [ "$SKIP_BUILD" = false ]; then
    if ! upload_sha256_manifest "$VERSION" "$OUTPUT_DIR"; then
        rc=$?
        echo "✘ sha256 manifest publish FAILED (rc=$rc) — /sha256/$VERSION.txt may 404" >&2
        echo "  Re-run manually:" >&2
        echo "    bash $SCRIPT_DIR/release-helpers/upload-sha256-manifest.sh $VERSION $OUTPUT_DIR" >&2
        echo "  …then re-run scripts/release.sh --skip-build for Phase 8." >&2
        exit "$rc"
    fi
else
    echo "  ⊘ Skipped (--skip-build: sidecars in $OUTPUT_DIR may be stale)"
    echo "    Run manually if needed:"
    echo "      bash $SCRIPT_DIR/release-helpers/upload-sha256-manifest.sh $VERSION $OUTPUT_DIR"
fi

echo
echo "▸ Phase 5: Update Homebrew formula..."
ARM_MAC=$(shasum -a 256 "$OUTPUT_DIR/anvil-aarch64-apple-darwin" | awk '{print $1}')
INTEL_MAC=$(shasum -a 256 "$OUTPUT_DIR/anvil-x86_64-apple-darwin" | awk '{print $1}')
ARM_LINUX=$(shasum -a 256 "$OUTPUT_DIR/anvil-aarch64-unknown-linux-gnu" | awk '{print $1}')
X86_LINUX=$(shasum -a 256 "$OUTPUT_DIR/anvil-x86_64-unknown-linux-gnu" | awk '{print $1}')
if [ -f "$OUTPUT_DIR/anvil.1" ]; then
    MANPAGE_SHA=$(shasum -a 256 "$OUTPUT_DIR/anvil.1" | awk '{print $1}')
else
    MANPAGE_SHA=""
fi

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
$( [ -n "$MANPAGE_SHA" ] && cat <<MANPAGE
  resource "manpage" do
    url "https://github.com/culpur/anvil/releases/download/$TAG/anvil.1"
    sha256 "$MANPAGE_SHA"
  end
MANPAGE
)
  def install
    downloaded = Dir["anvil-*"].first || "anvil"
    bin.install downloaded => "anvil"
$( [ -n "$MANPAGE_SHA" ] && echo '    resource("manpage").stage { man1.install "anvil.1" }' )
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

# WordPress shortcodes — update ANVIL_VERSION constant.
#
# T1-C: previously this used a raw remote sed ("s/ANVIL_VERSION.*,.*/...")
# that produced exactly the mismatched-quote corruption that took culpur.net
# down in incident #281 (and recurred in #396). Now it:
#   1. Fetches the existing file content
#   2. Applies a quote-preserving in-place replacement (Python — always
#      installed; no cross-repo dep on safe-edit)
#   3. Pre-flight checks: refuse if existing line has unbalanced quotes
#   4. Writes back via base64 (no shell escaping concerns)
#   5. Runs `php -l` on the result; rolls back to original if lint fails
WP_PHP_PATH="/var/www/html/wp-content/mu-plugins/culpur-hardening.php"
WP_SSH="ssh -p 30022 -i $HOME/.ssh/id_ed25519_guard soulofall@guard.armored.ninja"
WP_INNER="ssh 10.0.70.6"
WP_DOCKER="sudo docker exec wordpress-wordpress-1"

WP_UPDATE_OK=false
WP_OLD=$($WP_SSH "$WP_INNER '$WP_DOCKER cat $WP_PHP_PATH'" 2>/dev/null || true)
if [ -n "$WP_OLD" ]; then
    # Use Python (always available) to apply the quote-preserving replacement.
    # Backreference (\\3) forces closing quote to match opening — defeats the
    # mismatched-quote propagation that caused incidents #281 / #396.
    WP_NEW=$(python3 -c "
import re, sys
old = sys.stdin.read()
key = 'ANVIL_VERSION'
ver = '$VERSION'
# Pre-flight: refuse if existing line has unbalanced quotes
import re as _re
lm = _re.search(r'[^\n]*\b' + key + r'\b[^\n]*', old, _re.IGNORECASE)
if lm:
    ln = lm.group(0)
    if ln.count(chr(39)) % 2 != 0 or ln.count(chr(34)) % 2 != 0:
        sys.stderr.write('refusing to update: existing line has unbalanced quotes: ' + ln.strip() + '\n')
        sys.exit(2)
pat = re.compile(r'(define\s*\(\s*([\x27\x22])' + key + r'\2\s*,\s*([\x27\x22]))([^\x27\x22]*?)(\3\s*\))', re.IGNORECASE)
new = pat.sub(r'\1' + ver + r'\5', old, count=1)
if new == old:
    # No change needed — write old back verbatim, exit 0
    sys.stdout.write(old)
    sys.exit(0)
sys.stdout.write(new)
" <<< "$WP_OLD" 2>/tmp/anvil-wp-update.err)
    WP_RC=$?
    if [ $WP_RC -ne 0 ]; then
        echo "  ✘ WordPress update REFUSED by safe-edit: $(cat /tmp/anvil-wp-update.err)"
    elif [ "$WP_NEW" = "$WP_OLD" ]; then
        echo "  ✓ WordPress shortcode already at v$VERSION (no change)"
        WP_UPDATE_OK=true
    else
        # Write through base64 — no shell escaping concerns.
        # Each SSH call uses || true so a network blip or container-not-running
        # doesn't trigger set -e and silently kill the rest of the pipeline
        # (v2.2.17 incident: set -e + failing SSH in this block terminated
        #  release.sh before Phase 7+ ran, with exit 0, no error visible).
        WP_NEW_B64=$(printf '%s' "$WP_NEW" | base64 | tr -d '\n')
        $WP_SSH "$WP_INNER \"$WP_DOCKER sh -c 'echo $WP_NEW_B64 | base64 -d > $WP_PHP_PATH'\"" 2>/dev/null || true
        # php -l verification — roll back on failure.
        # The pipeline is captured into a variable first so pipefail can't fire
        # mid-pipeline and kill the script before we handle the result.
        WP_LINT_OUT=$($WP_SSH "$WP_INNER '$WP_DOCKER php -l $WP_PHP_PATH'" 2>&1 || true)
        if echo "$WP_LINT_OUT" | grep -q "No syntax errors detected"; then
            $WP_SSH "$WP_INNER '$WP_DOCKER rm -rf /var/www/html/wp-content/wphb-cache/'" 2>/dev/null || true
            echo "  ✓ WordPress shortcode updated to v$VERSION + lint passed + cache cleared"
            WP_UPDATE_OK=true
        else
            # Roll back to the original content
            WP_OLD_B64=$(printf '%s' "$WP_OLD" | base64 | tr -d '\n')
            $WP_SSH "$WP_INNER \"$WP_DOCKER sh -c 'echo $WP_OLD_B64 | base64 -d > $WP_PHP_PATH'\"" 2>/dev/null || true
            echo "  ✘ WordPress php -l REJECTED the new content — rolled back to original" >&2
            echo "    (release continues; manual investigation needed)" >&2
        fi
    fi
fi
if [ "$WP_UPDATE_OK" != "true" ]; then
    echo "  ⚠ WordPress update skipped or failed (SSH unavailable / lint rejected)"
fi

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


# ─── Phase 7: X/Twitter announcement ────────────────────────────────────────
#
# Posts the release announcement thread to X/Twitter.
#
# Gate: RELEASE_AUTO_POST=1 is REQUIRED to actually post. Without it, the
# planned thread is printed for review only (same caution pattern as the
# release-surfaces verification gate).
#
# Skip path: if X credentials are not configured, print a warning and continue.
# This is NOT a release-blocking failure — it's a manual-step reminder.
#
# Parsing: extracts hero paragraph + feature bullets from RELEASE-NOTES-<TAG>.md.
# Caps each tweet at 270 chars (leaving 10-char headroom for X's URL counter).
#
echo
echo "▸ Phase 7: X/Twitter announcement..."

X_CREDS_OK=false
X_CREDS_CHECK=$(node --input-type=module -e "
import { loadXCredentials } from '$(dirname "$0")/../node_modules/.bin/../../../mcp-servers/anvil-release/lib/helpers.js';
" 2>/dev/null) || true

# Use the anvil-release MCP directly to check creds
ANVIL_RELEASE_MCP_DIR="$HOME/projects/mcp-servers/anvil-release"
if node --input-type=module -e "
import { loadXCredentials } from '${ANVIL_RELEASE_MCP_DIR}/lib/helpers.js';
const r = loadXCredentials();
process.exit(r.ok ? 0 : 1);
" 2>/dev/null; then
    X_CREDS_OK=true
else
    echo "  ⚠ X announcement skipped (credentials not configured)"
    echo "    Run: node ${ANVIL_RELEASE_MCP_DIR}/scripts/enroll-x-pkce.js"
    echo "    Then add 8 X_* env vars to the vault."
fi

if [ "$X_CREDS_OK" = true ]; then
    # Build the announcement thread from the release notes file
    # Parser: first non-empty paragraph after the heading = hero paragraph (tweet 1)
    #         feature bullets (lines starting with -) = tweets 2..N (capped at 270 chars each)
    #         final tweet = install line
    THREAD_JSON=$(node --input-type=module -e "
import { readFileSync } from 'fs';

const notesPath = '${RELEASE_NOTES_FILE}';
const version = '${VERSION}';
const tag = '${TAG}';
const maxChars = 270;

let content;
try {
  content = readFileSync(notesPath, 'utf8');
} catch {
  process.stderr.write('Could not read ' + notesPath + '\n');
  process.exit(1);
}

const lines = content.split('\n');
const tweets = [];

// Find hero paragraph: first non-empty, non-heading block after the title
let heroPara = '';
let inHero = false;
for (const line of lines) {
  if (line.startsWith('#')) {
    if (heroPara.trim()) break; // we already have the hero
    continue;
  }
  if (!inHero && line.trim()) {
    inHero = true;
    heroPara += line + ' ';
  } else if (inHero && line.trim()) {
    heroPara += line + ' ';
  } else if (inHero && !line.trim()) {
    break; // end of paragraph
  }
}
heroPara = heroPara.trim();
if (heroPara.length > maxChars) heroPara = heroPara.slice(0, maxChars - 3) + '...';

// Prepend version to hero
const heroTweet = 'Anvil v' + version + ': ' + heroPara;
tweets.push({ text: heroTweet.length <= maxChars ? heroTweet : heroTweet.slice(0, maxChars - 3) + '...' });

// Feature bullets
for (const line of lines) {
  const trimmed = line.trim();
  if (!trimmed.startsWith('- ') && !trimmed.startsWith('* ')) continue;
  const bullet = trimmed.replace(/^[-*]\s+/, '');
  if (!bullet || bullet.startsWith('[')) continue; // skip links
  const tweetText = bullet.length <= maxChars ? bullet : bullet.slice(0, maxChars - 3) + '...';
  tweets.push({ text: tweetText });
  if (tweets.length >= 5) break; // cap at 5 tweets (hero + 3 features + install = 5)
}

// Install tweet (always last)
const installTweet = 'Install: brew install culpur/anvil/anvil — full notes https://github.com/culpur/anvil/releases/tag/' + tag;
tweets.push({ text: installTweet });

process.stdout.write(JSON.stringify({ tweets }, null, 2));
" 2>/tmp/anvil-x-thread-build.err) || {
        echo "  ⚠ Could not build X thread: $(cat /tmp/anvil-x-thread-build.err)"
        echo "    X announcement skipped; post manually from ${RELEASE_NOTES_FILE}"
        THREAD_JSON=""
    }

    if [ -n "$THREAD_JSON" ]; then
        TWEET_COUNT=$(echo "$THREAD_JSON" | node --input-type=module -e "
import { createInterface } from 'readline';
let json = '';
const rl = createInterface({ input: process.stdin });
rl.on('line', l => { json += l; });
rl.on('close', () => {
  const d = JSON.parse(json);
  process.stdout.write(String(d.tweets.length));
});
" 2>/dev/null || echo "0")

        echo "  Planned thread ($TWEET_COUNT tweets):"
        echo "$THREAD_JSON" | node --input-type=module -e "
import { createInterface } from 'readline';
let json = '';
const rl = createInterface({ input: process.stdin });
rl.on('line', l => { json += l; });
rl.on('close', () => {
  const d = JSON.parse(json);
  d.tweets.forEach((t, i) => {
    process.stdout.write('  [' + (i+1) + '] ' + t.text + '\n');
  });
});
" 2>/dev/null || echo "  (Could not parse thread JSON)"

        if [ "${RELEASE_AUTO_POST:-}" = "1" ]; then
            echo
            echo "  RELEASE_AUTO_POST=1 — posting thread to X..."
            X_POST_RESULT=$(node --input-type=module -e "
import { anvilXThread } from '${ANVIL_RELEASE_MCP_DIR}/lib/tools/x-thread.js';
const args = JSON.parse(process.env.THREAD_JSON || '{}');
const result = await anvilXThread(args);
process.stdout.write(JSON.stringify(result, null, 2));
" THREAD_JSON="$THREAD_JSON" 2>/tmp/anvil-x-post.err) || {
                rc=$?
                echo "  ✘ Phase 7 X post failed (rc=$rc): $(cat /tmp/anvil-x-post.err)" >&2
                echo "    Release continues — post announcement manually." >&2
                X_POST_RESULT=""
            }

            if [ -n "$X_POST_RESULT" ]; then
                X_POST_OK=$(echo "$X_POST_RESULT" | node --input-type=module -e "
import { createInterface } from 'readline';
let json = '';
const rl = createInterface({ input: process.stdin });
rl.on('line', l => { json += l; });
rl.on('close', () => { const d = JSON.parse(json); process.stdout.write(d.ok ? 'true' : 'false'); });
" 2>/dev/null || echo "false")

                if [ "$X_POST_OK" = "true" ]; then
                    ROOT_URL=$(echo "$X_POST_RESULT" | node --input-type=module -e "
import { createInterface } from 'readline';
let json = '';
const rl = createInterface({ input: process.stdin });
rl.on('line', l => { json += l; });
rl.on('close', () => { const d = JSON.parse(json); process.stdout.write(d.data?.root_url || ''); });
" 2>/dev/null || echo "")
                    echo "  ✓ Thread posted: ${ROOT_URL:-[url not returned]}"
                else
                    X_ERR=$(echo "$X_POST_RESULT" | node --input-type=module -e "
import { createInterface } from 'readline';
let json = '';
const rl = createInterface({ input: process.stdin });
rl.on('line', l => { json += l; });
rl.on('close', () => { const d = JSON.parse(json); process.stdout.write(d.error?.message || JSON.stringify(d.error)); });
" 2>/dev/null || echo "unknown error")
                    echo "  ✘ X post failed: ${X_ERR}" >&2
                    echo "    Post announcement manually." >&2
                fi
            fi
        else
            echo
            echo "  ℹ Thread printed above — NOT posted (RELEASE_AUTO_POST not set)"
            echo "    To auto-post: RELEASE_AUTO_POST=1 bash scripts/release.sh --skip-build"
        fi
    fi
fi

echo
echo "▸ Phase 8: Generate release notes draft..."
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

# ─── Phase 9: Post-publish verification gate ────────────────────────────────
#
# Two-stage verification (added task #614):
#
#   Stage 9a — release-surfaces.yaml comprehensive check
#     The manifest at the repo root catalogues every user-facing surface
#     (anvilhub homepage, /about, /install, culpur.net/anvil, /products,
#     public README "What's new", Homebrew, release notes file, etc.) and
#     scripts/verify-release-surfaces.sh probes them all. This is the strict
#     superset gate — if it passes, every surface is current.
#
#   Stage 9b — /api/version binary-URL probe (added task #611)
#     The legacy verifier focuses on the failure mode that caused v2.2.16's
#     Windows 404 (advertised filenames diverged from uploaded filenames).
#     It HEADs every URL in /api/version's .binaries{} and asserts
#     .latest_version matches.
#
# Both must pass. Stage 9a runs first because surface drift is more common
# than filename mismatch; failing fast on a stale homepage is more useful
# than waiting for the binary probe.
if [ "$SKIP_VERIFY" = "true" ]; then
    echo
    echo "▸ Phase 9: Post-publish verification SKIPPED (--skip-verify)"
elif [ "$DRY_RUN_VERIFY" = "true" ]; then
    echo
    echo "▸ Phase 9a: Surface manifest verification (dry-run)..."
    if ! bash "$SCRIPT_DIR/verify-release-surfaces.sh" "$VERSION" --dry-run; then
        echo "✘ Manifest dry-run reported a problem (rc=$?)." >&2
        exit 3
    fi
    echo
    echo "▸ Phase 9b: Binary-URL probe (dry-run)..."
    verify_release "$VERSION" --dry-run || {
        echo "✘ Verification dry-run reported a problem (rc=$?)." >&2
        exit 3
    }
else
    echo
    echo "▸ Phase 9a: Surface manifest verification..."
    if ! bash "$SCRIPT_DIR/verify-release-surfaces.sh" "$VERSION"; then
        rc=$?
        echo >&2
        echo "✘ Surface manifest verification FAILED (rc=$rc)." >&2
        echo "  One or more release surfaces is stale or unreachable." >&2
        echo "  See the FAILED SURFACES list above; each entry names the" >&2
        echo "  deploy_path so you can fix the source and re-run:" >&2
        echo "      bash scripts/verify-release-surfaces.sh $VERSION" >&2
        echo "  Then re-run the release with --skip-build to repeat Phase 9." >&2
        exit "$rc"
    fi
    echo
    echo "▸ Phase 9b: Binary-URL probe..."
    if ! verify_release "$VERSION"; then
        rc=$?
        echo >&2
        echo "✘ Post-publish verification FAILED (rc=$rc)." >&2
        echo "  The GitHub Release exists and binaries are uploaded, but a" >&2
        echo "  downstream surface is broken. See remediation block above." >&2
        exit "$rc"
    fi
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

# ─── Phase 99: Post-flight sentinel ──────────────────────────────────────────
#
# If this line executes, every phase above ran to completion (set -euo pipefail
# would have terminated the script on any unguarded error before reaching here).
# The explicit message lets pipeline observers distinguish "script completed all
# phases" from "script exited 0 mid-way" — the silent-exit mode that caused
# the v2.2.17 incident where Phases 7+ were skipped without any error output.
PHASES_RUN="0→1.5→2→2.5→2.6→3→4→4.5→5→6→7→8→9"
[ "$SKIP_BUILD" = true ]   && PHASES_RUN="(skipped 1) $PHASES_RUN"
[ "$BUILD_ONLY" = true ]   && PHASES_RUN="1→1.5→2→2.5→2.6 (build-only)"
[ "$SKIP_VERIFY" = true ]  && PHASES_RUN="$PHASES_RUN (skipped 9)"
[ "$DRY_RUN_VERIFY" = true ] && PHASES_RUN="$PHASES_RUN (dry-run 9)"
echo
echo "[release.sh complete: phases run = $PHASES_RUN | version = $TAG | exit 0]"
