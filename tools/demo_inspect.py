#!/usr/bin/env python3
"""Inspect a Jedi Academy demo (.dm_26 / .dm_16) and its message stream.

Phase 0 evidence tool for the server-side demo recorder. It validates the
demo framing, decodes the gamestate (configstrings, baselines, clientNum,
checksumFeed and the RMG tail) and decodes snapshot headers, playerstates and
packet-entity lists well enough to reach svc_EOF.

The field tables are read from the *binary itself* (the shipped server for
protocol-26 layout; the reference client to cross-check), because the
`original/jedi-academy/codemp/qcommon/msg.cpp` tables in this workspace are
older than the shipped binaries (113 vs 132 entity fields, 113 vs 137
playerstate fields). See docs/decisions/0027-server-demos.md.

Usage:
    tools/demo_inspect.py FILE [--frames N] [--dump-gamestate]
    tools/demo_inspect.py FILE --all-snapshots
"""

import argparse
import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "runtime"))
import jka_proto as jp  # noqa: E402

ROOT = os.path.dirname(HERE)
ENGINE = os.path.join(ROOT, "original", "jalinuxded_1.011", "linuxjampded")
CLIENT = os.path.join(ROOT, "original", "GameData", "taystjk.x86_64")

# Field-table locations (verified by disassembly; see the Phase 0 evidence doc).
# i386 linuxjampded, netField_t = 12 bytes, bits at +8, offset at +4.
SERVER_TABLES = {
    "entity": (0x081A15A0, 132),   # MSG_WriteDeltaEntity 0x8078d74
    "ps": (0x081A1BE0, 137),       # MSG_WriteDeltaPlayerstate 0x80799c4 (normal)
    "ps_pilot": (0x081A2260, 58),  # optimized-vehicle pilot branch
    "ps_veh": (0x081A2900, 69),    # isVehiclePS
}

MAX_GENTITIES = 1024
GENTITYNUM_BITS = 10
FLOAT_INT_BITS = 13
FLOAT_INT_BIAS = 1 << (FLOAT_INT_BITS - 1)


def reset_huffman():
    """Install a fresh copy of the static `msg_hData` Huffman tree.

    `MSG_initHuffman` preloads the compressor and decompressor trees from
    `msg_hData`; `MSG_WriteBits`/`MSG_ReadBits` only *walk* the trees
    (`Huff_offsetTransmit`/`Huff_offsetReceive`) and never call `Huff_addRef`
    per message. The tree is therefore static: every message is independently
    decodable, and a clean tree is the same for every pass.
    """
    h = jp._init_tree()
    for i in range(256):
        for _ in range(jp.MSG_HDATA[i]):
            h.add_ref(i)
    jp._DECOMPRESSOR = h


# ---------------------------------------------------------------------------
# Minimal ELF vaddr -> file offset mapping
# ---------------------------------------------------------------------------

def elf_load_segments(path):
    with open(path, "rb") as f:
        ident = f.read(16)
        if ident[:4] != b"\x7fELF":
            raise ValueError(f"{path}: not an ELF file")
        is64 = ident[4] == 2
        endian = "<" if ident[5] == 1 else ">"
        if is64:
            f.seek(32)
            e_phoff = struct.unpack(endian + "Q", f.read(8))[0]
            f.seek(54)
            e_phentsize, e_phnum = struct.unpack(endian + "HH", f.read(4))
            segs = []
            for i in range(e_phnum):
                f.seek(e_phoff + i * e_phentsize)
                p_type, p_flags = struct.unpack(endian + "II", f.read(8))
                p_offset, p_vaddr, p_paddr, p_filesz, p_memsz = struct.unpack(
                    endian + "QQQQQ", f.read(40)
                )
                if p_type == 1:
                    segs.append((p_vaddr, p_vaddr + p_filesz, p_offset))
            return segs
        f.seek(28)
        e_phoff = struct.unpack(endian + "I", f.read(4))[0]
        f.seek(42)
        e_phentsize, e_phnum = struct.unpack(endian + "HH", f.read(4))
        segs = []
        for i in range(e_phnum):
            f.seek(e_phoff + i * e_phentsize)
            p_type = struct.unpack(endian + "I", f.read(4))[0]
            p_offset, p_vaddr, p_paddr, p_filesz, p_memsz = struct.unpack(
                endian + "IIIII", f.read(20)
            )
            if p_type == 1:
                segs.append((p_vaddr, p_vaddr + p_filesz, p_offset))
        return segs


