# A8 helper: sample, once per `GAME_RUN_FRAME`, the first two game clients'
# `level.clients[i].ps.commandTime - level.time` and `ps.ping` for
# `SAMPLE_FRAMES` frames (default 120), then detach.
#
# Run with gdb attached to linuxjampded, inside the i386 runtime image:
#   gdb -q -p "$(pgrep linuxjampded)" -x /rt/a8_ping_equalize.gdb
#
# `level` is a dynamic symbol of the original game module; `level.clients` is
# at `level+0` (`g_local.h:813`), `sizeof(gclient_t)` is 7232
# (`sdk.rs:SIZEOF_GCLIENT`), `ps.commandTime` is the first int of the first
# `gclient_t` and `ps.ping` is at 524 (`sdk.rs:OFFSET_PS_PING`, the SDK
# `offsetof`). `level.time` is at `level+32` and `level.numConnectedClients` at
# `level+72` (R-004 pins).
#
# The sample runs at the engine's `SV_SendClientMessages` (`0x8059034`,
# `sv_main.cpp:901-926` order: `SV_CalcPings` -> `GAME_RUN_FRAME` loop ->
# `SV_SendClientMessages`), i.e. *after* the proxy's post-forward `ps.ping`
# write and before the snapshot is built. Sampling at `GAME_RUN_FRAME` entry
# would read the engine's freshly recalculated `ps.ping` before the write, so it
# could not verify OFFSET_PS_PING.
python
import gdb
import os

inf = gdb.selected_inferior()
level = int(gdb.parse_and_eval('(unsigned int)&level')) & 0xffffffff
want = int(os.environ.get('SAMPLE_FRAMES', '120'))

SIZEOF_GCLIENT = 7232
OFFSET_PS_PING = 524
OFFSET_LEVEL_TIME = 32
OFFSET_LEVEL_NUM_CONNECTED = 72
# Engine side (R-018): `svs.clients` is the pointer at 0x83121ec; `cl->ping`
# is at 234532 of client_t.
SVS_CLIENTS_PTR = 0x83121EC
SIZEOF_CLIENT_T = 332920
OFFSET_CLIENT_PING = 234532


def rd32(addr):
    return int.from_bytes(bytes(inf.read_memory(addr, 4)), 'little')


samples = {0: [], 1: []}
engine_samples = {0: [], 1: []}


class SendMessagesBreakpoint(gdb.Breakpoint):
    def stop(self):
        clients = rd32(level)
        connected = rd32(level + OFFSET_LEVEL_NUM_CONNECTED)
        if clients and connected >= 2:
            now = rd32(level + OFFSET_LEVEL_TIME)
            engine_clients = rd32(SVS_CLIENTS_PTR)
            for slot in (0, 1):
                ps = clients + slot * SIZEOF_GCLIENT
                samples[slot].append((rd32(ps) - now, rd32(ps + OFFSET_PS_PING)))
                engine_samples[slot].append(
                    rd32(engine_clients + slot * SIZEOF_CLIENT_T + OFFSET_CLIENT_PING)
                )
        return len(samples[0]) >= want and len(samples[1]) >= want


def median(values):
    values = sorted(values)
    return values[len(values) // 2]


bp = SendMessagesBreakpoint('*0x8059034', internal=True)
gdb.execute('set pagination off')
gdb.execute('continue')
bp.delete()
for slot in (0, 1):
    rows = samples[slot]
    if rows:
        shifts = [r[0] for r in rows]
        pings = [r[1] for r in rows]
        print('SUMMARY client%d n=%d shift_median=%d shift_min=%d shift_max=%d ping_median=%d engine_ping_median=%d' % (
            slot, len(rows), median(shifts), min(shifts), max(shifts), median(pings),
            median(engine_samples[slot])))
    else:
        print('SUMMARY client%d no samples' % slot)
gdb.execute('detach')
gdb.execute('quit')
end
