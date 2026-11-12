#!/usr/bin/env python3
"""Capture a real dedicated-server message stream as a demo-framed file.

Connects a synthetic protocol-26 client (the `jka_proto` wire codec) to a
running `linuxjampded`, and writes every complete server->client message it
receives using the client demo framing:

    frame := int32 sequence, int32 length, byte[length] payload
    file  := frame*, int32 -1, int32 -1

`payload` is the server message exactly as `CL_PacketEvent` leaves it after
`CL_Netchan_Process` (netchan header removed, XOR de-obfuscated), i.e. the
bytes `CL_WriteDemoMessage` would append.  The first frame is therefore the
server's real gamestate.

This is capture/evidence tooling, not a proxy component: it is used to settle
the gamestate layout (including the RMG tail) and to collect a real server
stream for the Phase 0 golden artifacts.

Usage:
    tools/runtime/demo_capture.py 127.0.0.1 29120 \
        --out artifacts/demo-phase0/server-ffa3.dm_26 --duration 3
"""

import argparse
import os
import re
import select
import socket
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jka_proto as jp  # noqa: E402

CA_DISCONNECTED = 0
CA_CHALLENGING = 1
CA_CONNECTED = 2


def read_nul(msg, limit):
    """MSG_ReadString/MSG_ReadBigString consumption (NUL or `limit`)."""
    out = bytearray()
    while True:
        c = msg.read_byte()
        if c <= 0:
            break
        out.append(c)
        if len(out) >= limit - 1:
            break
    return out.decode("latin-1")