class Binary:
    def __init__(self, path):
        self.path = path
        self.segs = elf_load_segments(path)
        self.data = open(path, "rb").read()

    def read(self, vaddr, size):
        for lo, hi, off in self.segs:
            if lo <= vaddr and vaddr + size <= hi:
                start = off + (vaddr - lo)
                return self.data[start : start + size]
        raise ValueError(f"{self.path}: 0x{vaddr:x} not in a PT_LOAD segment")

    def netfields(self, vaddr, count, entry_size=12, bits_off=8, ofs_off=4):
        raw = self.read(vaddr, count * entry_size)
        out = []
        for i in range(count):
            e = raw[i * entry_size : (i + 1) * entry_size]
            ofs = struct.unpack_from("<i", e, ofs_off)[0]
            bits = struct.unpack_from("<i", e, bits_off)[0]
            out.append((ofs, bits))
        return out


# ---------------------------------------------------------------------------
# Message readers (mirror MSG_ReadBits / MSG_ReadDeltaEntity /
# MSG_ReadDeltaPlayerstate of the shipped binaries)
# ---------------------------------------------------------------------------

def read_delta_entity(msg, fields, collect=False):
    """Skip one entityState_t delta; returns (number, removed, delta, changed_fields).

    With `collect=True` a fifth element is appended: the exact wire values of
    the changed fields (used for byte-exact gamestate re-encoding).
    """
    num = msg.read_bits(GENTITYNUM_BITS)
    if num == MAX_GENTITIES - 1:
        return None  # end-of-packetentities marker
    removed = msg.read_bits(1)
    if removed:
        return (num, True, False, [])
    has_delta = msg.read_bits(1)
    if not has_delta:
        return (num, False, False, [])
    lc = msg.read_byte()
    if lc < 0 or lc > len(fields):
        raise ValueError(f"entity {num}: bad field count {lc}")
    changed = []
    values = []
    for i in range(lc):
        if not msg.read_bits(1):
            continue
        changed.append(i)
        bits = fields[i][1]
        if bits == 0:
            if msg.read_bits(1) == 0:
                values.append(("fz", 0))
            elif msg.read_bits(1) == 0:
                values.append(("fi", msg.read_bits(FLOAT_INT_BITS)))
            else:
                values.append(("ff", msg.read_bits(32)))
        else:
            if msg.read_bits(1):  # nonzero integer
                values.append(("i", msg.read_bits(abs(bits))))
            else:
                values.append(("zero", 0))
    got = (num, False, True, changed)
    return got + (values,) if collect else got


def write_entity_field_bits(w, encoded):
    kind, value = encoded
    if kind == "fz":
        w.write_bits(0, 1)
    elif kind == "fi":
        w.write_bits(1, 1)
        w.write_bits(0, 1)
        w.write_bits(value, FLOAT_INT_BITS)
    elif kind == "ff":
        w.write_bits(1, 1)
        w.write_bits(1, 1)
        w.write_bits(value, 32)
    else:
        raise ValueError(f"not a float encoding: {kind}")


