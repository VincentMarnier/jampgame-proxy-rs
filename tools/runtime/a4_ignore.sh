#!/bin/sh
# A4: end-to-end check of the player `/ignore` command (D-016).
#
# Runs linuxjampded + the Rust proxy in the i386 runtime container and drives it
# with three synthetic JA MP clients (the A2 protocol client): one ignores
# another by name and the run checks that only the ignoring client misses the
# chat. `--mode enabled` also checks that `/unignore` restores delivery;
# `--mode disabled` checks the `proxy_sv_enableIgnore 0` reply and that no chat
# is filtered.
#
# Usage:
#   tools/runtime/a4_ignore.sh --mode enabled
#   tools/runtime/a4_ignore.sh --mode disabled
#
# Environment:
#   JAMP_PROXY_SO   host path to the proxy jampgamei386.so
#                   (default <repo>/rust/jampgamei386.so)
#   JAMP_GAMEDATA   host path to the read-only assets
#                   (default <repo>/original/GameData)
#   JAMP_IMAGE      runtime image (default jampgame-investigation:i386-bookworm)
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
SO=${JAMP_PROXY_SO:-"$ROOT/rust/jampgamei386.so"}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}

[ -f "$SO" ] || { echo "no proxy .so at $SO — build rust/ first" >&2; exit 1; }

mkdir -p "$ROOT/artifacts"

docker run --rm --name a4-ignore \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -v "$ROOT/artifacts":/srv/jka/artifacts \
    -w /srv/jka \
    "$IMAGE" \
    python3 /rt/a4_ignore.py "$@"
