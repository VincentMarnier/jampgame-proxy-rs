# D-013 — Opt-in crash diagnostics (`proxy_sv_debugLog`)

Date: 2026-11-02. Status: accepted (user request: identify how players crash
the server). Evidence labels: **SOURCE** for the engine/game hook sites
(`original/jedi-academy/codemp/server/{sv_client,sv_main}.cpp`), **STATIC** for
the `SV_ExecuteClientCommand` prologue (`0x804e3d4`, `55 8b ec 83 ec 20`, six
stealable bytes, objdump on `original/jalinuxded_1.011/linuxjampded`),
**RUNTIME** for the hook attach/console/file check in the engine runtime.

## Context

The server occasionally dies while players are connected. The proxy already
blocks a known list of crash/exploit inputs, but a blocked command is silent
today: when the crash comes through a path not covered yet, there is no record
of who sent what immediately before the fault. Running a debugger is not an
option on a live server, and the engine's `qconsole.log` only contains what
reached `Com_Printf`.

## Decision

Add a proxy cvar `proxy_sv_debugLog` (default `0`, `CVAR_ARCHIVE` like every
proxy cvar) with three levels:

| value | content |
|---|---|
| `0` | off — every call site tests the lock-free level first, so nothing is formatted, written or printed |
| `1` | security-relevant events: blocked client commands, userinfo/model/forcepowers fixes, anti-cheat kicks, rcon attempts (password redacted), rejected download paths, oversized connectionless queries, bad `svEntity` pointers and malformed client messages |
| `2` | level 1 plus every client command (full text and length), every `userinfo` update/command, and connect/begin/disconnect events |

The cvar is mirrored into an atomic once per frame by `update_proxy_cvars`;
log call sites never take the proxy state lock. The `svs.time` used in lines is
also an atomic, refreshed by `GAME_RUN_FRAME`.

## Where the events come from

- **`SV_ExecuteClientCommand`** (`0x804e3d4`, new entry-wrapper detour,
  `ENGINE_HOOKS` 15 → 16): the one engine point every client command passes
  through, including the server-level `userinfo`/`download`/`nextdl` commands.
  The wrapper logs **before** the pristine body, so the command that faults is
  already flushed.
- **`SV_ExecuteClientMessage`** (existing wrapper): stale `serverId` gate
  events, the dropped-gamestate resend path and out-of-range `clc_*` bytes.
- **`SV_UserinfoChanged` / `SV_UpdateUserinfo_f`** (existing wrappers): the
  userinfo string before the engine/game sanitises it, plus every model clamp.
- **`SVC_RemoteCommand`, `SVC_Status`, `SVC_Info`** (existing wrappers): rcon
  attempts (the supplied password is compared with `sv_rconpassword` and only
  the match result is recorded), the `writeconfig` restriction and q3infoboom
  drops.
- **Download wrappers** (existing): every download the proxy refuses and why.
- **vmMain client events** (`crate::lib`): connect/begin/disconnect identity.
- **`Proxy_SharedAPI_ClientCommand`** (`shared_api::client_command`): every
  command the proxy blocks, with its reason, raw command and arguments.
- **`Proxy_SharedAPI_ClientUserinfoChanged`**: darksidetools kicks, forced
  model and forcepowers replacements.

## Output

Lines are appended and flushed to `proxy_debug.log` in the server working
directory, then printed through `G_PRINT` (`Com_Printf("%s")`) so they also
reach the dedicated console and `qconsole.log` when `logfile` is enabled. The
write happens before the console call, so a crash in the engine/game code
cannot lose the last line. The file is reused across map changes; no rotation
is performed because the feature is meant to be enabled only while
investigating.

Format:

```
t=<svs.time> unix_ms=<epoch ms> level=<SUSPICIOUS|INFO> event=<category> client=<n> name="..." adr="ip:port" ... cmd="..." len=<n>
```

Every client-controlled byte string is escaped (`\n`, `\r`, `\t`, `\\`, `\"`,
`\xNN` for non-printables). The printable preview is capped at 512 bytes per
field but the original length is always reported, which is what matters for
overflow-driven crashes.

## Limits / non-goals

- A raw `SIGSEGV` inside the engine or pristine game module is not caught; the
  value is the last-logged command, not a stack trace. Reproducing under gdb
  with the same cvar remains the way to get a crash PC.
- The rcon password and full client userinfo of unrelated players are not
  secrets the log tries to protect beyond the password redaction; treat
  `proxy_debug.log` as admin-only data.
- Level 2 is high volume by design. It is not intended to stay on: every
  client command is written and flushed, and printed to the console.
- Attaching `SV_ExecuteClientCommand` follows the same version-gated,
  whole-instruction detour rules as every other engine hook (R-010/R-013); if
  the site ever stops matching `steal_len`, the hook refuses to attach and the
  rest of the proxy keeps working.