def write_delta_entity(w, fields, num, changed, values):
    """Re-emit a decoded forced entity delta (from == nullstate)."""
    w.write_bits(num, GENTITYNUM_BITS)
    w.write_bits(0, 1)  # not removed
    w.write_bits(1, 1)  # has delta
    lc = (changed[-1] + 1) if changed else 0
    w.write_byte(lc)
    vi = 0
    for i in range(lc):
        if i in changed:
            enc = values[vi]
            vi += 1
            bits = fields[i][1]
            if bits == 0:
                w.write_bits(1, 1)  # changed
                write_entity_field_bits(w, enc)
            else:
                w.write_bits(1, 1)  # changed
                if enc[0] == "zero":
                    w.write_bits(0, 1)
                else:
                    w.write_bits(1, 1)  # nonzero integer
                    w.write_bits(enc[1], abs(bits))
        else:
            w.write_bits(0, 1)  # unchanged


ENTITY_STATE_SIZE = 532


def state_from_values(num, changed, values, entity_fields):
    """Rebuild the 532-byte `entityState_t` image the encoder will see.

    Used by the sidecar generator: the Rust gamestate encoder takes raw state
    bytes (as Phase 2 will copy them from engine memory) and recomputes `lc`
    from the field table, rather than replaying the wire field list.
    """
    st = bytearray(ENTITY_STATE_SIZE)
    struct.pack_into("<i", st, 0, num)
    vi = 0
    for i in changed:
        enc = values[vi]
        vi += 1
        ofs, bits = entity_fields[i]
        if bits == 0:
            if enc[0] == "fz":
                raw = 0
            elif enc[0] == "fi":
                raw = struct.unpack("<I", struct.pack("<f", float(enc[1] - 4096)))[0]
            else:
                raw = enc[1] & 0xFFFFFFFF
            struct.pack_into("<I", st, ofs, raw)
        else:
            value = enc[1]
            if bits < 0:
                width = -bits
                if value & (1 << (width - 1)):
                    value -= 1 << width
            struct.pack_into("<i", st, ofs, value)
    return bytes(st)


def dump_gamestate_sidecar(path, gs, entity_fields):
    """Write the decoded first-frame values for the Rust byte-exact tests.

    Layout (little-endian):

        b"DMGS1\\0\\0\\0"
        i32 lastClientCommand
        i32 reliableSequence
        i32 clientNum
        i32 checksumFeed
        i16 rmgShort
        u32 meaningfulBits              # parse_first_frame end_bit
        u32 configstringCount
          { u16 index, u32 len, byte[len] }*
        u32 baselineCount
          { byte[532] entityState }*
    """
    out = bytearray(b"DMGS1\0\0\0")
    out += struct.pack(
        "<iiiihI",
        gs["lastClientCommand"],
        gs["reliableSequence"],
        gs["clientNum"],
        gs["checksumFeed"],
        gs["rmg_short"],
        gs["end_bit"],
    )
    items = sorted(gs["configstrings"].items())
    out += struct.pack("<I", len(items))
    for index, value in items:
        raw = value.encode("latin-1")
        out += struct.pack("<HI", index, len(raw))
        out += raw
    out += struct.pack("<I", len(gs["baseline_details"]))
    for num, has_delta, changed, values in gs["baseline_details"]:
        if has_delta:
            out += state_from_values(num, changed, values, entity_fields)
        else:
            state = bytearray(ENTITY_STATE_SIZE)
            struct.pack_into("<i", state, 0, num)
            out += state
    with open(path, "wb") as f:
        f.write(out)
    return len(out)


