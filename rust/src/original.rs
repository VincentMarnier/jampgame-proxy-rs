//! Loading and wiring of the original game module (`jampgame_original.so`).
//!
//! Mirrors `Proxy_Main.cpp:18-80` (`Proxy_GetOriginalGameAPI`) and
//! `Proxy_Files.cpp` (`Proxy_LoadGameLibrary`): the original library is
//! `dlopen`ed with `RTLD_NOW` from a CWD-relative path derived from the
//! `fs_game` cvar (default `base`), then its `dllEntry` and `vmMain` are
//! resolved and stored for the lifetime of the module.

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
}

/// dlopen flag used by the engine (`unix_main.c`) and the original proxy.
const RTLD_NOW: c_int = 2;

/// Handle of the loaded original module, `null_mut` once closed.
static HANDLE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static VM_MAIN: AtomicUsize = AtomicUsize::new(0);
static DLL_ENTRY: AtomicUsize = AtomicUsize::new(0);

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

/// dlopen the original module and resolve its `dllEntry`/`vmMain` symbols.
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
    // Roll back the open if either required symbol is missing.
    let (dll_entry, vm_main) = match (|| {
        let dll_entry = unsafe { symbol(handle, c"dllEntry")? };
        let vm_main = unsafe { symbol(handle, c"vmMain")? };
        Ok::<_, String>((dll_entry, vm_main))
    })() {
        Ok(v) => v,
        Err(e) => {
            unsafe { dlclose(handle) };
            return Err(e);
        }
    };

    HANDLE.store(handle, Ordering::Relaxed);
    DLL_ENTRY.store(dll_entry as usize, Ordering::Relaxed);
    VM_MAIN.store(vm_main as usize, Ordering::Relaxed);
    Ok(())
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
