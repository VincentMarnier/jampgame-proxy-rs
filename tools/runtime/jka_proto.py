# Jedi Academy MP (protocol 26) wire codec — a faithful Python port of the
# engine's bit-level message IO, adaptive Huffman and netchan XOR obfuscation.
#
# Source of truth (engine behaviour, `original/jedi-academy/codemp/`):
#   qcommon/msg.cpp       MSG_WriteBits/MSG_ReadBits, MSG_WriteDeltaUsercmdKey,
#                         msg_hData (the "Q3 TA freq. table", line 2719 — the
#                         other table at 2457 is inside a comment block),
#                         MSG_initHuffman
#   qcommon/huffman.cpp   Huff_putBit/getBit, Huff_addRef/increment/swap/
#                         swaplist, Huff_offsetTransmit/Receive,
#                         Huff_Compress/Huff_Decompress
#   qcommon/net_chan.cpp  Netchan_Transmit/Netchan_Process, NET_OutOfBandPrint,
#                         NET_OutOfBandData
#   client/cl_net_chan.cpp CL_Netchan_Encode/Decode
#   server/sv_net_chan.cpp SV_Netchan_Encode/Decode
#   client/cl_input.cpp   CL_WritePacket message layout
#   server/sv_client.cpp  SV_ExecuteClientMessage/SV_UserMove (the reader side)
#
# Notable engine quirks preserved here (verified in source, not assumed):
#   * non-OOB MSG_WriteBits writes the low (bits&7) bits *raw* (Huff_putBit,
#     LSB first) and the remaining whole bytes as adaptive-Huffman symbols;
#   * MSG_WriteString writes the terminating NUL;
#   * the XOR obfuscation covers compressed-payload byte offsets 12.. on
#     client->server messages (CL_ENCODE_START) and 4.. on server->client
#     messages (SV_ENCODE_START); the receiving side offsets are expressed
#     relative to the whole datagram (readcount 6+SV_DECODE_START == 12 for
#     C->S, readcount 4+CL_DECODE_START == 8 for S->C), which maps to the
#     same payload offsets;
#   * the XOR key is a byte and mixes the (last-acked reliable command) string
#     with `key ^= string[index] << (i & 1)` ('%' is written as '.').
#
# Used by a2_netstatus_groundtruth.py (synthetic client). Read-only reference:
# nothing here writes under original/.

from __future__ import annotations

import struct

PROTOCOL_VERSION = 26

# ---------------------------------------------------------------------------
# Protocol constants (qcommon.h enum svc_ops_e / clc_ops_e, q_shared.h)
# ---------------------------------------------------------------------------

SVC_BAD = 0
SVC_NOP = 1
SVC_GAMESTATE = 2
SVC_CONFIGSTRING = 3
SVC_BASELINE = 4
SVC_SERVERCOMMAND = 5
SVC_DOWNLOAD = 6
SVC_SNAPSHOT = 7
SVC_SETGAME = 8
SVC_MAPCHANGE = 9
SVC_EOF = 10

CLC_BAD = 0
CLC_NOP = 1
CLC_MOVE = 2
CLC_MOVE_NO_DELTA = 3
CLC_CLIENTCOMMAND = 4
CLC_EOF = 5

MAX_RELIABLE_COMMANDS = 64
MAX_STRING_CHARS = 1024
MAX_PACKET_USERCMDS = 32
PACKET_BACKUP = 32
PACKET_MASK = PACKET_BACKUP - 1
# clientActive_t.cmds[CMD_BACKUP] (client.h:114, cg_public.h:6-7)
CMD_BACKUP = 64
CMD_MASK = CMD_BACKUP - 1
CS_SYSTEMINFO = 1
GENTITYNUM_BITS = 10

FRAGMENT_BIT = 1 << 31
MAX_PACKETLEN = 1400
FRAGMENT_SIZE = MAX_PACKETLEN - 100  # net_chan.cpp PACKET_HEADER 10

# CL_Netchan_Encode starts at payload byte 12; SV_Netchan_Encode at payload
# byte 4 (the 3 leading longs of a C->S message are left unobfuscated).
CL_ENCODE_START = 12
SV_ENCODE_START = 4

# ---------------------------------------------------------------------------
# msg_hData — msg.cpp:2719 ("Q3 TA freq. table."), the table MSG_initHuffman
# feeds into both adaptive-Huffman trees.
# ---------------------------------------------------------------------------