def read_delta_playerstate(msg, fields, is_vehicle=False):
    """Decode one playerState delta. Returns a dict of decoded values we need."""
    if not is_vehicle:
        # _OPTIMIZED_VEHICLE_NETWORKING: pilot bit; use the pilot table if set.
        pilot = msg.read_bits(1)
        table = fields["ps_pilot"] if pilot else fields["ps"]
    else:
        pilot = False
        table = fields["ps_veh"]
    lc = msg.read_byte()
    if lc < 0 or lc > len(table):
        raise ValueError(f"playerstate: bad field count {lc}")
    values = {"pilot": pilot, "fields": {}}
    for i in range(lc):
        if not msg.read_bits(1):
            continue
        bits = table[i][1]
        if bits == 0:
            # float: 0 + 13-bit biased integer, or 1 + full 32-bit float
            if msg.read_bits(1) == 0:
                v = msg.read_bits(FLOAT_INT_BITS) - FLOAT_INT_BIAS
                values["fields"][i] = float(v)
            else:
                v = msg.read_bits(32)
                values["fields"][i] = struct.unpack(
                    "<f", struct.pack("<I", v & 0xFFFFFFFF)
                )[0]
        else:
            # integer: the value follows directly, no zero flag
            values["fields"][i] = msg.read_bits(abs(bits))
    # stats / persistant / ammo / powerups
    arrays = {}
    if msg.read_bits(1):
        for name in ("stats", "persistant", "ammo", "powerups"):
            bitmask = msg.read_short() if msg.read_bits(1) else 0
            vals = []
            for i in range(16):
                if bitmask & (1 << i):
                    if name == "powerups":
                        vals.append(msg.read_bits(32))
                    elif name == "stats" and i == 4:  # STAT_WEAPONS
                        vals.append(msg.read_bits(19))
                    else:
                        vals.append(msg.read_bits(16))
                else:
                    vals.append(0)
            arrays[name] = (bitmask, vals)
    values["arrays"] = arrays
    return values


# ---------------------------------------------------------------------------
# Framing
# ---------------------------------------------------------------------------

class Frames:
    def __init__(self, path):
        self.path = path
        self.data = open(path, "rb").read()
        self.frames = []
        off = 0
        terminated = False
        while off + 8 <= len(self.data):
            seq, ln = struct.unpack_from("<ii", self.data, off)
            if seq == -1 and ln == -1:
                terminated = True
                off += 8
                break
            if ln < 0 or off + 8 + ln > len(self.data):
                raise ValueError(f"frame at {off}: bad length {ln}")
            self.frames.append((off, seq, self.data[off + 8 : off + 8 + ln]))
            off += 8 + ln
        self.terminated = terminated
        self.end = off
        self.trailing = len(self.data) - off

    def check(self):
        assert self.terminated, "missing -1/-1 terminator"
        seqs = [s for _, s, _ in self.frames]
        for a, b in zip(seqs, seqs[1:]):
            assert b > a, f"demo sequence not increasing: {a} -> {b}"


# ---------------------------------------------------------------------------
# Gamestate / snapshot decode
# ---------------------------------------------------------------------------

def parse_snapshot(msg, fields):
    out = {}
    out["serverTime"] = msg.read_long()
    out["deltaNum"] = msg.read_byte()
    out["snapFlags"] = msg.read_byte()
    areabytes = msg.read_byte()
    out["areabytes"] = areabytes
    out["areamask"] = msg.read_bits(areabytes * 8) if areabytes else 0
    out["ps"] = read_delta_playerstate(msg, fields)
    # NOTE: SV_WriteSnapshotToClient writes a second (vehicle) playerstate
    # when ps.m_iVehicleNum != 0 (MSG_WriteDeltaPlayerstate with
    # isVehiclePS=1). None of the 49 shipped demos exercises that path, so it
    # is not decoded here (see docs/demo-format.md §8).
    out["entities"] = []
    while True:
        got = read_delta_entity(msg, fields["entity"])
        if got is None:
            break
        out["entities"].append(got)
    return out


