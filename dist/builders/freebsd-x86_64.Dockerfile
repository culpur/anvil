# culpur/anvil-builder-freebsd-x86_64
#
# Cross-compile Rust binaries for FreeBSD x86_64 (Tier-2 target) from a Linux x86_64
# container running on the local release host. Owned by Culpur; not dependent on
# cross-rs upstream tags (which lack cargo/rustc and have spotty BSD coverage).
#
# Build: cd dist/builders && docker buildx build --platform linux/amd64 \
#            -t culpur/anvil-builder-freebsd-x86_64:test \
#            -f freebsd-x86_64.Dockerfile --load .
# Use:   docker run --platform linux/amd64 --rm -v "$(pwd):/build" -w /build \
#            culpur/anvil-builder-freebsd-x86_64:test \
#            cargo build --release --target x86_64-unknown-freebsd
#
# scripts/release.sh phase 1f resolves the image from the local docker daemon
# (BUILDER_FREEBSD_X86_64 env var, defaults to culpur/anvil-builder-freebsd-x86_64:test).

FROM rust:1.94-bookworm

ARG FREEBSD_VERSION=14.3
ARG FREEBSD_ARCH=amd64
ARG TARGET=x86_64-unknown-freebsd

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

# Extract just the parts we need from the FreeBSD base distribution: /lib, /usr/lib, /usr/include
RUN mkdir -p /sysroot/${TARGET} && \
    curl -fsSL "https://download.freebsd.org/releases/${FREEBSD_ARCH}/${FREEBSD_VERSION}-RELEASE/base.txz" \
    | tar -xJ -C /sysroot/${TARGET} ./lib ./usr/lib ./usr/include && \
    echo "Sysroot extracted to /sysroot/${TARGET}"

ENV CC_x86_64_unknown_freebsd=clang \
    CXX_x86_64_unknown_freebsd=clang++ \
    AR_x86_64_unknown_freebsd=llvm-ar \
    CFLAGS_x86_64_unknown_freebsd="--target=x86_64-unknown-freebsd14 --sysroot=/sysroot/x86_64-unknown-freebsd" \
    CXXFLAGS_x86_64_unknown_freebsd="--target=x86_64-unknown-freebsd14 --sysroot=/sysroot/x86_64-unknown-freebsd" \
    CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_LINKER=clang \
    CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_RUSTFLAGS="-C link-arg=--target=x86_64-unknown-freebsd14 -C link-arg=--sysroot=/sysroot/x86_64-unknown-freebsd -C link-arg=-fuse-ld=lld"

WORKDIR /build
CMD ["cargo", "build", "--release", "--target", "x86_64-unknown-freebsd"]
