//! Loading and wiring of the original game module (`jampgame_original.so`).
//!
//! Mirrors `Proxy_Main.cpp:18-80` (`Proxy_GetOriginalGameAPI`) and
//! `Proxy_Files.cpp` (`Proxy_LoadGameLibrary`): the original library is
//! `dlopen`ed with `RTLD_NOW` from a CWD-relative path derived from the
//! `fs_game` cvar (default `base`), then its `dllEntry`, `vmMain`, `level` and
//! `g_entities` are resolved and its load base obtained via `dladdr`, all
//! stored for the lifetime of the module.

use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::syscall::EngineSyscall;

/// `void (*)(systemCallFuncPtr_t)` — the original module's entry function.
pub type DllEntryFn = unsafe extern "C" fn(EngineSyscall);

/// 13-word `vmMain` (command + 12 args), per the SDK `g_main.c:503`.
pub type VmMainFn = unsafe extern "C" fn(
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
) -> c_int;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *mut c_char;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

/// `struct Dl_info` from `<link.h>` (glibc, i386): four pointer-sized fields.
#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

/// dlopen flag used by the engine (`unix_main.c`) and the original proxy.
const RTLD_NOW: c_int = 2;
const ORIGINAL_LIBRARY_NAME: &str = "jampgame_original.so";

/// Handle of the loaded original module, `null_mut` once closed.
static HANDLE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static VM_MAIN: AtomicUsize = AtomicUsize::new(0);
static DLL_ENTRY: AtomicUsize = AtomicUsize::new(0);
/// Load base of the original module (`dladdr(dli_fbase)`), used to rebase
/// game offsets (`proxy.jampgameAddress`).
static BASE: AtomicUsize = AtomicUsize::new(0);
/// `level` (`level_locals_t *`) and `g_entities` (`gentity_t *`) as exported
/// by the original module (R-004).
static LEVEL: AtomicUsize = AtomicUsize::new(0);
static G_ENTITIES: AtomicUsize = AtomicUsize::new(0);

/// Look up a symbol, returning it as an opaque pointer or an error message.
unsafe fn symbol(handle: *mut c_void, name: &CStr) -> Result<*mut c_void, String> {
    let sym = unsafe { dlsym(handle, name.as_ptr()) };
    if sym.is_null() {
        Err(format!(
            "dlsym({}) failed: {}",
            name.to_string_lossy(),
            last_error()
        ))
    } else {
        Ok(sym)
    }
}

/// Message of the most recent dl* error, or a fallback.
fn last_error() -> String {
    // SAFETY: dlerror returns a pointer to a thread-local NUL-terminated
    // string valid until the next dl* call.
    let msg = unsafe { dlerror() };
    if msg.is_null() {
        "unknown dl error".to_string()
    } else {
        // SAFETY: pointer from dlerror is NUL-terminated.
        unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    }
}

/// dlopen the original module and resolve its `dllEntry`/`vmMain`/`level`/
/// `g_entities` symbols plus its load base.
///
/// `path` is CWD-relative (the proxy is launched from the install root, so
/// `base/jampgame_original.so` — see docs/runtime-environment.md).
pub fn load(path: &CStr) -> Result<(), String> {
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        return Err(format!(
            "dlopen({}) failed: {}",
            path.to_string_lossy(),
            last_error()
        ));
    }
    // Roll back the open if any required symbol is missing.
    let (dll_entry, vm_main, level, g_entities) = match (|| {
        let dll_entry = unsafe { symbol(handle, c"dllEntry")? };
        let vm_main = unsafe { symbol(handle, c"vmMain")? };
        let level = unsafe { symbol(handle, c"level")? };
        let g_entities = unsafe { symbol(handle, c"g_entities")? };
        Ok::<_, String>((dll_entry, vm_main, level, g_entities))
    })() {
        Ok(v) => v,
        Err(e) => {
            unsafe { dlclose(handle) };
            return Err(e);
        }
    };

    // Load base: `dladdr` on the module's own dllEntry gives `dli_fbase`
    // (`Proxy_Main.cpp:35-43`); the proxy rebases all game offsets by it.
    let mut info = DlInfo {
        dli_fname: core::ptr::null(),
        dli_fbase: core::ptr::null_mut(),
        dli_sname: core::ptr::null(),
        dli_saddr: core::ptr::null_mut(),
    };
    // SAFETY: dll_entry points into the just-loaded module.
    let dladdr_ok = unsafe { dladdr(dll_entry, &mut info) } != 0;
    if !dladdr_ok || info.dli_fbase.is_null() {
        unsafe { dlclose(handle) };
        return Err(format!(
            "dladdr({ORIGINAL_LIBRARY_NAME}) failed: {}",
            last_error()
        ));
    }

    HANDLE.store(handle, Ordering::Relaxed);
    DLL_ENTRY.store(dll_entry as usize, Ordering::Relaxed);
    VM_MAIN.store(vm_main as usize, Ordering::Relaxed);
    BASE.store(info.dli_fbase as usize, Ordering::Relaxed);
    LEVEL.store(level as usize, Ordering::Relaxed);
    G_ENTITIES.store(g_entities as usize, Ordering::Relaxed);
    Ok(())
}

