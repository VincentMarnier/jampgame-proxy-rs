# Investigation runtime for the original Jedi Academy Linux dedicated server
# (`linuxjampded`) and game module (`jampgamei386.so`).
#
# Rationale / provenance
# ----------------------
# Inspired by `original/jampgame-proxy/test.Dockerfile` (removed 2026-09-10; recover via `git clone https://github.com/VincentMarnier/jampgame_proxy <dir>`), which runs the 2003
# binaries in a 32-bit (`i386/`) Debian userspace. Unlike that image we do not
# download `jalinuxded` from the network at build time: the reference binaries
# are COPYied unmodified from the repository's `original/jalinuxded_1.011/`
# tree, so the image is byte-stable and reproducible offline.
#
# The user-provided game assets (`original/GameData`, ~1.7 GB) are NEVER baked
# in. They are mounted read-only at runtime under `/srv/jka/gamedata` and the
# server is pointed at them with `+set fs_cdpath /srv/jka/gamedata`
# (see `tools/docker/run.sh`). The engine's `fs_basepath` stays `/srv/jka`
# (a writable image layer) so the loader always finds *our* pristine game
# module under `/srv/jka/base/` regardless of what the assets contain.

FROM i386/debian:bookworm

LABEL org.opencontainers.image.title="jampgame-proxy-rs investigation runtime"
LABEL org.opencontainers.image.description="32-bit userspace to build and run the original Jedi Academy dedicated server / game module for reverse engineering"
LABEL org.opencontainers.image.authors="jampgame-proxy-rs"

ENV DEBIAN_FRONTEND=noninteractive
ENV LC_ALL=C
ENV HOME=/srv/jka

# libcxa.so.1 is the Intel C++ runtime both 2003 binaries NEEDED-depend on and
# no modern distro ships. It must sit where ld.so finds it by soname
# (same trick as original/jampgame-proxy/test.Dockerfile:9).
COPY original/jalinuxded_1.011/libcxa.so.1 /usr/lib/libcxa.so.1

# Reference binaries from original/jalinuxded_1.011/ (never modified).
# Layout mirrors a real dedicated-server deployment:
#   /srv/jka/linuxjampded
#   /srv/jka/base/jampgamei386.so          <- pristine game module
#   /srv/jka/base/jampgame_original.so     <- same module under the name the
#                                             proxy dlopen()s (base/
#                                             jampgame_original.so is
#                                             CWD-relative -> /srv/jka/base/)
COPY original/jalinuxded_1.011/linuxjampded /srv/jka/linuxjampded
COPY original/jalinuxded_1.011/jampgamei386.so /srv/jka/base/jampgamei386.so
COPY original/jalinuxded_1.011/jampgamei386.so /srv/jka/base/jampgame_original.so

RUN chmod 0755 /srv/jka/linuxjampded /srv/jka/base/jampgamei386.so /srv/jka/base/jampgame_original.so \
    && mkdir -p /srv/jka/gamedata \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        file \
        gcc \
        g++ \
        make \
        gdb \
        strace \
        procps \
        python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /srv/jka

# No ENTRYPOINT: pass any command as arguments (e.g. the linuxjampded command
# line). `tools/docker/run.sh` is the recommended front-end.
CMD ["bash"]