class Capture:
    def __init__(self, host, port, name, protocol, qport, verbose=False):
        self.addr = (host, port)
        self.name = name
        self.protocol = protocol
        self.qport = qport
        self.verbose = verbose
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setblocking(False)
        self.state = CA_DISCONNECTED
        self.challenge = 0
        self.netchan = jp.Netchan(qport)
        self.reliable_commands = [""] * jp.MAX_RELIABLE_COMMANDS
        self.reliable_acknowledge = 0
        self.server_command_sequence = 0
        self.server_id = 0
        self.frames = []
        self.first_gamestate_payload = None
        self.configstrings = {}

    # -- OOB ------------------------------------------------------------
    def send(self, data):
        self.sock.sendto(data, self.addr)

    def start(self):
        self.send(jp.oob_print("getchallenge"))

    def handle_oob(self, text):
        if self.verbose:
            print("OOB:", text[:160].replace("\n", "\\n"))
        if text.startswith("challengeResponse"):
            self.challenge = int(text.split()[1])
            self.state = CA_CHALLENGING
            info = (
                f"\\name\\{self.name}\\rate\\25000\\snaps\\20"
                f"\\model\\kyle\\handicap\\100\\cl_anonymous\\0"
                f"\\forcepowers\\7-1-030000000000003332"
                f"\\color1\\4\\color2\\4\\cg_predictItems\\1"
                f"\\saber1\\single_1\\saber2\\none"
                f"\\char_color_red\\255\\char_color_green\\255\\char_color_blue\\255"
                f"\\protocol\\{self.protocol}\\qport\\{self.qport}"
                f"\\challenge\\{self.challenge}"
            )
            self.send(jp.oob_connect(info))
        elif text.startswith("connectResponse"):
            self.state = CA_CONNECTED
        elif text.startswith("print"):
            pass

    # -- server messages ------------------------------------------------
    def handle_datagram(self, data):
        if len(data) >= 4 and data[:4] == b"\xff\xff\xff\xff":
            self.handle_oob(data[4:].decode("latin-1", "replace"))
            return
        if self.state < CA_CONNECTED:
            return
        got = self.netchan.process(data)
        if got is None:
            return
        seq, datagram = got
        # CL_Netchan_Decode: reliableAcknowledge is the first payload long.
        rel_ack = jp.MsgReader(datagram, bit=32).read_long()
        xor_string = self.reliable_commands[rel_ack & (jp.MAX_RELIABLE_COMMANDS - 1)]
        payload = jp.decode_server_payload(datagram, self.challenge, xor_string)

        self.reliable_acknowledge = rel_ack
        self.frames.append((seq, payload))

        # Inspect the first op byte after the leading lastClientCommand long.
        r = jp.MsgReader(payload)
        r.read_long()  # lastClientCommand
        first_op = r.read_byte()
        if first_op in (jp.SVC_GAMESTATE, jp.SVC_SERVERCOMMAND) and self.first_gamestate_payload is None:
            self.first_gamestate_payload = payload
            self.parse_gamestate_prefix(payload)
            if self.verbose:
                print(f"gamestate captured: seq={seq} payload={len(payload)} bytes")
        elif self.verbose:
            print(f"message seq={seq} payload={len(payload)} first_op={first_op}")

    def parse_gamestate_prefix(self, payload):
        """Read configstrings from the gamestate far enough for `sv_serverid`."""
        r = jp.MsgReader(payload)
        r.read_long()  # lastClientCommand
        while True:
            cmd = r.read_byte()
            if cmd in (-1, jp.SVC_EOF):
                return
            if cmd == jp.SVC_SERVERCOMMAND:
                seq = r.read_long()
                s = read_nul(r, 1024)
                if seq > self.server_command_sequence:
                    self.server_command_sequence = seq
                self.reliable_commands[seq & (jp.MAX_RELIABLE_COMMANDS - 1)] = s
                continue
            if cmd != jp.SVC_GAMESTATE:
                return
            r.read_long()  # reliableSequence
            while True:
                op = r.read_byte()
                if op in (-1, jp.SVC_EOF):
                    break
                if op != jp.SVC_CONFIGSTRING:
                    break  # first svc_baseline: stop before entity decoding
                idx = r.read_short()
                self.configstrings[idx] = read_nul(r, 8192)
            systeminfo = self.configstrings.get(jp.CS_SYSTEMINFO, "")
            m = re.search(r"\\sv_serverid\\(-?\d+)", systeminfo)
            if m:
                self.server_id = int(m.group(1))
            return

    # -- keepalive ------------------------------------------------------
    def send_keepalive(self):
        """One empty packet (three header longs + clc_EOF), as CL_WritePacket
        does when it has no usercmds to send."""
        msg = jp.MsgWriter()
        msg.write_long(self.server_id)
        msg.write_long(self.netchan.incoming_sequence)  # serverMessageSequence
        msg.write_long(self.server_command_sequence)
        msg.write_byte(jp.CLC_EOF)
        payload = msg.payload()
        # CL_Netchan_Encode XORs payload bytes from offset 12.
        encoded = jp.encode_client_payload(
            payload,
            self.challenge,
            self.server_id,
            self.netchan.incoming_sequence,
            self.server_command_sequence,
            self.reliable_commands[
                self.server_command_sequence & (jp.MAX_RELIABLE_COMMANDS - 1)
            ],
        )
        self.send(self.netchan.transmit(encoded))

    def run(self, duration):
        self.start()
        deadline = time.monotonic() + duration
        last_keepalive = 0.0
        while time.monotonic() < deadline:
            timeout = min(0.1, max(0.0, deadline - time.monotonic()))
            r, _, _ = select.select([self.sock], [], [], timeout)
            if r:
                try:
                    data, _ = self.sock.recvfrom(65535)
                except BlockingIOError:
                    continue
                self.handle_datagram(data)
            now = time.monotonic()
            if self.state >= CA_CONNECTED and now - last_keepalive >= 0.2:
                self.send_keepalive()
                last_keepalive = now


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("host")
    ap.add_argument("port", type=int)
    ap.add_argument("--out", required=True)
    ap.add_argument("--duration", type=float, default=3.0)
    ap.add_argument("--name", default="DemoCapture")
    ap.add_argument("--protocol", type=int, default=jp.PROTOCOL_VERSION)
    ap.add_argument("--qport", type=int, default=4242)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    cap = Capture(
        args.host, args.port, args.name, args.protocol, args.qport, args.verbose
    )
    cap.run(args.duration)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "wb") as f:
        for seq, payload in cap.frames:
            f.write(struct.pack("<ii", seq, len(payload)))
            f.write(payload)
        f.write(struct.pack("<ii", -1, -1))

    print(
        f"captured {len(cap.frames)} messages, "
        f"{os.path.getsize(args.out)} bytes -> {args.out}"
    )
    if cap.first_gamestate_payload is None:
        print("ERROR: no gamestate received", file=sys.stderr)
        return 1
    print(f"gamestate payload: {len(cap.first_gamestate_payload)} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
