#!/usr/bin/env python3
"""A10: how the duel ladder treats a lone player.

Three behaviours a solo player depends on:

1. They wait in the arena, as in regular duel: the stock
   `CheckTournament` → `AddTournamentPlayer` pull (`g_main.c:2955-2965`, the
   shipped `CheckTournament` at `0x89194`) calls `SetTeam(ent, "f")`
   (`AddTournamentPlayer` at `0x86924`), and the ladder lets the one willing
   player through so they can warm up until a partner arrives.
2. That automatic pull must stay silent: the court-assignment notice
   ("Parallel duels assign courts automatically; use team spectator to
   leave.") answers a player's own refused `/team` request only, not the pull
   (which runs every frame while a spectator is online).
3. The "waiting for at least two willing players" line is announced on roster
   changes, not on the old 5 s timer.

Scenario (inside the i386 runtime container, see a10_duel_lone_notice.sh):

- launch `g_gametype 3` (duel) with `proxy_parallel_duels_enable 1` and no bots;
- connect one synthetic client (`Solo`) and leave it alone for 5 s;
- connect `Duo`: the ladder forms the round and both spawn;
- connect `Third` while that round runs: it stays a willing non-court spectator;
- `Third` sends `team f`, then `team f` again (debounced).

Observables:
- `Solo` is `TEAM_FREE` (`t=0` in its own `CS_PLAYERS`) while waiting alone:
  the ladder let the regular-duel pull through;
- zero court-assignment notices before any explicit request (before the fix:
  one per frame);
- exactly one court-assignment notice for `Third`'s explicit `team f` (the
  ladder refuses a client it did not draft), and the immediate second request
  hits the base 5 s debounce (`switchTeamTime`) and adds none;
- exactly one "waiting for at least two willing players" line in the whole
  run (the connect-time roster change), not one per five seconds.

Exit code 0 = PASS. The server console log lands in `artifacts/`.
"""
from __future__ import annotations

import argparse
import os
import random
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import a2_netstatus_groundtruth as a2  # noqa: E402

NOTICE = "Parallel duels assign courts automatically"
WAITING = "waiting for at least two willing players"

# How long the client is left alone before the others connect.
SOLO_WINDOW_S = 5.0


class DuelServer(a2.ServerRun):
    """The A2 server launcher with the duel-ladder command line.

    `a2.ServerRun` always sets `g_gametype 0` before the extra arguments, and a
    second `+set` for the same `CVAR_LATCH` cvar is finalised as `0` by the
    engine's startup sequence, so the gametype is passed only once (the A6
    launcher's note).
    """

    def __init__(self, engine, port, sv_fps, gamedata, log_path):
        self.log_path = log_path
        self.log_file = open(log_path, "wb")
        cmd = [
            "stdbuf",
            "-oL",
            "-eL",
            engine,
            "+set",
            "dedicated",
            "1",
            "+set",
            "net_port",
            str(port),
            "+set",
            "fs_cdpath",
            gamedata,
            "+set",
            "sv_pure",
            "0",
            "+set",
            "sv_maxclients",
            "8",
            "+set",
            "sv_fps",
            str(sv_fps),
            "+set",
            "g_gametype",
            "3",
            "+set",
            "bot_enable",
            "0",
            "+set",
            "bot_minplayers",
            "0",
            "+set",
            "timelimit",
            "0",
            "+set",
            "fraglimit",
            "0",
            "+set",
            "duel_fraglimit",
            "0",
            "+set",
            "proxy_sv_enable",
            "1",
            "+set",
            "proxy_parallel_duels_enable",
            "1",
            "+set",
            "net_forcenonlocal",
            "1",
            "+map",
            "mp/duel1",
        ]
        self.proc = subprocess.Popen(
            cmd,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            cwd=os.path.dirname(engine),
        )


