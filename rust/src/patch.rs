//! Detour / inline-patch machinery — the Rust port of
//! `RuntimePatch/HookUtils/HookUtils.{hpp,cpp}` (Deathspike/Yberion).
//!
//! Two primitives:
//!
//! - **Detour** (`attach`/`detach`): overwrite ≥5 whole instruction bytes at a
//!   function entry with `E9 rel32` to a proxy wrapper, and build a *trampoline*
//!   (`mmap(RWX)`) that replays the stolen bytes then jumps back into the
//!   original function. The wrapper runs its logic and calls the trampoline
//!   (the "original"). The original proxy uses this for every entry wrapper.
//! - **Inline patches**: `patch_byte`, `nop_bytes`, `retarget_call` — byte /
//!   NOP writes and `E8/E9 rel32` operand retargeting inside an original body
//!   (the rcon NOP/byte patches, the `ipAuthorize` call removal, the
//!   `Com_Printf` vsnprintf retarget, the RMG branch force).
//!
//! Steal lengths come from `steal_len`, a whole-instruction length decoder for
//! the i386 legacy instruction forms these prologues use. Every one of the 19
//! real detour sites starts `55 8b ec` (`push %ebp; mov %esp,%ebp`) and the
//! sweep (`tools/sweep_engine_addrs.py`) proves 6 or 9 whole bytes are
//! stealable; the decoder is regression-pinned against the exact byte
//! sequences of those sites in the `tests` module (R-002, R-012, U-008).

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

// ---------------------------------------------------------------------------
// OS primitives (declared directly; the crate has no libc dependency)
// ---------------------------------------------------------------------------

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const PROT_EXEC: c_int = 4;
const MAP_PRIVATE: c_int = 0x02;
const MAP_ANONYMOUS: c_int = 0x20;

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: usize,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
    fn mprotect(addr: *mut c_void, length: usize, prot: c_int) -> c_int;
    fn getpagesize() -> c_int;
    fn memset(dst: *mut c_void, c: c_int, n: usize) -> *mut c_void;
}

/// `MAP_FAILED`, the mmap error sentinel on i386.
const MAP_FAILED: usize = usize::MAX;

