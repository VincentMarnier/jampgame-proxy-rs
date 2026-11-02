#!/bin/sh
# A3: end-to-end check of the server-side `cl_timeNudge` neutralisation
# (`proxy_sv_voidTimeNudge`, D-015).
#
# Runs the a2 synthetic client (tn = -15) against the real engine + this proxy,
# samples `level.clients[0].ps.commandTime - level.time` once per GAME_RUN_FRAME
# with gdb (a3_timenudge_void.gdb), then enables the feature at runtime with
# rcon and samples again. The clock offset must move by the estimated -14/-15 ms
# while the netStatus table keeps reporting the client's real setting.
#
# Prerequisites: rust/jampgamei386.so built, the i386 runtime image and
# original/GameData present (same as a2_netstatus_groundtruth.sh).
#
# Usage: tools/runtime/a3_timenudge_void.sh
# Env: JAMP_PROXY_SO, JAMP_GAMEDATA, JAMP_IMAGE, JAMP_PORT, A3_DURATION

set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
SO=${JAMP_PROXY_SO:-"$ROOT/rust/jampgamei386.so"}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
PORT=${JAMP_PORT:-29193}
DURATION=${A3_DURATION:-45}
NAME=a3-timenudge-void
LOG="$ROOT/artifacts/a2_single_server.log"

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

echo "== starting engine + synthetic client (tn -15, void off, log on) =="
docker run -d --name "$NAME" --cap-add=SYS_PTRACE \
  -v "$SO":/srv/jka/base/jampgamei386.so:ro \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  -v "$HERE":/rt:ro \
  -v "$ROOT/artifacts":/srv/jka/artifacts \
  -w /srv/jka \
  "$IMAGE" \
  python3 /rt/a2_netstatus_groundtruth.py \
    --mode single --port "$PORT" --tn -15 --lan --duration "$DURATION" \
    --server-set proxy_sv_voidTimeNudge=0 \
    --server-set proxy_sv_voidTimeNudgeLog=1 \
    --server-set rconpassword=test >/dev/null

# The log line is the estimator's settled verdict (it needs ~1 s + the window).
echo "== waiting for the settled timeNudge verdict =="
i=0
while [ "$i" -lt 60 ]; do
  if [ -f "$LOG" ] && grep -q "timeNudge non-zero" "$LOG"; then
    break
  fi
  i=$((i + 1))
  sleep 1
done
grep -q "timeNudge non-zero" "$LOG" || {
  echo "== no settled verdict in the server log: FAIL ==" >&2
  tail -40 "$LOG" >&2 || true
  exit 1
}

sample() {
  docker exec "$NAME" sh -c \
    'PID=$(pgrep linuxjampded); gdb -q -p $PID -x /rt/a3_timenudge_void.gdb 2>/dev/null' \
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

echo "== sampling with the feature off =="
BEFORE=$(sample)
echo "   $BEFORE"

echo "== enabling proxy_sv_voidTimeNudge at runtime =="
rcon "proxy_sv_voidTimeNudge 1"
# Let the estimator refresh, confirm (two readings) and ramp (1 ms per cmd).
sleep 4

echo "== sampling with the feature on =="
AFTER=$(sample)
echo "   $AFTER"

echo "== disabling proxy_sv_voidTimeNudge again (table shows the range) =="
rcon "proxy_sv_voidTimeNudge 0"
sleep 2

# The estimator must not have converged to 0 while the correction was active:
# its feed is raw, so the verdict stays non-zero (no "back to zero" line).
feedback_bad=0
if grep -q "timeNudge back to zero" "$LOG"; then
  echo "== estimator converged while the feature was on: FAIL ==" >&2
  feedback_bad=1
fi

# The netStatus table, printed by the client at the end of its run, reports the
# range again now that the feature is off.
i=0
while [ "$i" -lt "$((DURATION + 20))" ] \
  && docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null | grep -q true; do
  i=$((i + 1))
  sleep 1
done

median() {
  echo "$1" | sed -n 's/.*median=\(-*[0-9][0-9]*\).*/\1/p'
}
before_median=$(median "$BEFORE")
after_median=$(median "$AFTER")
[ -n "$before_median" ] && [ -n "$after_median" ] || {
  echo "== gdb produced no samples: FAIL ==" >&2
  exit 1
}
shift=$((after_median - before_median))
echo "== clock shift (after - before): $shift ms (expected about -15) =="

fail=$feedback_bad
if [ "$shift" -gt -10 ] || [ "$shift" -lt -20 ]; then
  fail=1
fi

table=$(docker logs "$NAME" 2>&1 | grep -E '^client0 ' | tail -1 || true)
echo "== netStatus scored row seen by the client =="
printf '%s\n' "$table"
# The estimator must keep reporting the client's true -15, not 0.
if printf '%s\n' "$table" | grep -q ' yes '; then
  echo "   table still reports the client's real setting"
else
  echo "== netStatus table lost the real setting: FAIL ==" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "== FAIL =="
  exit 1
fi
echo "== PASS =="