MSG_HDATA = [
    250315, 41193, 6292, 7106, 3730, 3750, 6110, 23283, 33317, 6950,
    7838, 9714, 9257, 17259, 3949, 1778, 8288, 1604, 1590, 1663,
    1100, 1213, 1238, 1134, 1749, 1059, 1246, 1149, 1273, 4486,
    2805, 3472, 21819, 1159, 1670, 1066, 1043, 1012, 1053, 1070,
    1726, 888, 1180, 850, 960, 780, 1752, 3296, 10630, 4514,
    5881, 2685, 4650, 3837, 2093, 1867, 2584, 1949, 1972, 940,
    1134, 1788, 1670, 1206, 5719, 6128, 7222, 6654, 3710, 3795,
    1492, 1524, 2215, 1140, 1355, 971, 2180, 1248, 1328, 1195,
    1770, 1078, 1264, 1266, 1168, 965, 1155, 1186, 1347, 1228,
    1529, 1600, 2617, 2048, 2546, 3275, 2410, 3585, 2504, 2800,
    2675, 6146, 3663, 2840, 14253, 3164, 2221, 1687, 3208, 2739,
    3512, 4796, 4091, 3515, 5288, 4016, 7937, 6031, 5360, 3924,
    4892, 3743, 4566, 4807, 5852, 6400, 6225, 8291, 23243, 7838,
    7073, 8935, 5437, 4483, 3641, 5256, 5312, 5328, 5370, 3492,
    2458, 1694, 1821, 2121, 1916, 1149, 1516, 1367, 1236, 1029,
    1258, 1104, 1245, 1006, 1149, 1025, 1241, 952, 1287, 997,
    1713, 1009, 1187, 879, 1099, 929, 1078, 951, 1656, 930,
    1153, 1030, 1262, 1062, 1214, 1060, 1621, 930, 1106, 912,
    1034, 892, 1158, 990, 1175, 850, 1121, 903, 1087, 920,
    1144, 1056, 3462, 2240, 4397, 12136, 7758, 1345, 1307, 3278,
    1950, 886, 1023, 1112, 1077, 1042, 1061, 1071, 1484, 1001,
    1096, 915, 1052, 995, 1070, 876, 1111, 851, 1059, 805,
    1112, 923, 1103, 817, 1899, 1872, 976, 841, 1127, 956,
    1159, 950, 7791, 954, 1289, 933, 1127, 3207, 1020, 927,
    1355, 768, 1040, 745, 952, 805, 1073, 740, 1013, 805,
    1008, 796, 996, 1057, 11457, 13504,
]
assert len(MSG_HDATA) == 256

NYT = 256  # qcommon.h: NYT HMAX, HMAX 256
INTERNAL_NODE = 257


# ---------------------------------------------------------------------------
# Adaptive Huffman (huffman.cpp), node-for-node port.
# ---------------------------------------------------------------------------


class _Node:
    __slots__ = ("left", "right", "parent", "next", "prev", "head", "weight", "symbol")

    def __init__(self):
        self.left = None
        self.right = None
        self.parent = None
        self.next = None
        self.prev = None
        self.head = None  # a _Box holding the "highest ranked node in block"
        self.weight = 0
        self.symbol = 0


class _Box:
    """A `node_t **`: a one-word pointer cell (nodePtrs[] / freelist)."""

    __slots__ = ("node",)

    def __init__(self):
        self.node = None


