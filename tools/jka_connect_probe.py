#!/usr/bin/env python3
"""Probe a Jedi Academy server's connect path (getchallenge + connect).

The engine Huffman-decompresses any connectionless packet that starts with
"connect" (codemp/server/sv_main.cpp:533-535), so a plain-text connect is not
a valid probe. This script is a direct port of the engine's adaptive Huffman
compressor (codemp/qcommon/huffman.cpp: Huff_Compress/Huff_transmit/
Huff_addRef) and sends the same packet a real client would, then prints the
server's reply.

Useful for checking connect-time behaviour that a stock UDP query cannot reach
(the ban gate of `proxy_sv_enableBanFile`, the userinfo sanitizer, mutes at
connect, ...).

Usage:
    tools/jka_connect_probe.py HOST PORT [--name NAME] [--protocol N]
                                       [--model MODEL] [--extra KEY=VALUE ...]

Example:
    tools/jka_connect_probe.py 127.0.0.1 29070 --name Probe
    # print\\nBanned.\\n      (source listed in proxy_sv_banFile)
    # connectResponse        (accepted)

The probe's source address is the address of the host running the script; the
minimal userinfo does not carry a model, so on a proxy with userinfo validation
an un-banned probe is expected to be refused with
`Failed userinfo validation: model field not found` — that still proves the ban
gate was passed. Pass `--model kyle/default` (plus `--extra`) if a full
acceptance is wanted.
"""

import argparse
import re
import socket
import sys

NYT = 256
INTERNAL_NODE = 257


class Node:
    __slots__ = ("left", "right", "parent", "next", "prev", "head", "weight", "symbol")

    def __init__(self):
        self.left = self.right = self.parent = self.next = self.prev = None
        self.head = None
        self.weight = 0
        self.symbol = 0


class Slot:
    __slots__ = ("node",)

    def __init__(self):
        self.node = None


class Huff:
    """Adaptive Huffman compressor, mirroring `huff_t` + `Huff_*` in huffman.cpp."""

    def __init__(self):
        self.bloc_node = 0
        self.bloc_ptrs = 0
        self.tree = None
        self.lhead = None
        self.ltail = None
        self.loc = [None] * (NYT + 1)
        self.freelist = None
        self.node_list = [Node() for _ in range(768)]
        self.node_ptrs = [Slot() for _ in range(768)]
        self.out = bytearray()
        self.bloc = 0

    def new_node(self):
        node = self.node_list[self.bloc_node]
        self.bloc_node += 1
        node.left = node.right = node.parent = node.next = node.prev = None
        node.head = None
        node.weight = 0
        node.symbol = 0
        return node

    def get_ppnode(self):
        if self.freelist is None:
            slot = self.node_ptrs[self.bloc_ptrs]
            self.bloc_ptrs += 1
            return slot
        slot = self.freelist
        self.freelist = slot.node
        return slot

    def free_ppnode(self, slot):
        slot.node = self.freelist
        self.freelist = slot

    def add_bit(self, bit):
        if (self.bloc & 7) == 0:
            self.out.append(0)
        self.out[-1] |= bit << (self.bloc & 7)
        self.bloc += 1

    def swap(self, n1, n2):
        p1, p2 = n1.parent, n2.parent
        if p1:
            if p1.left is n1:
                p1.left = n2
            else:
                p1.right = n2
        else:
            self.tree = n2
        if p2:
            if p2.left is n2:
                p2.left = n1
            else:
                p2.right = n1
        else:
            self.tree = n1
        n1.parent = p2
        n2.parent = p1

    @staticmethod
    def swaplist(n1, n2):
        p = n1.next
        n1.next = n2.next
        n2.next = p
        p = n1.prev
        n1.prev = n2.prev
        n2.prev = p
        if n1.next is n1:
            n1.next = n2
        if n2.next is n2:
            n2.next = n1
        if n1.next:
            n1.next.prev = n1
        if n2.next:
            n2.next.prev = n2
        if n1.prev:
            n1.prev.next = n1
        if n2.prev:
            n2.prev.next = n2

    def increment(self, node):
        if node is None:
            return
        if node.next is not None and node.next.weight == node.weight:
            lnode = node.head.node
            if lnode is not node.parent:
                self.swap(lnode, node)
            self.swaplist(lnode, node)
        if node.prev is not None and node.prev.weight == node.weight:
            node.head.node = node.prev
        else:
            node.head.node = None
            self.free_ppnode(node.head)
        node.weight += 1
        if node.next is not None and node.next.weight == node.weight:
            node.head = node.next.head
        else:
            node.head = self.get_ppnode()
            node.head.node = node
        if node.parent:
            self.increment(node.parent)
            if node.prev is node.parent:
                self.swaplist(node, node.parent)
                if node.head.node is node:
                    node.head.node = node.parent

    def add_ref(self, ch):
        if self.loc[ch] is None:
            tnode = self.new_node()
            tnode2 = self.new_node()

            tnode2.symbol = INTERNAL_NODE
            tnode2.weight = 1
            tnode2.next = self.lhead.next
            if self.lhead.next:
                self.lhead.next.prev = tnode2
                if self.lhead.next.weight == 1:
                    tnode2.head = self.lhead.next.head
                else:
                    tnode2.head = self.get_ppnode()
                    tnode2.head.node = tnode2
            else:
                tnode2.head = self.get_ppnode()
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
                    tnode.head = self.get_ppnode()
                    tnode.head.node = tnode2
            else:
                tnode.head = self.get_ppnode()
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
            self.increment(tnode2.parent)
        else:
            self.increment(self.loc[ch])

    def send(self, node, child):
        if node.parent:
            self.send(node.parent, node)
        if child is not None:
            self.add_bit(1 if node.right is child else 0)

    def transmit(self, ch):
        if self.loc[ch] is None:
            self.transmit(NYT)
            for i in range(7, -1, -1):
                self.add_bit((ch >> i) & 1)
        else:
            self.send(self.loc[ch], None)


