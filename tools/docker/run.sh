#!/bin/sh
# Run a command (default: the original dedicated server) in the investigation
# container.
#
# Environment:
#   JAMP_IMAGE       image name (default jampgame-investigation:i386-bookworm)
#   JAMP_GAMEDATA    host path of the read-only game assets
#                    (default <repo>/original/GameData)
#   JAMP_PROXY_SO    optional host path to a proxy build of jampgamei386.so
#                    that is mounted read-only over /srv/jka/base/jampgamei386.so
#   JAMP_ARTIFACTS   optional host directory mounted rw at /srv/jka/artifacts
#                    (logs, core dumps, patched copies produced in-container)
#
# Usage:
#   tools/docker/run.sh                       # start the default duel server
#   tools/docker/run.sh ./linuxjampded +set dedicated 1 +set net_port 29099
#   tools/docker/run.sh bash                  # interactive shell
#   tools/docker/run.sh gdb ./linuxjampded

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}
GAMEDATA=${JAMP_GAMEDATA:-"$ROOT/original/GameData"}

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "Image $IMAGE not found; building it first." >&2
  "$ROOT/tools/docker/build.sh"
fi

# Default command: dedicated duel server on the original engine + game module.
if [ "$#" -eq 0 ]; then
  set -- ./linuxjampded \
    +set dedicated 1 \
    +set net_port 29099 \
    +set fs_cdpath /srv/jka/gamedata \
    +set sv_pure 0 \
    +set sv_hostname "^7jampgame-proxy-rs^7 ^5investigation" \
    +set sv_maxclients 8 \
    +set g_gametype 6 \
    +set timelimit 0 \
    +set fraglimit 0 \
    +map mp/duel1
fi

docker run --rm -it \
  --cap-add=SYS_PTRACE \
  ${JAMP_ARTIFACTS:+-v "$JAMP_ARTIFACTS":/srv/jka/artifacts} \
  ${JAMP_PROXY_SO:+-v "$JAMP_PROXY_SO":/srv/jka/base/jampgamei386.so:ro} \
  -v "$GAMEDATA":/srv/jka/gamedata:ro \
  "$IMAGE" \
  "$@"