class Huff:
    """Port of `huff_t` plus its methods (one tree)."""

    def __init__(self):
        self.node_list = [_Node() for _ in range(768)]
        self.bloc_node = 0
        self.tree = None
        self.lhead = None
        self.ltail = None
        self.loc = [None] * (NYT + 1)
        self.freelist = []  # freed _Box cells (C: node_t** freelist chain)

    # -- node pointer cell management ------------------------------------
    def _get_ppnode(self):
        if self.freelist:
            return self.freelist.pop()
        return _Box()

    def _free_ppnode(self, box):
        self.freelist.append(box)

    # -- tree/list primitives -------------------------------------------
    def _swap(self, node1, node2):
        par1 = node1.parent
        par2 = node2.parent
        if par1:
            if par1.left is node1:
                par1.left = node2
            else:
                par1.right = node2
        else:
            self.tree = node2
        if par2:
            if par2.left is node2:
                par2.left = node1
            else:
                par2.right = node1
        else:
            self.tree = node1
        node1.parent = par2
        node2.parent = par1

    def _swaplist(self, node1, node2):
        par1 = node1.next
        node1.next = node2.next
        node2.next = par1
        par1 = node1.prev
        node1.prev = node2.prev
        node2.prev = par1
        if node1.next is node1:
            node1.next = node2
        if node2.next is node2:
            node2.next = node1
        if node1.next:
            node1.next.prev = node1
        if node2.next:
            node2.next.prev = node2
        if node1.prev:
            node1.prev.next = node1
        if node2.prev:
            node2.prev.next = node2

    def _increment(self, node):
        if node is None:
            return
        if node.next is not None and node.next.weight == node.weight:
            lnode = node.head.node
            if lnode is not node.parent:
                self._swap(lnode, node)
            self._swaplist(lnode, node)
        if node.prev and node.prev.weight == node.weight:
            node.head.node = node.prev
        else:
            node.head.node = None
            self._free_ppnode(node.head)
        node.weight += 1
        if node.next and node.next.weight == node.weight:
            node.head = node.next.head
        else:
            node.head = self._get_ppnode()
            node.head.node = node
        if node.parent:
            self._increment(node.parent)
            if node.prev is node.parent:
                self._swaplist(node, node.parent)
                if node.head.node is node:
                    node.head.node = node.parent

    def add_ref(self, ch):
        if self.loc[ch] is None:
            tnode = self.node_list[self.bloc_node]
            self.bloc_node += 1
            tnode2 = self.node_list[self.bloc_node]
            self.bloc_node += 1

            tnode2.symbol = INTERNAL_NODE
            tnode2.weight = 1
            tnode2.next = self.lhead.next
            if self.lhead.next:
                self.lhead.next.prev = tnode2
                if self.lhead.next.weight == 1:
                    tnode2.head = self.lhead.next.head
                else:
                    tnode2.head = self._get_ppnode()
                    tnode2.head.node = tnode2
            else:
                tnode2.head = self._get_ppnode()
                tnode2.head.node = tnode2
            self.lhead.next = tnode2
            tnode2.prev = self.lhead

            tnode.symbol = ch
            tnode.weight = 1
            tnode.next = self.lhead.next
            if self.lhead.next:
                self.lhead.next.prev = tnode
                if self.lhead.next.weight == 1:
                    tnode.head = self.lhead.next.head
                else:
                    tnode.head = self._get_ppnode()
                    tnode.head.node = tnode2  # engine's own quirk (huffman.cpp)
            else:
                tnode.head = self._get_ppnode()
                tnode.head.node = tnode
            self.lhead.next = tnode
            tnode.prev = self.lhead
            tnode.left = tnode.right = None

            if self.lhead.parent:
                if self.lhead.parent.left is self.lhead:
                    self.lhead.parent.left = tnode2
                else:
                    self.lhead.parent.right = tnode2
            else:
                self.tree = tnode2

            tnode2.right = tnode
            tnode2.left = self.lhead
            tnode2.parent = self.lhead.parent
            self.lhead.parent = tnode.parent = tnode2
            self.loc[ch] = tnode
            self._increment(tnode2.parent)
        else:
            self._increment(self.loc[ch])

    def _send(self, node, child, out):
        # iterative version of huffman.cpp send(): walks to the root, then
        # emits the child bits top-down.
        chain = []
        cur = node
        ch = child
        while cur is not None:
            chain.append((cur, ch))
            ch = cur
            cur = cur.parent
        for cur, ch in reversed(chain):
            if ch is not None:
                out.append_bit(1 if cur.right is ch else 0)

    def transmit_symbol(self, ch, out):
        self._send(self.loc[ch], None, out)

    def transmit(self, ch, out):
        """Huff_transmit: the NYT branch (first transmission of a symbol) is
        only used by the OOB `Huff_Compress` path (a fresh tree)."""
        if self.loc[ch] is None:
            self.transmit(NYT, out)
            for i in range(7, -1, -1):
                out.append_bit((ch >> i) & 1)
        else:
            self._send(self.loc[ch], None, out)

    def receive_symbol(self, src):
        node = self.tree
        while node is not None and node.symbol == INTERNAL_NODE:
            node = node.right if src.get_bit() else node.left
        if node is None:
            return 0
        return node.symbol


def _init_tree():
    huff = Huff()
    huff.tree = huff.lhead = huff.ltail = huff.loc[NYT] = huff.node_list[huff.bloc_node]
    huff.bloc_node += 1
    huff.tree.symbol = NYT
    huff.tree.weight = 0
    huff.lhead.next = huff.lhead.prev = None
    huff.tree.parent = huff.tree.left = huff.tree.right = None
    return huff


