#!/bin/sh
# End-to-end smoke test: run the real 2003 engine in the i386 runtime
# container with THIS proxy overlaid as jampgamei386.so, and check that
#  1. the engine loads the proxy and finds its vmMain,
#  2. the proxy dllEntry prints (banner once loaded),
#  3. the proxy loads the original module and forwards GAME_INIT to it,
#  4. the dedicated server starts and answers protocol-26 UDP queries
#     (proving traps flow back through the proxy's syscall shim).
#
# Prerequisites:
#  - rust/jampgamei386.so built (rust/dev.sh sh ./scripts/build.sh)
#  - tools/docker runtime image built (tools/docker/build.sh)
#  - original/GameData present
#
# Usage: rust/scripts/engine-test.sh
# Env: JAMP_IMAGE, JAMP_GAMEDATA, JAMP_PORT (default 29999), JAMP_KEEP (1 to
#      keep the container after the run)

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
RUST="$ROOT/rust"
SO="$RUST/jampgamei386.so"

if [ ! -f "$SO" ]; then
  echo "missing $SO — build it first: rust/dev.sh sh ./scripts/build.sh" >&2
  exit 2
fi

IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
PORT=${JAMP_PORT:-29999}
NAME=${JAMP_NAME:-jka-proxy-rs-test}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "image $IMAGE missing — build it first: tools/docker/build.sh" >&2
  exit 2
fi
if [ ! -d "$GAMEDATA" ]; then
  echo "game assets missing at $GAMEDATA" >&2
  exit 2
fi

cleanup() {
  if [ "${JAMP_KEEP:-0}" != "1" ]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

docker rm -f "$NAME" >/dev/null 2>&1 || true

echo "== starting engine with proxy overlay ($SO) on udp/$PORT =="
docker run -d --name "$NAME" --cap-add=SYS_PTRACE \
  -p "127.0.0.1:$PORT:$PORT/udp" \
  -v "$SO":/srv/jka/base/jampgamei386.so:ro \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  "$IMAGE" \
  ./linuxjampded \
    +set dedicated 1 +set net_port "$PORT" \
    +set fs_cdpath /srv/jka/gamedata \
    +set sv_pure 0 +set sv_maxclients 8 \
    +set g_gametype 6 +set timelimit 0 +set fraglimit 0 \
    +map mp/duel1 >/dev/null

fail=1
i=0
while [ "$i" -lt 60 ]; do
  logs=$(docker logs "$NAME" 2>&1 || true)
  # All four markers must appear: engine found the proxy, proxy dllEntry
  # fired, proxy loaded the original, original module ran its GAME_INIT.
  if printf '%s' "$logs" | grep -q "found \*\*vmMain\*\*" \
     && printf '%s' "$logs" | grep -q "proxy-rs .*dllEntry, engine syscall registered" \
     && printf '%s' "$logs" | grep -q "original game library" \
     && printf '%s' "$logs" | grep -q "properly loaded"; then
    fail=0
    break
  fi
  i=$((i + 1))
  sleep 1
done

echo "== engine/proxy log (first 60 lines) =="
printf '%s\n' "$logs" | head -60

if [ "$fail" -eq 0 ] && command -v python3 >/dev/null 2>&1; then
  echo "== probing server (protocol 26 query) =="
  # The proxy's own load markers appear before the map/UDP listener is fully
  # ready, so retry the probe until the server answers (or give up).
  probe_fail=1
  j=0
  while [ "$j" -lt 20 ]; do
    if python3 "$ROOT/tools/q3_udp_query.py" 127.0.0.1 "$PORT" >/dev/null 2>&1; then
      probe_fail=0
      break
    fi
    j=$((j + 1))
    sleep 1
  done
  if [ "$probe_fail" -eq 0 ]; then
    echo "== server answered the query: PASS =="
    exit 0
  else
    echo "== server did not answer the query: FAIL =="
    exit 1
  fi
fi

if [ "$fail" -eq 0 ]; then
  echo "== log markers present: PASS =="
else
  echo "== log markers missing: FAIL =="
  echo "full log:" >&2
  printf '%s\n' "$logs" >&2
fi
exit "$fail"
