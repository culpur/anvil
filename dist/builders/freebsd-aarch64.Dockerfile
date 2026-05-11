# culpur/anvil-builder-freebsd-aarch64
#
# Cross-compile Rust binaries for FreeBSD ARM64 (Tier-2 target) from a Linux x86_64 host.
#
# Build: docker buildx build --platform linux/amd64 -t registry.culpur.net/culpur/anvil-builder-freebsd-aarch64:rust-1.94 -f freebsd-aarch64.Dockerfile .
# Push:  docker push registry.culpur.net/culpur/anvil-builder-freebsd-aarch64:rust-1.94
# Use:   docker run --platform linux/amd64 --rm -v "$(pwd):/build" -w /build \
#            registry.culpur.net/culpur/anvil-builder-freebsd-aarch64:rust-1.94 \
#            cargo build --release --target aarch64-unknown-freebsd

FROM rust:1.94-bookworm

ARG FREEBSD_VERSION=14.3
ARG FREEBSD_ARCH=arm64/aarch64
ARG TARGET=aarch64-unknown-freebsd

RUN apt-get update && apt-get install -y --no-install-recommends \
        clang \
        lld \
        llvm \
        curl \
        ca-certificates \
        xz-utils \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add ${TARGET}

# FreeBSD ARM64 base.txz lives under arm64/aarch64/
RUN mkdir -p /sysroot/${TARGET} && \
    curl -fsSL "https://download.freebsd.org/releases/${FREEBSD_ARCH}/${FREEBSD_VERSION}-RELEASE/base.txz" \
    | tar -xJ -C /sysroot/${TARGET} ./lib ./usr/lib ./usr/include && \
    echo "Sysroot extracted to /sysroot/${TARGET}"

ENV CC_aarch64_unknown_freebsd=clang \
    CXX_aarch64_unknown_freebsd=clang++ \
    AR_aarch64_unknown_freebsd=llvm-ar \
    CFLAGS_aarch64_unknown_freebsd="--target=aarch64-unknown-freebsd14 --sysroot=/sysroot/aarch64-unknown-freebsd" \
    CXXFLAGS_aarch64_unknown_freebsd="--target=aarch64-unknown-freebsd14 --sysroot=/sysroot/aarch64-unknown-freebsd" \
    CARGO_TARGET_AARCH64_UNKNOWN_FREEBSD_LINKER=clang \
    CARGO_TARGET_AARCH64_UNKNOWN_FREEBSD_RUSTFLAGS="-C link-arg=--target=aarch64-unknown-freebsd14 -C link-arg=--sysroot=/sysroot/aarch64-unknown-freebsd -C link-arg=-fuse-ld=lld"

WORKDIR /build
CMD ["cargo", "build", "--release", "--target", "aarch64-unknown-freebsd"]
