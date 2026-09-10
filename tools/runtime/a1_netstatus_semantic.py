# A1 netStatus semantic test (gdb-driven inferior calls).
#
# Runs linuxjampded + the Rust proxy as the inferior, waits for the map to
# warm up and bots to be active, then:
#
#   1. forces cl->ping = 20 on the first ACTIVE client,
#   2. allocates a scratch usercmd and calls the Rust `sv_client_think_stub`
#      40 times with serverTime advancing 50 ms per call and
#      cl->messageAcknowledge bumped every second call (packet identity),
#   3. calls the Rust `client_command_net_status` for that client,
#   4. reads the last reliable commands of that client (the /netStatus table),
#   5. recomputes the expected fps/packets/timenudge in Python (mirror of
#      Proxy_Engine_Client.cpp math over the exact injected sequence) and
#      prints EXPECTED vs TABLE.
#
# Run (from the repo root):
#   docker run --rm --cap-add=SYS_PTRACE \
#     -v $PWD/rust/jampgamei386.so:/srv/jka/base/jampgamei386.so:ro \
#     -v $PWD/original/GameData:/srv/jka/gamedata:ro \
#     -v $PWD/tools/runtime:/rt:ro \
#     -w /srv/jka jampgame-investigation:i386-bookworm \
#     gdb -q -batch -x /rt/a1_netstatus_semantic.py
#
# All addresses are the shipped 2003 engine's fixed .text/data (R-018).

import gdb
import os
import re

PORT = "29191"

# Symbol offsets come from the host-side `nm` of the exact staged .so (the
# wrapper script computes them), because Rust legacy mangling hashes change
# on every rebuild. STUB_MANGLED (if it matches) is only a cross-check.
STUB_OFF = int(os.environ["A1_STUB_OFF"], 16)
PRINTER_OFF = int(os.environ["A1_PRINTER_OFF"], 16)
FORWARD_OFF = int(os.environ["A1_FORWARD_OFF"], 16)

CLIENTS_PTR = 0x0831_21EC  # serverStatic_t.clients (client_t*)
SVS_TIME = 0x0831_21E4     # serverStatic_t.time
CALC_PINGS = 0x0805_7204
SIZEOF_CLIENT_T = 332920
OFF_STATE = 0
OFF_PING = 234532
OFF_MSGACK = 132116
OFF_RELSEQ = 132104
OFF_RELCMDS = 1032
OFF_NAME = 133192
MAX_RELIABLE = 128
MAX_STRING_CHARS = 1024
CMD_MASK = 1024
STUB_MANGLED = "_ZN12jampgamei3865hooks9netstatus20sv_client_think_stub17h63e6b8957452c275E"

state = {"hits": 0}


def u32(addr):
    return int(gdb.parse_and_eval(f"*(unsigned int*){addr}"))


def proxy_base():
    out = gdb.execute("info proc mappings", to_string=True)
    best = None
    for line in out.splitlines():
        if "jampgamei386.so" in line:
            addr = int(line.split()[0], 16)
            if best is None or addr < best:
                best = addr
    return best


def find_active():
    clients = u32(CLIENTS_PTR)
    if clients == 0:
        return None
    for i in range(8):
        cl = clients + i * SIZEOF_CLIENT_T
        if u32(cl + OFF_STATE) == 4:  # CS_ACTIVE
            return i, cl
    return None


class Warmup(gdb.Breakpoint):
    def __init__(self):
        super().__init__(f"*{CALC_PINGS:#x}", internal=True)
        self.silent = True

    def stop(self):
        state["hits"] += 1
        if state["hits"] < 60:
            return False
        if proxy_base() is None:
            return False
        if find_active() is None:
            return False
        return True


def instrument(base, stub, printer, forward):
    """Log printer entry and every G_SEND_SERVER_COMMAND trap passthrough."""

    class PrinterBP(gdb.Breakpoint):
        def __init__(self):
            super().__init__(f"*{printer:#x}", internal=True)
            self.silent = True

        def stop(self):
            esp = int(gdb.parse_and_eval("$esp"))
            num = int(gdb.parse_and_eval(f"*(unsigned int*){esp + 4}"))
            print(f"=== A1: PRINTER entered (client_num={num})", flush=True)
            return False

    class SyscallBP(gdb.Breakpoint):
        def __init__(self):
            super().__init__(f"*{forward:#x}", internal=True)
            self.silent = True

        def stop(self):
            esp = int(gdb.parse_and_eval("$esp"))
            cmd = int(gdb.parse_and_eval(f"*(unsigned int*){esp + 4}"))
            if cmd == 19:  # G_SEND_SERVER_COMMAND
                text = int(gdb.parse_and_eval(f"*(unsigned int*){esp + 8}"))
                txt = gdb.execute(f"x/s {text:#x}", to_string=True)
                print(f"=== A1: syscall G_SEND_SERVER_COMMAND: {txt}", flush=True)
            return False

    class StubBP(gdb.Breakpoint):
        def __init__(self):
            super().__init__(f"*{stub:#x}", internal=True)
            self.silent = True

        def stop(self):
            state["stub_hits"] = state.get("stub_hits", 0) + 1
            if state["stub_hits"] == 1:
                print("=== A1: STUB entered for the first time (live or injected)", flush=True)
            return False

    PrinterBP()
    SyscallBP()
    StubBP()


