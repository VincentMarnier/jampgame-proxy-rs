#!/bin/sh
# Build the proxy and run a persistent dedicated server under the real 2003
# engine, with the proxy overlaid as jampgamei386.so, bots enabled, and the
# UDP port published so real game clients can connect. Intended for manual
# end-to-end testing with human players and bots.
#
# Flow:
#   1. rebuild + stage the proxy via the dev container (rust/dev.sh), unless
#      JAMP_SKIP_BUILD is set;
#   2. run linuxjampded in the i386 runtime image with
#        - the staged proxy mounted over /srv/jka/base/jampgamei386.so,
#        - original/GameData mounted read-only (assets provide maps + bots),
#        - bot_enable/bot_minplayers so bots fill the server and make room
#          for humans as they join,
#        - the UDP port published on the host so real clients can join.
#
# The server runs in the foreground (Ctrl-C stops it); on an interactive
# terminal the engine's stdin is attached so console commands can be typed.
#
# Prerequisites:
#  - tools/docker runtime image built (tools/docker/build.sh)
#  - original/GameData present
#  - docker (used by dev.sh for the build step and here for the run)
#
# Usage: rust/scripts/run-server.sh
# Env:
#   JAMP_PORT       UDP port to publish (default 29070, the stock JKA port)
#   JAMP_BIND       host bind address for the published port (default 0.0.0.0)
#   JAMP_MAP        map to load (default mp/ffa1)
#   JAMP_GAMETYPE   g_gametype (default 0 = FFA)
#   JAMP_BOTS       bot_minplayers (default 4; 0 disables bots)
#   JAMP_MAXCLIENTS sv_maxclients (default 16)
#   JAMP_IMAGE      runtime image (default jampgame-investigation:i386-bookworm)
#   JAMP_GAMEDATA   host game assets (default <repo>/original/GameData)
#   JAMP_NO_PROXY   set to 1 to run the pristine module instead of the proxy
#   JAMP_SKIP_BUILD set to 1 to reuse the staged rust/jampgamei386.so
#   JAMP_KEEP       set to 1 to keep the container after exit (no --rm)
#   JAMP_EXTRA      extra engine command-line arguments (e.g. +set g_gravity 100)

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
RUST="$ROOT/rust"
SO="$RUST/jampgamei386.so"

IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}
PORT=${JAMP_PORT:-29070}
BIND=${JAMP_BIND:-0.0.0.0}
MAP=${JAMP_MAP:-mp/duel1}
GAMETYPE=${JAMP_GAMETYPE:-6}
BOTS=${JAMP_BOTS:-4}
MAXCLIENTS=${JAMP_MAXCLIENTS:-16}
NAME=${JAMP_NAME:-jka-proxy-rs-server}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "image $IMAGE missing - build it first: tools/docker/build.sh" >&2
  exit 2
fi
if [ ! -d "$GAMEDATA" ]; then
  echo "game assets missing at $GAMEDATA" >&2
  exit 2
fi

if [ "${JAMP_NO_PROXY:-0}" != "1" ]; then
  if [ "${JAMP_SKIP_BUILD:-0}" = "1" ]; then
    if [ ! -f "$SO" ]; then
      echo "missing $SO and JAMP_SKIP_BUILD is set - build it first" >&2
      exit 2
    fi
  else
    echo "== building + staging proxy (rust/dev.sh sh ./scripts/build.sh) =="
    "$RUST/dev.sh" sh ./scripts/build.sh
  fi
fi

echo "== starting server: $MAP (g_gametype $GAMETYPE), up to $BOTS bots, udp/$PORT =="
echo "   real players connect to: <host-ip>:$PORT"
if [ "${JAMP_NO_PROXY:-0}" = "1" ]; then
  echo "   module: pristine jampgamei386.so (no proxy)"
else
  echo "   module: proxy $SO"
fi

# Free the name if a stale container is still around, so re-runs work.
docker rm -f "$NAME" >/dev/null 2>&1 || true

TTY=
if [ -t 0 ] && [ -t 1 ]; then
  TTY=-it
fi
RM=
if [ "${JAMP_KEEP:-0}" != "1" ]; then
  RM=--rm
fi

PROXY_MOUNT=
if [ "${JAMP_NO_PROXY:-0}" != "1" ]; then
  PROXY_MOUNT="-v $SO:/srv/jka/base/jampgamei386.so:ro"
fi

# shellcheck disable=SC2086
exec docker run $RM $TTY --name "$NAME" --cap-add=SYS_PTRACE \
  $PROXY_MOUNT \
  -p "$BIND:$PORT:$PORT/udp" \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  "$IMAGE" \
  ./linuxjampded \
    +set rconpassword "proxy" \
    +set dedicated 1 +set net_port "$PORT" \
    +set fs_cdpath /srv/jka/gamedata \
    +set sv_pure 0 +set sv_maxclients "$MAXCLIENTS" \
    +set bot_enable 1 +set bot_minplayers "$BOTS" \
    +set g_gametype "$GAMETYPE" \
    +set sv_hostname "^7proxy-rs^7 ^5test server" \
    +set timelimit 0 +set fraglimit 0 \
    +set proxy_sv_teamSizeRules "2:1:0:0,3:0:1:0" \
    +set proxy_sv_enableNetStatus 1 \
    +set g_forcepowerdisable 163837 \
    +set g_weaponDisable 524279 \
    +set g_friendlyFire 1 \
    +set g_friendlySaber 1 \
    +map "$MAP" \
    ${JAMP_EXTRA:-}