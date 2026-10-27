#!/usr/bin/env python3
# A2 netStatus ground-truth harness: a faithful synthetic JA MP client.
#
# Speaks the protocol-26 client protocol (jka_proto.py) to a live
# `linuxjampded` + Rust proxy and reproduces the real client's *timing loop*
# from `original/jedi-academy/codemp/client/`:
#
#   cl_cgame.cpp  CL_SetCGameTime (cl_timeNudge clamp, cl->serverTime
#                 construction, the monotonic guard, the extrapolation flag),
#                 CL_FirstSnapshot (serverTimeDelta seeding), CL_AdjustTimeDelta
#                 (RESET_TIME 500 / 100 / +1 / -2 drift correction);
#   cl_input.cpp  CL_CreateNewCommands (one cmd per frame, stamped with the
#                 *previous* frame's cl->serverTime — CL_SendCmd runs before
#                 CL_SetCGameTime in CL_Frame, cl_main.cpp:1846/1852),
#                 CL_ReadyToSendPacket (cl_maxpackets 15..100 packet timer,
#                 LAN/loopback bypass), CL_WritePacket (cl_packetdup cmd
#                 window, clc_move vs clc_moveNoDelta, outPackets[]).
#
# Ground truth = the `cl_timeNudge` fed in. The scenario matrix drives
# sv_fps x client fps x snaps x cl_maxpackets x LAN/non-LAN x tn (+ loss and
# jitter cases) against the printed `netStatus` table — its `timeNudge` cell
# `[lo..hi]` is symmetric around the point estimate — and reports the estimate
# error. The proxy exposes no estimator internals at runtime.
#
# Documented simplification (does not touch the timing): the usercmds carry
# all-zero movement/angle fields, so MSG_WriteDeltaUsercmdKey takes its
# "no change" path and the message never needs the checksumFeed/usercmd delta
# key — the harness then does not have to parse the gamestate baselines to
# recover sv.checksumFeed. cmd.serverTime encoding (the only field the
# estimator reads) is byte-identical to the real client's.
#
# Usage (inside the i386 container, see a2_netstatus_groundtruth.sh):
#   python3 /rt/a2_netstatus_groundtruth.py --mode single --sv-fps 20 \
#       --client-fps 60 --snaps 20 --cl-maxpackets 30 --tn -15 --duration 15
#   python3 /rt/a2_netstatus_groundtruth.py --mode matrix
#
# Read-only reference: nothing here writes under original/.

from __future__ import annotations

import argparse
import json
import os
import random
import re
import select
import socket
import struct
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jka_proto as jp  # noqa: E402

# ---------------------------------------------------------------------------
# Client states (q_shared.h) and snapshot flags
# ---------------------------------------------------------------------------

CA_CONNECTING = 0
CA_CHALLENGING = 1
CA_CONNECTED = 2
CA_PRIMED = 4
CA_ACTIVE = 5

SNAPFLAG_NOT_ACTIVE = 2


def clamp(v, lo, hi):
    return lo if v < lo else hi if v > hi else v


def int_div(a, b):
    """C integer division (truncation toward zero) for positive operands."""
    return a // b


# ---------------------------------------------------------------------------
# Link: UDP socket with optional loss / jitter in both directions
# ---------------------------------------------------------------------------


class Link:
    """One client's datagram path. Loss and jitter are emulated client-side,
    so both directions are covered without touching the server."""

    def __init__(self, server_addr, loss_up=0.0, loss_down=0.0, jitter_ms=0.0, rng=None):
        self.server_addr = server_addr
        self.loss_up = loss_up
        self.loss_down = loss_down
        self.jitter_ms = jitter_ms
        self.rng = rng or random.Random(1)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("0.0.0.0", 0))
        self.sock.connect(server_addr)
        self.sock.setblocking(False)
        self.inbox = []  # (deliver_at_ms, datagram)
        self.sent = 0
        self.dropped_up = 0
        self.dropped_down = 0

    def local_addr(self):
        return self.sock.getsockname()

    def _delay(self):
        return self.rng.uniform(0.0, self.jitter_ms) if self.jitter_ms else 0.0

    def send(self, data, now_ms):
        self.sent += 1
        if self.loss_up and self.rng.random() < self.loss_up:
            self.dropped_up += 1
            return
        if self.jitter_ms:
            # deliver later: keep a small send queue keyed by time
            self._pending_out = getattr(self, "_pending_out", [])
            self._pending_out.append((now_ms + self._delay(), data))
        else:
            self.sock.send(data)

    def flush_out(self, now_ms):
        pending = getattr(self, "_pending_out", None)
        if not pending:
            return
        keep = []
        for at, data in pending:
            if at <= now_ms:
                self.sock.send(data)
            else:
                keep.append((at, data))
        self._pending_out = keep

    def poll(self, now_ms):
        self.flush_out(now_ms)
        while True:
            try:
                data = self.sock.recv(65535)
            except (BlockingIOError, InterruptedError):
                break
            except OSError:
                break
            if self.loss_down and self.rng.random() < self.loss_down:
                self.dropped_down += 1
                continue
            self.inbox.append((now_ms + self._delay(), data))
        due = [d for at, d in self.inbox if at <= now_ms]
        self.inbox = [(at, d) for at, d in self.inbox if at > now_ms]
        return due

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------
# The synthetic client (faithful port of the engine's timing loop)
# ---------------------------------------------------------------------------


