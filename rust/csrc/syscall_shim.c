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
 * See rust/src/syscall.rs for the Rust side of the boundary.
 */

#include <stdarg.h>

/* Implemented in Rust (rust/src/syscall.rs). */
extern int jampgame_syscall_forward(
	int command,
	int a0,  int a1,  int a2,  int a3,
	int a4,  int a5,  int a6,  int a7,
	int a8,  int a9,  int a10, int a11,
	int a12, int a13, int a14, int a15);

/* Max handled: 1 (command) + 16 args, as in the original proxy. */
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