/// Load base of the original module, or 0 if none is loaded.
pub fn base() -> usize {
    BASE.load(Ordering::Relaxed)
}

/// Address of the original module's exported `level` symbol, or 0. Bound at
/// load; read once the game-side `level_locals_t` handlers land.
#[allow(dead_code)]
pub fn level_address() -> usize {
    LEVEL.load(Ordering::Relaxed)
}

/// Address of the original module's exported `g_entities` symbol, or 0.
pub fn g_entities_address() -> usize {
    G_ENTITIES.load(Ordering::Relaxed)
}

/// Hand the proxy's own syscall forwarder to the original module, exactly like
/// `Proxy_GetOriginalGameAPI` does. Must only be called after a successful
/// `load`.
///
/// # Safety
///
/// Requires a successfully loaded original module.
pub unsafe fn wire_original_dll_entry(forwarder: EngineSyscall) {
    let dll_entry = DLL_ENTRY.load(Ordering::Relaxed);
    if dll_entry == 0 {
        return;
    }
    // usize -> fn ptr: pointer-sized, stores the pointer from dlsym.
    let dll_entry: DllEntryFn = unsafe { core::mem::transmute(dll_entry) };
    unsafe { dll_entry(forwarder) };
}

/// Call the original module's `vmMain`, forwarding all 13 words unchanged.
///
/// Returns `None` if the original module is not currently loaded.
pub fn forward_vm_main(command: c_int, args: &[c_int; 12]) -> Option<c_int> {
    let vm_main = VM_MAIN.load(Ordering::Relaxed);
    if vm_main == 0 {
        return None;
    }
    let vm_main: VmMainFn = unsafe { core::mem::transmute(vm_main) };
    // SAFETY: `args` carries exactly the 12 words the engine passed to us.
    Some(unsafe {
        vm_main(
            command, args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
            args[8], args[9], args[10], args[11],
        )
    })
}

/// Close the original module and reset the stored state (GAME_SHUTDOWN path).
pub fn unload() {
    let handle = HANDLE.swap(core::ptr::null_mut(), Ordering::Relaxed);
    VM_MAIN.store(0, Ordering::Relaxed);
    DLL_ENTRY.store(0, Ordering::Relaxed);
    BASE.store(0, Ordering::Relaxed);
    LEVEL.store(0, Ordering::Relaxed);
    G_ENTITIES.store(0, Ordering::Relaxed);
    if !handle.is_null() {
        unsafe { dlclose(handle) };
    }
}

/// Whether an original module is currently loaded.
pub fn is_loaded() -> bool {
    !HANDLE.load(Ordering::Relaxed).is_null()
}

#[cfg(test)]
mod tests {
    #[test]
    fn cstr_symbol_names_are_ascii() {
        assert_eq!(c"dllEntry".to_bytes(), b"dllEntry");
        assert_eq!(c"vmMain".to_bytes(), b"vmMain");
    }
}
