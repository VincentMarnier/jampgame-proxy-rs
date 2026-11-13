# A9 helper: sample ONE game client's `level.clients[slot].ps.commandTime -
# level.time`, `ps.ping` and the engine's `cl->ping` once per
# `SV_SendClientMessages`, then detach.
#
# Used by the D-028 real-client validation: the two synthetic anchor clients
# occupy slots 0 and 1, the user's real client is slot 2 (`SLOT` overrides).
# Sampling at `SV_SendClientMessages` (0x8059034) is after the proxy's
# post-forward `ps.ping` write and after `SV_CalcPings`, so it sees exactly what
# the next snapshot carries.
#
#   gdb -q -p "$(pgrep linuxjampded)" -x /rt/a9_ping_equalize_real.gdb
#
# Env: SLOT (default 2), SAMPLE_FRAMES (default 240).
python
import gdb
import os

inf = gdb.selected_inferior()
level = int(gdb.parse_and_eval('(unsigned int)&level')) & 0xffffffff
slot = int(os.environ.get('SLOT', '2'))
want = int(os.environ.get('SAMPLE_FRAMES', '240'))

SIZEOF_GCLIENT = 7232
OFFSET_PS_PING = 524
OFFSET_LEVEL_TIME = 32
# Engine side (R-018): `svs.clients` pointer and `client_t` fields.
SVS_CLIENTS_PTR = 0x83121EC
SIZEOF_CLIENT_T = 332920
OFFSET_CLIENT_STATE = 0
OFFSET_CLIENT_PING = 234532
OFFSET_CLIENT_LAST_USERCMD = 132128
CS_ACTIVE = 4


def rd32(addr):
    return int.from_bytes(bytes(inf.read_memory(addr, 4)), 'little')


shifts = []
pings = []
engine_pings = []
newest = []
gaps = []


class SendMessagesBreakpoint(gdb.Breakpoint):
    def stop(self):
        clients = rd32(level)
        engine_clients = rd32(SVS_CLIENTS_PTR)
        if clients and engine_clients:
            engine_cl = engine_clients + slot * SIZEOF_CLIENT_T
            if rd32(engine_cl + OFFSET_CLIENT_STATE) == CS_ACTIVE:
                now = rd32(level + OFFSET_LEVEL_TIME)
                ps = clients + slot * SIZEOF_GCLIENT
                cmd = rd32(ps)
                last = rd32(engine_cl + OFFSET_CLIENT_LAST_USERCMD)
                shifts.append(cmd - now)
                pings.append(rd32(ps + OFFSET_PS_PING))
                engine_pings.append(rd32(engine_cl + OFFSET_CLIENT_PING))
                newest.append(last - now)
                gaps.append(last - cmd)
        return len(shifts) >= want


def median(values):
    values = sorted(values)
    return values[len(values) // 2]


bp = SendMessagesBreakpoint('*0x8059034', internal=True)
gdb.execute('set pagination off')
gdb.execute('continue')
bp.delete()
if shifts:
    print('SUMMARY client%d n=%d shift_median=%d shift_min=%d shift_max=%d '
          'ping_median=%d engine_ping_median=%d newest_median=%d gap_median=%d '
          'gap_min=%d gap_max=%d' % (
              slot, len(shifts), median(shifts), min(shifts), max(shifts),
              median(pings), median(engine_pings), median(newest),
              median(gaps), min(gaps), max(gaps)))
else:
    print('SUMMARY client%d no samples (not CS_ACTIVE?)' % slot)
gdb.execute('detach')
gdb.execute('quit')
end
