# A3 helper: sample `level.clients[0].ps.commandTime - level.time` once per
# `GAME_RUN_FRAME` for `SAMPLE_FRAMES` frames (default 120), then detach.
#
# Run with gdb attached to linuxjampded, inside the i386 runtime image:
#   gdb -q -p "$(pgrep linuxjampded)" -x /rt/a3_timenudge_void.gdb
#
# `level` and `g_entities` are dynamic symbols of the original game module;
# `level.clients` is at `level+0` (`g_local.h:813`) and `ps.commandTime` is the
# first int of the first `gclient_t` (`g_local.h:533-535`). `level.time` is at
# `level+32` (R-004 pin). The engine enters the game through `vmMain` at
# `orig_base+0x85684`; the breakpoint filters on `command == GAME_RUN_FRAME`
# (= 8, `g_public.h:756`).
python
import gdb
import os

inf = gdb.selected_inferior()
pid = inf.pid
base = None
for line in open('/proc/%d/maps' % pid).read().splitlines():
    if line.rstrip().endswith('jampgame_original.so'):
        start = int(line.split('-')[0], 16)
        if base is None or start < base:
            base = start
if base is None:
    raise SystemExit('jampgame_original.so is not loaded')
level = int(gdb.parse_and_eval('(unsigned int)&level')) & 0xffffffff
want = int(os.environ.get('SAMPLE_FRAMES', '120'))


def rd32(addr):
    return int.from_bytes(bytes(inf.read_memory(addr, 4)), 'little')


samples = []


class RunFrameBreakpoint(gdb.Breakpoint):
    def stop(self):
        try:
            command = int(gdb.parse_and_eval('*(int*)($esp+4)'))
        except Exception:
            return False
        if command == 8:  # GAME_RUN_FRAME
            clients = rd32(level)
            if clients:
                samples.append(rd32(clients) - rd32(level + 32))
        return len(samples) >= want


bp = RunFrameBreakpoint('*0x%x' % (base + 0x85684), internal=True)
gdb.execute('set pagination off')
gdb.execute('continue')
bp.delete()
samples.sort()
if samples:
    print('SUMMARY n=%d mean=%0.2f median=%d min=%d max=%d' % (
        len(samples), sum(samples) / len(samples), samples[len(samples) // 2],
        samples[0], samples[-1]))
else:
    print('SUMMARY no samples')
gdb.execute('detach')
gdb.execute('quit')
end
