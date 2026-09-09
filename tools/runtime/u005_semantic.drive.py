#!/usr/bin/env python3
"""Drive the U-005 semantic-identity gdb session.

Waits for the engine on 127.0.0.1:29101 to come up, then fires:
  - getchallenge / getinfo / getstatus (connectionless query family)
  - one rcon "testpw status" packet (redirect family)
"""
import socket, sys, time, re

HOST, PORT = "127.0.0.1", 29101
FF = b"\xff" * 4

def send(payload: bytes, timeout=3):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(timeout)
    s.sendto(FF + payload, (HOST, PORT))
    try:
        return s.recv(8192)
    except socket.timeout:
        return None
    finally:
        s.close()

def wait_up(attempts=60):
    for _ in range(attempts):
        r = send(b"getchallenge", timeout=1)
        if r and b"challengeResponse" in r:
            return
        time.sleep(0.5)
    raise SystemExit("server never came up")

wait_up()
print("server is up", flush=True)
time.sleep(1)

# query family
m = re.search(rb"challengeResponse\s+(\d+)", send(b"getchallenge") or b"")
ch = m.group(1) if m else b"0"
print("getchallenge done, challenge", ch, flush=True)
send(b"getinfo " + ch)
print("getinfo sent", flush=True)
time.sleep(0.3)
send(b"getstatus " + ch)
print("getstatus sent", flush=True)
time.sleep(0.3)

# rcon family (no challenge needed for connectionless rcon in Q3-era engine)
r = send(b"rcon testpw status", timeout=3)
print("rcon sent; reply bytes:", (r or b"(no reply)"), flush=True)
print("drive complete", flush=True)
