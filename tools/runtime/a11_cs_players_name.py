#!/usr/bin/env python3
"""A11: a re-pushed `CS_PLAYERS` name never exceeds `MAX_NETNAME`.

The proxy decodes configstrings as UTF-8 and stores `CS_PLAYERS` bases as
`String`s. A legacy single-byte name is not UTF-8, so `from_utf8_lossy` can
inflate it: two latin-1 `0xE9` bytes become two 3-byte U+FFFD replacements.
This harness reproduces the report

    Com_sprintf: Output length 38 too short, require 41 bytes.

which TaystJK's `CG_Obituary` emits when `targetInfo->name` is 38 bytes (its
destination is `char targetName[MAX_NETNAME+2]`, and it appends `^7`).

`Long` connects with a legal 34-byte name (32 `x` + two `0xE9`) and creates a
TFFA match. `Watcher` follows Long, which changes Long from a prefixed outsider
to the Watcher's own match player; the per-viewer `CS_PLAYERS` repair therefore
re-sends the stored base, not just the prefixed value. The harness then reads
every `CS_PLAYERS` `n` the Watcher received for Long's slot and requires the
largest latin-1 byte length to be at most `MAX_NETNAME-1` (35).

Pre-fix this prints `max n bytes: 38` and fails; post-fix it prints 35 and
passes. A pass covers the wire value, not the client's cgame itself (the
displayed warning is the client-side symptom of that byte length).
"""

from __future__ import annotations

import argparse
import random
import sys
import time

sys.path.insert(0, "/rt")
import a2_netstatus_groundtruth as a2  # noqa: E402
import a6_tffa_spectate_join as a6  # noqa: E402

# `MAX_NETNAME` (q_shared.h) is 36 including the NUL, so 35 name bytes are the
# game's own cap (`ClientCleanName`).
MAX_NETNAME = 36
MAX_NAME_BYTES = MAX_NETNAME - 1
# 32 ASCII + two latin-1 accented bytes = 34 bytes, legal for ClientCleanName.
LONG = "x" * 32 + "\u00e9\u00e9"
assert len(LONG.encode("latin-1")) == 34


def run(*, port: int, engine: str, gamedata: str, artifacts: str, duration: float) -> int:
    log_path = f"{artifacts}/a11_cs_players_name_server.log"
    server = a6.TffaServer(engine, port, 20, gamedata, log_path)
    try:
        if not server.wait_ready(port):
            raise RuntimeError(f"server did not become ready (see {log_path})")

        rng = random.Random(11)
        long_client = a6.TestClient(
            LONG,
            a2.Link(("127.0.0.1", port), rng=rng),
            tn=0,
            fps=60,
            maxpackets=30,
            snaps=20,
            lan=False,
        )
        watcher = a6.TestClient(
            "Watcher",
            a2.Link(("127.0.0.1", port), rng=rng),
            tn=0,
            fps=60,
            maxpackets=30,
            snaps=20,
            lan=False,
        )
        clients = [long_client, watcher]
        for client in clients:
            client.start_connect(time.monotonic() * 1000.0)

        period = 1000.0 / 60.0
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients}
        started = time.monotonic()
        created = followed = False
        while time.monotonic() - started < duration:
            now = time.monotonic() * 1000.0
            elapsed = time.monotonic() - started
            if not created and elapsed > 3.0:
                long_client.send_command("tffa_create", now)
                created = True
            if not followed and elapsed > 5.0:
                watcher.send_command("follow " + LONG, now)
                followed = True
            due = [c for c in clients if next_frame[id(c)] <= now]
            if not due:
                time.sleep(0.001)
                continue
            for client in due:
                next_frame[id(client)] += period
                if next_frame[id(client)] < now - 100:
                    next_frame[id(client)] = now + period
                client.frame(now)

        rows = []
        for command in watcher.captured:
            if not command.startswith("cs "):
                continue
            args = a6.tokenize(command)
            if len(args) < 3:
                continue
            try:
                index = int(args[1])
            except ValueError:
                continue
            if not (a6.CS_PLAYERS <= index < a6.CS_PLAYERS + 32):
                continue
            name = a6.info_pairs(args[2]).get("n", "")
            raw = name.encode("latin-1")
            if b"x" * 8 in raw:
                rows.append((index - a6.CS_PLAYERS, len(raw), raw))

        for slot, size, raw in rows:
            print(f"slot={slot} n_bytes={size} n={raw!r}")
        if not rows:
            print("FAIL: Watcher received no CS_PLAYERS entry for Long")
            return 1
        longest = max(size for _, size, _ in rows)
        print(
            f"long latin-1 name: {len(LONG.encode('latin-1'))} bytes; "
            f"largest re-sent n: {longest} bytes (cap {MAX_NAME_BYTES})"
        )
        if longest > MAX_NAME_BYTES:
            print(
                "FAIL: a re-sent CS_PLAYERS name exceeds MAX_NETNAME-1; "
                "TaystJK's CG_Obituary targetName[MAX_NETNAME+2] overflows"
            )
            return 1
        print("PASS: every re-sent CS_PLAYERS name fits MAX_NETNAME-1")
        return 0
    finally:
        server.stop()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", default="/srv/jka/linuxjampded")
    parser.add_argument("--gamedata", default="/srv/jka/gamedata")
    parser.add_argument("--artifacts", default="/srv/jka/artifacts")
    parser.add_argument("--port", type=int, default=29171)
    parser.add_argument("--duration", type=float, default=12.0)
    args = parser.parse_args()
    sys.exit(
        run(
            port=args.port,
            engine=args.engine,
            gamedata=args.gamedata,
            artifacts=args.artifacts,
            duration=args.duration,
        )
    )


if __name__ == "__main__":
    main()
