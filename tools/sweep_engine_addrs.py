#!/usr/bin/env python3
"""U-005 static sweep: verify every hard-coded engine address in
Proxy_Engine_Wrappers.hpp against original/jalinuxded_1.011/linuxjampded.

Method (cheapest capable tool: objdump, NOT Ghidra/GDB):
  - NOTE: plain `objdump -d` does NOT disassemble this binary's .text
    (stripped Intel-C++ binary; -d prints raw byte dumps). Use `objdump -D
    -j .text`, which yields true per-instruction disassembly.
  - For each func_/call_ address: accumulate whole-instruction lengths from
    the target address until >= 5 bytes (minimum steal for an E9 rel32
    detour, cf. HookUtils::GetLen). Report first byte + steal length.
  - For each var_/cvar_ address: assert it lies in the engine RW LOAD segment.
  - Re-verify the NOP range, the single-byte patch context, and the call-site
    rel32 target.

Run from repo root:  python3 tools/sweep_engine_addrs.py
Exit 0 = all checks pass.
"""
import re
import subprocess
import sys

HEADER = "original/jampgame-proxy/src/jampgame_proxy/RuntimePatch/Engine/Proxy_Engine_Wrappers.hpp"
# The proxy source was removed from the workspace (2026-09-10); fall back to
# the upstream repo (offline alternative: the retained submodule gitdir
# `.git/modules/original/jampgame-proxy`) when the local file is absent.
UPSTREAM = "https://github.com/VincentMarnier/jampgame_proxy"
PIN = "687997412ea6e5ead93f5c7b25db552590a2eeb1"
DED = "original/jalinuxded_1.011/linuxjampded"

TEXT_START, TEXT_END = 0x0804A1E0, 0x08199494
RW_START, RW_END = 0x0819A4E0, 0x08396BB4

import pathlib
try:
    src = pathlib.Path(HEADER).read_text()
    header_source = f"workspace copy: {HEADER}"
except FileNotFoundError:
    tmp = pathlib.Path("/tmp/opencode/proxy-src-sweep")
    if not tmp.joinpath(".git").exists():
        subprocess.run(["git", "clone", "-q", UPSTREAM, str(tmp)], check=True)
        subprocess.run(["git", "-C", str(tmp), "checkout", "-q", PIN], check=True)
    out = subprocess.run(
        ["git", "-C", str(tmp), "show",
         PIN + ":" + HEADER.removeprefix("original/jampgame-proxy/")],
        capture_output=True, text=True, check=True)
    src = out.stdout
    header_source = f"upstream clone: {UPSTREAM} @ {PIN[:7]}"

addrs = dict(re.findall(r"constexpr\s+intptr_t\s+(\w+)\s*=\s*(0x[0-9a-fA-F]+)", src))
addrs = {k: int(v, 16) for k, v in addrs.items()}
nop_len = int(re.search(r"func_SVC_RemoteCommand_timer_NOP_amount\s*=\s*(\d+)", src).group(1))

func_names = sorted(n for n in addrs if n.startswith("func_") or n.startswith("call_"))
# The two inline-patch addresses point mid-instruction by design; they get
# byte-value checks below, not steal-length checks.
PATCH_NAMES = {
    "func_SVC_RemoteCommand_timer_start_block_addr",
    "func_SVC_RemoteCommand_Com_BeginRedirect_SV_OUTPUTBUF_LENGTH_addr",
}
var_names = sorted(n for n in addrs if n.startswith(("var_", "cvar_")))
print(f"Header holds {len(addrs)} addresses ({len(func_names)} func/call) [{header_source}].")


def disasm(addr, window=48):
    out = subprocess.run(
        ["objdump", "-D", "-j", ".text",
         f"--start-address={addr:#x}", f"--stop-address={addr + window:#x}", DED],
        capture_output=True, text=True)
    return out.stdout


line_re = re.compile(r"^\s*([0-9a-f]+):\s*((?:[0-9a-f]{2} )+)\s*(.*)$")
failures = []
for name in func_names:
    addr = addrs[name]
    if name.startswith("call_") or name in PATCH_NAMES:
        continue
    if not (TEXT_START <= addr < TEXT_END):
        failures.append((name, "OUTSIDE .text"))
        continue
    total, first_byte, mnems = 0, None, []
    for line in disasm(addr).splitlines():
        m = line_re.match(line)
        if not m:
            continue
        if int(m.group(1), 16) < addr:
            continue  # label/overlap line below target; skip
        bs = m.group(2).strip().split()
        if first_byte is None:
            first_byte = bs[0]
        total += len(bs)
        mnems.append(m.group(3).strip())
        if total >= 5:
            break
    ok = total >= 5
    if not ok:
        failures.append((name, f"only {total} stealable bytes"))
    print(f"{name}={addr:#x} first=0x{first_byte} steal={total} "
          f"{'OK' if ok else 'SHORT'}  [{' ; '.join(mnems[:3])}]")

