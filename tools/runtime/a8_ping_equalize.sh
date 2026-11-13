#!/bin/sh
# A8: end-to-end check of server-side ping equalisation
# (`proxy_sv_pingEqualize`, D-028).
#
# Runs two synthetic clients through the a2 protocol harness with different
# one-way delays (client0 fast, client1 slow), samples each game client's
# `level.clients[i].ps.commandTime - level.time` and `ps.ping` at the engine's
# `SV_SendClientMessages` (after the post-forward write, before the snapshot;
# a8_ping_equalize.gdb), then:
#   1. with the feature off, checks ps.ping == cl->ping (the OFFSET_PS_PING
#      assumption);
#   2. enables a cap at runtime and checks that the fast client's clock shift
#      moved by -(target - its ping) and its ps.ping reads the target, while
#      the slow client is untouched;
#   3. sets the cap back to 0 and checks the 1 ms/command ramp returns the
#      fast client to its natural shift.
#
# Prerequisites: rust/jampgamei386.so built, the i386 runtime image and
# original/GameData present (same as a2_netstatus_groundtruth.sh).
#
# Usage: tools/runtime/a8_ping_equalize.sh
# Env: JAMP_PROXY_SO, JAMP_GAMEDATA, JAMP_IMAGE, JAMP_PORT, A8_DURATION,
#      A8_CAP, A8_FAST_DELAY_MS, A8_SLOW_DELAY_MS

set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
SO=${JAMP_PROXY_SO:-"$ROOT/rust/jampgamei386.so"}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
PORT=${JAMP_PORT:-29194}
DURATION=${A8_DURATION:-150}
CAP=${A8_CAP:-200}
FAST_DELAY=${A8_FAST_DELAY_MS:-15}
SLOW_DELAY=${A8_SLOW_DELAY_MS:-150}
NAME=a8-ping-equalize
LOG="$ROOT/artifacts/a2_equalize_server.log"

[ -f "$SO" ] || { echo "no proxy .so at $SO — build rust/ first" >&2; exit 1; }
[ -d "$GAMEDATA" ] || { echo "game assets missing at $GAMEDATA" >&2; exit 1; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "image $IMAGE missing — build it with tools/docker/build.sh" >&2
  exit 1
}