/// `mmap(RWX|PRIVATE|ANON)` — the original proxy's `AllocateMemory`. Returns
/// the base address or 0 on failure.
fn alloc_trampoline(len: usize) -> usize {
    // SAFETY: kernel call with an anonymous, no-fd mapping; any returned
    // pointer is either valid or MAP_FAILED (checked below).
    let p = unsafe {
        mmap(
            core::ptr::null_mut(),
            len + 5,
            PROT_READ | PROT_WRITE | PROT_EXEC,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    let addr = p as usize;
    if addr == MAP_FAILED { 0 } else { addr }
}

/// `munmap` of an allocated trampoline.
fn release_trampoline(addr: usize, len: usize) -> bool {
    // SAFETY: addr/len come from `alloc_trampoline`.
    unsafe { munmap(addr as *mut c_void, len + 5) == 0 }
}

/// `UnProtect`: flip the page(s) covering `[addr, addr+len)` to RWE.
///
/// # Safety
///
/// `addr` must be a readable engine/game mapping; the page must be mappable.
unsafe fn unprotect(addr: usize, len: usize) {
    // SAFETY: page size is a constant of the running kernel.
    let page = unsafe { getpagesize() } as usize;
    let p1 = addr & !(page - 1);
    let p2 = (addr + len) & !(page - 1);
    // SAFETY: p1/p2 are page-aligned within the mapped region (caller contract).
    unsafe {
        mprotect(p1 as *mut c_void, page, PROT_READ | PROT_WRITE | PROT_EXEC);
    }
    if p1 != p2 {
        unsafe {
            mprotect(p2 as *mut c_void, page, PROT_READ | PROT_WRITE | PROT_EXEC);
        }
    }
}

/// `ReProtect`: flip the page(s) back to RE.
///
/// # Safety
///
/// Must be called on the same range as `unprotect`.
unsafe fn reprotect(addr: usize, len: usize) {
    // SAFETY: page size is a constant of the running kernel.
    let page = unsafe { getpagesize() } as usize;
    let p1 = addr & !(page - 1);
    let p2 = (addr + len) & !(page - 1);
    // SAFETY: same page-range contract as `unprotect`.
    unsafe {
        mprotect(p1 as *mut c_void, page, PROT_READ | PROT_EXEC);
    }
    if p1 != p2 {
        unsafe {
            mprotect(p2 as *mut c_void, page, PROT_READ | PROT_EXEC);
        }
    }
}

// ---------------------------------------------------------------------------
// i386 length decoder (LEGACY_32 / STACK_32)
// ---------------------------------------------------------------------------

/// Decode the length (in bytes) of the instruction starting at `bytes[0..]`.
/// Returns 0 if the instruction cannot be decoded safely (caller treats that
/// as "not a valid detour site").
pub fn instr_len(bytes: &[u8]) -> usize {
    let mut i = 0usize;
    loop {
        let Some(&b) = bytes.get(i) else {
            return 0;
        };
        match b {
            // Prefixes: repeat / segment / operand-size / address-size.
            0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3 | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => {
                i += 1;
            }
            // 0F two-byte opcode.
            0x0F => {
                let Some(&op) = bytes.get(i + 1) else {
                    return 0;
                };
                // Jcc rel32 (0F 80..0F 8F): 2 + 4 = 6 bytes, no ModRM.
                if (0x80..=0x8F).contains(&op) {
                    return i + 6;
                }
                // Everything else that needs a ModRM byte.
                if !modrm_required_2(op) {
                    return 0; // unknown 0F form; refuse to guess
                }
                let Some(m) = modrm_len(&bytes[i + 2..]) else {
                    return 0;
                };
                let imm = imm_len_0f(op);
                return i + 2 + m + imm;
            }
            // Relative branches: rel8 (2 bytes), rel32 (5 bytes).
            0xE8 | 0xE9 => return i + 5,
            0xEB | 0x70..=0x7F => return i + 2,
            // mov al/eax, moffs32 / mov moffs32, al/eax (5 bytes, no ModRM).
            0xA0..=0xA3 => return i + 5,
            // One-byte opcodes that never take a ModRM/immediate.
            0x06
            | 0x07
            | 0x0E
            | 0x16
            | 0x17
            | 0x1E
            | 0x1F
            | 0x27
            | 0x2F
            | 0x37
            | 0x3F
            | 0x40..=0x5F
            | 0x60
            | 0x61
            | 0x90..=0x9F
            | 0xC3
            | 0xC9
            | 0xCB
            | 0xCC
            | 0xCE
            | 0xCF
            | 0xD7
            | 0xE3
            | 0xF1
            | 0xF4
            | 0xF5
            | 0xF9
            | 0xFB
            | 0xFC
            | 0xFD => {
                return i + 1;
            }
            // push/pop r/m32 / other ModRM one-byte ops.
            _ => {
                if !modrm_required(b) {
                    return 0; // unknown opcode; refuse to guess
                }
                let Some(m) = modrm_len(&bytes[i + 1..]) else {
                    return 0;
                };
                let imm = imm_len(b);
                return i + 1 + m + imm;
            }
        }
    }
}

/// Whether one-byte opcode `op` requires a ModRM byte.
fn modrm_required(op: u8) -> bool {
    matches!(
        op,
        0x00..=0x05 | 0x08..=0x0D | 0x10..=0x15 | 0x18..=0x1D | 0x20..=0x25 | 0x28..=0x2D
            | 0x30..=0x35 | 0x38..=0x3D | 0x62 | 0x63 | 0x68 | 0x69 | 0x6A..=0x6F | 0x80..=0x83
            | 0x84..=0x8F | 0xA8..=0xAF | 0xC0..=0xC2 | 0xC4..=0xC8 | 0xCA | 0xD0..=0xD3
            | 0xD8..=0xDF | 0xE0..=0xE2 | 0xF6 | 0xF7 | 0xFE | 0xFF
    )
}

/// Whether a 0F two-byte opcode requires a ModRM byte.
fn modrm_required_2(op: u8) -> bool {
    // 0F xx with ModRM: 90-9F (setcc), A0-A3 (push/pop fs/gs - no modrm but
    // rare), A8-AF (test), B0-B7 (cmpxchg), B8-BF (mov), C0-C7 (xadd/xchg),
    // BA (bt imm8), and the FPU/MMX families. Be permissive: most 0F opcodes
    // take a ModRM byte.
    !matches!(
        op,
        0x04 | 0x05 | 0x0B | 0x31 | 0x33 | 0x34 | 0x35 | 0x3F | 0x77 | 0x7D | 0x7F | 0x80..=0x8F
    )
}

/// Immediate size (bytes) following the ModRM byte, per one-byte opcode.
fn imm_len(op: u8) -> usize {
    match op {
        0x04
        | 0x0C
        | 0x14
        | 0x1C
        | 0x24
        | 0x2C
        | 0x34
        | 0x3C
        | 0x6A
        | 0x6B
        | 0x70
        | 0x80
        | 0x83
        | 0xA8
        | 0xB0..=0xB7
        | 0xC0
        | 0xC1
        | 0xC6
        | 0xCD
        | 0xE0..=0xE2
        | 0xE4..=0xE7
        | 0xEC
        | 0xED => 1,
        0x05
        | 0x0D
        | 0x15
        | 0x1D
        | 0x25
        | 0x2D
        | 0x35
        | 0x3D
        | 0x68
        | 0x69
        | 0x81
        | 0xA9
        | 0xB8..=0xBF
        | 0xC2
        | 0xC7
        | 0xC8
        | 0xEA => 4,
        _ => 0,
    }
}

/// Immediate size (bytes) after a 0F two-byte opcode's ModRM.
fn imm_len_0f(op: u8) -> usize {
    match op {
        0xBA => 1, // bt/bts/btr/btc r/m32, imm8
        0xC7 => 1, // cmpxchg8b - no imm actually; but reserved. keep 0
        _ => 0,
    }
}

/// Length of the ModRM byte (+ optional SIB + displacement) starting at `bytes[0]`.
/// `bytes` must contain at least the ModRM byte.
fn modrm_len(bytes: &[u8]) -> Option<usize> {
    let modrm = *bytes.first()?;
    let m = (modrm >> 6) & 0b11;
    let rm = modrm & 0b111;
    let mut len = 1usize;
    if m != 0b11 {
        if rm == 0b100 {
            // SIB byte follows.
            let sib = *bytes.get(1)?;
            len += 1;
            if sib & 0b111 == 0b101 && m == 0b00 {
                len += 4; // disp32, no base
            }
        }
        match m {
            0b00 => {
                if rm == 0b101 {
                    len += 4; // disp32
                }
            }
            0b01 => len += 1, // disp8
            0b10 => len += 4, // disp32
            _ => {}
        }
    }
    Some(len)
}

/// Whole-instruction length from `addr` until ≥5 bytes (the `HookUtils::GetLen`
/// equivalent). Returns 0 if any instruction in the span fails to decode.
///
/// # Safety
///
/// `addr` must point to readable machine code with at least 16 readable bytes.
pub unsafe fn steal_len(addr: usize) -> usize {
    let mut total = 0usize;
    // SAFETY: caller guarantees readable code at `addr`.
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, 16) };
    while total < 5 {
        let l = instr_len(&bytes[total..]);
        if l == 0 {
            return 0;
        }
        total += l;
    }
    total
}

