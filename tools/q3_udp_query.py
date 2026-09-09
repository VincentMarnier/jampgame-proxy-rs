#!/usr/bin/env python3
"""Query a running Jedi Academy (Q3-protocol) dedicated server over UDP.

Performs the standard two-step connectionless handshake and prints the
server's getinfo / getstatus responses.

Usage:
    q3_udp_query.py [host] [port]

Example:
    tools/q3_udp_query.py 127.0.0.1 29099
"""
import socket
import sys
import time
import re

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 29099
FF = b"\xff" * 4


def send(sock, payload: bytes) -> bytes | None:
    sock.sendto(FF + payload, (HOST, PORT))
    try:
        return sock.recv(4096)
    except socket.timeout:
        return None


def main() -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(3)

    # Step 1: obtain a challenge.
    challenge = b""
    for _ in range(3):
        resp = send(sock, b"getchallenge")
        if resp:
            m = re.search(rb"challengeResponse\s+(\d+)", resp)
            if m:
                challenge = m.group(1)
                break
    if not challenge:
        print(f"no challenge response from {HOST}:{PORT}")
        return 1
    print(f"challenge: {challenge.decode()}")

    # Step 2: query the two info commands with the challenge.
    for cmd in (b"getinfo", b"getstatus"):
        time.sleep(0.2)
        resp = send(sock, cmd + b" " + challenge)
        print(f"{cmd.decode()} ->")
        print((resp or b"(no response)").decode(errors="replace"))
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