class ClientModel:
    def __init__(self, name, link, *, tn, fps, maxpackets, snaps, lan, packetdup=1):
        self.name = name
        self.link = link
        self.cl_timeNudge = tn
        self.cl_maxpackets = maxpackets
        self.cl_packetdup = packetdup
        self.snaps = snaps
        self.fps = fps
        self.lan = lan

        # client state (cls/cl/clc mirrors)
        self.state = CA_CONNECTING
        self.realtime = 0  # cls.realtime (ms, int)
        self.last_frame_ms = None  # com_frameTime of the previous frame
        self.challenge = 0
        # net_qport: keep it within the positive short range so the packet's
        # MSG_WriteShort qport and the connect infostring qport compare equal
        # (the engine reads it back sign-extended).
        self.qport = ((link.local_addr()[1]) & 0x7FFF) or 1
        self.server_id = 0
        self.server_time = 0  # cl.serverTime
        self.old_server_time = 0
        self.server_time_delta = 0
        self.old_frame_server_time = -99999
        self.extrapolated_snapshot = False
        self.new_snapshots = False
        self.snap = None  # (serverTime, messageNum, snapFlags)
        self.cmd_number = 0
        self.cmds = [jp.Usercmd(0) for _ in range(jp.CMD_MASK + 1)]
        self.out_packets = [(0, 0, 0) for _ in range(jp.PACKET_BACKUP)]
        self.last_packet_sent_time = -9999
        self.server_message_sequence = 0
        self.server_command_sequence = 0
        self.server_commands = [""] * jp.MAX_RELIABLE_COMMANDS
        self.reliable_sequence = 0  # our own client commands
        self.reliable_acknowledge = 0
        self.reliable_commands = [""] * jp.MAX_RELIABLE_COMMANDS
        self.netchan = jp.Netchan(self.qport)
        self.configstrings = {}

        # ground truth / observation records
        self.log = []  # free-form events
        self.packets = []  # dicts: t_ms, seq, count, stamps, new_stamps, msg_ack
        self.snapshots = []  # dicts: t_recv_ms, server_time, message_num
        self.messages = []  # (t_recv_ms, seq) for every accepted message
        self.adjusts = []  # CL_AdjustTimeDelta samples (E = new_delta - delta)
        self.highest_stamp_sent = None
        self.table_lines = []
        self.connected = False
        self.first_active_ms = None

    # -- connection ------------------------------------------------------
    def start_connect(self, now_ms):
        self.link.send(jp.oob_print("getchallenge"), now_ms)

    def handle_datagram(self, data, now_ms):
        if len(data) >= 4 and data[:4] == b"\xff\xff\xff\xff":
            self.handle_oob(data[4:].decode("latin-1", "replace"), now_ms)
            return
        if self.state < CA_CONNECTED:
            return
        got = self.netchan.process(data)
        if got is None:
            return
        seq, datagram = got
        self.server_message_sequence = seq
        self.messages.append((now_ms, seq))
        # CL_Netchan_Decode: the reliableAck long is Huffman-read from datagram
        # bit 32 (= payload byte 0), then the payload is de-obfuscated from
        # datagram offset 8 (payload offset 4 == SV_ENCODE_START).
        rel_ack = jp.MsgReader(datagram, bit=32).read_long()
        self.reliable_acknowledge = rel_ack
        xor_string = self.reliable_commands[rel_ack & (jp.MAX_RELIABLE_COMMANDS - 1)]
        payload = jp.decode_server_payload(datagram, self.challenge, xor_string)
        self.parse_message(payload, now_ms)

    def handle_oob(self, text, now_ms):
        if text.startswith("challengeResponse"):
            try:
                self.challenge = int(text.split()[1])
            except (IndexError, ValueError):
                return
            self.state = CA_CHALLENGING
            info = (
                f"\\name\\{self.name}\\rate\\25000\\snaps\\{self.snaps}"
                f"\\model\\kyle\\handicap\\100\\cl_anonymous\\0"
                f"\\protocol\\{jp.PROTOCOL_VERSION}\\qport\\{self.qport}"
                f"\\challenge\\{self.challenge}"
            )
            self.link.send(jp.oob_connect(info), now_ms)
        elif text.startswith("connectResponse"):
            self.state = CA_CONNECTED
            self.connected = True
            self.log.append({"event": "connected", "t": now_ms})

    # -- server message parse (CL_ParseServerMessage, partial) ------------
    def parse_message(self, payload, now_ms):
        msg = jp.MsgReader(payload)
        self.reliable_acknowledge = msg.read_long()  # our client commands ack
        while True:
            cmd = msg.read_byte()
            if cmd in (-1, jp.SVC_EOF):
                break
            if cmd == jp.SVC_NOP:
                continue
            if cmd == jp.SVC_SERVERCOMMAND:
                seq = msg.read_long()
                s = msg.read_string()
                if seq > self.server_command_sequence:
                    self.server_command_sequence = seq
                    self.server_commands[seq & (jp.MAX_RELIABLE_COMMANDS - 1)] = s
                self.handle_server_command(s)
                continue
            if cmd == jp.SVC_GAMESTATE:
                self.parse_gamestate(msg, now_ms)
                return
            if cmd == jp.SVC_SNAPSHOT:
                server_time = msg.read_long()
                delta_num = msg.read_byte()
                snap_flags = msg.read_byte()
                self.snap = (server_time, self.server_message_sequence, snap_flags)
                self.new_snapshots = True
                self.snapshots.append(
                    {
                        "t_recv": now_ms,
                        "server_time": server_time,
                        "message_num": self.server_message_sequence,
                        "snap_flags": snap_flags,
                        "realtime": self.realtime,
                        "delta": self.server_time_delta,
                    }
                )
                # the snapshot body (playerstate/entity deltas) is not needed
                return
            if cmd == jp.SVC_MAPCHANGE:
                continue
            if cmd == jp.SVC_SETGAME:
                msg.read_string()  # the gamedir (cl_parse.cpp CL_ParseSetGame)
                continue
            self.log.append({"event": "unknown_svc", "cmd": cmd, "t": now_ms})
            return

    def parse_gamestate(self, msg, now_ms):
        # CL_ClearState(): the whole clientActive_t is memset.
        self.server_time = 0
        self.old_server_time = 0
        self.server_time_delta = 0
        self.old_frame_server_time = -99999
        self.extrapolated_snapshot = False
        self.new_snapshots = False
        self.snap = None
        self.cmd_number = 0
        self.out_packets = [(0, 0, 0) for _ in range(jp.PACKET_BACKUP)]
        self.configstrings = {}

        self.server_command_sequence = msg.read_long()
        while True:
            cmd = msg.read_byte()
            if cmd in (-1, jp.SVC_EOF):
                break
            if cmd == jp.SVC_CONFIGSTRING:
                idx = msg.read_short()
                s = msg.read_string()
                self.configstrings[idx] = s
                continue
            # svc_baseline (or anything else): the harness does not need
            # checksumFeed (keyless usercmds) — stop parsing here.
            break
        systeminfo = self.configstrings.get(jp.CS_SYSTEMINFO, "")
        m = re.search(r"\\sv_serverid\\(-?\d+)", systeminfo)
        if m:
            self.server_id = int(m.group(1))
        self.state = CA_PRIMED
        self.log.append(
            {"event": "gamestate", "t": now_ms, "server_id": self.server_id}
        )

    def handle_server_command(self, s):
        if s.startswith("print "):
            self.table_lines.append(s)

    # -- per-frame (CL_Frame order: CL_SendCmd then CL_SetCGameTime) ------
    def frame(self, now_ms):
        for data in self.link.poll(now_ms):
            self.handle_datagram(data, now_ms)

        frame_ms = int(now_ms)
        if self.last_frame_ms is None:
            self.last_frame_ms = frame_ms
        msec = frame_ms - self.last_frame_ms
        self.last_frame_ms = frame_ms
        self.realtime += msec

        if self.state >= CA_CONNECTED:
            self.create_new_commands()
            if self.ready_to_send_packet(now_ms):
                self.write_packet(now_ms)
        self.set_cgame_time(now_ms)

    def create_new_commands(self):
        # CL_CreateNewCommands: one cmd per frame, stamped with the current
        # cl->serverTime (computed at the end of the previous frame).
        if self.state < CA_PRIMED:
            return
        self.cmd_number += 1
        idx = self.cmd_number & jp.CMD_MASK
        self.cmds[idx] = jp.Usercmd(self.server_time)

    def ready_to_send_packet(self, now_ms):
        if self.state != CA_ACTIVE and self.state != CA_PRIMED:
            if self.realtime - self.last_packet_sent_time < 1000:
                return False
        if self.lan:
            return True  # NA_LOOPBACK / Sys_IsLANAddress bypass
        mp = clamp(self.cl_maxpackets, 15, 100)
        old = (self.netchan.outgoing_sequence - 1) & jp.PACKET_MASK
        delta = self.realtime - self.out_packets[old][0]
        return delta >= int_div(1000, mp)

    def write_packet(self, now_ms):
        msg = jp.MsgWriter()
        msg.write_long(self.server_id)
        msg.write_long(self.server_message_sequence)
        msg.write_long(self.server_command_sequence)
        for i in range(self.reliable_acknowledge + 1, self.reliable_sequence + 1):
            msg.write_byte(jp.CLC_CLIENTCOMMAND)
            msg.write_long(i)
            msg.write_string(self.reliable_commands[i & (jp.MAX_RELIABLE_COMMANDS - 1)])

        dup = clamp(self.cl_packetdup, 0, 5)
        old_packet_num = (self.netchan.outgoing_sequence - 1 - dup) & jp.PACKET_MASK
        count = self.cmd_number - self.out_packets[old_packet_num][2]
        if count > jp.MAX_PACKET_USERCMDS:
            count = jp.MAX_PACKET_USERCMDS

        sent_stamps = []
        oldcmd = jp.Usercmd(0)  # nullcmd
        if count >= 1:
            snap_valid = self.snap is not None and not (self.snap[2] & SNAPFLAG_NOT_ACTIVE)
            if self.snap is None or self.server_message_sequence != self.snap[1]:
                msg.write_byte(jp.CLC_MOVE_NO_DELTA)
            else:
                msg.write_byte(jp.CLC_MOVE)
            msg.write_byte(count)
            key = (
                self.server_message_sequence
                ^ jp.com_hash_key(
                    self.server_commands[self.server_command_sequence & (jp.MAX_RELIABLE_COMMANDS - 1)]
                )
            )
            for i in range(count):
                j = (self.cmd_number - count + i + 1) & jp.CMD_MASK
                cmd = self.cmds[j]
                jp.write_delta_usercmd_key(msg, key, oldcmd, cmd)
                oldcmd = cmd
                sent_stamps.append(cmd.server_time)

        packet_num = self.netchan.outgoing_sequence & jp.PACKET_MASK
        self.out_packets[packet_num] = (self.realtime, oldcmd.server_time, self.cmd_number)
        self.last_packet_sent_time = self.realtime
        msg.write_byte(jp.CLC_EOF)
        payload = jp.encode_client_payload(
            msg.payload(),
            self.challenge,
            self.server_id,
            self.server_message_sequence,
            self.server_command_sequence,
            self.server_commands[self.server_command_sequence & (jp.MAX_RELIABLE_COMMANDS - 1)],
        )
        seq = self.netchan.outgoing_sequence
        self.link.send(self.netchan.transmit(payload), now_ms)
        new_stamps = [
            s for s in sent_stamps if self.highest_stamp_sent is None or s > self.highest_stamp_sent
        ]
        if new_stamps:
            self.highest_stamp_sent = new_stamps[-1]
        self.packets.append(
            {
                "t_ms": now_ms,
                "realtime": self.realtime,
                "seq": seq,
                "count": count,
                "stamps": sent_stamps,
                "new_stamps": new_stamps,
                "msg_ack": self.server_message_sequence,
            }
        )

    def set_cgame_time(self, now_ms):
        if self.state != CA_ACTIVE:
            if self.state != CA_PRIMED:
                return
            if self.new_snapshots:
                self.new_snapshots = False
                self.first_snapshot(now_ms)
            if self.state != CA_ACTIVE:
                return
        if self.snap is None:
            return
        if self.snap[0] < self.old_frame_server_time:
            self.log.append({"event": "snap_time_went_backwards", "t": now_ms})
            return
        self.old_frame_server_time = self.snap[0]

        tn = clamp(self.cl_timeNudge, -30, 30)  # release build clamp
        self.server_time = self.realtime + self.server_time_delta - tn
        if self.server_time < self.old_server_time:
            self.server_time = self.old_server_time
        self.old_server_time = self.server_time
        if self.realtime + self.server_time_delta >= self.snap[0] - 5:
            self.extrapolated_snapshot = True
        if self.new_snapshots:
            self.adjust_time_delta(now_ms)

    def first_snapshot(self, now_ms):
        if self.snap[2] & SNAPFLAG_NOT_ACTIVE:
            return
        self.state = CA_ACTIVE
        self.server_time_delta = self.snap[0] - self.realtime
        self.old_server_time = self.snap[0]
        if self.first_active_ms is None:
            self.first_active_ms = now_ms
        self.log.append(
            {
                "event": "first_snapshot",
                "t": now_ms,
                "delta": self.server_time_delta,
            }
        )

    def adjust_time_delta(self, now_ms):
        self.new_snapshots = False
        new_delta = self.snap[0] - self.realtime
        delta_delta = abs(new_delta - self.server_time_delta)
        self.adjusts.append(
            {
                "t": now_ms,
                "new_delta": new_delta,
                "delta": self.server_time_delta,
                "gap": new_delta - self.server_time_delta,
                "delta_delta": delta_delta,
            }
        )
        if delta_delta > 500:
            self.server_time_delta = new_delta
            self.old_server_time = self.snap[0]
            self.server_time = self.snap[0]
            self.log.append({"event": "delta_reset", "t": now_ms})
        elif delta_delta > 100:
            self.server_time_delta = (self.server_time_delta + new_delta) >> 1
            self.log.append({"event": "delta_fast", "t": now_ms})
        else:
            if self.extrapolated_snapshot:
                self.extrapolated_snapshot = False
                self.server_time_delta -= 2
            else:
                self.server_time_delta += 1

    # -- helpers ---------------------------------------------------------
    def send_command(self, text, now_ms):
        self.reliable_sequence += 1
        self.reliable_commands[self.reliable_sequence & (jp.MAX_RELIABLE_COMMANDS - 1)] = text
        self.last_cmd_time = now_ms

    def table_text(self):
        out = []
        for line in self.table_lines:
            m = re.match(r'print "(.*)"$', line, re.S)
            out.append(m.group(1) if m else line)
        return "".join(out)

    def ground_truth(self):
        return {
            "name": self.name,
            "tn": self.cl_timeNudge,
            "fps": self.fps,
            "maxpackets": self.cl_maxpackets,
            "snaps": self.snaps,
            "lan": self.lan,
            "first_active_ms": self.first_active_ms,
            "packets": self.packets,
            "snapshots": self.snapshots,
            "messages": self.messages,
            "adjusts": self.adjusts,
            "table": self.table_text(),
            "events": self.log,
        }