// ---------------------------------------------------------------------------
// Hook entries
// ---------------------------------------------------------------------------

/// A tracked detour (one `attach`/`detach` pair). All mutated fields are
/// interior-mutable so the hook tables can be `static`.
pub struct Hook {
    pub name: &'static str,
    /// Engine absolute address, or game-module *offset* when `is_game` is set
    /// (rebased by the module load base at attach, like the original proxy).
    pub address: usize,
    pub is_game: bool,
    pub trampoline: AtomicUsize,
    pub stolen_len: AtomicUsize,
    pub attached: AtomicBool,
}

impl Hook {
    const fn new(name: &'static str, address: usize, is_game: bool) -> Hook {
        Hook {
            name,
            address,
            is_game,
            trampoline: AtomicUsize::new(0),
            stolen_len: AtomicUsize::new(0),
            attached: AtomicBool::new(false),
        }
    }

    /// Resolve the runtime address (rebasing game offsets like
    /// `Proxy_Engine_Patch.cpp:98`).
    fn runtime_address(&self) -> usize {
        if self.is_game {
            self.address.wrapping_add(crate::original::base())
        } else {
            self.address
        }
    }

    /// The "original" trampoline as a callable address, or 0 if not attached.
    pub fn original(&self) -> usize {
        self.trampoline.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Attach / detach
// ---------------------------------------------------------------------------

/// Attach a detour: build the trampoline and overwrite the entry with `E9`.
///
/// `wrapper` is the proxy function the detour jumps to.
///
/// # Safety
///
/// The target address must be a detour-safe engine/game function entry
/// (validated by `steal_len` returning ≥5), and `wrapper` must be a function
/// pointer with the exact signature of the target.
pub unsafe fn attach(hook: &Hook, wrapper: usize) -> bool {
    let target = hook.runtime_address();
    if target == 0 {
        return false;
    }
    // SAFETY: target is engine/game code (validated by the sweep for the
    // fixed table; game offsets are nm-verified). Caller guarantees the bytes
    // are readable.
    let len = unsafe { steal_len(target) };
    if !(5..=16).contains(&len) {
        eprintln!(
            "----- proxy-rs: {name}: bad steal length {len}",
            name = hook.name
        );
        return false;
    }
    let tramp = alloc_trampoline(len);
    if tramp == 0 {
        eprintln!(
            "----- proxy-rs: {name}: trampoline allocation failed",
            name = hook.name
        );
        return false;
    }

    // Copy the stolen bytes + append `E9 rel32` back into the original.
    // SAFETY: tramp is a fresh RWX mapping of len+5 bytes; target is readable.
    unsafe {
        core::ptr::copy_nonoverlapping(target as *const u8, tramp as *mut u8, len);
    }
    let back = target + len;
    let rel = back.wrapping_sub(tramp + len + 5) as i32;
    // SAFETY: tramp mapping is len+5; the 5-byte jump fits.
    unsafe {
        *(tramp as *mut u8).add(len) = 0xE9;
        core::ptr::write_unaligned((tramp as *mut u8).add(len + 1) as *mut i32, rel);
    }

    // Overwrite the original entry.
    // SAFETY: target is writable after unprotect.
    unsafe { unprotect(target, len) };
    // SAFETY: covered by unprotect.
    unsafe {
        memset(target as *mut c_void, 0x90, len);
        *(target as *mut u8) = 0xE9;
        let rel2 = wrapper.wrapping_sub(target + 5) as i32;
        core::ptr::write_unaligned((target as *mut u8).add(1) as *mut i32, rel2);
    }
    // SAFETY: same range as unprotect.
    unsafe { reprotect(target, len) };

    hook.trampoline.store(tramp, Ordering::Relaxed);
    hook.stolen_len.store(len, Ordering::Relaxed);
    hook.attached.store(true, Ordering::Relaxed);
    eprintln!(
        "----- proxy-rs: attached {name} @ {target:#x} (trampoline {tramp:#x}, steal {len})",
        name = hook.name
    );
    true
}

/// Detach a detour: restore the stolen bytes and release the trampoline.
///
/// # Safety
///
/// Must match a successful `attach`.
pub unsafe fn detach(hook: &Hook) -> bool {
    if !hook.attached.load(Ordering::Relaxed) {
        return false;
    }
    let target = hook.runtime_address();
    let len = hook.stolen_len.load(Ordering::Relaxed);
    let tramp = hook.trampoline.load(Ordering::Relaxed);
    // SAFETY: target was patched by attach and is writable after unprotect;
    // the trampoline still holds the stolen bytes (it is only released below).
    unsafe {
        unprotect(target, len);
        core::ptr::copy_nonoverlapping(tramp as *const u8, target as *mut u8, len);
        reprotect(target, len);
    }
    hook.trampoline.store(0, Ordering::Relaxed);
    hook.stolen_len.store(0, Ordering::Relaxed);
    hook.attached.store(false, Ordering::Relaxed);
    // SAFETY: tramp came from alloc_trampoline.
    release_trampoline(tramp, len);
    eprintln!("----- proxy-rs: detached {name}", name = hook.name);
    true
}

// ---------------------------------------------------------------------------
// Inline patches
// ---------------------------------------------------------------------------

/// `Patch`: write a single byte (the rcon `Com_BeginRedirect` length, the RMG
/// branch opcode, ...).
///
/// # Safety
///
/// `addr` must be a writable-after-unprotect engine/game address holding the
/// byte to overwrite (a mid-instruction immediate or an instruction opcode).
pub unsafe fn patch_byte(addr: usize, byte: u8) {
    // SAFETY: caller guarantees a patchable address (the fixed table is
    // version-swept; the RMG branch opcode was verified by objdump).
    unsafe {
        unprotect(addr, 1);
        *(addr as *mut u8) = byte;
        reprotect(addr, 1);
    }
}

/// `Patch_NOP_Bytes`: NOP out `len` bytes at `addr`.
///
/// # Safety
///
/// `addr`+`len` must tile whole instructions (verified for the rcon timer
/// block by `tools/sweep_engine_addrs.py`).
pub unsafe fn nop_bytes(addr: usize, len: usize) {
    // SAFETY: caller guarantees the range tiles whole instructions and is
    // writable after unprotect.
    unsafe {
        unprotect(addr, len);
        memset(addr as *mut c_void, 0x90, len);
        reprotect(addr, len);
    }
}

/// `InlinePatch`: retarget the `E8/E9 rel32` operand at `addr` to `new_target`.
/// Returns the previous target (like the original's `InlineFetch`).
///
/// # Safety
///
/// `addr` must hold an `E8`/`E9` instruction (the sweep verifies the engine
/// call sites; the Com_Printf `vsprintf` call was objdump-verified).
pub unsafe fn retarget_call(addr: usize, new_target: usize) -> usize {
    // SAFETY: the instruction byte at addr must be E8/E9 (caller contract).
    let old = unsafe { inline_fetch(addr) };
    // SAFETY: addr is writable after unprotect.
    unsafe {
        unprotect(addr, 5);
        let rel = (new_target as i64).wrapping_sub((addr + 5) as i64) as i32;
        core::ptr::write_unaligned((addr as *mut u8).add(1) as *mut i32, rel);
        reprotect(addr, 5);
    }
    old
}

/// Read the rel32 target of the `E8/E9` instruction at `addr`.
///
/// # Safety
///
/// `addr` must hold an `E8`/`E9` instruction.
pub unsafe fn inline_fetch(addr: usize) -> usize {
    // SAFETY: caller guarantees an E8/E9 at addr with a readable operand.
    let rel = unsafe { core::ptr::read_unaligned((addr as *const u8).add(1) as *const i32) };
    ((addr + 5) as i64 + i64::from(rel)) as usize
}

/// Whether `bytes` begins a 6-byte `0F 8x rel32` long conditional branch, the
/// only form `jcc_to_jmp` may convert.
pub fn is_jcc_rel32(bytes: &[u8; 6]) -> bool {
    bytes[0] == 0x0F && (0x80..=0x8F).contains(&bytes[1])
}

/// The 6-byte `0F 8x rel32` (long conditional jump) at `bytes` rewritten as a
/// 5-byte `E9 rel32` plus a trailing NOP, landing on the *same* target: the
/// `E9` operand starts one byte earlier than the `0F 8x` operand, so the
/// rel32 grows by the one-byte opcode-length difference.
pub fn jcc_to_jmp_bytes(bytes: &[u8; 6]) -> [u8; 6] {
    debug_assert!(
        is_jcc_rel32(bytes),
        "jcc_to_jmp_bytes: not a 0F 8x rel32 branch"
    );
    let rel = i32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
    let new_rel = rel.wrapping_add(1);
    let mut out = [0xE9, 0, 0, 0, 0, 0x90];
    out[1..5].copy_from_slice(&new_rel.to_le_bytes());
    out
}

/// Convert the 6-byte `0F 8x rel32` conditional branch at `addr` into a
/// 5-byte `E9 rel32` with the same target (`jcc_to_jmp_bytes` applied in
/// place: `E9` over the `0F`, the adjusted rel32 at `addr+1`, and the
/// leftover operand byte at `addr+5` NOP'd).
///
/// Idempotent: a site that is already an `E9 rel32` is left untouched.
///
/// # Why
///
/// The intra patches are deliberately *not* reverted by `detach_all` (the
/// original proxy leaves them in place too). A map change unloads and reloads
/// the proxy module (`VM_Free`/`VM_Create`), so `GAME_INIT` — and thus
/// `attach_all` — runs again with the engine code still flipped. Without this
/// guard the second pass would read the jump's rel32 bytes as a `0F 8x`
/// operand (`rel = bytes[2..6]`) and write a wild `E9` target, crashing the
/// server the first time the patched branch executes.
///
/// # Safety
///
/// `addr` must point to at least 6 readable bytes and, when it holds a
/// `0F 8x rel32`, be writable after `unprotect`.
pub unsafe fn jcc_to_jmp(addr: usize) {
    // SAFETY: caller guarantees 6 readable bytes at addr.
    let mut bytes = [0u8; 6];
    unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, bytes.as_mut_ptr(), 6) };
    if !is_jcc_rel32(&bytes) {
        // Already converted (or not a conditional branch): leave it alone.
        return;
    }
    let patched = jcc_to_jmp_bytes(&bytes);
    // SAFETY: the 6-byte range is writable after unprotect.
    unsafe {
        unprotect(addr, 6);
        core::ptr::copy_nonoverlapping(patched.as_ptr(), addr as *mut u8, 6);
        reprotect(addr, 6);
    }
}

