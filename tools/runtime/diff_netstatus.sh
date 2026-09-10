#!/bin/sh
# Differential netStatus harness driver (see diff_netstatus_semantic.py).
#
# Usage: tools/runtime/diff_netstatus.sh rust|original [path-to-proxy.so]
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
KIND=${1:?rust|original}
case "$KIND" in
    rust)     SO=${2:-"$ROOT/rust/jampgamei386.so"} ;;
    original) SO=${2:-"$ROOT/artifacts/exp/proxy/jampgamei386.so"} ;;
    *) echo "kind must be rust|original" >&2; exit 1 ;;
esac

case "$KIND" in
    rust)     PRINTER_OFF=$(nm "$SO" | awk '/client_command_net_status/{print $1; exit}') ;;
    original) PRINTER_OFF=$(nm "$SO" | awk '/Proxy_Engine_ClientCommand_NetStatus/{print $1; exit}') ;;
esac
[ -n "$PRINTER_OFF" ] || { echo "printer symbol missing in $SO" >&2; exit 1; }

docker run --rm --name diff-netstatus --cap-add=SYS_PTRACE \
    -e DIFF_KIND="$KIND" \
    -e DIFF_PRINTER_OFF="$PRINTER_OFF" \
    -e RUST_BACKTRACE=1 \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$ROOT/original/GameData":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -w /srv/jka \
    jampgame-investigation:i386-bookworm \
    gdb -q -batch -x /rt/diff_netstatus_semantic.py
