# D-017 — File-backed IP ban list

Date: 2026-11-03. Status: accepted (user directive). Feature:
`proxy_sv_enableBanFile` / `proxy_sv_banFile`, `crate::banfile`.

## Context

The pristine ban mechanism is unusable beyond a handful of addresses:

- `addip <mask>` / `removeip` / `listip` are **game-module** console commands
  (`g_svcmds.c:226-239`, `:446-457`). They fill an in-memory `ipFilters[1024]`
  array and `ClientConnect` refuses a match with `"Banned."`
  (`g_client.c:2268-2274`).
- Persistence is a side effect: `UpdateIPBans` rewrites the archived cvar
  `g_banIPs` (`g_svcmds.c:109-127`), the engine writes it to
  `<homepath>/.ja/base/jampserver.cfg` as `seta g_banIPs "..."` on the next
  frame, and `exec jampserver.cfg` restores it at startup
  (`qcommon/common.cpp:1390-1401`, `:1605-1626`, `:1758`).
- The game's `vmCvar_t` mirror is 256 bytes (`MAX_CVAR_VALUE_STRING`,
  `q_shared.h:1723`) and `Cvar_Update` makes a longer value a fatal
  `Com_Error(ERR_DROP)` (`qcommon/cvar.cpp:939-949`). **Measured** on the
  retail 1.011 engine + shipped game module (the `jka-orig` container, no
  proxy): 25 entries / 267 bytes →
  `ERROR: Cvar_Update: src g_banIPs length 267 exceeds MAX_CVAR_VALUE_STRING`,
  `ShutdownGame`, process exit 139. The 267-byte value was already in
  `jampserver.cfg`, so **every later start also exited 139** until the line was
  edited out. The practical capacity is about 15-30 addresses, and overrunning
  it is not graceful.
- The mask is not CIDR: `StringToFilter` derives it from which octets are
  non-zero and ignores `/prefix`. **Measured**: `addip 84.17.32.0/19` is stored
  as `84.17.32.0` and bans `84.17.32.0/24`, not `/19`; `addip 10.0.0.0` bans
  `10.0.0.0/8`.

The user's directive: make a file the only source of bans, allow IP ranges,
intercept `addip`/`removeip`, and put the whole feature behind a cvar so it can
be disabled.

## Decision

A dedicated text file is the source of truth; the in-memory vector is only a
cache of it.

- New cvars (both `CVAR_ARCHIVE`, registered with the rest):
  - `proxy_sv_enableBanFile` — `0` off (default), non-zero on.
  - `proxy_sv_banFile` — path, default `proxy_bans.txt`, relative to the
    server's working directory (like `proxy_debug.log`) or absolute.
- With the feature **off**, the pristine handlers run unchanged: `addip` /
  `removeip` / `listip` are forwarded to the game and no connect is checked.
  The proxy-only `reloadbans` is still claimed and reports that the feature is
  off (forwarding it would make the pristine `ConsoleCommand` echo it to every
  client as `server: ...`).
- With the feature **on**:
  - `GAME_CONSOLE_COMMAND` claims `addip`, `removeip`, `listip` and
    `reloadbans`.
  - `addip <ip[/prefix]>` adds an entry (real IPv4 CIDR), writes the file
    first, and only then commits the list to memory; `removeip` is the reverse;
    `listip` prints the loaded entries (first 100, then a count); `reloadbans`
    re-reads the file.
  - `GAME_CLIENT_CONNECT` reads the engine-forced `ip` userinfo value
    (`SV_DirectConnect` writes it, `server/sv_client.cpp:299-300`), strips the
    `:port`, and refuses a match by returning a static `"Banned."` — the
    engine's own refusal path (`print\n<reason>`, `sv_client.cpp:550-560`).
    Bots and `localhost` (LAN/loopback) are never checked.
  - Entries are IPv4 `a.b.c.d` (`/32`) or `a.b.c.d/n` with `1 <= n <= 32`.
    Host bits are masked off. `/0` is refused: it would ban every address,
    which is never what an `addip` typo means (`g_filterBan 0` is the
    allow-only mode). Invalid file lines are reported with their line number
    and skipped.
  - A missing or unreadable file is an empty list (fail open) with one console
    line; the file is created by the first `addip`.
  - `addip`/`removeip` rewrite the file canonically (sorted, deduplicated,
    header), atomically (temporary file + `rename`). Comments are therefore not
    preserved by those commands; `reloadbans` never rewrites the file.
- The game module's `g_banIPs` is never written by the proxy. A non-empty
  legacy list is still enforced by the game's own `ClientConnect`; when the
  file is loaded, the proxy prints a one-line warning telling the admin to
  migrate the entries and clear the cvar.

## Why a file and not `g_banIPs`

The cvar path is capped at 256 bytes and fails fatally (above). A VPN range
list is tens of thousands of prefixes; even the in-memory `ipFilters` cap
(1024) and its non-CIDR mask rule make it unusable. The file has no such limit,
supports real ranges, is easy to generate and diff, and is not touched by the
engine's config writer.

## Verification

Unit tests (`crate::banfile`): parsing (bare address, CIDR, masked host bits,
comments, CRLF, duplicate lines, bad lines, `/0`, `/33`, leading zeros),
matching boundaries, canonical save/load round trip, the engine `ip` value
parsing (`a.b.c.d:port`, `localhost`, garbage), and the "covered by a wider
range" removal message.

Real-engine checks (i386 container + shipped 1.011 engine, proxy overlaid):

- `addip 172.30.0.0/24` → `proxy_bans.txt` contains the range, `listip` shows
  it, `g_banIPs` stays empty; a Huffman-compressed `connect` from that source
  gets `print\nBanned.`; after `removeip` the same connect reaches the game and
  is refused by userinfo validation instead (`model field not found`), proving
  the gate is specific.
- `addip 172.30.0.0/16`, then `removeip 172.30.0.1` → reports that the wider
  ban covers it.
- Hand-editing the file and running `reloadbans` picks it up.
- `proxy_sv_enableBanFile 0` restores the pristine `listip` (`g_banIPs`) and
  re-enabling reloads the file.
- `rust/scripts/engine-test.sh` still passes.

The connect probe is `tools/jka_connect_probe.py` (a port of the engine's
adaptive Huffman compressor, required because `SV_ConnectionlessPacket`
Huffman-decompresses every `connect` packet, `sv_main.cpp:533-535`).

## Consequences / limits

- Detection is out of scope: the file enforces ranges; it does not identify
  VPNs (D-017 note in the README).
- Bans apply at connect (and at the map-change reconnect). A player already in
  the game is not kicked by editing the file; `reloadbans` only changes the
  list used by the next connect.
- Comments are lost whenever `addip`/`removeip` rewrite the file.
- `reloadbans` is required for hand edits during a running session; a server
  restart or a cvar change also reloads it.
