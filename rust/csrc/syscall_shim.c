/*
 * syscall_shim.c — i386 cdecl variadic receive shim for the engine syscall
 * interface ("trap" calls).
 *
 * Why C: the game module calls back into the engine through a variadic
 * function pointer (`int QDECL (*)(int command, ...)`, QDECL == cdecl on
 * Linux). Stable Rust cannot *define* a C-variadic function body, so the
 * variadic receive + harvest lives here, mirroring the original proxy's
 * Proxy_OriginalAPI_VM_DllSyscall (Proxy_OriginalAPI_Wrappers.cpp): harvest up
 * to 1 (command) + 16 int-width arguments, then forward them, fixed-arity, to
 * the Rust side, which owns the stored engine syscall pointer.
 *
 * This function is handed to the *original* game module's dllEntry as the
 * syscall pointer, so the original game's traps pass through the proxy.
 *
 * Also hosts the two variadic *call* shims the hook layer needs:
 *   - jampgame_proxy_vsnprintf_shim — the retarget for Com_Printf's internal
 *     `call vsprintf` (engine calls it with the same 3 args as vsprintf; it
 *     adds the fixed buffer size so vsnprintf can truncate).
 *   - jampgame_proxy_com_printf — a forwarder for the proxy's own Com_Printf
 *     calls (the engine's Com_Printf, hardened by the retarget above).
 *
 * See rust/src/syscall.rs for the Rust side of the boundary.
 */

#include <stdarg.h>
#include <stdio.h>

/* Implemented in Rust (rust/src/syscall.rs). */
extern int jampgame_syscall_forward(
	int command,
	int a0,  int a1,  int a2,  int a3,
	int a4,  int a5,  int a6,  int a7,
	int a8,  int a9,  int a10, int a11,
	int a12, int a13, int a14, int a15);

/* Max handled: 1 (command) + 16 args, as in the original proxy. */
int jampgame_vm_dllsyscall(int command, ...)
	__attribute__((force_align_arg_pointer));

int jampgame_vm_dllsyscall(int command, ...)
{
	va_list ap;
	int args[16];
	int i;
	int ret;

	va_start(ap, command);
	for (i = 0; i < 16; ++i)
	{
		args[i] = va_arg(ap, int);
	}
	va_end(ap);

	ret = jampgame_syscall_forward(
		command,
		args[0],  args[1],  args[2],  args[3],
		args[4],  args[5],  args[6],  args[7],
		args[8],  args[9],  args[10], args[11],
		args[12], args[13], args[14], args[15]);

	return ret;
}

/*
 * Com_Printf hardening (D-002 "Com_Printf" redesign row): the engine's
 * Com_Printf formats into a 0x1100-byte stack buffer and calls vsprintf
 * (0x8072cca -> vsprintf@plt). The call is retargeted here: same 3 args
 * (buffer, fmt, va_list), plus a hard-coded truncation bound so an oversized
 * "%f"-crasher format can no longer overflow the buffer. 4096 matches the
 * original proxy's Proxy_Com_Printf `char msg[4096]` truncation and is strictly
 * smaller than the engine's 4352-byte buffer, so no overflow is possible.
 */
int jampgame_proxy_vsnprintf_shim(char *buf, const char *fmt, va_list ap)
{
	return vsnprintf(buf, 4096, fmt, ap);
}

/*
 * Forward a printf-style call to the engine's Com_Printf (0x8072ca4). The
 * engine's Com_Printf is the hardened version after the retarget above, so
 * this is what the original proxy reached through
 * `server.common.functions.Com_Printf`.
 */
extern void jampgame_proxy_com_printf(const char *fmt, ...)
	__attribute__((force_align_arg_pointer));

void jampgame_proxy_com_printf(const char *fmt, ...)
{
	void (*engine_printf)(const char *, ...) = (void (*)(const char *, ...))0x08072ca4;
	va_list ap;

	va_start(ap, fmt);
	/* The engine's Com_Printf is variadic cdecl: pass the va_list through
	 * (it is just a char* on i386). */
	((void (*)(const char *, char *))engine_printf)(fmt, (char *)ap);
	va_end(ap);
}
