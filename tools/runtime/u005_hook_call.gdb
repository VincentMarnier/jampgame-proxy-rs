# U-005 live hook-call evidence: break inside the proxy's own
# SV_ConnectionlessPacket hook (0xbbb0 in the proxy .so) and confirm it fires
# on a real query, then detach.  Backtrace shows who called the hook.

set pagination off
set confirm off

python
import gdb
maps = open('/proc/1/maps').read()
for line in maps.splitlines():
    if line.endswith('/srv/jka/base/jampgamei386.so') and 'r-xp' in line:
        base = int(line.split('-')[0], 16)
        break
print('proxy base 0x%08x; hook at 0x%08x' % (base, base + 0xbbb0))
gdb.set_convenience_variable('hook', base + 0xbbb0)
end

break *$hook
commands
  silent
  printf "\n=== HIT Proxy_SV_ConnectionlessPacket hook ===\n"
  bt 6
  detach
  quit
end

continue
