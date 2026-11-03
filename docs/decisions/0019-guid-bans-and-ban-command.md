# D-019 — `ja_guid` ban registry and `rcon ban`

Date: 2026-11-03. Status: accepted (user directive). Feature:
`proxy_sv_enableGuidBan` / `proxy_sv_guidBanFile`, `crate::guidban`,
`rcon ban` (`crate::ban`), extending D-017.

## Context

Modern JKA clients send a `ja_guid` userinfo key. The value is **supplied by
the client**: the player can change it at will, and it is not cryptographically
bound to an install. It is still useful as a cheap persistent identity: keeping
a known value out of the server stops the casual case, and it survives reconnects
and IP changes. It is explicitly not a security boundary and is documented as
such.

Two constraints drove the design:

1. **Never let a `ja_guid` reach the IP ban file.** The value is
   attacker-controlled userinfo. If it were appended to `proxy_bans.txt` (or
   parsed with the IP grammar), a crafted value could look like an address range
   or inject a line. The registry is a separate file and a separate cvar, and a
   GUID is stored as a strict token that cannot be an IPv4 range.
2. **Do not make the admin do three commands.** The requested `rcon ban
   <id|name>` resolves the target once, bans its IP (through the active IP ban
   mechanism), adds its `ja_guid` when the GUID registry is enabled, and kicks
   it.

## Decision

### Registry (`crate::guidban`)

- Cvars (both `CVAR_ARCHIVE`): `proxy_sv_enableGuidBan` (default `0`) and
  `proxy_sv_guidBanFile` (default `proxy_guid_bans.txt`, relative to the
  server's working directory like `proxy_debug.log`, or absolute).
- File: one token per line, `#` comments, blank lines skipped. A token is
  1-64 printable ASCII characters, excluding whitespace, `#`, `\` and `"`;
  normalised to lower case (GUIDs are hex/UUID-like and case-insensitive).
  Invalid lines are reported with their line number and skipped.
- Canonical writes (sorted, deduplicated, header) through the same libc
  `open`/`write`/`fsync`/`rename` helper as the IP list
  (`crate::banfile::write_atomic`): file first, memory second.
- Loaded at startup / on cvar change, reloaded by `reloadbans` (which now
  reloads both registries).
- `GAME_CLIENT_CONNECT`: read the connect userinfo, normalise `ja_guid`, binary
  search the list. A match returns the stock `"Banned."`; bots and malformed or
  missing values are not checked. The check runs after the IP gate and before
  the game's userinfo validation.

### Commands

- `addguid <guid>` / `removeguid <guid>` / `listguid` — proxy-only (claimed
  even when the feature is off, where they answer that it is disabled);
  `listguid` prints the first 100 entries and a count.
- `rcon ban <clientNumber|playerName>` (`crate::ban`):
  1. resolve with the shared `player_target` resolver (same replies as
     `mute`: empty slot, unknown name, ambiguous list);
  2. read the target's live userinfo (`trap_GetUserinfo`), split the engine's
     `ip` value from its `:port` (`banfile::parse_userinfo_ip`) and take
     `ja_guid`;
  3. IP: when `proxy_sv_enableBanFile 1`, `banfile::ban_address` adds a `/32`
     to the file; when it is off, queue the pristine
     `addip <ip>` with `G_SEND_CONSOLE_COMMAND`/`EXEC_APPEND` (the same shape
     the snapshot-counter wrap fix uses), so the active `addip` is always the
     one used;
  4. GUID: when `proxy_sv_enableGuidBan 1`, validate and add the value; a
     client without `ja_guid` (stock client) or with an invalid one is
     reported, and the IP half still happens;
  5. kick with `trap_DropClient(slot, "Banned.")`.
  The reply is one summary line naming each half as added / already present /
  not banned and the reason.
- The command exists even when both registries are off: it then only kicks
  (and reports that no registry took an entry), matching the requested
  "add IP per the cvar, GUID if its cvar is on, kick".

## Security notes

- A client can change `ja_guid` to any value, so a GUID ban does not stop a
  determined cheater; it stops the same install rejoining casually. It must not
  be treated as an identity or an authorisation check.
- A player could present another player's GUID (values are not secret: they
  travel in userinfo), so an admin banning a spoofed value could ban the value's
  real owner too. This is inherent to a self-asserted ID.
- The strict token grammar and the separate file mean a malicious `ja_guid`
  can never add an IP entry or a second line anywhere.
- Enforcement is at connect time; editing the file does not kick a player who
  is already in the game (`ban` does, by design). A later userinfo change to a
  banned value is not re-checked; the connection gate is the requested policy.

## Verification

Unit tests (`crate::guidban`, `crate::ban`, `crate::banfile`): GUID validation
(case folding, length, whitespace/`#`/`\`/`"` refused), file parsing with
comments/duplicates/bad lines, canonical round trip, the shared `ip` parsing,
and the legacy `addip` fallback text. `cargo test`: 336 passing; `clippy
--all-targets` clean.

Real-engine checks (i386 container, shipped 1.011 engine, proxy overlaid):

- `addguid 0123456789ABCDEF` → file contains the lower-case value,
  `listguid` prints it; a probe sending `ja_guid=0123456789ABCDEF` is refused
  with `print\nBanned.`; after `removeguid` the same probe reaches the game
  (`connectResponse` with a full userinfo).
- `ban BanTarget` (a probe connected with `--valid --name BanTarget --extra
  ja_guid=CAFEBABE01`) → `proxy: banned #4 "BanTarget": ip 172.30.0.1 banned;
  GUID cafebabe01 banned; kicked.` The engine logged the disconnect, both files
  contained the entries, `listip`/`listguid` showed them.
- Removing only the IP (`removeip 172.30.0.1`) left the probe refused
  (`Banned.`) — the GUID gate is independently effective; removing the GUID let
  it connect again.
- An ambiguous name (`Probe` twice) was refused with the candidate list; no
  registry was touched.
- The drive tool is `tools/jka_connect_probe.py` (`--valid` sends the retail
  mandatory userinfo, `--extra ja_guid=...` adds the GUID).

## Consequences / limits

- The registry is a list of opaque tokens; it does not validate that a value
  looks like a GUID issued by any particular client.
- Case is folded, so two values differing only by case are one entry.
- `addguid`/`removeguid` rewrite the file canonically (comments are lost on
  rewrite, as in D-017); hand edits are picked up by `reloadbans`.
- The IP half of `ban` uses the pristine `addip` when the IP file is off, with
  the game's ~25-entry `g_banIPs` limitation (D-017); enabling
  `proxy_sv_enableBanFile` is the recommended configuration.
