#!/usr/bin/env python3
"""A6: end-to-end check of a team request from a TFFA spectator.

Scenario (D-006, the follow-spectate join):

- `Alpha` creates match 1 (open) and is playing there.
- `Bravo` creates match 2 with the password `hunter2`.
- `Charlie` (spectator in main) follows `Alpha`, `Delta` follows `Bravo`.
- `Charlie` sends `join red` (the proxy's spelling for clients without a
  base `join` command): must travel into match 1 and play there. A second
  `join blue` right after is refused by the base 5-second team-switch debounce,
  which the proxy reproduces for that spelling.
- `Delta` sends `team red`: match 2 is protected, so the `SetTeam` password
  gate must answer with the `tffa_join 2 <password>` invite (and Delta must not
  be put on main's red team). `tffa_join 2 hunter2` then unlocks it and a
  second `team red` joins match 2.

Observables:
- `tffa_list` counts (match 0 main, 1, 2), so a moved spectator shows up in the
  watched match instead of main.
- The per-viewer `CS_PLAYERS` strings: before the move a player in another match
  is demoted to `(TFFA N)`/`t=3`; after the move the watched player is base
  (`n=<name>`, `t=1|2`) from the follower's view, and the moved spectator is no
  longer prefixed from the watched match's players' view.
- The invite print reaches Delta.

Usage (inside the i386 runtime container, see a6_tffa_spectate_join.sh):
    python3 /rt/a6_tffa_spectate_join.py

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
import jka_proto as jp  # noqa: E402

PASSWORD = "hunter2"

# `CS_PLAYERS = CS_ICONS + MAX_ICONS = 1131` (`bg_public.h` chain; the proxy's
# `sdk::CS_PLAYERS` is pinned to the same value).
CS_PLAYERS = 1131


class TffaServer(a2.ServerRun):
    """The A2 server launcher with a TFFA command line.

    `a2.ServerRun` always sets `g_gametype 0` before the extra arguments. Both
    `sv_maxclients` and `g_gametype` are `CVAR_LATCH`, and a `+set` on top of an
    earlier `+set` for the same cvar is finalised as `0` by the engine's startup
    sequence (measured: `+set g_gametype 0 +set g_gametype 6` starts as 0, while
    `+set g_gametype 6` alone starts as 6). This launcher therefore passes the
    team gametype only once.
    """

    def __init__(self, engine, port, sv_fps, gamedata, log_path, **kwargs):
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
            "6",
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
            "proxy_sv_enable",
            "1",
            "+set",
            "proxy_parallel_tffas_enable",
            "1",
            "+set",
            "net_forcenonlocal",
            "1",
            "+map",
            "mp/ctf1",
        ]
        self.proc = subprocess.Popen(
            cmd,
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            cwd=os.path.dirname(engine),
        )


class TestClient(a2.ClientModel):
    """An A2 client that records every server command it receives."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.captured = []
        self.captured_at = []

    def handle_server_command(self, s):
        self.captured.append(s)
        self.captured_at.append(time.monotonic())


def print_texts(client):
    """The argument the client's cgame would pass to `CG_Printf`."""
    texts = []
    for command in client.captured:
        if not command.startswith("print "):
            continue
        args = tokenize(command)
        if len(args) >= 2:
            texts.append(args[1])
    return texts


def tokenize(line):
    """The parts of `Cmd_TokenizeString` a `print` line needs."""
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


def info_pairs(info):
    """Split one `key\\value` info string into a dict."""
    if info is None:
        return {}
    parts = info.split("\\")
    return {parts[i]: parts[i + 1] for i in range(0, len(parts) - 1, 2)}


def player_info(client, name, before=None):
    """Latest `CS_PLAYERS` info string for the slot whose `n` is `name`."""
    found = None
    for command, at in zip(client.captured, client.captured_at):
        if before is not None and at >= before:
            break
        if not command.startswith("cs "):
            continue
        args = tokenize(command)
        if len(args) < 3:
            continue
        try:
            index = int(args[1])
        except ValueError:
            continue
        if not (CS_PLAYERS <= index < CS_PLAYERS + 32):
            continue
        value = args[2]
        if info_pairs(value).get("n") != name:
            continue
        found = value
    return found


def player_field(info, key):
    return info_pairs(info).get(key)


def tffa_lists(client):
    """Texts of every `tffa_list` reply this client received."""
    return [text for text in print_texts(client) if text.startswith("Matches:")]


def match_counts(text):
    """`{id: (players, red, blue)}` from one `tffa_list` reply."""
    counts = {}
    for line in text.splitlines():
        match = re.match(r"(\d+)(?: \(main\))?: (\d+) players, (-?\d+)-(-?\d+)", line)
        if match:
            counts[int(match.group(1))] = (
                int(match.group(2)),
                int(match.group(3)),
                int(match.group(4)),
            )
    return counts


