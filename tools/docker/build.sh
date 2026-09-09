#!/bin/sh
# Build the investigation runtime image.
#
# Usage: tools/docker/build.sh
#
# The build context is the repository root. The root .dockerignore keeps the
# context small and guarantees original/GameData never enters an image layer.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
IMAGE=${JAMP_IMAGE:-jampgame-investigation:i386-bookworm}

docker build \
  -t "$IMAGE" \
  -f "$ROOT/tools/docker/run.Dockerfile" \
  "$ROOT"

echo
echo "Built $IMAGE"
echo "Run a server with: tools/docker/run.sh"
