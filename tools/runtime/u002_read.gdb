# U-002 runtime read-back of engine var/cvar slots.
# Attach to the running linuxjampded (PID 1 in the container) and dump the
# exact addresses the proxy dereferences in Proxy_Engine_Initialize_MemoryLayer
# (Proxy_Engine_Wrappers.hpp lines 135-173).  Expectation values come from
# engine source (original/jedi-academy/codemp/): qcommon.h, server/server.h.

set pagination off
set confirm off

python
import gdb

inf = gdb.selected_inferior()
def rd32(a):
    b = inf.read_memory(a, 4)
    return int.from_bytes(b, 'little')
def cstr(p, maxlen=256):
    if p == 0:
        return b'(null)'
    try:
        b = bytes(inf.read_memory(p, maxlen))
    except gdb.error as e:
        return ('(unreadable %s)' % e).encode()
    z = b.find(b'\0')
    return b[:z] if z >= 0 else b

print('=== engine var slots ===')
slots = {
    'svs            ': 0x83121e0,
    'svs.clients    ': 0x83121ec,
    'sv             ': 0x8273ec0,
    'rd_buffer      ': 0x81e90c0,
    'rd_buffersize  ': 0x81e90c4,
    'rd_flush       ': 0x81e90c8,
    'logfile        ': 0x831f24c,
    'cmd_argc       ': 0x8260e20,
    'cmd_argv       ': 0x8260e40,
    'cmd_tokenized  ': 0x8264440,
}
for name, a in slots.items():
    try:
        print('%s 0x%08x = 0x%08x' % (name, a, rd32(a)))
    except gdb.error as e:
        print('%s 0x%08x = READ ERROR: %s' % (name, a, e))

print()
print('=== svs (serverStatic_t @0x83121e0) fields ===')
try:
    a = 0x83121e0
    print('initialized      @+0x00 = %d' % rd32(a))
    print('time             @+0x04 = %d' % rd32(a+4))
    print('snapFlagServerBit@+0x08 = %d' % rd32(a+8))
    print('clients          @+0x0c = 0x%08x' % rd32(a+12))
    print('numSnapshotEnts  @+0x10 = %d' % rd32(a+16))
    print('nextSnapshotEnts @+0x14 = %d' % rd32(a+20))
    print('snapshotEntities @+0x18 = 0x%08x' % rd32(a+24))
    print('nextHeartbeatTime@+0x1c = %d' % rd32(a+28))
except gdb.error as e:
    print('svs read error: %s' % e)

print()
print('=== sv (server_t @0x8273ec0) first fields ===')
try:
    a = 0x8273ec0
    print('state        @+0x00 = %d (SS_GAME=2 expected)' % rd32(a))
    print('restarting   @+0x04 = %d' % rd32(a+4))
    print('serverId     @+0x08 = %d' % rd32(a+8))
    print('checksumFeed @+0x10 = %d' % rd32(a+16))
    print('timeResidual @+0x18 = %d' % rd32(a+24))
    print('nextFrameTime@+0x1c = %d' % rd32(a+28))
except gdb.error as e:
    print('sv read error: %s' % e)

print()
print('=== svs.clients[0] (client_t) sanity ===')
try:
    cl = rd32(0x83121ec)
    print('svs.clients array ptr = 0x%08x' % cl)
    if cl:
        # client_t fields (engine server.h): clientState state @0,
        # netchan etc. Only check a few well-known-ish offsets heuristically.
        for off, lab in ((0, 'state(CS_*=int)'), (4, 'lastClientCommand'),
                         (8, 'gentity?/next'), (12, 'userinfo[?]')):
            print('  [0x%02x] %-20s = 0x%08x' % (off, lab, rd32(cl+off)))
except gdb.error as e:
    print('clients read error: %s' % e)

print()
print('=== cvar pointer slots (deref once -> cvar_t) ===')
cvars = {
    'sv_fps':          0x8273e84,
    'sv_gametype':     0x83121cc,
    'sv_hostname':     0x8273e9c,
    'sv_mapname':      0x8273e90,
    'sv_maxclients':   0x8273ea4,
    'sv_privateClients':0x8273e80,
    'sv_pure':         0x83121a8,
    'sv_maxRate':      0x831219c,
    'sv_rconPassword': 0x83121d4,
    'sv_floodProtect': 0x8273e88,
    'sv_allowDownload':0x83121b0,
    'com_dedicated':   0x831f254,
    'com_sv_running':  0x831f300,
    'com_cl_running':  0x831f400,
    'com_logfile':     0x831f41c,
    'com_developer':   0x831e204,
    'fs_gamedirvar':   0x838aaec,
}
for label, slot in cvars.items():
    try:
        p = rd32(slot)
        if p == 0:
            print('%18s slot@0x%08x -> NULL' % (label, slot))
            continue
        name = cstr(rd32(p)).decode(errors='replace')
        string = cstr(rd32(p+4)).decode(errors='replace')
        integer = rd32(p+32)
        flags = rd32(p+16)
        print('%18s slot@0x%08x -> cvar_t@0x%08x name=%r string=%r integer=%d flags=0x%x'
              % (label, slot, p, name, string, integer, flags))
    except gdb.error as e:
        print('%18s slot@0x%08x READ ERROR: %s' % (label, slot, e))
end

detach
quit