// ---------------------------------------------------------------------------
// Hook registry
// ---------------------------------------------------------------------------

/// All engine detours, in attach order (mirrors `hookEntries` in
/// `Proxy_Engine_Patch.cpp:29-52`), minus the dropped features (D-002):
/// anti-wallhack (`SV_SendClientSnapshot`, `SV_AddEntitiesVisibleFromPoint`),
/// ping-fix (`SV_CalcPings`, `SV_SendMessageToClient`), the
/// `SV_ExecuteClientMessage` rewrite (hack guards already in the shipped
/// binary — see `docs/`), `SV_PacketEvent` (pristine `SV_ReadPackets` already
/// has both fixes) and the `Cmd_TokenizeString` hook (proxy-local helper
/// instead). `Com_Printf` is not detoured either: its hardening is the
/// `vsprintf`→`vsnprintf` call-site retarget (`CALL_VSPRINTF`).
pub static ENGINE_HOOKS: [Hook; 14] = [
    Hook::new("SVC_Status", crate::engine::functions::SVC_STATUS, false),
    Hook::new("SVC_Info", crate::engine::functions::SVC_INFO, false),
    Hook::new(
        "SVC_RemoteCommand",
        crate::engine::functions::SVC_REMOTE_COMMAND,
        false,
    ),
    Hook::new(
        "SV_UserinfoChanged",
        crate::engine::functions::SV_USERINFO_CHANGED,
        false,
    ),
    Hook::new(
        "SV_BeginDownload_f",
        crate::engine::functions::SV_BEGIN_DOWNLOAD_F,
        false,
    ),
    Hook::new(
        "SV_NextDownload_f",
        crate::engine::functions::SV_NEXT_DOWNLOAD_F,
        false,
    ),
    Hook::new(
        "SV_StopDownload_f",
        crate::engine::functions::SV_STOP_DOWNLOAD_F,
        false,
    ),
    Hook::new(
        "SV_DoneDownload_f",
        crate::engine::functions::SV_DONE_DOWNLOAD_F,
        false,
    ),
    Hook::new(
        "SV_UpdateUserinfo_f",
        crate::engine::functions::SV_UPDATE_USERINFO_F,
        false,
    ),
    Hook::new(
        "SV_WriteDownloadToClient",
        crate::engine::functions::SV_WRITE_DOWNLOAD_TO_CLIENT,
        false,
    ),
    Hook::new(
        "SV_SvEntityForGentity",
        crate::engine::functions::SV_SVENTITY_FOR_GENTITY,
        false,
    ),
    Hook::new(
        "CNavigator::Load",
        crate::engine::functions::NAVIGATOR_LOAD,
        false,
    ),
    Hook::new(
        "SV_SendClientGameState",
        crate::engine::functions::SV_SEND_CLIENT_GAME_STATE,
        false,
    ),
    Hook::new(
        "SV_ExecuteClientMessage",
        crate::engine::functions::SV_EXECUTE_CLIENT_MESSAGE,
        false,
    ),
];