# The two shared trees MSG_initHuffman builds (compressor + decompressor),
# pre-loaded with msg_hData like the engine does.
_COMPRESSOR = _init_tree()
_DECOMPRESSOR = _init_tree()
for _i in range(256):
    for _j in range(MSG_HDATA[_i]):
        _COMPRESSOR.add_ref(_i)
        _DECOMPRESSOR.add_ref(_i)


# ---------------------------------------------------------------------------
# Bit-level message IO (msg.cpp), non-OOB (bitstream) mode only. The proxy
# client never uses the OOB MSG path (OOB payloads are built raw + Huff_Compress).
# ---------------------------------------------------------------------------


class MsgWriter:
    """msg_t write side: a raw-bit / Huffman-symbol mix into `data`."""

    def __init__(self):
        self.data = bytearray(65536)
        self.bit = 0
        self.cursize = 0

    def put_bit(self, bit):
        bloc = self.bit
        if (bloc & 7) == 0:
            self.data[bloc >> 3] = 0
        self.data[bloc >> 3] |= (bit & 1) << (bloc & 7)
        self.bit = bloc + 1

    def _xmit_byte(self, value):
        class _Out:
            pass

        out = _Out()
        out.append_bit = self.put_bit
        _COMPRESSOR.transmit_symbol(value, out)

    def write_bits(self, value, bits):
        if bits == 0 or bits < -31 or bits > 32:
            raise ValueError(f"MSG_WriteBits: bad bits {bits}")
        if bits < 0:
            bits = -bits
        value &= 0xFFFFFFFF >> (32 - bits)
        if bits & 7:
            nbits = bits & 7
            for _ in range(nbits):
                self.put_bit(value & 1)
                value >>= 1
            bits -= nbits
        if bits:
            for _ in range(0, bits, 8):
                self._xmit_byte(value & 0xFF)
                value >>= 8
        self.cursize = (self.bit >> 3) + 1

    def write_byte(self, value):
        self.write_bits(value, 8)

    def write_short(self, value):
        self.write_bits(value, 16)

    def write_long(self, value):
        self.write_bits(value, 32)

    def write_string(self, s):
        data = s.encode("latin-1") + b"\x00"
        for b in data:
            self.write_byte(b)

    def payload(self):
        return bytes(self.data[: self.cursize])


class MsgReader:
    """msg_t read side, mirroring MSG_ReadBits semantics (readcount included)."""

    def __init__(self, data, bit=0, cursize=None):
        self.data = data
        self.bit = bit
        self.cursize = len(data) if cursize is None else cursize
        self.readcount = (bit >> 3) + 1 if bit else 0

    def get_bit(self):
        bloc = self.bit
        t = (self.data[bloc >> 3] >> (bloc & 7)) & 0x1
        self.bit = bloc + 1
        return t

    def _recv_byte(self):
        class _Src:
            pass

        src = _Src()
        src.get_bit = self.get_bit
        return _DECOMPRESSOR.receive_symbol(src)

    def read_bits(self, bits):
        sgn = bits < 0
        if sgn:
            bits = -bits
        value = 0
        nbits = 0
        if bits & 7:
            nbits = bits & 7
            for i in range(nbits):
                value |= self.get_bit() << i
            bits -= nbits
        if bits:
            for i in range(0, bits, 8):
                value |= self._recv_byte() << (i + nbits)
        self.readcount = (self.bit >> 3) + 1
        if sgn and (value & (1 << (bits - 1))):
            value |= -1 ^ ((1 << bits) - 1)
        return value

    def read_byte(self):
        c = self.read_bits(8)
        if self.readcount > self.cursize:
            return -1
        return c

    def read_short(self):
        c = self.read_bits(16)
        if self.readcount > self.cursize:
            return -1
        if c & 0x8000:
            c -= 0x10000
        return c

    def read_long(self):
        c = self.read_bits(32)
        if c & 0x80000000:
            c -= 0x100000000
        return c

    def read_string(self):
        out = bytearray()
        while True:
            c = self.read_byte()
            if c <= 0:
                break
            out.append(c)
            if len(out) >= MAX_STRING_CHARS - 1:
                break
        return out.decode("latin-1")


# ---------------------------------------------------------------------------
# OOB (connectionless) packets: raw text or Huff_Compress'ed "connect".
# ---------------------------------------------------------------------------


def oob_print(text):
    return b"\xff\xff\xff\xff" + text.encode("latin-1")