def main():
    gdb.execute("set pagination off")
    gdb.execute("set confirm off")
    gdb.execute("handle SIGALRM nostop noprint pass")
    gdb.execute("handle SIGPIPE nostop noprint pass")
    gdb.execute("handle SIGVTALRM nostop noprint pass")
    gdb.execute("file ./linuxjampded")

    Warmup()
    gdb.execute(
        f"run +set dedicated 1 +set net_port {PORT} "
        "+set fs_cdpath /srv/jka/gamedata +set sv_pure 0 +set sv_maxclients 8 "
        "+set g_gametype 0 +set bot_minplayers 2 +set proxy_sv_enableNetStatus 1 "
        "+map mp/ffa1"
    )

    base = proxy_base()
    i, cl = find_active()
    name = gdb.execute(f"x/s {cl + OFF_NAME}", to_string=True).strip()
    print(f"=== A1: proxy base {base:#x}, client {i} active ({name})", flush=True)

    # Resolve the stub via gdb when the legacy-mangling hash still matches
    # (bakes in the load base); otherwise derive from the nm offsets.
    try:
        stub = int(gdb.parse_and_eval(f"(unsigned int)&{STUB_MANGLED}"))
        base = stub - STUB_OFF
        resolved = "gdb"
    except gdb.error:
        base = proxy_base()
        stub = base + STUB_OFF
        resolved = "nm-offset"
    printer = base + PRINTER_OFF
    forward = base + FORWARD_OFF
    print(
        f"=== A1: base {base:#x} via {resolved}: stub {stub:#x}, "
        f"printer {printer:#x}, forward {forward:#x}",
        flush=True,
    )
    instrument(base, stub, printer, forward)

    t0 = u32(SVS_TIME)
    print(f"=== A1: svs->time = {t0}", flush=True)
    # Deliberately leave cl->ping alone (a bot's ping is 0): since the
    # svFlags-based bot discriminator, the feed and the table must work for
    # ping-0 clients — this exercises the exact localhost scenario. Clear the
    # bot flag too so the test client behaves like a real localhost player.
    gentity = u32(cl + 133188)
    if gentity:
        svflags = u32(gentity + 0x238)
        gdb.execute(f"set *(unsigned int*){gentity + 0x238} = {svflags & ~8}")
        print(f"=== A1: cleared SVF_BOT on gentity {gentity:#x} (svflags {svflags:#x})", flush=True)

    stub_addr = stub
    # Scratch usercmd (inferior malloc).
    uc = int(gdb.parse_and_eval("(unsigned int)malloc(64)"))
    # Zero it, then serverTime is filled per call below.
    gdb.execute(f"call (void*)memset((void*){uc}, 0, 64)")

    printer_addr = printer

    # A1 semantics: drive the stub directly. packet_counter stays 0 (only the
    # SV_ExecuteClientMessage wrapper bumps it — in-vivo, A1b below), so the
    # direct stub calls share identity 0. Note the calc's `lastPacketIndex`
    # also starts at 0, so a constant identity 0 never counts a packet — the
    # original's initial-value quirk, preserved.
    writes = []
    for k in range(1, 41):
        t = t0 + 1 + 50 * k
        gdb.execute(f"set *(unsigned int*){uc} = {t}")
        gdb.execute(f"call ((void(*)(unsigned int, void*))({stub_addr}))({cl}, {uc})")
        writes.append((t, 0))

    print("=== A1: 40 stub calls done; calling client_command_net_status", flush=True)
    relseq_before = u32(cl + OFF_RELSEQ)
    print(f"=== A1: relseq before printer call = {relseq_before}", flush=True)
    try:
        gdb.execute(f"call ((void(*)(int))({printer_addr}))({i})")
    except gdb.error as exc:
        print(f"=== A1: PRINTER call failed: {exc}", flush=True)
        try:
            print(gdb.execute("bt 12", to_string=True), flush=True)
        except gdb.error:
            pass
        raise
    relseq_after = u32(cl + OFF_RELSEQ)
    print(f"=== A1: relseq after printer call = {relseq_after}", flush=True)

    # Read the last 4 reliable commands of client i. The engine increments
    # reliableSequence BEFORE the Q_strncpyz (sv_main.cpp:130-149), so the
    # newest command sits at [relseq & (MAX_RELIABLE_COMMANDS-1)].
    for back in (0, 1, 2, 3):
        idx = (relseq_after - back) & (MAX_RELIABLE - 1)
        addr = cl + OFF_RELCMDS + idx * MAX_STRING_CHARS
        print(f"=== A1 RAW reliable[{idx}] (relseq-{back}) @ {addr:#x}:\n"
              + gdb.execute(f"x/s {addr:#x}", to_string=True), flush=True)

    # Expected values over the exact injected sequence (mirror of
    # Proxy_Engine_Client_CalcPacketsAndFPS / UpdateUcmdStats).
    slots = [(0, 0)] * CMD_MASK
    for idx, (t, pi) in enumerate(writes):
        slots[(idx + 1) & (CMD_MASK - 1)] = (t, pi)
    last_cmd_time = slots[len(writes) & (CMD_MASK - 1)][0]
    fps = 0
    packets = 0
    last_pi = 0
    for st, pi in slots:
        if st + 1000 >= last_cmd_time:
            fps += 1
            if last_pi != pi:
                last_pi = pi
                packets += 1
    print(
        f"=== A1 EXPECTED: last_cmd_time={last_cmd_time} fps={fps} packets={packets}",
        flush=True,
    )

    # ------------------------------------------------------------------
    # A1b — in-vivo path: craft a clc_move message with the engine's own
    # MSG writers and run it through the REAL SV_ExecuteClientMessage
    # (0x804e8c4), which dispatches to SV_UserMove and must reach the Rust
    # stub through the retargeted `call` at 0x804e891. The SyscallBP /
    # PrinterBP instrumentation above plus the stub-entry count prove the
    # live retarget.
    # ------------------------------------------------------------------
    print("=== A1b: crafting clc_move message via engine MSG writers", flush=True)
    buf = int(gdb.parse_and_eval("(unsigned int)malloc(256)"))
    gdb.execute(f"call (void*)memset((void*){buf}, 0, 256)")
    msg = int(gdb.parse_and_eval("(unsigned int)malloc(32)"))
    gdb.execute(f"call (void*)memset((void*){msg}, 0, 32)")
    # msg_t: allowoverflow(0) overflowed(4) oob(8) data(12) maxsize(16)
    #        cursize(20) readcount(24) bit(28)
    gdb.execute(f"set *(unsigned int*){msg + 12} = {buf}")
    gdb.execute(f"set *(unsigned int*){msg + 16} = 256")

    WRITE_LONG = 0x0807_7ad4
    WRITE_BYTE = 0x0807_7a24
    WRITE_BITS = 0x0807_7634
    EXEC_CLIENT_MSG = 0x0804_e8c4
    OFF_LASTUSERCMD = 0x20420  # SV_ClientThink store target (R-verified)

    server_id = u32(0x0827_3ec8)
    ack_now = u32(cl + OFF_MSGACK)
    rel_ack = u32(cl + 132108)
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {server_id})")
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {ack_now})")
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {rel_ack})")
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_BYTE}))({msg}, 2)")  # clc_move
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_BYTE}))({msg}, 1)")  # cmdCount
    # one cmd: explicit serverTime (bit=0, ReadBits(32)), angles passthrough
    client_time = t0 - 5
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, 0, 1)")
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, {client_time}, 32)")
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, 0, 1)")

    bit = u32(msg + 28)
    cursize = (bit + 7) >> 3
    gdb.execute(f"set *(unsigned int*){msg + 20} = {cursize}")
    gdb.execute(f"set *(unsigned int*){msg + 24} = 0")  # readcount
    gdb.execute(f"set *(unsigned int*){msg + 28} = 0")  # bit
    print(f"=== A1b: msg built (bit={bit} cursize={cursize})", flush=True)

    # Ensure the cmd passes the stale-cmd filters.
    gdb.execute(f"set *(unsigned int*){cl + OFF_LASTUSERCMD} = {client_time - 100}")

    state["stub_hits"] = 0
    print("=== A1b: calling SV_ExecuteClientMessage (expect STUB hit)", flush=True)
    gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({EXEC_CLIENT_MSG}))({cl}, {msg})")

    print("=== A1b: calling client_command_net_status to observe live-updated stats", flush=True)
    # The 500 ms rate limit compares against svs->time, frozen while the
    # inferior is stopped (the A1 print stored 10050): advance it.
    gdb.execute(f"set *(unsigned int*){SVS_TIME} = {t0 + 1000}")
    relseq2_before = u32(cl + OFF_RELSEQ)
    gdb.execute(f"call ((void(*)(int))({printer_addr}))({i})")
    relseq2 = u32(cl + OFF_RELSEQ)
    idx = relseq2 & (MAX_RELIABLE - 1)
    addr = cl + OFF_RELCMDS + idx * MAX_STRING_CHARS
    print(f"=== A1b relseq {relseq2_before} -> {relseq2}", flush=True)
    print("=== A1b TABLE:\n" + gdb.execute(f"x/s {addr:#x}", to_string=True), flush=True)
    print(
        "=== A1b EXPECTED: fps=1 packets=1 timeNudge=(avg delay "
        f"{(client_time - t0)} + ping 0 - magic + 1000/sv_fps) * -1",
        flush=True,
    )
    print("=== A1: done", flush=True)

    gdb.execute("kill")
    gdb.execute("quit")


main()