/// Game-module detours (rebased), mirroring `jampgameHookEntries`
/// (`Proxy_Engine_Patch.cpp:61-70`) minus the dropped `WP_SaberPositionUpdate`
/// (sabersFps) and `G_RegisterCvars`/`G_UpdateCvars` (vmMain cvar mirror).
pub static GAME_HOOKS: [Hook; 5] = [
    Hook::new("G_Damage", crate::jampgame::FN_G_DAMAGE, true),
    Hook::new("player_die", crate::jampgame::FN_PLAYER_DIE, true),
    Hook::new(
        "BeginIntermission",
        crate::jampgame::FN_BEGIN_INTERMISSION,
        true,
    ),
    Hook::new("G_AddEvent", crate::jampgame::FN_G_ADD_EVENT, true),
    Hook::new(
        "ClientThink_real",
        crate::jampgame::FN_CLIENT_THINK_REAL,
        true,
    ),
];

/// The single call-site feed for the netStatus per-usercmd stats: the
/// `call SV_ClientThink` inside the pristine `SV_UserMove` (0x804e891, target
/// `SV_ClientThink` 0x804e634 — objdump-verified). Retargeted to a proxy stub
/// that calls the original `SV_ClientThink` and then feeds the cmd stats
/// (D-002.1 technique 3, `docs/inventory.md` → "Players net status").
pub const CALL_SV_CLIENT_THINK: usize = 0x0804_e891;