def oob_connect(infostring):
    """NET_OutOfBandData for `connect "<infostring>"` (net_chan.cpp:631-651).

    Bytes 0..11 stay raw (0xffffffff + "connect "), everything from offset 12
    is adaptive-Huffman compressed with a 2-byte big-endian length prefix.
    """
    body = f'connect "{infostring}"'.encode("latin-1")
    raw = b"\xff\xff\xff\xff" + body
    out = bytearray(raw[:12])
    out += huff_compress(raw[12:])
    return bytes(out)


def huff_compress(data):
    huff = _init_tree()
    seq = bytearray(65536)
    seq[0] = (len(data) >> 8) & 0xFF
    seq[1] = len(data) & 0xFF

    class _Out:
        pass

    bloc = 16

    def append_bit(bit):
        nonlocal bloc
        if (bloc & 7) == 0:
            seq[bloc >> 3] = 0
        seq[bloc >> 3] |= (bit & 1) << (bloc & 7)
        bloc += 1

    out = _Out()
    out.append_bit = append_bit
    for ch in data:
        huff.transmit(ch, out)
        huff.add_ref(ch)
    bloc += 8  # next byte (huffman.cpp Huff_Compress)
    return bytes(seq[: bloc >> 3])


# ---------------------------------------------------------------------------
# Netchan XOR obfuscation (cl_net_chan.cpp / sv_net_chan.cpp)
# ---------------------------------------------------------------------------


def _xor_mix(data, start, key, string):
    out = bytearray(data)
    if isinstance(string, str):
        string = string.encode("latin-1")
    index = 0
    nul = len(string)
    for i in range(start, len(out)):
        if index >= nul or string[index] == 0:
            index = 0
        ch = string[index] if nul else 0
        if ch == ord("%"):
            key ^= ord(".") << (i & 1)
        else:
            key ^= ch << (i & 1)
        key &= 0xFF
        index += 1
        out[i] ^= key
    return bytes(out)


def encode_client_payload(payload, challenge, server_id, message_ack, rel_ack, server_cmd_string):
    """CL_Netchan_Encode: XOR compressed payload bytes from offset 12."""
    if len(payload) <= CL_ENCODE_START:
        return payload
    key = (challenge ^ server_id ^ message_ack) & 0xFF
    return _xor_mix(payload, CL_ENCODE_START, key, server_cmd_string)


def decode_server_payload(datagram, challenge, rel_ack_cmd_string):
    """CL_Netchan_Decode: `datagram` is the whole S->C packet (seq first).

    Returns the payload (datagram bytes from 4 onwards) with bytes from
    absolute offset 8 (= payload offset 4 == SV_ENCODE_START) de-obfuscated.
    """
    if len(datagram) <= 8:
        return datagram[4:]
    seq = struct.unpack("<I", datagram[:4])[0]
    key = (challenge ^ seq) & 0xFF
    out = _xor_mix(datagram, 8, key, rel_ack_cmd_string)
    return out[4:]


def com_hash_key(s, maxlen=32):
    """common.cpp:737 Com_HashKey."""
    h = 0
    for i, ch in enumerate(s.encode("latin-1") if isinstance(s, str) else s):
        if i >= maxlen:
            break
        h = (h + ch * (119 + i)) & 0xFFFFFFFF
    h ^= h >> 10
    h ^= h >> 20
    return h


# ---------------------------------------------------------------------------
# usercmd delta writer (msg.cpp MSG_WriteDeltaUsercmdKey)
# ---------------------------------------------------------------------------


class Usercmd:
    __slots__ = (
        "server_time",
        "angles",
        "forwardmove",
        "rightmove",
        "upmove",
        "buttons",
        "weapon",
        "forcesel",
        "invensel",
        "generic_cmd",
    )

    def __init__(self, server_time=0):
        self.server_time = server_time
        self.angles = (0, 0, 0)
        self.forwardmove = 0
        self.rightmove = 0
        self.upmove = 0
        self.buttons = 0
        self.weapon = 0
        self.forcesel = 0
        self.invensel = 0
        self.generic_cmd = 0

    def same_move_fields(self, other):
        return (
            self.angles == other.angles
            and self.forwardmove == other.forwardmove
            and self.rightmove == other.rightmove
            and self.upmove == other.upmove
            and self.buttons == other.buttons
            and self.weapon == other.weapon
            and self.forcesel == other.forcesel
            and self.invensel == other.invensel
            and self.generic_cmd == other.generic_cmd
        )

    def copy(self):
        c = Usercmd(self.server_time)
        c.angles = self.angles
        c.forwardmove = self.forwardmove
        c.rightmove = self.rightmove
        c.upmove = self.upmove
        c.buttons = self.buttons
        c.weapon = self.weapon
        c.forcesel = self.forcesel
        c.invensel = self.invensel
        c.generic_cmd = self.generic_cmd
        return c