def client_model_terms(client, settle_frac=0.25):
    """Ground-truth side of the estimator's model terms (netstatus_est.rs):
    the measured mean cmd age at send and the measured ack delay of the
    message each packet acks. `lag` must match the first, `ackdelay` the mean
    of the second."""
    msgs = {seq: t for t, seq in client["messages"]}
    pk = [p for p in client["packets"] if p["new_stamps"]]
    pk = pk[int(len(pk) * settle_frac) :]
    if not pk:
        return None
    stamps = [s for p in pk for s in p["new_stamps"]]
    stamps = sorted(set(stamps))
    i_cmd = (stamps[-1] - stamps[0]) / max(len(stamps) - 1, 1)
    ages = []
    ackdelays = []
    for p in pk:
        ns = p["new_stamps"]
        tbar = sum(ns) / len(ns)
        ages.append((max(ns) - tbar) + i_cmd)
        recv = msgs.get(p["msg_ack"])
        if recv is not None:
            ackdelays.append(p["t_ms"] - recv)
    out = {
        "packets": len(pk),
        "i_cmd": i_cmd,
        "mean_age": sum(ages) / len(ages),
    }
    if ackdelays:
        mean = sum(ackdelays) / len(ackdelays)
        var = sum((a - mean) ** 2 for a in ackdelays) / len(ackdelays)
        out.update(
            {
                "ackdelay_mean": mean,
                "ackdelay_sd": var ** 0.5,
                "ackdelay_min": min(ackdelays),
                "ackdelay_max": max(ackdelays),
            }
        )
    return out


