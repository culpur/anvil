#!/usr/bin/env bash
#
# Build the three Culpur-owned anvil-builder images and push to registry.culpur.net.
# Run this on dev0001 (Linux x86_64) or any host with docker login to the registry.
#
# Usage:   ./build-and-push.sh
# Tag:     each image is tagged registry.culpur.net/culpur/anvil-builder-<target>:rust-<version>
#          plus a :latest tag for the most recent.
#
# Cadence: only re-run when the Rust toolchain version bumps, when a BSD sysroot
# needs refreshing, or when the Dockerfile changes. The release.sh pipeline pulls
# pinned :rust-<version> tags, not :latest, so day-to-day releases are independent.

set -euo pipefail

cd "$(dirname "$0")"

RUST_VERSION="${RUST_VERSION:-1.94}"
REGISTRY="${REGISTRY:-registry.culpur.net/culpur}"

IMAGES=(
    "freebsd-x86_64"
    "freebsd-aarch64"
    "netbsd-x86_64"
)

echo "Building anvil-builder images for rust-${RUST_VERSION}"
echo "Registry: ${REGISTRY}"
echo ""

for image in "${IMAGES[@]}"; do
    full_tag="${REGISTRY}/anvil-builder-${image}:rust-${RUST_VERSION}"
    latest_tag="${REGISTRY}/anvil-builder-${image}:latest"

    echo "==> Building ${image}"
    docker buildx build \
        --platform linux/amd64 \
        --tag "${full_tag}" \
        --tag "${latest_tag}" \
        --file "${image}.Dockerfile" \
        --load \
        .

    echo "==> Pushing ${full_tag}"
    docker push "${full_tag}"
    docker push "${latest_tag}"

    echo "==> Done with ${image}"
    echo ""
done

echo "All ${#IMAGES[@]} anvil-builder images built and pushed."