def write_delta_usercmd_key(msg, key, from_cmd, to_cmd):
    if to_cmd.server_time - from_cmd.server_time < 256:
        msg.write_bits(1, 1)
        msg.write_bits((to_cmd.server_time - from_cmd.server_time) & 0xFF, 8)
    else:
        msg.write_bits(0, 1)
        msg.write_bits(to_cmd.server_time, 32)
    if to_cmd.same_move_fields(from_cmd):
        msg.write_bits(0, 1)  # no change
        return
    key ^= to_cmd.server_time
    msg.write_bits(1, 1)

    def dk(old_v, new_v, bits):
        if old_v == new_v:
            msg.write_bits(0, 1)
        else:
            msg.write_bits(1, 1)
            msg.write_bits((new_v ^ key) & ((1 << bits) - 1), bits)

    dk(from_cmd.angles[0], to_cmd.angles[0], 16)
    dk(from_cmd.angles[1], to_cmd.angles[1], 16)
    dk(from_cmd.angles[2], to_cmd.angles[2], 16)
    dk(from_cmd.forwardmove, to_cmd.forwardmove, 8)
    dk(from_cmd.rightmove, to_cmd.rightmove, 8)
    dk(from_cmd.upmove, to_cmd.upmove, 8)
    dk(from_cmd.buttons, to_cmd.buttons, 16)
    dk(from_cmd.weapon, to_cmd.weapon, 8)
    dk(from_cmd.forcesel, to_cmd.forcesel, 8)
    dk(from_cmd.invensel, to_cmd.invensel, 8)
    dk(from_cmd.generic_cmd, to_cmd.generic_cmd, 8)


# ---------------------------------------------------------------------------
# Netchan (client side): framing + fragment reassembly (net_chan.cpp)
# ---------------------------------------------------------------------------


class Netchan:
    def __init__(self, qport):
        self.qport = qport
        self.outgoing_sequence = 1
        self.incoming_sequence = 0
        self.dropped = 0
        self.fragment_sequence = -1
        self.fragment_length = 0
        self.fragment_buffer = bytearray()

    def transmit(self, payload):
        """Netchan_Transmit: [seq i32le][qport i16le][payload]."""
        pkt = struct.pack("<IH", self.outgoing_sequence & 0xFFFFFFFF, self.qport & 0xFFFF) + payload
        self.outgoing_sequence += 1
        return pkt

    def process(self, datagram):
        """Netchan_Process (NS_CLIENT, unfragmented + fragment reassembly).

        Returns (sequence, message) where `message` is the complete message in
        datagram form — `[seq i32][payload]` — exactly what the engine leaves
        in `msg->data` after Netchan_Process reassembles fragments (the
        XOR-decode and header offsets are relative to that layout). Returns
        None when the packet carries no complete message (out of order,
        intermediate fragment).
        """
        if len(datagram) < 4:
            return None
        sequence = struct.unpack("<I", datagram[:4])[0]
        fragmented = bool(sequence & FRAGMENT_BIT)
        sequence &= ~FRAGMENT_BIT
        pos = 4
        if fragmented:
            if len(datagram) < pos + 4:
                return None
            frag_start, frag_len = struct.unpack("<HH", datagram[pos : pos + 4])
            pos += 4
        else:
            frag_start = frag_len = 0
        if sequence <= self.incoming_sequence:
            return None
        self.dropped = sequence - (self.incoming_sequence + 1)
        if fragmented:
            if sequence != self.fragment_sequence:
                self.fragment_sequence = sequence
                self.fragment_length = 0
                self.fragment_buffer = bytearray()
            if frag_start != self.fragment_length:
                return None
            if frag_len < 0 or pos + frag_len > len(datagram):
                return None
            self.fragment_buffer += datagram[pos : pos + frag_len]
            self.fragment_length += frag_len
            if frag_len == FRAGMENT_SIZE:
                return None
            payload = bytes(self.fragment_buffer)
            self.fragment_buffer = bytearray()
            self.fragment_length = 0
            self.incoming_sequence = sequence
            return sequence, struct.pack("<I", sequence) + payload
        self.incoming_sequence = sequence
        return sequence, datagram