# ---------------------------------------------------------------------------
# Server process
# ---------------------------------------------------------------------------


class ServerRun:
    def __init__(self, engine, port, sv_fps, gamedata, log_path, extra_args=(), net_forcenonlocal=False):
        self.log_path = log_path
        self.log_file = open(log_path, "wb")
        cmd = [
            "stdbuf", "-oL", "-eL", engine,
            "+set", "dedicated", "1",
            "+set", "net_port", str(port),
            "+set", "fs_cdpath", gamedata,
            "+set", "sv_pure", "0",
            "+set", "sv_maxclients", "32",
            "+set", "g_gametype", "0",
            "+set", "bot_minplayers", "0",
            "+set", "sv_fps", str(sv_fps),
            "+set", "proxy_sv_enable", "1",
            "+set", "proxy_sv_enableNetStatus", "1",
        ]
        if net_forcenonlocal:
            cmd += ["+set", "net_forcenonlocal", "1"]
        cmd += list(extra_args)
        cmd += ["+map", "mp/ffa1"]
        self.proc = subprocess.Popen(
            cmd, stdout=self.log_file, stderr=subprocess.STDOUT, cwd=os.path.dirname(engine)
        )

    def wait_ready(self, port, timeout=60.0):
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        probe.settimeout(1.0)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                probe.sendto(jp.oob_print("getstatus"), ("127.0.0.1", port))
                data, _ = probe.recvfrom(65535)
            except socket.timeout:
                continue
            if data[:4] == b"\xff\xff\xff\xff" and b"protocol" in data:
                probe.close()
                return True
        probe.close()
        return False

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log_file.close()