cleanup() {
  [ "${JAMP_KEEP:-0}" = "1" ] || docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker rm -f "$NAME" >/dev/null 2>&1 || true
mkdir -p "$ROOT/artifacts"
rm -f "$LOG"

echo "== starting engine + two synthetic clients (fast ${FAST_DELAY}ms, slow ${SLOW_DELAY}ms one-way) =="
docker run -d --name "$NAME" --cap-add=SYS_PTRACE \
  -v "$SO":/srv/jka/base/jampgamei386.so:ro \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  -v "$HERE":/rt:ro \
  -v "$ROOT/artifacts":/srv/jka/artifacts \
  -w /srv/jka \
  "$IMAGE" \
  python3 /rt/a2_netstatus_groundtruth.py \
    --mode equalize --port "$PORT" --duration "$DURATION" \
    --delay-ms "$FAST_DELAY" --slow-delay-ms "$SLOW_DELAY" \
    --server-set proxy_sv_pingEqualize=0 \
    --server-set proxy_sv_pingEqualizeLog=1 \
    --server-set rconpassword=test >/dev/null

# Wait for both clients to be in the game and the preview target to be logged.
echo "== waiting for both clients and the log =="
i=0
while [ "$i" -lt 90 ]; do
  if [ -f "$LOG" ] && grep -q "pingEqualize target=" "$LOG"; then
    break
  fi
  i=$((i + 1))
  sleep 1
done
grep -q "pingEqualize target=" "$LOG" || {
  echo "== no pingEqualize log line in the server log: FAIL ==" >&2
  tail -60 "$LOG" >&2 || true
  exit 1
}
# Let the slow client's 32-snapshot ping average settle.
sleep 8

sample() {
  docker exec "$NAME" sh -c \
    'PID=$(pgrep linuxjampded); gdb -q -p $PID -x /rt/a8_ping_equalize.gdb 2>/dev/null' \
    | grep -E '^SUMMARY'
}

rcon() {
  docker exec -e CMD="$1" -e PORT="$PORT" "$NAME" python3 -c '
import os, socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(2.0)
s.sendto(b"\xff\xff\xff\xffrcon test " + os.environ["CMD"].encode(),
         ("127.0.0.1", int(os.environ["PORT"])))
s.recvfrom(65535)
'
}

field() {
  # $1 = SUMMARY line, $2 = field name, $3 = default
  printf '%s\n' "$1" | sed -n "s/.* $2=\\(-*[0-9][0-9]*\\).*/\\1/p" | head -1 | grep . || printf '%s' "$3"
}

echo "== sampling with the feature off =="
BEFORE=$(sample)
echo "$BEFORE"

before0=$(printf '%s\n' "$BEFORE" | grep 'client0 ')
before1=$(printf '%s\n' "$BEFORE" | grep 'client1 ')
[ -n "$before0" ] && [ -n "$before1" ] || {
  echo "== gdb produced no samples for both clients: FAIL ==" >&2
  exit 1
}
shift0_before=$(field "$before0" shift_median 0)
ping0_before=$(field "$before0" ping_median 0)
engine0_before=$(field "$before0" engine_ping_median 0)
shift1_before=$(field "$before1" shift_median 0)

# Assumption 1: with the feature off the game-side ps.ping is the engine's
# cl->ping, so OFFSET_PS_PING = 524 is the field the engine writes.
drift=$((ping0_before - engine0_before))
[ "$drift" -le 3 ] && [ "$drift" -ge -3 ] || {
  echo "== ps.ping ($ping0_before) != cl->ping ($engine0_before) with the feature off: FAIL ==" >&2
  exit 1
}

echo "== enabling proxy_sv_pingEqualize $CAP at runtime =="
rcon "proxy_sv_pingEqualize $CAP"
# Target refresh + the depth ramp (one command per 100 ms; the queue rounds
# the delay to the client's command interval, D-028.8, so the tolerance below
# is one command interval wide).
sleep 10

AFTER=$(sample)
echo "$AFTER"
after0=$(printf '%s\n' "$AFTER" | grep 'client0 ')
after1=$(printf '%s\n' "$AFTER" | grep 'client1 ')
[ -n "$after0" ] && [ -n "$after1" ] || {
  echo "== gdb produced no samples after enabling: FAIL ==" >&2
  exit 1
}
shift0_after=$(field "$after0" shift_median 0)
ping0_after=$(field "$after0" ping_median 0)
shift1_after=$(field "$after1" shift_median 0)

TARGET=$(sed -n 's/.*pingEqualize target=\([0-9][0-9]*\).*/\1/p' "$LOG" | tail -1)
[ -n "$TARGET" ] || {
  echo "== no target in the log: FAIL ==" >&2
  exit 1
}
delay_expected=$((TARGET - ping0_before))
[ "$delay_expected" -ge 0 ] || delay_expected=0
# D-028.9 splits the delay: the action (queue) half reaches ps.commandTime
# directly, the view half is a snapshot-clock shift the *client's* clock walk
# absorbs. The gdb stalls during sampling make the client extrapolate and undo
# part of that absorption, so the sampled shift must show at least the action
# half and at most the full delay; the full delay is pinned by the ps.ping
# check below (the proxy's own applied value).
action_expected=$((delay_expected / 2))
shift_at_least=$((shift0_before - delay_expected))
shift_at_most=$((shift0_before - action_expected + 20))
echo "== target=$TARGET fast ping=$ping0_before delay=$delay_expected action~$action_expected shift expected in [$shift_at_least..$shift_at_most] =="

fail=0
if [ "$shift0_after" -lt "$shift_at_least" ] || [ "$shift0_after" -gt "$shift_at_most" ]; then
  echo "== fast client shift $shift0_after is outside [$shift_at_least..$shift_at_most]: FAIL ==" >&2
  fail=1
fi
eff_delta=$((ping0_after - TARGET))
if [ "$eff_delta" -gt 25 ] || [ "$eff_delta" -lt -25 ]; then
  echo "== fast client ps.ping $ping0_after does not read the target $TARGET: FAIL ==" >&2
  fail=1
fi
slow_delta=$((shift1_after - shift1_before))
if [ "$slow_delta" -gt 10 ] || [ "$slow_delta" -lt -10 ]; then
  echo "== the slow client was delayed too ($shift1_before -> $shift1_after): FAIL ==" >&2
  fail=1
fi
grep -q "pingEqualize client=0 .*delay=" "$LOG" || {
  echo "== no per-client engage line for client0: FAIL ==" >&2
  fail=1
}

echo "== disabling proxy_sv_pingEqualize (ramp out) =="
rcon "proxy_sv_pingEqualize 0"
sleep 10

RELEASED=$(sample)
echo "$RELEASED"
release0=$(printf '%s\n' "$RELEASED" | grep 'client0 ')
[ -n "$release0" ] || {
  echo "== gdb produced no samples after release: FAIL ==" >&2
  exit 1
}
shift0_released=$(field "$release0" shift_median 0)
release_delta=$((shift0_released - shift0_before))
if [ "$release_delta" -gt 20 ] || [ "$release_delta" -lt -20 ]; then
  echo "== fast client did not return to its natural shift ($shift0_before -> $shift0_released): FAIL ==" >&2
  fail=1
fi
grep -q "pingEqualize client=0 delay=0 (released)" "$LOG" || {
  echo "== no per-client release line for client0: FAIL ==" >&2
  fail=1
}

if [ "$fail" -ne 0 ]; then
  echo "== FAIL =="
  exit 1
fi
echo "== PASS =="
