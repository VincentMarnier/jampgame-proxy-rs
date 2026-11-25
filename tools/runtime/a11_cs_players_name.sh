#!/bin/sh
# A11: a re-pushed CS_PLAYERS name stays inside MAX_NETNAME.
#
# Runs linuxjampded + the Rust proxy in the i386 runtime container with two
# synthetic JA MP clients. `Long` has a legal 34-byte latin-1 name (32 ASCII
# bytes + two 0xE9). It creates a TFFA match; `Watcher` follows it, so the
# proxy re-sends Long's stored CS_PLAYERS base to the Watcher. The harness
# requires every re-sent `n` to be at most MAX_NETNAME-1 (35) bytes. Pre-fix the
# lossy UTF-8 decode sends 38 bytes and fails, matching the client's
# `Com_sprintf: Output length 38 too short, require 41 bytes.` report.
#
# Usage:
#   tools/runtime/a11_cs_players_name.sh
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

docker run --rm --name a11-cs-players-name \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -v "$ROOT/artifacts":/srv/jka/artifacts \
    -w /srv/jka \
    "$IMAGE" \
    python3 /rt/a11_cs_players_name.py "$@"