class TestClient(a2.ClientModel):
    """An A2 client that records every server command with its arrival time."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.captured = []
        self.captured_at = []

    def handle_server_command(self, s):
        self.captured.append(s)
        self.captured_at.append(time.monotonic())


def notices(client, needle):
    """Arrival times of every server command containing `needle`."""
    return [
        at
        for command, at in zip(client.captured, client.captured_at)
        if needle in command
    ]


CS_PLAYERS = 1131


def info_pairs(info):
    """Split one `key\\value` info string into a dict."""
    parts = (info or "").split("\\")
    return {parts[i]: parts[i + 1] for i in range(0, len(parts) - 1, 2)}


def live_configstrings(client, before=None):
    """The client's `CS_*` map with the engine's `cs` command updates applied.

    JKA sends live configstring changes as the reliable server command
    `cs <index> "<value>"` (`SV_SetConfigstring` → `SV_SendServerCommand`), not
    as `svc_configstring`; `client.configstrings` only holds the gamestate, so
    the captured commands are what reflect the current session team.
    """
    live = dict(client.configstrings)
    for command, at in zip(client.captured, client.captured_at):
        if before is not None and at >= before:
            break
        if not command.startswith("cs "):
            continue
        args = tokenize(command)
        if len(args) < 3:
            continue
        try:
            live[int(args[1])] = args[2]
        except ValueError:
            continue
    return live


def tokenize(line):
    """The parts of `Cmd_TokenizeString` a `cs`/`print` line needs."""
    args = []
    i, n = 0, len(line)
    while i < n:
        while i < n and line[i] <= " ":
            i += 1
        if i >= n:
            break
        if line[i] == '"':
            i += 1
            start = i
            while i < n and line[i] != '"':
                i += 1
            args.append(line[start:i])
            i += 1
        else:
            start = i
            while i < n and line[i] > " ":
                i += 1
            args.append(line[start:i])
    return args


def team_of(client, name, before=None):
    """Latest `t` value in the `CS_PLAYERS` entry whose `n` is `name`.

    The proxy's per-viewer board prefixes the name with a `(Duel N) `,
    `(Bench) ` or `(Spec) ` label, so the label is stripped before comparing.
    """
    live = live_configstrings(client, before)
    for index in range(CS_PLAYERS, CS_PLAYERS + 32):
        pairs = info_pairs(live.get(index))
        n = pairs.get("n") or ""
        label = re.match(r"\((?:Duel \d+|Bench|Spec)\) (.*)$", n)
        if (label.group(1) if label else n) == name:
            return pairs.get("t")
    return None


def run(*, port, engine, gamedata, artifacts, duration):
    log_path = os.path.join(artifacts, "a10_duel_lone_notice_server.log")
    server = DuelServer(engine, port, 20, gamedata, log_path)
    try:
        if not server.wait_ready(port):
            raise RuntimeError(f"server did not become ready (see {log_path})")

        rng = random.Random(10)
        clients = {}
        for name in ("Solo", "Duo", "Third"):
            link = a2.Link(("127.0.0.1", port), rng=rng)
            clients[name] = TestClient(
                name, link, tn=0, fps=60, maxpackets=30, snaps=20, lan=False
            )
        solo, duo, third = clients["Solo"], clients["Duo"], clients["Third"]
        solo.start_connect(time.monotonic() * 1000.0)

        seen = {}
        fired_at = {}

        def snapshot(key):
            seen[key] = team_of(solo, "Solo", before=time.monotonic())

        def connect(client):
            client.start_connect(time.monotonic() * 1000.0)

        steps = [
            # The stock pull happens on the first frames after the connect.
            (SOLO_WINDOW_S - 1.0, "lobby", lambda now: snapshot("lobby")),
            (SOLO_WINDOW_S, "duo-connects", lambda now: connect(duo)),
            (SOLO_WINDOW_S + 1.5, "third-connects", lambda now: connect(third)),
            (SOLO_WINDOW_S + 3.0, "team-free", lambda now: third.send_command("team f", now)),
            (SOLO_WINDOW_S + 3.5, "team-free-again", lambda now: third.send_command("team f", now)),
        ]
        done = set()

        period = 1000.0 / 60.0
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients.values()}
        start = time.monotonic()
        while time.monotonic() - start < duration:
            now = time.monotonic() * 1000.0
            elapsed = time.monotonic() - start
            for at, key, action in steps:
                if key not in done and elapsed > at:
                    fired_at[key] = time.monotonic()
                    action(now)
                    done.add(key)
            due = [c for c in clients.values() if next_frame[id(c)] <= now]
            if not due:
                time.sleep(0.001)
                continue
            for client in due:
                next_frame[id(client)] += period
                if next_frame[id(client)] < now - 100:
                    next_frame[id(client)] = now + period
                client.frame(now)

        ok = True

        def check(condition, message):
            nonlocal ok
            print(("PASS: " if condition else "FAIL: ") + message)
            ok = ok and condition

        print(
            f"--- Solo: state={solo.state} connected={solo.connected} "
            f"team lobby={seen.get('lobby')}"
        )
        print(
            f"--- Duo: connected={duo.connected} team={team_of(duo, 'Duo')} "
            f"| Third: connected={third.connected} team={team_of(third, 'Third')}"
        )
        solo_notices = notices(solo, NOTICE)
        times = notices(third, NOTICE)
        waits = notices(solo, WAITING)
        first_at = fired_at["team-free"]
        second_at = fired_at["team-free-again"]
        before = [at for at in times if at < first_at]
        first = [at for at in times if first_at <= at < second_at]
        second = [at for at in times if at >= second_at]
        print(
            f"--- notices: solo={len(solo_notices)} third-before={len(before)} "
            f"third-first={len(first)} third-second={len(second)} "
            f"waiting={len(waits)}"
        )

        check(solo.connected and duo.connected and third.connected, "all three clients connected")
        check(
            seen.get("lobby") == "0",
            f"Solo waits in the arena like regular duel (t=0, got "
            f"{seen.get('lobby')!r})",
        )
        check(
            len(solo_notices) == 0,
            f"the game's per-frame AddTournamentPlayer pull is silent "
            f"({len(solo_notices)} notice(s) for the lone player)",
        )
        check(
            team_of(duo, "Duo") == "0",
            f"the round formed with Solo and Duo in the arena (Duo t="
            f"{team_of(duo, 'Duo')!r})",
        )
        check(
            team_of(third, "Third") == "3",
            f"the late joiner waits as a spectator (t="
            f"{team_of(third, 'Third')!r})",
        )
        check(
            len(first) == 1,
            f"the explicit `team f` got exactly one notice ({len(first)})",
        )
        check(
            len(second) == 0,
            f"the debounced second `team f` got no notice ({len(second)})",
        )
        check(
            len(waits) == 1,
            f"the waiting line is announced once, not on a timer ({len(waits)} "
            f"in {duration:.1f} s)",
        )

        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        server.stop()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--engine", default="/srv/jka/linuxjampded", help="dedicated server binary"
    )
    parser.add_argument("--gamedata", default="/srv/jka/gamedata")
    parser.add_argument("--artifacts", default="/srv/jka/artifacts")
    parser.add_argument("--port", type=int, default=29140)
    parser.add_argument("--duration", type=float, default=11.0)
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