# ---------------------------------------------------------------------------
# Scenario runner
# ---------------------------------------------------------------------------


def load_server_log(path):
    try:
        with open(path, "r", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def run_batch(
    *,
    engine,
    gamedata,
    port,
    sv_fps,
    client_specs,
    duration,
    net_forcenonlocal,
    artifacts,
    tag,
    loss=0.0,
    jitter_ms=0.0,
    request_table=True,
    seed=1,
):
    """One server run with a set of synthetic clients."""
    log_path = os.path.join(artifacts, f"a2_{tag}_server.log")
    server = ServerRun(engine, port, sv_fps, gamedata, log_path, net_forcenonlocal=net_forcenonlocal)
    try:
        if not server.wait_ready(port):
            raise RuntimeError(f"server did not become ready (see {log_path})")
        rng = random.Random(seed)
        clients = []
        for spec in client_specs:
            link = Link(
                ("127.0.0.1", port),
                loss_up=loss,
                loss_down=loss,
                jitter_ms=jitter_ms,
                rng=rng,
            )
            c = ClientModel(
                spec["name"],
                link,
                tn=spec["tn"],
                fps=spec["fps"],
                maxpackets=spec["maxpackets"],
                snaps=spec["snaps"],
                lan=spec["lan"],
                packetdup=spec.get("packetdup", 1),
            )
            c.start_connect(time.monotonic() * 1000.0)
            clients.append(c)

        period = {id(c): 1000.0 / c.fps for c in clients}
        next_frame = {id(c): time.monotonic() * 1000.0 for c in clients}
        start = time.monotonic()
        table_requested = False
        hard_stop = duration + 2.0
        while True:
            now_ms = time.monotonic() * 1000.0
            elapsed = time.monotonic() - start
            if elapsed >= hard_stop:
                break
            if request_table and not table_requested and elapsed >= duration - 2.5:
                for c in clients:
                    if c.connected:
                        c.send_command("netStatus", now_ms)
                table_requested = True
            if (
                table_requested
                and elapsed >= duration
                and all((not c.connected) or c.table_lines for c in clients)
            ):
                break
            due = [c for c in clients if next_frame[id(c)] <= now_ms]
            if not due:
                nxt = min(next_frame.values())
                wait = (nxt - now_ms) / 1000.0
                # sleep off the bulk, then spin the last ~2 ms so the frame
                # pacing (the client's fps) stays within ~1 ms of the target
                if wait > 0.003:
                    time.sleep(wait - 0.002)
                continue
            for c in due:
                next_frame[id(c)] += period[id(c)]
                if next_frame[id(c)] < now_ms - 5 * period[id(c)]:
                    next_frame[id(c)] = now_ms + period[id(c)]
                c.frame(now_ms)

        for c in clients:
            c.link.close()
        return {
            "tag": tag,
            "sv_fps": sv_fps,
            "duration": duration,
            "loss": loss,
            "jitter_ms": jitter_ms,
            "net_forcenonlocal": net_forcenonlocal,
            "log_path": log_path,
            "clients": [c.ground_truth() for c in clients],
        }
    finally:
        server.stop()


# The numeric prefix is what we score against; the name is the trailing field
# (it may contain spaces).
TABLE_ROW = re.compile(
    r"^\s*(?P<ping>\S+)\s+(?P<rtt>\S+)\s+(?P<jit>\S+)\s+(?P<fps>\d+)\s+(?P<pps>\d+)\s+"
    r"(?P<cell>\[[^]]+\]|-)\s+(?P<snaps>\d+)\s+(?P<rate>-?\d+)\s+(?P<name>\S.*?)\s*$"
)


def table_estimate(client):
    """Point estimate and range from the printed `netStatus` table — the only
    runtime surface (the `proxy_sv_netStatusDebug` cvar was dropped at the
    user's request). The cell is `[lo..hi]`, symmetric around the estimate."""
    for line in client["table"].splitlines():
        line = re.sub(r"\^[0-9]", "", line).rstrip()  # strip color codes
        m = TABLE_ROW.match(line)
        if not m:
            continue
        if m.group("name").strip() != client["name"]:
            continue
        r = re.match(r"\[(-?[0-9.]+)\.\.(-?[0-9.]+)\]", m.group("cell"))
        if not r:
            continue
        lo, hi = float(r.group(1)), float(r.group(2))
        return (lo + hi) / 2.0, lo, hi
    return None


def compare_run(run, expect_settled=True):
    """Compare the printed `netStatus` table with the ground truth tn."""
    out = []
    for client in run["clients"]:
        got = table_estimate(client)
        rec = {
            "name": client["name"],
            "tn": client["tn"],
            "estimate": None,
            "range": None,
            "error": None,
            "inside": None,
            "width": None,
            "terms": client_model_terms(client),
        }
        if got is not None:
            est, lo, hi = got
            rec["estimate"] = est
            rec["range"] = (lo, hi)
            rec["error"] = est - client["tn"]
            rec["inside"] = lo <= client["tn"] <= hi
            rec["width"] = hi - lo
        out.append(rec)
    return out


def print_report(results):
    hdr = (
        f'{"client":<12} {"tn":>5} {"estimate":>9} {"error":>7} {"range":>13} {"width":>6} '
        f'{"inside":>6} {"measLag":>8} {"measAck":>8}'
    )
    print(hdr)
    print("-" * len(hdr))
    ok = True
    for r in results:
        if r["estimate"] is None:
            print(f'{r["name"]:<12} {r["tn"]:>5} {"(no table row)":>9}')
            ok = False
            continue
        lo, hi = r["range"]
        terms = r.get("terms") or {}
        print(
            f'{r["name"]:<12} {r["tn"]:>5} {r["estimate"]:>9.2f} {r["error"]:>7.2f} '
            f'[{lo:>5.1f}..{hi:<5.1f}] {r["width"]:>6.1f} {"yes" if r["inside"] else "NO":>6} '
            f'{terms.get("mean_age", float("nan")):>8.2f} '
            f'{terms.get("ackdelay_mean", float("nan")):>8.2f}'
        )
        if not r["inside"] or abs(r["error"]) > 2.0 or r["width"] > 5.0:
            ok = False
    print("ACCEPT" if ok else "CHECK")
    return ok


# ---------------------------------------------------------------------------
# Matrix
# ---------------------------------------------------------------------------

BASELINE = {"fps": 60, "maxpackets": 30, "snaps": 20, "lan": False, "tn": 0}


def matrix_specs(full=False):
    """Client configs: one sweep per axis against the baseline, all tn values
    in one batch (one client each), plus loss/jitter cases."""
    batches = []
    tn_values = [-30, -15, 0, 15, 30]

    def specs_for(**overrides):
        out = []
        for tn in tn_values:
            spec = dict(BASELINE)
            spec.update(overrides)
            spec["tn"] = tn
            # short, unique name (the debug log identifies rows by name)
            spec["name"] = re.sub(
                r"[^A-Za-z0-9_]",
                "",
                "c%d_fps%d_mp%d_sn%d_%s_tn%s"
                % (
                    len(out),
                    spec["fps"],
                    spec["maxpackets"],
                    spec["snaps"],
                    "L" if spec["lan"] else "N",
                    ("m%d" % -tn) if tn < 0 else ("p%d" % tn),
                ),
            )
            out.append(spec)
        return out

    axes = [
        {"fps": 30}, {"fps": 125},
        {"snaps": 10}, {"snaps": 30},
        {"maxpackets": 15}, {"maxpackets": 100},
        {"lan": True},
    ]
    if full:
        axes += [
            {"fps": 30, "maxpackets": 15, "snaps": 30},
            {"fps": 125, "maxpackets": 100, "snaps": 10},
            {"fps": 30, "lan": True},
            {"fps": 125, "lan": True, "snaps": 30},
        ]
    for ax in axes:
        batches.append((specs_for(**ax), 0.0, 0.0))
    # loss / jitter cases (baseline client shape)
    batches.append((specs_for(), 0.10, 0.0))
    batches.append((specs_for(), 0.0, 20.0))
    batches.append((specs_for(maxpackets=15), 0.05, 15.0))
    return batches


def run_matrix(args):
    sv_fps_values = [20, 30, 40, 60, 80]
    all_results = []
    runs = []
    idx = 0
    done = 0
    for sv_fps in sv_fps_values:
        for specs, loss, jitter in matrix_specs(args.full_grid):
            idx += 1
            if args.max_batches and done >= args.max_batches:
                break
            done += 1
            tag = f"m{idx:03d}_svf{sv_fps}_l{int(loss*100)}_j{int(jitter)}"
            print(f"=== batch {tag}: {len(specs)} clients, sv_fps={sv_fps}", flush=True)
            run = run_batch(
                engine=args.engine,
                gamedata=args.gamedata,
                port=args.port,
                sv_fps=sv_fps,
                client_specs=specs,
                duration=args.duration,
                net_forcenonlocal=args.net_forcenonlocal,
                artifacts=args.artifacts,
                tag=tag,
                loss=loss,
                jitter_ms=jitter,
                seed=idx,
            )
            run["sv_fps"] = sv_fps
            runs.append(run)
            results = compare_run(run)
            for r in results:
                r["sv_fps"] = sv_fps
                r["loss"] = loss
                r["jitter"] = jitter
            all_results.extend(results)
            print_report(results)
            print(flush=True)
    with open(os.path.join(args.artifacts, "a2_matrix_results.json"), "w") as fh:
        json.dump({"runs": runs, "results": all_results}, fh, indent=1)
    bad = [r for r in all_results if not r.get("inside") or abs(r.get("error") or 99) > 2.0 or (r.get("width") or 99) > 5.0]
    print(f"=== {len(all_results)} client runs, {len(bad)} outside acceptance")
    return 0 if not bad else 1


def run_real(args):
    """Real-client run: start the server and let the user's own JA MP client
    connect (`cl_timeNudge` set as desired). The `netStatus` table is printed
    to the *client's* console — the server log holds no estimator internals
    (the `proxy_sv_netStatusDebug` cvar was dropped at the user's request), so
    copy the table rows here to score them."""
    log_path = os.path.join(args.artifacts, "a2_real_server.log")
    server = ServerRun(
        args.engine,
        args.port,
        args.sv_fps,
        args.gamedata,
        log_path,
        net_forcenonlocal=args.net_forcenonlocal,
    )
    try:
        if not server.wait_ready(args.port):
            raise RuntimeError(f"server did not become ready (see {log_path})")
        print(
            f"server ready on udp/{args.port} — connect your client, set "
            f"cl_timeNudge and run /netStatus; the table prints to your client "
            f"console (copy it to score). Running {args.duration:.0f} s",
            flush=True,
        )
        deadline = time.monotonic() + args.duration
        while time.monotonic() < deadline:
            time.sleep(1.0)
        return {"log_path": log_path}
    finally:
        server.stop()
        print(f"server log: {log_path} (no estimator internals: "
              f"proxy_sv_netStatusDebug no longer exists)")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--mode", choices=["single", "matrix", "real"], default="single")
    ap.add_argument("--engine", default="./linuxjampded")
    ap.add_argument("--gamedata", default="/srv/jka/gamedata")
    ap.add_argument("--artifacts", default="/srv/jka/artifacts")
    ap.add_argument("--port", type=int, default=29191)
    ap.add_argument("--sv-fps", type=int, default=20)
    ap.add_argument("--client-fps", type=int, default=60)
    ap.add_argument("--snaps", type=int, default=20)
    ap.add_argument("--cl-maxpackets", type=int, default=30)
    ap.add_argument("--tn", type=int, default=0)
    ap.add_argument("--lan", action="store_true", help="client packet mode: LAN bypass")
    ap.add_argument("--loss", type=float, default=0.0)
    ap.add_argument("--jitter-ms", type=float, default=0.0)
    ap.add_argument("--duration", type=float, default=15.0)
    ap.add_argument("--full-grid", action="store_true")
    ap.add_argument("--max-batches", type=int, default=0, help="matrix smoke test: stop after N batches")
    ap.add_argument(
        "--net-forcenonlocal",
        action="store_true",
        help="server-side: net_forcenonlocal 1 (Sys_IsLANAddress false for everyone)",
    )
    args = ap.parse_args()
    os.makedirs(args.artifacts, exist_ok=True)

    if args.mode == "single":
        spec = {
            "name": "client0",
            "tn": args.tn,
            "fps": args.client_fps,
            "maxpackets": args.cl_maxpackets,
            "snaps": args.snaps,
            "lan": args.lan,
        }
        run = run_batch(
            engine=args.engine,
            gamedata=args.gamedata,
            port=args.port,
            sv_fps=args.sv_fps,
            client_specs=[spec],
            duration=args.duration,
            net_forcenonlocal=args.net_forcenonlocal,
            artifacts=args.artifacts,
            tag="single",
            loss=args.loss,
            jitter_ms=args.jitter_ms,
        )
        with open(os.path.join(args.artifacts, "a2_single.json"), "w") as fh:
            json.dump(run, fh, indent=1)
        print(json.dumps(run["clients"], indent=1)[:2000])
        print("=== netStatus table (printed to the client) ===")
        print(run["clients"][0]["table"])
        print("=== scored from the printed table ===")
        results = compare_run(run)
        if any(r["estimate"] is not None for r in results):
            print_report(results)
        return 0
    if args.mode == "real":
        run_real(args)
        return 0
    return run_matrix(args)


if __name__ == "__main__":
    sys.exit(main())
