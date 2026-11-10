#!/usr/bin/env python3
"""A5: end-to-end check of `callvote coinflip`.

Runs `linuxjampded` + the Rust proxy with two synthetic JA MP clients (the A2
`ClientModel`). Both join the FFA, the caller starts `callvote coinflip`, the
other client votes yes, and the run checks the normal vote UI configstrings,
the pass message, and the final `svsay` chat (`Server: Coin flip: Heads!` or
`Tails!`).

Usage (inside the i386 runtime container, see a5_coinflip.sh):
    python3 /rt/a5_coinflip.py

Exit code 0 = PASS. The server console log lands in `artifacts/`.
"""
from __future__ import annotations

import argparse
import os
import random
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import a2_netstatus_groundtruth as a2  # noqa: E402


class TestClient(a2.ClientModel):
    """An A2 client that records every server command it receives."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.captured = []

    def handle_server_command(self, s):
        self.captured.append(s)


def configstring(client, index):
    prefix = f"cs {index} "
    for command in client.captured:
        if command.startswith(prefix):
            value = command[len(prefix):].strip()
            if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
                value = value[1:-1]
            return value
    return None


def run(*, port, engine, gamedata, artifacts, duration):
    extra_args = [
        "+set",
        "sv_maxclients",
        "8",
        "+set",
        "g_gametype",
        "0",
        "+set",
        "bot_enable",
        "0",
        "+set",
        "g_allowVote",
        "1",
    ]
    log_path = os.path.join(artifacts, "a5_coinflip_server.log")
    server = a2.ServerRun(
        engine,
        port,
        20,
        gamedata,
        log_path,
        extra_args=extra_args,
        net_forcenonlocal=True,
    )
    try:
        if not server.wait_ready(port):
            raise RuntimeError(f"server did not become ready (see {log_path})")

        rng = random.Random(11)
        clients = []
        for name in ("Caller", "Voter"):
            link = a2.Link(("127.0.0.1", port), rng=rng)
            client = TestClient(
                name, link, tn=0, fps=60, maxpackets=30, snaps=20, lan=False
            )
            client.start_connect(time.monotonic() * 1000.0)
            clients.append(client)
        caller, voter = clients

        steps = [
            # A synthetic client starts as a spectator; join the FFA first
            # (Cmd_CallVote_f refuses spectator votes outside duel).
            (2.5, lambda now: caller.send_command("team free", now)),
            (2.6, lambda now: voter.send_command("team free", now)),
            (4.0, lambda now: caller.send_command("callvote coinflip", now)),
            (6.0, lambda now: voter.send_command("vote yes", now)),
        ]

        period = 1000.0 / 60.0
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients}
        start = time.monotonic()
        done = set()
        while time.monotonic() - start < duration:
            now = time.monotonic() * 1000.0
            elapsed = time.monotonic() - start
            for index, (at, action) in enumerate(steps):
                if index not in done and elapsed > at:
                    action(now)
                    done.add(index)
            due = [c for c in clients if next_frame[id(c)] <= now]
            if not due:
                time.sleep(0.001)
                continue
            for client in due:
                next_frame[id(client)] += period
                if next_frame[id(client)] < now - 100:
                    next_frame[id(client)] = now + period
                client.frame(now)

        ok = True
        all_commands = [command for client in clients for command in client.captured]
        results = [c for c in all_commands if "Coin flip:" in c]

        caller_commands = caller.captured
        if not any("called a vote" in c for c in caller_commands):
            print("FAIL: caller did not see the 'called a vote' message")
            ok = False
        # The vote UI reads these four configstrings: time (non-empty), string,
        # yes and no.
        if not configstring(caller, 8):
            print("FAIL: CS_VOTE_TIME was never set")
            ok = False
        if configstring(caller, 9) != "coinflip":
            print(f"FAIL: CS_VOTE_STRING = {configstring(caller, 9)!r}")
            ok = False
        if configstring(caller, 10) != "1":
            print(f"FAIL: CS_VOTE_YES = {configstring(caller, 10)!r}")
            ok = False
        if configstring(caller, 11) != "0":
            print(f"FAIL: CS_VOTE_NO = {configstring(caller, 11)!r}")
            ok = False
        if not any("VOTEPASSED" in c for c in all_commands):
            print("FAIL: the vote did not pass")
            ok = False
        if not results:
            print("FAIL: no coin flip result")
            ok = False
        else:
            # svsay sends `Server: ...` as a normal `chat` server command.
            chats = [c for c in results if c.startswith("chat ") and "Server:" in c]
            if not chats:
                print(f"FAIL: result was not an svsay chat: {results!r}")
                ok = False
            elif not any("Heads" in c or "Tails" in c for c in chats):
                print(f"FAIL: result did not name Heads or Tails: {chats!r}")
                ok = False

        for client in clients:
            print(f"--- {client.name}: state={client.state} connected={client.connected}")
            print(f"    result commands: {[c for c in client.captured if 'Coin flip:' in c]!r}")
        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        server.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=29299)
    parser.add_argument("--engine", default="/srv/jka/linuxjampded")
    parser.add_argument("--gamedata", default="/srv/jka/gamedata")
    parser.add_argument("--artifacts", default="/srv/jka/artifacts")
    parser.add_argument("--duration", type=float, default=14.0)
    args = parser.parse_args()
    return run(
        port=args.port,
        engine=args.engine,
        gamedata=args.gamedata,
        artifacts=args.artifacts,
        duration=args.duration,
    )


if __name__ == "__main__":
    sys.exit(main())
