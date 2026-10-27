#!/bin/sh
# Master-switch regression: `proxy_sv_enable 0` at runtime must hand rcon back
# to the pristine engine in a usable state, and `proxy_sv_enable 1` must work
# again through rcon.
#
# The rcon timer block `SVC_RemoteCommand` NOPs (25 B @ 0x8056b26) is followed
# by an unpatched `mov %eax,0x81dc464` (0x8056b3f) that keeps storing an
# undefined `%eax` into the gate's "last accepted rcon" slot. While the proxy is
# attached the gate is NOP'd, so the garbage is inert; reverting the gate
# without clearing the slot made it drop every rcon for ~49 days (the live bug
# this test pins).
#
# Checks, via gdb on the running engine:
#   - enabled: NOP tile at 0x8056b26, redirect length 0x03 at 0x8056c7d;
#   - disabled: pristine timer block, 0x81dc464 cleared to 0 right after the
#     switch and advancing to a sane uptime value after one accepted rcon,
#     redirect length 0xbf;
#   - rcon answers before disabling, while disabled (spaced past the pristine
#     500 ms gate), after a map change while disabled, and after re-enabling.
#
# Prerequisites / env: same as rust/scripts/engine-test.sh.
# Usage: rust/scripts/enable-toggle-test.sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
RUST="$ROOT/rust"
SO="$RUST/jampgamei386.so"

IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
PORT=${JAMP_PORT:-29998}
NAME=${JAMP_NAME:-jka-proxy-rs-enable-toggle}

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
    +set g_gametype 6 +set timelimit 0 +set fraglimit 0 \
    +set rconpassword test +map mp/duel1 >/dev/null

i=0
while [ "$i" -lt 90 ]; do
  if docker logs "$NAME" 2>&1 | grep -q "Engine properly patched"; then
    break
  fi
  i=$((i + 1))
  sleep 1
done

# rcon <cmd>: succeeds when the engine answered within 2 s.
rcon() {
  docker exec -e CMD="$1" -e PORT="$PORT" "$NAME" python3 -c '
import os, socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(2.0)
s.sendto(b"\xff\xff\xff\xffrcon test " + os.environ["CMD"].encode(),
         ("127.0.0.1", int(os.environ["PORT"])))
try:
    s.recvfrom(65535)
except Exception:
    sys.exit(1)
' || return 1
}

# expect_reply <description> <cmd>: retry past the pristine 500 ms gate.
expect_reply() {
  desc=$1
  cmd=$2
  n=0
  while [ "$n" -lt 5 ]; do
    if rcon "$cmd"; then
      echo "   $desc: reply"
      sleep 1
      return 0
    fi
    n=$((n + 1))
    sleep 0.6
  done
  echo "== $desc: no rcon reply: FAIL =="
  docker logs "$NAME" 2>&1 | tail -30
  exit 1
}

# read <gdb format> <addr>: the value gdb prints after the address/symbol.
read_mem() {
  docker exec "$NAME" gdb -p 1 -batch -ex "x/$1 $2" 2>&1 \
    | grep -E "^$2" | head -1 | sed 's/.*:[[:space:]]*//'
}

expect_eq() {
  desc=$1
  got=$2
  want=$3
  if [ "$got" != "$want" ]; then
    echo "== $desc: got '$got', want '$want': FAIL =="
    exit 1
  fi
  echo "   $desc: $got"
}

echo "== enabled at boot =="
expect_reply "rcon status" "status"
expect_eq "rcon timer patched" "$(read_mem 1xb 0x8056b26)" "0x90"
expect_eq "redirect length patched" "$(read_mem 1xb 0x8056c7d)" "0x03"

echo "== proxy_sv_enable 0 =="
expect_reply "disable" "proxy_sv_enable 0"
sleep 1
expect_eq "rcon timer pristine" "$(read_mem 1xb 0x8056b26)" "0xe8"
expect_eq "redirect length pristine" "$(read_mem 1xb 0x8056c7d)" "0xbf"
expect_eq "gate timestamp cleared" "$(read_mem 1xw 0x81dc464)" "0x00000000"
expect_reply "rcon status while disabled" "status"
last=$(read_mem 1xw 0x81dc464)
if [ "$last" = "0x00000000" ]; then
  echo "== gate timestamp still 0 after an accepted rcon: FAIL =="
  exit 1
fi
echo "   gate timestamp advanced to $last"

echo "== map change while disabled =="
loads_before=$(docker logs "$NAME" 2>&1 | grep -c "proxy-rs: loading original game library" || true)
expect_reply "map mp/duel1" "map mp/duel1"
i=0
while [ "$i" -lt 60 ]; do
  loads_now=$(docker logs "$NAME" 2>&1 | grep -c "proxy-rs: loading original game library" || true)
  if [ "$loads_now" -gt "$loads_before" ]; then
    break
  fi
  i=$((i + 1))
  sleep 1
done
sleep 2
running=$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)
if [ "$running" != "true" ]; then
  echo "== server died on the map change: FAIL =="
  docker logs "$NAME" 2>&1 | tail -30
  exit 1
fi
expect_eq "rcon timer still pristine" "$(read_mem 1xb 0x8056b26)" "0xe8"
expect_reply "rcon status after map change" "status"

echo "== proxy_sv_enable 1 =="
expect_reply "re-enable" "proxy_sv_enable 1"
sleep 1
expect_eq "rcon timer patched again" "$(read_mem 1xb 0x8056b26)" "0x90"
expect_eq "redirect length patched again" "$(read_mem 1xb 0x8056c7d)" "0x03"
expect_reply "rcon status after re-enable" "status"

echo "== master switch off/on keeps rcon working: PASS =="