def huff_compress(payload: bytes) -> bytes:
    huff = Huff()
    huff.tree = huff.lhead = huff.loc[NYT] = huff.new_node()
    huff.tree.symbol = NYT
    huff.tree.weight = 0
    huff.lhead.next = huff.lhead.prev = None
    huff.tree.parent = huff.tree.left = huff.tree.right = None
    huff.loc[NYT] = huff.tree

    size = len(payload)
    huff.out = bytearray([(size >> 8) & 0xFF, size & 0xFF])
    huff.bloc = 16
    for ch in payload:
        huff.transmit(ch)
        huff.add_ref(ch)
    huff.bloc += 8
    return bytes(huff.out[: huff.bloc >> 3])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("host")
    parser.add_argument("port", type=int)
    parser.add_argument("--name", default="Probe")
    parser.add_argument("--protocol", default="26")
    parser.add_argument("--model", default=None)
    parser.add_argument(
        "--extra",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="additional userinfo key (repeatable)",
    )
    parser.add_argument("--timeout", type=float, default=2.0)
    args = parser.parse_args()

    info = [
        ("protocol", args.protocol),
        ("qport", "4321"),
        ("name", args.name),
        ("rate", "25000"),
        ("snaps", "20"),
    ]
    if args.model:
        info.append(("model", args.model))
    for item in args.extra:
        key, sep, value = item.partition("=")
        if not sep:
            parser.error(f"--extra expects KEY=VALUE, got {item!r}")
        info.append((key, value))

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(args.timeout)
    try:
        sock.sendto(b"\xff\xff\xff\xffgetchallenge", (args.host, args.port))
        reply = sock.recvfrom(2048)[0]
    except socket.timeout:
        print("no challenge reply", file=sys.stderr)
        return 1
    match = re.search(rb"challengeResponse (-?\d+)", reply)
    if not match:
        print(f"unexpected challenge reply: {reply[:80]!r}", file=sys.stderr)
        return 1

    fields = "".join(f"\\{key}\\{value}" for key, value in info)
    userinfo = f"\\challenge\\{match.group(1).decode()}{fields}"
    packet = b"\xff\xff\xff\xffconnect " + huff_compress(
        ('"' + userinfo + '"').encode()
    )
    sock.sendto(packet, (args.host, args.port))
    try:
        answer = sock.recvfrom(4096)[0]
    except socket.timeout:
        print("<no connect reply>")
        return 0
    print(answer.decode("latin1"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
