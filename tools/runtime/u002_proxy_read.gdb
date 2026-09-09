# U-002 exact proxy experiment: read the proxy's own bookkeeping globals at
# runtime and compare them against /proc maps + nm expectations.
#   proxy.jampgameHandle / jampgameAddress / originalVmMain / originalDllEntry /
#   originalSystemCall   (Proxy_t layout: offsets 0,4,8,12,16)
#   server.svs / server.sv  (ProxyServer_t layout: offsets 0,4)
python
import gdb

def rd32(a):
    return int.from_bytes(bytes(gdb.selected_inferior().read_memory(a, 4)), 'little')
def rdptr(a):
    v = rd32(a)
    return v

inf = gdb.selected_inferior()

# resolve module bases from /proc/1/maps
maps = open('/proc/1/maps').read()
proxy_base = None
orig_base = None
for line in maps.splitlines():
    if line.endswith('/srv/jka/base/jampgamei386.so') and 'r-xp' in line:
        proxy_base = int(line.split('-')[0], 16)
    if line.endswith('/srv/jka/base/jampgame_original.so') and 'r-xp' in line:
        orig_base = int(line.split('-')[0], 16)
print('proxy  module r-xp base = 0x%08x' % proxy_base)
print('orig   module r-xp base = 0x%08x' % orig_base)

# symbol offsets from nm of the built proxy (not stripped)
sym = {
    'proxy':   0x000741e0,
    'server':  0x000b9b60,
    'jampgame':0x000b9b20,
}
proxy_addr = proxy_base + sym['proxy']
server_addr = proxy_base + sym['server']

print()
print('=== proxy (Proxy_t @0x%08x) ===' % proxy_addr)
print('jampgameHandle      @+0x00 = 0x%08x' % rdptr(proxy_addr+0x00))
print('jampgameAddress     @+0x04 = 0x%08x   (orig base 0x%08x, dladdr dli_fbase)'
      % (rdptr(proxy_addr+0x04), orig_base))
va = rdptr(proxy_addr+0x04)
print('  -> diff vs /proc orig base: %s' % ('MATCH' if va == orig_base else 'MISMATCH'))
print('originalVmMain      @+0x08 = 0x%08x   (expect orig_base+0x85684 = 0x%08x)'
      % (rdptr(proxy_addr+0x08), orig_base + 0x85684))
print('originalDllEntry    @+0x0c = 0x%08x   (expect orig_base+0x15e104 = 0x%08x)'
      % (rdptr(proxy_addr+0x0c), orig_base + 0x15e104))
print('originalSystemCall  @+0x10 = 0x%08x' % rdptr(proxy_addr+0x10))
print('originalVmMainResp  @+0x14 = 0x%08x' % rd32(proxy_addr+0x14))
lgd = proxy_addr + 0x1c  # LocatedGameData_t after ProxyData_t(int) @0x18
print('  locatedGameData.g_entities  = 0x%08x' % rdptr(lgd+0))
print('  locatedGameData.g_entitySize= %d' % rd32(lgd+4))
print('  locatedGameData.num_entities= %d' % rd32(lgd+8))
print('  locatedGameData.g_clients   = 0x%08x' % rdptr(lgd+12))
print('  locatedGameData.g_clientSize= %d' % rd32(lgd+16))

print()
print('=== server (ProxyServer_t @0x%08x) ===' % server_addr)
svs = rdptr(server_addr+0)
sv  = rdptr(server_addr+4)
print('server.svs @+0x00 = 0x%08x (expect 0x083121e0)' % svs)
print('server.sv  @+0x04 = 0x%08x (expect 0x08273ec0)' % sv)
print('  svs->initialized = %d ; svs->time = %d ; svs->clients = 0x%08x'
      % (rd32(svs+0), rd32(svs+4), rdptr(svs+12)))
print('  sv->state = %d' % rd32(sv+0))
end
detach
quit