def run(*, port, engine, gamedata, artifacts, duration):
    log_path = os.path.join(artifacts, "a6_tffa_spectate_join_server.log")
    server = TffaServer(engine, port, 20, gamedata, log_path)
    try:
        if not server.wait_ready(port):
            raise RuntimeError(f"server did not become ready (see {log_path})")

        rng = random.Random(13)
        clients = {}
        for name in ("Alpha", "Bravo", "Charlie", "Delta"):
            link = a2.Link(("127.0.0.1", port), rng=rng)
            client = TestClient(
                name, link, tn=0, fps=60, maxpackets=30, snaps=20, lan=False
            )
            client.start_connect(time.monotonic() * 1000.0)
            clients[name] = client
        alpha, bravo, charlie, delta = (
            clients["Alpha"],
            clients["Bravo"],
            clients["Charlie"],
            clients["Delta"],
        )

        steps = [
            (3.0, "create1", lambda now: alpha.send_command("tffa_create", now)),
            (
                3.2,
                "create2",
                lambda now: bravo.send_command(f"tffa_create {PASSWORD}", now),
            ),
            (5.0, "follow1", lambda now: charlie.send_command("follow Alpha", now)),
            (5.2, "follow2", lambda now: delta.send_command("follow Bravo", now)),
            (7.5, "list-before", lambda now: alpha.send_command("tffa_list", now)),
            (8.5, "charlie-join", lambda now: charlie.send_command("join red", now)),
            (9.5, "charlie-retry", lambda now: charlie.send_command("join blue", now)),
            (9.0, "delta-join", lambda now: delta.send_command("team red", now)),
            (11.0, "list-mid", lambda now: alpha.send_command("tffa_list", now)),
            (
                12.0,
                "delta-unlock",
                lambda now: delta.send_command(f"tffa_join 2 {PASSWORD}", now),
            ),
            (16.5, "delta-join-2", lambda now: delta.send_command("team red", now)),
            (18.0, "list-after", lambda now: alpha.send_command("tffa_list", now)),
        ]

        period = 1000.0 / 60.0
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients.values()}
        start = time.monotonic()
        done = set()
        fired_at = {}
        while time.monotonic() - start < duration:
            now = time.monotonic() * 1000.0
            elapsed = time.monotonic() - start
            for at, key, action in steps:
                if key not in done and elapsed > at:
                    action(now)
                    fired_at[key] = time.monotonic()
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

        for client in clients.values():
            print(f"--- {client.name}: state={client.state} connected={client.connected}")

        # Follow is what makes the watched instance the client's effective one:
        # Charlie sees Alpha base (own team), not as a prefixed outsider.
        charlie_alpha = player_info(
            charlie, "Alpha", before=fired_at.get("charlie-join")
        )
        check(
            charlie_alpha is not None
            and player_field(charlie_alpha, "n") == "Alpha"
            and player_field(charlie_alpha, "t") == "1",
            f"Charlie follows Alpha and sees him base: {charlie_alpha!r}",
        )

        lists = tffa_lists(alpha)
        check(len(lists) >= 3, f"Alpha received {len(lists)} tffa_list replies")
        if len(lists) >= 3:
            before, mid, after = lists[0], lists[1], lists[2]
            print(f"--- list before: {before!r}")
            print(f"--- list mid:    {mid!r}")
            print(f"--- list after:  {after!r}")
            b, m, a = match_counts(before), match_counts(mid), match_counts(after)
            # Two players created their matches; both spectators are still in
            # main when the first list is taken.
            check(b.get(0, (0,))[0] == 2, f"before: main holds both spectators: {b}")
            check(b.get(1, (0,))[0] == 1, f"before: match 1 has Alpha: {b}")
            check(b.get(2, (0,))[0] == 1, f"before: match 2 has Bravo: {b}")
            check(
                any(
                    line.startswith("2: ") and "[password]" in line
                    for line in before.splitlines()
                ),
                "before: match 2 is marked [password]",
            )
            # Charlie travelled into match 1 and joined red; Delta travelled to
            # match 2 but was refused the team (password) and stays a spectator
            # there, i.e. it still counts as a member of match 2.
            check(m.get(0, (0,))[0] == 0, f"mid: main is empty: {m}")
            check(m.get(1, (0,))[0] == 2, f"mid: match 1 has Alpha + Charlie: {m}")
            check(m.get(2, (0,))[0] == 2, f"mid: match 2 has Bravo + Delta: {m}")
            # Delta unlocked the password and joined a team of match 2.
            check(a.get(0, (0,))[0] == 0, f"after: main is still empty: {a}")
            check(a.get(1, (0,))[0] == 2, f"after: match 1 has Alpha + Charlie: {a}")
            check(a.get(2, (0,))[0] == 2, f"after: match 2 has Bravo + Delta: {a}")

        # The password invite reached Delta through the SetTeam gate.
        texts = print_texts(delta)
        invite = [
            text
            for text in texts
            if "password protected" in text and "tffa_join 2 <password>" in text
        ]
        check(bool(invite), f"Delta got the tffa_join 2 <password> invite: {invite!r}")
        accepted = [text for text in texts if "Password accepted" in text]
        check(bool(accepted), f"Delta unlocked match 2: {accepted!r}")

        # After the move Bravo sees Delta in his own match: no prefix, still a
        # spectator before the unlock and a playing team after it.
        bravo_delta_after = player_info(bravo, "Delta")
        check(
            bravo_delta_after is not None
            and player_field(bravo_delta_after, "n") == "Delta"
            and player_field(bravo_delta_after, "t") in ("1", "2"),
            f"Delta is in match 2 and playing, as seen from Bravo: "
            f"{bravo_delta_after!r}",
        )
        charlie_texts = print_texts(charlie)
        check(
            any("May not switch teams more than once per 5 seconds" in text for text in charlie_texts),
            f"Charlie's immediate second join hit the 5 s debounce: {charlie_texts!r}",
        )
        alpha_charlie = player_info(alpha, "Charlie")
        check(
            alpha_charlie is not None
            and player_field(alpha_charlie, "n") == "Charlie"
            and player_field(alpha_charlie, "t") == "1",
            f"Charlie is in match 1 and playing, as seen from Alpha: "
            f"{alpha_charlie!r}",
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
    parser.add_argument("--port", type=int, default=29130)
    parser.add_argument("--duration", type=float, default=21.0)
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