print()
print("--- vars/cvars: RW LOAD range ---")
for name in var_names:
    addr = addrs[name]
    ok = RW_START <= addr < RW_END
    if not ok:
        failures.append((name, "OUTSIDE RW LOAD"))
    print(f"{name}={addr:#x} {'OK' if ok else 'OUTSIDE RW LOAD!'}")

def read_byte(addr):
    out = subprocess.run(
        ["objdump", "-s", f"--start-address={addr:#x}",
         f"--stop-address={addr + 1:#x}", DED],
        capture_output=True, text=True).stdout
    for line in out.splitlines():
        m = re.match(r"^\s*([0-9a-f]+)\s+((?:[0-9a-f]{2,} ?)+)", line)
        if m:
            row = int(m.group(1), 16)
            blob = "".join(m.group(2).split())
            off = addr - row
            hx = blob[off * 2:off * 2 + 2]
            return int(hx, 16) if hx else None
    return None


def insns_from(addr, count=12, window=64):
    """Yield (addr, size, text) instructions from a true disassembly."""
    asm = disasm(addr, window)
    out = []
    for line in asm.splitlines():
        m = line_re.match(line)
        if not m or int(m.group(1), 16) < addr:
            continue
        out.append((int(m.group(1), 16), len(m.group(2).strip().split()),
                    m.group(3).strip()))
        if len(out) >= count:
            break
    return out


print()
print("--- inline patches (byte-value + tiling checks) ---")
nop_addr = addrs["func_SVC_RemoteCommand_timer_start_block_addr"]
b = read_byte(nop_addr)
print(f"NOP start {nop_addr:#x}: byte=0x{b:02x} (want 0xe8 call) "
      f"{'OK' if b == 0xE8 else 'FAIL'}")
if b != 0xE8:
    failures.append(("NOP start byte", "MISMATCH"))
# The 25 NOP bytes must tile whole instructions exactly.
ins = insns_from(nop_addr)
tiled, cov = [], 0
for a, s, t in ins:
    if a < nop_addr + nop_len:
        tiled.append((a, s, t))
        cov += s
    else:
        break
tiling_ok = (cov == nop_len)
print(f"NOP tiling over {nop_len} bytes: covers {cov} "
      f"{'EXACT — OK' if tiling_ok else 'MISALIGNED — FAIL'}")
for a, s, t in tiled:
    print(f"    {a:#x} (+{s}) {t}")
if not tiling_ok:
    failures.append(("NOP tiling", "MISALIGNED"))
if not (TEXT_START <= nop_addr and nop_addr + nop_len < TEXT_END):
    failures.append(("NOP range", "OUTSIDE .text"))
patch_addr = addrs["func_SVC_RemoteCommand_Com_BeginRedirect_SV_OUTPUTBUF_LENGTH_addr"]
b = read_byte(patch_addr)
print(f"byte-patch {patch_addr:#x}: byte=0x{b:02x} (want 0xbf: imm32 0xbff0=49136 -> 0x03f0=1008) "
      f"{'OK' if b == 0xBF else 'FAIL'}")
if b != 0xBF:
    failures.append(("byte-patch value", "MISMATCH"))
call_addr = addrs["call_SV_AddEntToSnapshot_For_Players_addr"]
asm = disasm(call_addr, 16)
m = re.search(r"^\s*[0-9a-f]+:\s*e8(?:\s+[0-9a-f]{2})+\s*call\s+([0-9a-f]+)",
              asm, re.M)
target = int(m.group(1), 16) if m else None
want = addrs["func_SV_AddEntToSnapshot_addr"]
print(f"call-site {call_addr:#x}: E8 call -> {target:#x} "
      f"(want {want:#x}) {'OK' if target == want else 'FAIL'}")
if target != want:
    failures.append(("call-site target", "MISMATCH"))

print()
if failures:
    print(f"FAILURES ({len(failures)}):")
    for n, s in failures:
        print(f"  {n}: {s}")
    sys.exit(1)
print("SWEEP PASS.")
