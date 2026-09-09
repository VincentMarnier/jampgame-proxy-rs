#!/bin/sh
# Build the passthrough proxy cdylib and stage it as jampgamei386.so (the
# filename the engine's Sys_LoadDll looks for). Runs in the dev container.
#
# Usage: rust/dev.sh ./scripts/build.sh   (from repo root: rust/dev.sh ./scripts/build.sh)
# The staged artifact lands at rust/jampgamei386.so (gitignored).

set -eu

cargo build --target i686-unknown-linux-gnu --release
cp target/i686-unknown-linux-gnu/release/libjampgamei386.so jampgamei386.so

echo
echo "Staged: $(pwd)/jampgamei386.so"
file jampgamei386.so