def consume_message(payload, fields, keep=True, commands=None):
    """Walk one server->client message exactly like CL_ParseServerMessage.

    Returns (gamestate, snapshots) decoded along the way. A MsgReader cannot
    rewind, so each payload is consumed once; the Huffman tree itself is the
    static `msg_hData` tree and does not depend on stream position. When
    `commands` is a list, every `svc_serverCommand` is appended as
    `(sequence, text)` in wire order.
    """
    msg = jp.MsgReader(payload)
    msg.read_long()  # lastClientCommand
    gamestate = None
    snapshots = []
    while True:
        cmd = msg.read_byte()
        if cmd in (-1, jp.SVC_EOF):
            break
        if cmd == jp.SVC_NOP:
            continue
        if cmd == jp.SVC_SERVERCOMMAND:
            seq = msg.read_long()
            text = read_until_nul(msg, 1024)
            if commands is not None:
                commands.append((seq, text))
            continue
        if cmd == jp.SVC_GAMESTATE:
            gamestate = parse_gamestate_body(msg, fields)
            continue
        if cmd == jp.SVC_SNAPSHOT:
            snap = parse_snapshot(msg, fields)
            if keep:
                snapshots.append(snap)
            continue
        if cmd == jp.SVC_MAPCHANGE:
            continue
        if cmd == jp.SVC_SETGAME:
            read_until_nul(msg, 1024)
            continue
        if cmd == jp.SVC_DOWNLOAD:
            block = msg.read_short()
            if block == 0:
                msg.read_long()
            else:
                msg.read_bits(block * 8)
            continue
        if 11 <= cmd <= 20:  # XBOX-only svc_plyrPos0..9
            msg.read_bits(24)
            continue
        raise ValueError(f"illegal svc op {cmd} at bit {msg.bit}")
    return gamestate, snapshots


def read_until_nul(msg, max_len):
    """MSG_ReadString/MSG_ReadBigString consumption (stops at NUL or max)."""
    out = bytearray()
    while True:
        c = msg.read_byte()
        if c <= 0:
            break
        out.append(c)
        if len(out) >= max_len - 1:
            break
    return out.decode("latin-1")


def read_big_string(msg):
    # MSG_ReadBigString: char string[BIG_INFO_STRING] (8192).
    return read_until_nul(msg, 8192)


def parse_gamestate_body(msg, fields, collect=False):
    out = {"reliableSequence": msg.read_long(), "configstrings": {}, "baselines": []}
    if collect:
        out["baseline_details"] = []
    while True:
        cmd = msg.read_byte()
        if cmd in (-1, jp.SVC_EOF):
            break
        if cmd == jp.SVC_CONFIGSTRING:
            idx = msg.read_short()
            out["configstrings"][idx] = read_big_string(msg)
            continue
        if cmd == jp.SVC_BASELINE:
            got = read_delta_entity(msg, fields["entity"], collect=collect)
            if got is None:
                raise ValueError("end-of-entities marker inside gamestate baselines")
            if got[1]:
                raise ValueError(f"baseline {got[0]} is a remove")
            out["baselines"].append(got[0])
            if collect:
                if got[2]:
                    out["baseline_details"].append((got[0], True, got[3], got[4]))
                else:
                    out["baseline_details"].append((got[0], False, [], []))
            continue
        raise ValueError(f"CL_ParseGamestate: bad command byte {cmd}")
    out["clientNum"] = msg.read_long()
    out["checksumFeed"] = msg.read_long()
    out["rmg_short"] = msg.read_short()
    out["end_bit"] = msg.bit
    out["end_byte"] = msg.readcount
    return out


def parse_first_frame(payload, fields):
    """Decode frame 0 (client-synthesised gamestate) and report its layout."""
    msg = jp.MsgReader(payload)
    head = {
        "lastClientCommand": msg.read_long(),
        "server_commands": [],
    }
    while True:
        cmd = msg.read_byte()
        if cmd == jp.SVC_SERVERCOMMAND:
            seq = msg.read_long()
            s = read_until_nul(msg, 1024)
            head["server_commands"].append((seq, s))
            continue
        break
    if cmd != jp.SVC_GAMESTATE:
        raise ValueError(f"first frame: expected svc_gamestate, got {cmd}")
    gs = parse_gamestate_body(msg, fields, collect=True)
    # The first demo frame ends with a trailing svc_EOF after the RMG short.
    tail = msg.read_byte()
    if tail != jp.SVC_EOF:
        raise ValueError(f"first frame: tail svc_EOF expected, got {tail}")
    gs.update(head)
    gs["tail_eof"] = tail
    gs["end_bit"] = msg.bit  # includes the trailing svc_EOF
    gs["payload_bytes"] = len(payload)
    return gs


