#!/usr/bin/env bash
#
# Build the local anvil-builder images used by scripts/release.sh for BSD
# cross-compile targets. Runs on the release host (the Mac that runs the rest
# of release.sh) — same model as the existing anvil-builder-linux + anvil-builder-win
# images that release.sh builds inline.
#
# Usage:   ./build-and-push.sh    # builds + tags locally (no push)
#
# Tag:     each image is tagged culpur/anvil-builder-<target>:test
#          (matches the BUILDER_FREEBSD_X86_64 / BUILDER_NETBSD_X86_64 defaults
#          in scripts/release.sh)
#
# Cadence: re-run when Rust toolchain bumps, when a BSD sysroot needs refreshing,
# or when a Dockerfile changes. release.sh resolves images from the local docker
# daemon, so no registry push is required.

set -euo pipefail

cd "$(dirname "$0")"

RUST_VERSION="${RUST_VERSION:-1.94}"
TAG="${TAG:-test}"

# FreeBSD ARM64 is intentionally not built — rustup ships no
# aarch64-unknown-freebsd rust-std today. Source-only path is documented
# in install/install.sh and release-notes.
IMAGES=(
    "freebsd-x86_64"
    "netbsd-x86_64"
)

echo "Building anvil-builder images locally (rust ${RUST_VERSION}, tag :${TAG})"
echo ""

for image in "${IMAGES[@]}"; do
    full_tag="culpur/anvil-builder-${image}:${TAG}"

    echo "==> Building ${full_tag}"
    docker buildx build \
        --platform linux/amd64 \
        --tag "${full_tag}" \
        --file "${image}.Dockerfile" \
        --load \
        .

    echo "==> Done with ${image}"
    echo ""
done

echo "All ${#IMAGES[@]} anvil-builder images built. release.sh will resolve them locally."
