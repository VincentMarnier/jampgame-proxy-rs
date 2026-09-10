# Differential netStatus semantic test (gdb inferior calls).
#
# Drives K crafted clc_move client messages through the REAL engine
# `SV_ExecuteClientMessage` (which dispatches to whichever proxy is mounted:
# the original C++ jampgame_proxy detour, or the Rust port), then calls that
# proxy's own netStatus printer and reads the resulting table from the
# client's reliable-command buffer.
#
# PROXY_KIND selects the printer + symbol offsets (computed by the driver
# script `diff_netstatus.sh` from the mounted .so):
#   rust:     netstatus::client_command_net_status(int)
#   original: Proxy_Engine_ClientCommand_NetStatus(int)
#
# Sequence: one message per "packet", one cmd per message, serverTime
# advancing 50 ms per cmd, `lastPacketTime` advanced per message so the Rust
# packet-boundary feed counts one packet per message (the original counts one
# per SV_UserMove by construction).
#
# Run: tools/runtime/diff_netstatus.sh rust|original

import gdb
import os
import re

CLIENTS_PTR = 0x0831_21EC
SVS_TIME = 0x0831_21E4
SV_SERVER_ID = 0x0827_3EC8

EXEC_CLIENT_MSG = 0x0804_e8c4
# Warmup anchor: SV_ClientThink (0x804e634) is NOT detoured by either proxy
# (SV_CalcPings IS detoured by the original proxy's ping fix, which would
# overwrite a breakpoint placed there).
WARMUP_ANCHOR = 0x0804_e634
WRITE_LONG = 0x0807_7ad4
WRITE_BYTE = 0x0807_7a24
WRITE_BITS = 0x0807_7634

SIZEOF_CLIENT_T = 332920
OFF_STATE = 0
OFF_PING = 234532
OFF_LASTUSERCMD = 0x20420
OFF_RELSEQ = 132104
OFF_RELCMDS = 1032
OFF_NAME = 133192
MAX_RELIABLE = 128
MAX_STRING_CHARS = 1024

K_MESSAGES = 40

kind = os.environ["DIFF_KIND"]          # rust | original
PRINTER_OFF = int(os.environ["DIFF_PRINTER_OFF"], 16)

state = {"hits": 0}


def u32(addr):
    return int(gdb.parse_and_eval(f"*(unsigned int*){addr}"))


def proxy_base():
    best = None
    for line in gdb.execute("info proc mappings", to_string=True).splitlines():
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
        if u32(cl + OFF_STATE) == 4:
            return i, cl
    return None


class Warmup(gdb.Breakpoint):
    def __init__(self):
        super().__init__(f"*{WARMUP_ANCHOR:#x}", internal=True)
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


class Tracer(gdb.Breakpoint):
    """Log-and-continue breakpoint; the first hit of the labeled tracer can
    also single-step (step_trace=True) to expose an early-return path."""

    def __init__(self, addr, label, step_trace=False):
        super().__init__(f"*{addr:#x}", internal=True)
        self.silent = True
        self.label = label
        self.count = 0
        self.step_trace = step_trace

    def stop(self):
        self.count += 1
        if self.count <= 6:
            try:
                eax = int(gdb.parse_and_eval("$eax"))
                print(f"=== DIFF: trace {self.label} hit #{self.count} eax={eax}", flush=True)
            except gdb.error:
                print(f"=== DIFF: trace {self.label} hit #{self.count}", flush=True)
        return False


