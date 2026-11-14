#!/bin/sh
# A10: how the duel ladder treats a lone player: they wait in the arena like
# regular duel (the stock `CheckTournament` → `AddTournamentPlayer` pull is let
# through), the court-assignment notice never answers that automatic pull, and
# the "waiting for at least two willing players" line is announced on roster
# changes instead of every 5 s.
#
# Runs linuxjampded + the Rust proxy in the i386 runtime container with one
# synthetic JA MP client on `g_gametype 3` and `proxy_parallel_duels_enable 1`.
# The client is left alone for six seconds (TEAM_FREE, no notice, one waiting
# line), then opts out with `team spectator`, and an explicit `team f` after
# that gets exactly one court-assignment notice (the debounced repeat none).
#
# Usage:
#   tools/runtime/a10_duel_lone_notice.sh
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

docker run --rm --name a10-duel-lone-notice \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -v "$ROOT/artifacts":/srv/jka/artifacts \
    -w /srv/jka \
    "$IMAGE" \
    python3 /rt/a10_duel_lone_notice.py "$@"