/// The `call vsprintf` inside the engine's `Com_Printf` (0x8072cca, target
/// `vsprintf@plt` — objdump-verified). Retargeted to the proxy's `vsnprintf`
/// shim to harden Com_Printf against "%f"/oversized-format overflows
/// (D-002 "Com_Printf" redesign row).
pub const CALL_VSPRINTF: usize = 0x0807_2cca;

/// The `call SV_AuthorizeIpPacket` inside the pristine `SV_ConnectionlessPacket`
/// (0x8056f64 — the `ipAuthorize` dispatch arm, objdump-verified). NOP-ing it
/// drops the anti-DDoS hole (D-002, `docs/inventory.md`).
pub const CALL_IP_AUTHORIZE: usize = 0x0805_6f64;

/// The RMG branch inside the pristine `SV_SendClientGameState` (0x804d131:
/// `je 0x804d3a1` over the Random-Mission terrain block, objdump-verified).
/// Flipped to an unconditional `jmp` so the "old RMG" else path
/// (`MSG_WriteShort(&msg, 0)`) is always taken. The flip rewrites the rel32
/// too (`jcc_to_jmp`): with `E9` the operand shifts one byte earlier, so
/// keeping the old operand bytes would jump into unrelated code.
pub const RMG_BRANCH: usize = 0x0804_d131;

// ---------------------------------------------------------------------------
// Registry bookkeeping
// ---------------------------------------------------------------------------

struct PatchedCall {
    addr: usize,
    original: usize,
}

