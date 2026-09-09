# Dev/build environment for the rust/ workspace.
#
# Produces the 32-bit game-module cdylib the engine dlopens. The host arch is
# x86_64 (native rust toolchain); the crate is cross-compiled for
# i686-unknown-linux-gnu and linked with gcc-multilib's `-m32` support. The
# runtime target (where the engine actually runs) is the separate 32-bit image
# in tools/docker/run.Dockerfile; dev.sh only builds.

FROM rust:1.85-bookworm

ENV DEBIAN_FRONTEND=noninteractive
ENV LC_ALL=C

# i386 link + analysis tools. gcc-multilib makes the x86_64 gcc emit i386
# (-m32) and provides the 32-bit crt/libc that the i686 rustc target links.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        file \
        gcc-multilib \
        libc6-dev-i386 \
    && rm -rf /var/lib/apt/lists/*

# Rust standard library for the i386 build target.
RUN rustup target add i686-unknown-linux-gnu \
    && rustup component add rustfmt clippy

WORKDIR /work
CMD ["bash"]
