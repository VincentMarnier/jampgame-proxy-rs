#!/usr/bin/env python3
"""A4: end-to-end check of the player `/ignore` command (D-016).

Runs `linuxjampded` + the Rust proxy with three synthetic JA MP clients (the A2
`ClientModel`): one ignores another by name, the speaker says something, and the
run checks that only the ignoring client misses the chat. With the feature on it
then unignores and checks delivery comes back; with it off it checks the
disabled reply and that nothing is filtered.

Usage (inside the i386 runtime container, see a4_ignore.sh):
    python3 /rt/a4_ignore.py --mode enabled
    python3 /rt/a4_ignore.py --mode disabled

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


def chats(client):
    return [
        s
        for s in client.captured
        if s.startswith(("chat ", "tchat ", "lchat ", "ltchat "))
    ]


def tokenize(line):
    """The parts of `Cmd_TokenizeString` that matter to a `print` line.

    Whitespace separates tokens and a quoted token ends at the next `"` -- it
    has no escape handling, so an inner quote truncates the argument (and with
    it the trailing newline).
    """
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


def print_texts(client):
    """The argument the client's cgame would pass to `CG_Printf`."""
    texts = []
    for command in client.captured:
        if command.startswith("print "):
            args = tokenize(command)
            if len(args) >= 2:
                texts.append(args[1])
    return texts


def proxy_prints(client):
    return [text for text in print_texts(client) if "proxy:" in text]


def run(*, enabled, port, engine, gamedata, artifacts, duration):
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
    ]
    if enabled:
        extra_args += ["+set", "proxy_sv_enableIgnore", "1"]
    log_path = os.path.join(
        artifacts, f"a4_ignore_{'on' if enabled else 'off'}_server.log"
    )
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
        rng = random.Random(7)
        clients = []
        for name in ("Ignorer", "Speaker", "SpeakerTwo", "Witness"):
            link = a2.Link(("127.0.0.1", port), rng=rng)
            client = TestClient(
                name, link, tn=0, fps=60, maxpackets=30, snaps=20, lan=False
            )
            client.start_connect(time.monotonic() * 1000.0)
            clients.append(client)
        ignorer, speaker, _speaker_two, witness = clients

        steps = [
            # "Speak" is a prefix of two names: an ambiguity reply.
            (3.5, "ambiguous", lambda now: ignorer.send_command("ignore Speak", now)),
            # The exact name resolves to one player.
            (4.5, "ignore", lambda now: ignorer.send_command("ignore Speaker", now)),
            (6.0, "say", lambda now: speaker.send_command("say live-ignore-check", now)),
        ]
        if enabled:
            steps += [
                (
                    9.0,
                    "unignore",
                    lambda now: ignorer.send_command("unignore Speaker", now),
                ),
                (
                    11.0,
                    "say2",
                    lambda now: speaker.send_command("say after-unignore", now),
                ),
            ]

        period = 1000.0 / 60.0
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients}
        start = time.monotonic()
        done = set()
        while time.monotonic() - start < duration:
            now = time.monotonic() * 1000.0
            elapsed = time.monotonic() - start
            for at, key, action in steps:
                if key not in done and elapsed > at:
                    action(now)
                    done.add(key)
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
        for client in clients:
            print(
                f"--- {client.name}: state={client.state} "
                f"connected={client.connected}"
            )
            print(f"    proxy prints: {proxy_prints(client)!r}")
            print(f"    chat: {chats(client)!r}")

        ignorer_chats = chats(ignorer)
        speaker_chats = chats(speaker)
        witness_chats = chats(witness)
        prints = proxy_prints(ignorer)
        # Every proxy reply must arrive as one closed `print` token ending in the
        # newline; an inner `"` used to truncate it at that point.
        for text in prints:
            if not text.endswith("\n"):
                print(f"FAIL: truncated proxy print without newline: {text!r}")
                ok = False
        if enabled:
            ambiguous = [text for text in prints if " matches " in text]
            if not ambiguous:
                print("FAIL: no ambiguity reply")
                ok = False
            elif not ("Speaker" in ambiguous[0] and "SpeakerTwo" in ambiguous[0]):
                print(f"FAIL: ambiguity reply truncated: {ambiguous[0]!r}")
                ok = False
            if not any("you are now ignoring" in p for p in prints):
                print("FAIL: no ignore confirmation on the ignoring client")
                ok = False
            if not any("no longer ignoring" in p for p in prints):
                print("FAIL: no unignore confirmation on the ignoring client")
                ok = False
            if any("live-ignore-check" in c for c in ignorer_chats):
                print("FAIL: the ignoring client received the speaker's chat")
                ok = False
            if not any("after-unignore" in c for c in ignorer_chats):
                print("FAIL: chat did not come back after /unignore")
                ok = False
        else:
            if not any("is disabled" in p for p in prints):
                print("FAIL: no disabled reply on the ignoring client")
                ok = False
            if not any("live-ignore-check" in c for c in ignorer_chats):
                print("FAIL: chat was filtered while the feature is off")
                ok = False
        if not any("live-ignore-check" in c for c in speaker_chats):
            print("FAIL: the speaker lost their own echo")
            ok = False
        if not any("live-ignore-check" in c for c in witness_chats):
            print("FAIL: the witness did not receive the speaker's chat")
            ok = False
        print("RESULT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        server.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mode",
        choices=("enabled", "disabled"),
        default="enabled",
        help="whether proxy_sv_enableIgnore is set on the server",
    )
    parser.add_argument("--port", type=int, default=29199)
    parser.add_argument("--engine", default="/srv/jka/linuxjampded")
    parser.add_argument("--gamedata", default="/srv/jka/gamedata")
    parser.add_argument("--artifacts", default="/srv/jka/artifacts")
    parser.add_argument("--duration", type=float, default=15.0)
    args = parser.parse_args()
    return run(
        enabled=args.mode == "enabled",
        port=args.port,
        engine=args.engine,
        gamedata=args.gamedata,
        artifacts=args.artifacts,
        duration=args.duration,
    )


if __name__ == "__main__":
    sys.exit(main())
