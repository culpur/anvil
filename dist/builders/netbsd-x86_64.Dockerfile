# culpur/anvil-builder-netbsd-x86_64
#
# Cross-compile Rust binaries for NetBSD x86_64 (Tier-3 target) from a Linux x86_64 host.
# NetBSD is Tier-3 in Rust; this image may break on complex deps and the release.sh
# pipeline treats it as soft-fail.
#
# Build: docker buildx build --platform linux/amd64 -t registry.culpur.net/culpur/anvil-builder-netbsd-x86_64:rust-1.94 -f netbsd-x86_64.Dockerfile .
# Push:  docker push registry.culpur.net/culpur/anvil-builder-netbsd-x86_64:rust-1.94
# Use:   docker run --platform linux/amd64 --rm -v "$(pwd):/build" -w /build \
#            registry.culpur.net/culpur/anvil-builder-netbsd-x86_64:rust-1.94 \
#            cargo build --release --target x86_64-unknown-netbsd

FROM rust:1.94-bookworm

ARG NETBSD_VERSION=9.3
ARG TARGET=x86_64-unknown-netbsd

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

# NetBSD splits libraries (base.tar.xz) from libc headers (comp.tar.xz); we need both.
RUN mkdir -p /sysroot/${TARGET} && \
    curl -fsSL "https://cdn.netbsd.org/pub/NetBSD/NetBSD-${NETBSD_VERSION}/amd64/binary/sets/base.tar.xz" \
    | tar -xJ -C /sysroot/${TARGET} ./lib ./usr/lib ./usr/include && \
    curl -fsSL "https://cdn.netbsd.org/pub/NetBSD/NetBSD-${NETBSD_VERSION}/amd64/binary/sets/comp.tar.xz" \
    | tar -xJ -C /sysroot/${TARGET} ./usr/include ./usr/lib && \
    echo "Sysroot extracted to /sysroot/${TARGET}"

ENV CC_x86_64_unknown_netbsd=clang \
    CXX_x86_64_unknown_netbsd=clang++ \
    AR_x86_64_unknown_netbsd=llvm-ar \
    CFLAGS_x86_64_unknown_netbsd="--target=x86_64-unknown-netbsd --sysroot=/sysroot/x86_64-unknown-netbsd" \
    CXXFLAGS_x86_64_unknown_netbsd="--target=x86_64-unknown-netbsd --sysroot=/sysroot/x86_64-unknown-netbsd" \
    CARGO_TARGET_X86_64_UNKNOWN_NETBSD_LINKER=clang \
    CARGO_TARGET_X86_64_UNKNOWN_NETBSD_RUSTFLAGS="-C link-arg=--target=x86_64-unknown-netbsd -C link-arg=--sysroot=/sysroot/x86_64-unknown-netbsd -C link-arg=-fuse-ld=lld -C link-arg=-L/sysroot/x86_64-unknown-netbsd/usr/lib -C link-arg=-L/sysroot/x86_64-unknown-netbsd/lib"

WORKDIR /build
CMD ["cargo", "build", "--release", "--target", "x86_64-unknown-netbsd"]