def main():
    gdb.execute("set pagination off")
    gdb.execute("set confirm off")
    gdb.execute("handle SIGALRM nostop noprint pass")
    gdb.execute("handle SIGPIPE nostop noprint pass")
    gdb.execute("handle SIGVTALRM nostop noprint pass")
    gdb.execute("file ./linuxjampded")

    Warmup()
    gdb.execute(
        "run +set dedicated 1 +set net_port 29193 "
        "+set fs_cdpath /srv/jka/gamedata +set sv_pure 0 +set sv_maxclients 8 "
        "+set g_gametype 0 +set bot_minplayers 2 +set proxy_sv_enableNetStatus 1 "
        "+map mp/ffa1"
    )

    base = proxy_base()
    i, cl = find_active()
    name = gdb.execute(f"x/s {cl + OFF_NAME}", to_string=True).strip()
    print(f"=== DIFF[{kind}]: base {base:#x}, client {i} active ({name})", flush=True)
    printer = base + PRINTER_OFF

    # Debug: log the printer's received client_num at entry.
    class ArgProbe(gdb.Breakpoint):
        def __init__(self, addr, label):
            super().__init__(f"*{addr:#x}", internal=True)
            self.silent = True
            self.label = label

        def stop(self):
            esp = int(gdb.parse_and_eval("$esp"))
            arg = int(gdb.parse_and_eval(f"*(unsigned int*){esp + 4}"))
            print(f"=== DIFF: {self.label} entered, arg0={arg:#x}", flush=True)
            return False

    ArgProbe(printer, "printer")
    # Catch the panic at its source: core::panicking::panic_bounds_check —
    # cdecl args: [esp+4] = len, [esp+8] = index; backtrace 8 to see the
    # caller through the inlining.
    class BoundsProbe(gdb.Breakpoint):
        def __init__(self, addr):
            super().__init__(f"*{addr:#x}", internal=True)
            self.silent = True

        def stop(self):
            esp = int(gdb.parse_and_eval("$esp"))
            words = gdb.execute(f"x/2wx {esp:#x}", to_string=True).split(":")[1].split()
            length, index = int(words[0], 16), int(words[1], 16)
            print(f"=== DIFF: panic_bounds_check len={length} index={index:#x}", flush=True)
            try:
                bt = gdb.execute("bt 12", to_string=True)
                print("=== DIFF panic bt:\n" + bt, flush=True)
            except gdb.error:
                pass
            return True

    BoundsProbe(base + 0x4656)

    # Patch-state probe: are the engine entries detoured (E9) or pristine?
    for label, addr in [("SV_ExecuteClientMessage", 0x0804_e8c4),
                        ("SV_CalcPings", 0x0805_7204),
                        ("SV_SendClientGameState", 0x0804_cee4)]:
        raw = gdb.execute(f"x/3xb {addr:#x}", to_string=True)
        print(f"=== DIFF[{kind}]: entry {label} @ {addr:#x}: {raw.strip()}", flush=True)

    # Trace the proxy's internal feed chain (original C++ symbols only).
    if kind == "original":
        for label, addr in [("Proxy_SV_ExecuteClientMessage", base + 0x0AE10),
                            ("js_af06_bad_msgack", base + 0x0AE58),
                            ("eof_exit_af3a", base + 0x0AF3A),
                            ("relack_jg_af00", base + 0x0AE7C),
                            ("mismatch_path_aece", base + 0x0AECE),
                            ("dispatch_af2a_loop", base + 0x0AF2A),
                            ("dispatch_af7d_clc_move", base + 0x0AF7D),
                            ("Proxy_SV_UserMove", base + 0x0AAA0),
                            ("Proxy_Engine_Client_UpdateUcmdStats", base + 0x07BE0),
                            ("Proxy_Engine_Client_UpdateTimenudge", base + 0x07CB0)]:
            bp = Tracer(addr, label)
            state.setdefault("bps", []).append(bp)

    t0 = u32(SVS_TIME)
    print(f"=== DIFF[{kind}]: svs->time = {t0}", flush=True)
    gdb.execute(f"set *(unsigned int*){cl + OFF_PING} = 20")

    buf = int(gdb.parse_and_eval("(unsigned int)malloc(256)"))
    gdb.execute(f"call (void*)memset((void*){buf}, 0, 256)")
    msg = int(gdb.parse_and_eval("(unsigned int)malloc(32)"))
    gdb.execute(f"call (void*)memset((void*){msg}, 0, 32)")
    gdb.execute(f"set *(unsigned int*){msg + 12} = {buf}")
    gdb.execute(f"set *(unsigned int*){msg + 16} = 256")

    off_lpt = 133392
    writes = []
    server_id = u32(SV_SERVER_ID)
    for k in range(1, K_MESSAGES + 1):
        t = t0 + 1 + 50 * k
        gdb.execute(f"call (void*)memset((void*){buf}, 0, 256)")
        gdb.execute(f"set *(unsigned int*){msg + 20} = 0")   # cursize
        gdb.execute(f"set *(unsigned int*){msg + 24} = 0")   # readcount
        gdb.execute(f"set *(unsigned int*){msg + 28} = 0")   # bit
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {server_id})")
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {u32(cl + 132116)})")
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_LONG}))({msg}, {u32(cl + 132108)})")
        if k == 1:
            print(f"=== DIFF[{kind}]: bit after 3 WriteLongs = {u32(msg + 28)}", flush=True)
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_BYTE}))({msg}, 2)")  # clc_move
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({WRITE_BYTE}))({msg}, 1)")  # cmdCount
        if k == 1:
            print(f"=== DIFF[{kind}]: bit after 2 WriteBytes = {u32(msg + 28)}", flush=True)
        # one cmd: explicit serverTime (bit=0 + 32 bits), angles passthrough
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, 0, 1)")
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, {t}, 32)")
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int, unsigned int))({WRITE_BITS}))({msg}, 0, 1)")
        bit = u32(msg + 28)
        gdb.execute(f"set *(unsigned int*){msg + 20} = {(bit + 7) >> 3}")
        if k == 1:
            raw = gdb.execute(f"x/{(bit + 7) >> 3}xb {buf:#x}", to_string=True)
            print(f"=== DIFF[{kind}]: msg#1 bytes: {raw.strip()}", flush=True)
            print(
                f"=== DIFF[{kind}]: sv.serverId={u32(SV_SERVER_ID):#x} "
                f"restarted={u32(0x0827_3ECC):#x} ack={u32(cl + 132116)} "
                f"relAck={u32(cl + 132108)} relSeq={u32(cl + 132104)}",
                flush=True,
            )
        # one packet per message for both proxies' identity schemes
        gdb.execute(f"set *(unsigned int*){cl + 133392} = {t0 + 1000 + k}")
        gdb.execute(f"set *(unsigned int*){cl + OFF_LASTUSERCMD} = {t - 100}")
        gdb.execute(f"call ((void(*)(unsigned int, unsigned int))({EXEC_CLIENT_MSG}))({cl}, {msg})")
        writes.append(t)

    print(f"=== DIFF[{kind}]: {K_MESSAGES} messages done; calling netStatus printer", flush=True)
    gdb.execute(f"set *(unsigned int*){SVS_TIME} = {t0 + 5000}")
    # A real client always has snapshotMsec > 0 (the original proxy divides by
    # it unguarded once ping >= 1 — SIGFPE otherwise, e.g. with a bot).
    gdb.execute(f"set *(unsigned int*){cl + 234540} = 40")
    relseq = u32(cl + OFF_RELSEQ)
    # Probe the cvar slot the printer's sv_maxclients() reads.
    slot = u32(0x0827_3EA4)
    print(
        f"=== DIFF[{kind}]: sv_maxclients slot ptr={slot:#x} "
        f"integer@+32={u32(slot + 32) if slot else 'null'} "
        f"fpsslot={u32(0x0827_3E84):#x}",
        flush=True,
    )
    gdb.execute(f"call ((void(*)(int))({printer}))({i})")
    relseq_after = u32(cl + OFF_RELSEQ)
    # SV_SendServerCommand increments reliableSequence BEFORE the write.
    idx = relseq_after & (MAX_RELIABLE - 1)
    addr = cl + OFF_RELCMDS + idx * MAX_STRING_CHARS
    print(f"=== DIFF[{kind}] TABLE (relseq {relseq} -> {relseq_after}):\n"
          + gdb.execute(f"x/s {addr:#x}", to_string=True), flush=True)
    print(
        f"=== DIFF[{kind}] EXPECTED: last_cmd_time={writes[-1]} "
        f"fps={sum(1 for t in writes if t + 1000 >= writes[-1])} "
        f"packets={sum(1 for t in writes if t + 1000 >= writes[-1])}",
        flush=True,
    )

    gdb.execute("kill")
    gdb.execute("quit")


main()