static PATCHED_CALLS: Mutex<Vec<PatchedCall>> = Mutex::new(Vec::new());

fn lock() -> MutexGuard<'static, Vec<PatchedCall>> {
    PATCHED_CALLS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Retarget `addr` to `new_target`, remembering the pre-attach target so
/// `restore_calls` can put it back at shutdown.
///
/// # Safety
///
/// `addr` must hold an `E8`/`E9` instruction.
pub unsafe fn retarget_call_tracked(addr: usize, new_target: usize) {
    // SAFETY: addr must be a verified E8/E9 site.
    let original = unsafe { inline_fetch(addr) };
    lock().push(PatchedCall { addr, original });
    // SAFETY: same contract as retarget_call.
    unsafe { retarget_call(addr, new_target) };
}

/// Restore every retargeted call to its pre-attach target (GAME_SHUTDOWN).
pub fn restore_calls() {
    let calls = core::mem::take(&mut *lock());
    for c in calls {
        // SAFETY: each addr was verified as an E8/E9 site at attach time.
        unsafe {
            retarget_call(c.addr, c.original);
        }
    }
}

use core::ffi::c_int;

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole-instruction steal lengths of the real engine/game hook sites,
    /// byte fixtures extracted from the reference binaries (vaddr == file
    /// offset for both text segments). First instruction must be `push %ebp`
    /// (0x55) and the computed steal must match the sweep (6 or 9).
    struct Site {
        name: &'static str,
        bytes: &'static [u8],
        want: usize,
    }

    const ENGINE_SITES: &[Site] = &[
        Site {
            name: "SVC_RemoteCommand",
            bytes: b"\x55\x8b\xec\x81\xec\x20\xc5\x00\x00",
            want: 9,
        },
        Site {
            name: "SVC_Status",
            bytes: b"\x55\x8b\xec\x81\xec\x30\xcd\x00\x00",
            want: 9,
        },
        Site {
            name: "SVC_Info",
            bytes: b"\x55\x8b\xec\x81\xec\x18\x04\x00\x00",
            want: 9,
        },
        Site {
            name: "SV_UserinfoChanged",
            bytes: b"\x55\x8b\xec\x83\xec\x20",
            want: 6,
        },
        Site {
            name: "SV_BeginDownload_f",
            bytes: b"\x55\x8b\xec\x83\xec\x18",
            want: 6,
        },
        Site {
            name: "SV_NextDownload_f",
            bytes: b"\x55\x8b\xec\x83\xec\x20",
            want: 6,
        },
        Site {
            name: "SV_StopDownload_f",
            bytes: b"\x55\x8b\xec\x83\xec\x18",
            want: 6,
        },
        Site {
            name: "SV_DoneDownload_f",
            bytes: b"\x55\x8b\xec\x83\xec\x10",
            want: 6,
        },
        Site {
            name: "SV_UpdateUserinfo_f",
            bytes: b"\x55\x8b\xec\x83\xec\x18",
            want: 6,
        },
        Site {
            name: "SV_WriteDownloadToClient",
            bytes: b"\x55\x8b\xec\x81\xec\x20\x04\x00\x00",
            want: 9,
        },
        Site {
            name: "SV_SvEntityForGentity",
            bytes: b"\x55\x8b\xec\x83\xec\x10",
            want: 6,
        },
        Site {
            name: "Navigator_Load",
            bytes: b"\x55\x8b\xec\x83\xec\x40",
            want: 6,
        },
        Site {
            name: "SV_SendClientGameState",
            bytes: b"\x55\x8b\xec\x81\xec\x18\xfe\x00\x00",
            want: 9,
        },
        Site {
            name: "SV_ExecuteClientMessage",
            bytes: b"\x55\x8b\xec\x83\xec\x20",
            want: 6,
        },
        Site {
            name: "Com_Printf",
            bytes: b"\x55\x8b\xec\x81\xec\x20\x11\x00\x00",
            want: 9,
        },
    ];

    const GAME_SITES: &[Site] = &[
        Site {
            name: "G_Damage",
            bytes: b"\x55\x8b\xec\x81\xec\x48\x01\x00\x00",
            want: 9,
        },
        Site {
            name: "player_die",
            bytes: b"\x55\x8b\xec\x83\xec\x60",
            want: 6,
        },
        Site {
            name: "BeginIntermission",
            bytes: b"\x55\x8b\xec\x83\xec\x18",
            want: 6,
        },
        Site {
            name: "G_AddEvent",
            bytes: b"\x55\x8b\xec\x83\xec\x18",
            want: 6,
        },
        Site {
            name: "ClientThink_real",
            bytes: b"\x55\x8b\xec\x81\xec\x10\x03\x00\x00",
            want: 9,
        },
    ];

    #[test]
    fn decoder_matches_sweep_steal_lengths() {
        let mut total = 0;
        for site in ENGINE_SITES.iter().chain(GAME_SITES) {
            let mut off = 0usize;
            let mut sum = 0usize;
            while sum < 5 {
                let l = instr_len(&site.bytes[off..]);
                assert!(l > 0, "{}: undecodable at {off}", site.name);
                sum += l;
                off += l;
            }
            assert_eq!(sum, site.want, "{}: steal length", site.name);
            assert_eq!(site.bytes[0], 0x55, "{}: prologue push %ebp", site.name);
            total += 1;
        }
        assert_eq!(total, ENGINE_SITES.len() + GAME_SITES.len());
    }

    #[test]
    fn jcc_to_jmp_is_idempotent_across_map_change() {
        // A map change re-runs attach_all without reverting the intra patches
        // (detach_all leaves them in place), so the RMG site arrives already
        // flipped the second time. `is_jcc_rel32` must reject it, otherwise
        // the E9 rel32 would be misread as a 0F 8x operand and reprocessed
        // into a wild jump target.
        let pristine = [0x0f, 0x84, 0x6a, 0x02, 0x00, 0x00]; // je 0x804d3a1
        assert!(is_jcc_rel32(&pristine));
        let flipped = jcc_to_jmp_bytes(&pristine);
        assert_eq!(flipped, [0xe9, 0x6b, 0x02, 0x00, 0x00, 0x90]);
        assert!(
            !is_jcc_rel32(&flipped),
            "already-flipped site must be skipped on re-apply"
        );
        // The bogus conversion the guard prevents is what the old code wrote:
        // rel = le(0x02,0x00,0x00,0x90) + 1 => E9 03 00 00 90 90, i.e.
        // jmp 0x9804d139 — the post-map-change corruption observed live.
    }

    #[test]
    fn jcc_to_jmp_preserves_target() {
        // The RMG site (0x804d131, objdump-verified): `je 0x804d3a1`
        // (`0f 84 6a 02 00 00`, rel = 0x26a, ends at +6). The flipped `E9`
        // must target the same address: +5 + 0x26b = +6 + 0x26a.
        let site = [0x0f, 0x84, 0x6a, 0x02, 0x00, 0x00];
        let patched = jcc_to_jmp_bytes(&site);
        assert_eq!(patched, [0xe9, 0x6b, 0x02, 0x00, 0x00, 0x90]);
        let old_target = 6usize + i32::from_le_bytes([site[2], site[3], site[4], site[5]]) as usize;
        let new_target =
            5usize + i32::from_le_bytes([patched[1], patched[2], patched[3], patched[4]]) as usize;
        assert_eq!(old_target, new_target);
        // Negative displacement must wrap correctly too.
        let neg = [0x0f, 0x84, 0xf6, 0xff, 0xff, 0xff]; // rel = -10
        let neg_patched = jcc_to_jmp_bytes(&neg);
        assert_eq!(
            i32::from_le_bytes([
                neg_patched[1],
                neg_patched[2],
                neg_patched[3],
                neg_patched[4]
            ]),
            -9
        );
    }

    #[test]
    fn decoder_handles_common_prologue_forms() {
        // push ebp; mov esp,ebp; sub $imm8,%esp / $imm32,%esp
        assert_eq!(instr_len(b"\x55"), 1);
        assert_eq!(instr_len(b"\x8b\xec"), 2);
        assert_eq!(instr_len(b"\x83\xec\x20"), 3);
        assert_eq!(instr_len(b"\x81\xec\x20\xc5\x00\x00"), 6);
        // mov r/m32, imm32 (C7) and mov eax, moffs32 (A1)
        assert_eq!(instr_len(b"\xc7\x06\x03\x00\x00\x00"), 6);
        assert_eq!(instr_len(b"\xa1\x3c\x23\x00\x00"), 5);
        // call/jmp rel32, jcc rel8, 0F jcc rel32
        assert_eq!(instr_len(b"\xe8\x19\x6e\xfd\xff"), 5);
        assert_eq!(instr_len(b"\xeb\xcc"), 2);
        assert_eq!(instr_len(b"\x0f\x84\x6a\x02\x00\x00"), 6);
        // mov edi,-0x8(%ebp) (89 / ModRM mod=01)
        assert_eq!(instr_len(b"\x89\x7d\xf8"), 3);
        // mov esi,0x8(%ebp) (8b / ModRM mod=01)
        assert_eq!(instr_len(b"\x8b\x75\x08"), 3);
        // lea -0x1110(%ebp),%eax (8d / ModRM mod=10 disp32)
        assert_eq!(instr_len(b"\x8d\x85\xf0\xee\xff\xff"), 6);
    }
}