def reencode_first_frame(gs, fields):
    """Rebuild a decoded first frame from its values.

    The Huffman tree is static (`MSG_initHuffman` preloads `msg_hData` and
    `MSG_WriteBits`/`MSG_ReadBits` do not adapt it), so a value-exact
    re-encode must reproduce the payload byte for byte. This is the Phase 0
    byte-level check of the gamestate layout.
    """
    w = jp.MsgWriter()
    w.write_long(gs["lastClientCommand"])
    for seq, s in gs["server_commands"]:
        w.write_byte(jp.SVC_SERVERCOMMAND)
        w.write_long(seq)
        w.write_string(s)
    w.write_byte(jp.SVC_GAMESTATE)
    w.write_long(gs["reliableSequence"])
    for idx in sorted(gs["configstrings"]):
        w.write_byte(jp.SVC_CONFIGSTRING)
        w.write_short(idx)
        w.write_string(gs["configstrings"][idx])
    for num, has_delta, changed, values in gs["baseline_details"]:
        w.write_byte(jp.SVC_BASELINE)
        if has_delta:
            write_delta_entity(w, fields["entity"], num, changed, values)
        else:
            w.write_bits(num, GENTITYNUM_BITS)
            w.write_bits(0, 1)
            w.write_bits(0, 1)
    w.write_byte(jp.SVC_EOF)
    w.write_long(gs["clientNum"])
    w.write_long(gs["checksumFeed"])
    w.write_short(gs["rmg_short"])
    w.write_byte(jp.SVC_EOF)
    return w.payload()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--frames", type=int, default=6)
    ap.add_argument("--all-snapshots", action="store_true")
    ap.add_argument("--dump-configstrings", action="store_true")
    ap.add_argument(
        "--verify-gamestate",
        action="store_true",
        help="re-encode the first frame from decoded values and compare bytes",
    )
    ap.add_argument(
        "--validate-stream",
        action="store_true",
        help="check framing, delta distance, entity order/range and cs commands",
    )
    ap.add_argument(
        "--dump-gamestate-sidecar",
        metavar="PATH",
        help="write the decoded first frame for the Rust Phase 1 tests",
    )
    args = ap.parse_args()

    server = Binary(ENGINE)
    fields = {
        name: server.netfields(addr, count)
        for name, (addr, count) in SERVER_TABLES.items()
    }

    d = Frames(args.file)
    d.check()
    print(f"file: {args.file}")
    print(f"size: {len(d.data)}  frames: {len(d.frames)}  terminator: {d.terminated}")
    print(f"first seq: {d.frames[0][1]}  last seq: {d.frames[-1][1]}  trailing: {d.trailing}")

    reset_huffman()
    first = d.frames[0][2]
    gs = parse_first_frame(first, fields)
    print(
        f"gamestate: payload={gs['payload_bytes']}B "
        f"configstrings={len(gs['configstrings'])} baselines={len(gs['baselines'])} "
        f"clientNum={gs['clientNum']} checksumFeed=0x{gs['checksumFeed'] & 0xFFFFFFFF:08x} "
        f"rmg_short={gs['rmg_short']} end_bit={gs['end_bit']} end_byte={gs['end_byte']}"
    )
    if args.dump_configstrings:
        for idx in sorted(gs["configstrings"]):
            s = gs["configstrings"][idx]
            print(f"  cs[{idx}] = {s[:120]!r}{'...' if len(s) > 120 else ''}")

    if args.dump_gamestate_sidecar:
        written = dump_gamestate_sidecar(
            args.dump_gamestate_sidecar, gs, fields["entity"]
        )
        print(f"gamestate sidecar: {written} bytes -> {args.dump_gamestate_sidecar}")

    if args.validate_stream:
        reset_huffman()
        commands = []
        snapshots = 0
        max_delta = 0
        bad_order = 0
        bad_range = 0
        bad_cs = 0
        cs_commands = 0
        for idx, (off, seq, payload) in enumerate(d.frames):
            embedded_gs, snaps = consume_message(
                payload, fields, keep=True, commands=commands
            )
            if idx == 0 and embedded_gs is None:
                raise ValueError("first frame is not a gamestate")
            for sn in snaps:
                snapshots += 1
                max_delta = max(max_delta, sn["deltaNum"])
                numbers = [entity[0] for entity in sn["entities"]]
                if numbers != sorted(numbers):
                    bad_order += 1
                if any(number < 0 or number >= MAX_GENTITIES for number in numbers):
                    bad_range += 1
        for _seq, text in commands:
            if text.startswith("cs ") or text.startswith("bcs"):
                cs_commands += 1
                if not re.fullmatch(r"(?:cs|bcs[012]) -?\d+ \".*\"\n", text, re.S):
                    bad_cs += 1
        print(
            f"stream validation: gamestate first={True} snapshots={snapshots} "
            f"max_delta={max_delta} bad_order={bad_order} bad_range={bad_range} "
            f"cs_commands={cs_commands} bad_cs={bad_cs} commands={len(commands)}"
        )
        if max_delta > 31 or bad_order or bad_range or bad_cs:
            return 2

    if args.verify_gamestate:
        rebuilt = reencode_first_frame(gs, fields)
        # `end_bit` is the reader's bit position after the trailing svc_EOF:
        # bits [0, end_bit) are message content, the rest of the final byte is
        # padding. A few real demos carry an extra, uninitialised trailing byte
        # from the client's stack buffer (CL_Record_f writes msg.cursize; the
        # last byte is never written); it is not message data and the demo
        # player ignores it.
        full_bytes, tail_bits = divmod(gs["end_bit"], 8)
        ok = rebuilt[:full_bytes] == first[:full_bytes]
        if ok and tail_bits and full_bytes < len(first):
            mask = (1 << tail_bits) - 1
            ok = (rebuilt[full_bytes] & mask) == (first[full_bytes] & mask)
        if ok:
            padding = len(first) - full_bytes - (1 if tail_bits else 0)
            print(
                f"gamestate round-trip: meaningful bytes identical "
                f"({full_bytes} full + {tail_bits} bits; {padding} padding byte(s) ignored)"
            )
        else:
            n = min(len(rebuilt), len(first))
            diff = next((i for i in range(n) if rebuilt[i] != first[i]), n)
            print(
                f"gamestate round-trip: MISMATCH rebuilt={len(rebuilt)} "
                f"original={len(first)} first_diff={diff}"
            )
            return 2

    for _, seq, payload in d.frames[1 : args.frames]:
        _, snaps = consume_message(payload, fields)
        for info in snaps:
            ps = info["ps"]
            print(
                f"seq {seq}: snapshot t={info['serverTime']} delta={info['deltaNum']} "
                f"flags=0x{info['snapFlags']:02x} areabytes={info['areabytes']} "
                f"ps(pilot={ps['pilot']}, changed={len(ps['fields'])}) "
                f"entities={len(info['entities'])}"
            )

    if args.all_snapshots:
        reset_huffman()
        bad = 0
        n_snap = 0
        for idx, (off, seq, payload) in enumerate(d.frames):
            try:
                gs2, snaps = consume_message(payload, fields)
                if idx == 0 and gs2 is None:
                    raise ValueError("first frame is not a gamestate")
                n_snap += len(snaps)
            except Exception as e:
                bad += 1
                if bad <= 5:
                    print(f"  frame {idx} (seq {seq}) decode error: {e}")
        print(f"decoded {len(d.frames)} frames ({n_snap} snapshots), {bad} errors")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
