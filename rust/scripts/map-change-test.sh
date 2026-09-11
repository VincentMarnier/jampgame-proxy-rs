#!/bin/sh
# Map-change regression: the engine unloads/reloads the game module on a map
# change (SV_ShutdownGameProgs -> VM_Free -> dlclose, SV_InitGameProgs ->
# VM_Create -> dlopen -> GAME_INIT), so the proxy's attach_all runs a second
# time. The intra patches are deliberately not reverted by detach_all, so a
# non-idempotent patch (the RMG je->jmp at 0x804d131) used to be re-applied to
# its own output and rewrote the branch into a wild jump (E9 03 00 00 90 90 ->
# 0x9804d139, unmapped), crashing the server once a real client hit
# SV_SendClientGameState.
#
# This test drives several map changes on a bot-filled server and asserts the
# site stays the intended `E9 6B 02 00 00 90` (je 0x804d3a1 forced to jmp) and
# the dedicated server stays alive.
#
# Prerequisites / env: same as rust/scripts/engine-test.sh.
# Usage: rust/scripts/map-change-test.sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
RUST="$ROOT/rust"
SO="$RUST/jampgamei386.so"

IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
PORT=${JAMP_PORT:-29997}
NAME=${JAMP_NAME:-jka-proxy-rs-mapchange}

# 0x804d131 after the first (and every subsequent) attach_all must be the
# pristine `0f 84 6a 02 00 00` (je 0x804d3a1) converted to
# `e9 6b 02 00 00 90`; the check below matches those exact bytes.

if [ ! -f "$SO" ]; then
  echo "missing $SO - build it first: rust/dev.sh sh ./scripts/build.sh" >&2
  exit 2
fi
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "image $IMAGE missing - build it first: tools/docker/build.sh" >&2
  exit 2
}
[ -d "$GAMEDATA" ] || {
  echo "game assets missing at $GAMEDATA" >&2
  exit 2
}

cleanup() {
  [ "${JAMP_KEEP:-0}" = "1" ] || docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker rm -f "$NAME" >/dev/null 2>&1 || true

echo "== starting engine with proxy overlay (udp/$PORT) =="
docker run -d --name "$NAME" --cap-add=SYS_PTRACE \
  -v "$SO":/srv/jka/base/jampgamei386.so:ro \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  "$IMAGE" \
  ./linuxjampded \
    +set dedicated 1 +set net_port "$PORT" \
    +set fs_cdpath /srv/jka/gamedata \
    +set sv_pure 0 +set sv_maxclients 8 \
    +set bot_enable 1 +set bot_minplayers 2 \
    +set g_gametype 0 +set timelimit 0 +set fraglimit 0 \
    +set rconpassword test +map mp/duel1 >/dev/null

# Wait for the first attach to complete.
i=0
while [ "$i" -lt 60 ]; do
  if docker logs "$NAME" 2>&1 | grep -q "Engine properly patched"; then
    break
  fi
  i=$((i + 1))
  sleep 1
done

rcon() {
  docker exec "$NAME" python3 -c "
import socket,time
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2)
s.sendto(b'\xff'*4 + b'rcon test $1', ('127.0.0.1', $PORT)); time.sleep(1)
try: s.recvfrom(65535)
except Exception: pass
"
}

check_site() {
  # Print the 6 bytes at 0x804d131 as seen by a gdb attach.
  docker exec "$NAME" gdb -p 1 -batch -ex 'x/6xb 0x804d131' 2>&1 \
    | grep -A0 "0x804d131" | sed 's/.*:\t//'
}

cycles=0
i=0
while [ "$i" -lt 3 ]; do
  cycle=$((i + 1))
  echo "== map change $cycle =="
  rcon "map mp/duel1"
  sleep 20

  running=$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)
  if [ "$running" != "true" ]; then
    echo "== server died on map change $cycle: FAIL =="
    docker logs "$NAME" 2>&1 | tail -30
    exit 1
  fi

  got=$(check_site || true)
  if ! printf '%b' "$got" | grep -qE "0xe9[[:space:]]+0x6b[[:space:]]+0x02[[:space:]]+0x00[[:space:]]+0x00[[:space:]]+0x90"; then
    echo "== RMG branch corrupted after change $cycle: got '$got': FAIL =="
    docker logs "$NAME" 2>&1 | tail -30
    exit 1
  fi
  cycles=$cycle
  i=$((i + 1))
done

echo "== survived $cycles map changes with the branch intact: PASS =="
