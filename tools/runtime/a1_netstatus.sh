#!/bin/sh
# A1/A1b netStatus semantic + in-vivo harness driver (see a1_netstatus_semantic.py).
#
# Computes the Rust symbol offsets from the staged .so via nm (they change on
# every rebuild) and runs the gdb session inside the i386 runtime container.
#
# Usage: tools/runtime/a1_netstatus.sh [path-to-so] [host-gamedata]
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
SO=${1:-"$ROOT/rust/jampgamei386.so"}
GAMEDATA=${2:-"$ROOT/original/GameData"}

STUB_OFF=$(nm "$SO" | awk '/sv_client_think_stub/{print $1; exit}')
PRINTER_OFF=$(nm "$SO" | awk '/client_command_net_status/{print $1; exit}')
FORWARD_OFF=$(nm -D "$SO" | awk '/jampgame_syscall_forward/{print $1; exit}')
[ -n "$STUB_OFF" ] && [ -n "$PRINTER_OFF" ] && [ -n "$FORWARD_OFF" ] || {
    echo "missing symbols in $SO — build first" >&2
    exit 1
}

docker run --rm --name a1-netstatus --cap-add=SYS_PTRACE \
    -e A1_STUB_OFF="$STUB_OFF" \
    -e A1_PRINTER_OFF="$PRINTER_OFF" \
    -e A1_FORWARD_OFF="$FORWARD_OFF" \
    -v "$SO":/srv/jka/base/jampgamei386.so:ro \
    -v "$GAMEDATA":/srv/jka/gamedata:ro \
    -v "$HERE":/rt:ro \
    -w /srv/jka \
    jampgame-investigation:i386-bookworm \
    gdb -q -batch -x /rt/a1_netstatus_semantic.py
