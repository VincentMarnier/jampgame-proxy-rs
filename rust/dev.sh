#!/bin/sh
# Dev-environment front-end for the rust/ workspace.
#
# Builds the dev image (rust + i686 target + multilib) on first use, then runs
# a command inside it with the current directory (rust/) mounted at /work.
#
# Usage:
#   rust/dev.sh                  interactive shell in the dev container
#   rust/dev.sh cargo build --target i686-unknown-linux-gnu --release
#   rust/dev.sh cargo test
#   rust/dev.sh cargo fmt --check
#   rust/dev.sh ./scripts/build.sh
#
# The crate is a 32-bit cdylib: always pass --target i686-unknown-linux-gnu to
# cargo build (the engine is i386). `cargo test`/`cargo fmt` run on the host
# architecture, which is fine for unit tests and formatting.

set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
IMAGE=${JAMP_DEV_IMAGE:-jampgame-proxy-rs:dev}
REGISTRY_CACHE=${JAMP_DEV_CACHE:-jampgame-proxy-rs-cargo-registry}

docker build -q -t "$IMAGE" -f "$HERE/Dockerfile" "$HERE" >/dev/null
docker volume inspect "$REGISTRY_CACHE" >/dev/null 2>&1 \
  || docker volume create "$REGISTRY_CACHE" >/dev/null

# Run as the invoking user so build products on the bind-mounted host tree
# (target/, Cargo.lock) are owned by the user, not root.
USERARG=
if [ "$(id -u)" -ne 0 ]; then
  USERARG="--user $(id -u):$(id -g)"
  # A freshly created named volume is root-owned; hand it to the user.
  docker run --rm -v "$REGISTRY_CACHE":/reg "$IMAGE" \
    chown "$(id -u):$(id -g)" /reg >/dev/null
fi

if [ -t 0 ]; then
  TTY=-it
else
  TTY=
fi

if [ "$#" -eq 0 ]; then
  set -- bash
fi

exec docker run --rm $TTY $USERARG \
  -e HOME=/work \
  -v "$REGISTRY_CACHE":/usr/local/cargo/registry \
  -v "$HERE":/work \
  -w /work \
  "$IMAGE" \
  "$@"
