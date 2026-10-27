#!/bin/sh
# A2 netStatus ground-truth harness driver (see a2_netstatus_groundtruth.py).
#
# Runs linuxjampded + the Rust proxy in the i386 runtime container and drives
# it with the synthetic JA MP client(s) of a2_netstatus_groundtruth.py, which
# speak the real protocol over UDP and reproduce the real client timing loop.
#
# Usage (all arguments go to a2_netstatus_groundtruth.py):
#   tools/runtime/a2_netstatus_groundtruth.sh --mode single --sv-fps 20 \
#       --client-fps 60 --tn -15 --duration 15
#   tools/runtime/a2_netstatus_groundtruth.sh --mode matrix
#
# Environment:
#   JAMP_PROXY_SO   host path to the proxy jampgamei386.so
#                   (default <repo>/rust/jampgamei386.so)
#   JAMP_GAMEDATA   host path to the read-only assets
#                   (default <repo>/original/GameData)
#   JAMP_IMAGE      runtime image (default jampgame-investigation:i386-bookworm)
#   JAMP_PUBLISH_PORT  publish this UDP port so a *real* client on the host can
#                   connect (--mode real): e.g. JAMP_PUBLISH_PORT=29191
#
# Artifacts (the server console log and the matrix JSON) land in
# <repo>/artifacts/; the `netStatus` table itself is printed to the client.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
SO=${JAMP_PROXY_SO:-"$ROOT/rust/jampgamei386.so"}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}

[ -f "$SO" ] || { echo "no proxy .so at $SO — build rust/ first" >&2; exit 1; }

mkdir -p "$ROOT/artifacts"

docker run --rm --name a2-netstatus \
    ${JAMP_PUBLISH_PORT:+-p "$JAMP_PUBLISH_PORT:$JAMP_PUBLISH_PORT/udp"} \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -v "$ROOT/artifacts":/srv/jka/artifacts \
    -w /srv/jka \
    "$IMAGE" \
    python3 /rt/a2_netstatus_groundtruth.py "$@"
