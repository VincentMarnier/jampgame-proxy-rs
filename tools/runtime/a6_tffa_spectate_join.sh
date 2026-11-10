#!/bin/sh
# A6: end-to-end check of `/join red` from a spectator watching a TFFA instance.
#
# Runs linuxjampded + the Rust proxy in the i386 runtime container with four
# synthetic JA MP clients (the A2 protocol client). Two matches are created,
# two spectators follow one player each, and the run checks that an explicit
# team request moves them into the match they are watching (not the match their
# own assignment still points at); a password-protected match answers with the
# `tffa_join <id> <password>` invite and the same client then unlocks it and
# joins a team.
#
# Usage:
#   tools/runtime/a6_tffa_spectate_join.sh
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

docker run --rm --name a6-tffa-spectate-join \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -v "$ROOT/artifacts":/srv/jka/artifacts \
    -w /srv/jka \
    "$IMAGE" \
    python3 /rt/a6_tffa_spectate_join.py "$@"
