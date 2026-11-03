# 0011 — Content refresh for the v0.11 line

Date: 2026-11-03. Status: accepted. Extends 0002 (content from `main`) and
0010 (v0.10.0 refresh).

## Context

The site was last content-synced against `main` at `98cb2fd` (ADR 0010).
`main` has since moved to `9ad9c99`, through v0.11.0 (`a009adf`) and 28
commits after the tag, changing what admins and players can do:

- **Player ignore** (`139b94b`, D-016):
  `proxy_sv_enableIgnore` (default `0`) lets any player hide another player's
  chat for themselves with `/ignore`/`/unignore`. The target shape is the same
  as `rcon mute`; the ignored player is never told and is not affected for
  anyone else. Only client chat is covered (`say`, `say_team`, `tell`,
  localized variants), not `voice_cmd`. Lists are per slot, cleared when either
  player leaves, kept across `map_restart`, reset by a full map change.
- **File-backed IP bans** (`40232ae`, D-017): `proxy_sv_enableBanFile`
  (default `0`) and `proxy_sv_banFile` (default `proxy_bans.txt`). The file is
  the source of truth; `addip`/`removeip`/`listip`/`reloadbans` work on it,
  entries may be IPv4 addresses or real CIDR ranges, and every connect is
  checked before the game sees the client. The stock `g_banIPs` path is a
  256-byte cvar that fails fatally past ~25 addresses.
- **GUID bans and `ban`** (`e86d9d3`, D-019): `proxy_sv_enableGuidBan`
  (default `0`) and `proxy_sv_guidBanFile` (default `proxy_guid_bans.txt`) keep
  a separate `ja_guid` registry. `ban <client|name>` bans the target's IP,
  adds its `ja_guid` when enabled and present, and kicks it; `addguid`/
  `removeguid`/`listguid` manage the registry. `ja_guid` is client-asserted, so
  the docs call the ban a deterrent, not a security boundary.
- **Shipped cloud/VPN list** (`45074c0`, D-020): `rust/proxy_bans.txt` (tens of
  thousands of ranges) and `tools/update_banlist.py` regenerate it from the
  AWS, Google Cloud, Azure, Oracle Cloud, DigitalOcean, Linode, Vultr and
  X4BNet feeds.
- **Anti-macro guard** (`e964988`, D-021): `proxy_sv_antiMacro` (default `0`)
  freezes a usercmd channel whose reversals form a run faster than a hand can
  (rate: ≤3 commands for movement, 65 ms for the view, 60 ms for attack;
  spread: holds within 2 commands after four reversals). It holds the last
  value instead of zeroing, releases 400 ms after the last reversal, and never
  notifies the player. `proxy_sv_antiMacroLog` levels `1`/`2`/`3` report
  transitions, locked-channel figures and every channel with a run, and work
  without the guard.
- **TFFA instance-switch rendering** (`77bd400`, `9ad9c99`, D-006.14): a
  per-viewer `CS_PLAYERS` correction now also sends the client's own
  `loaddefered` command, so deferred player models load immediately and
  in-game players keep their skin and animation instead of T-posing until the
  scoreboard opens; a following spectator gets the animation restart through
  the playerstate flips.
- **Engine and hardening fixes with no new cvar** (`f7c167d` … `9ad9c99`,
  D-018 and the fix commits): MSG_ReadBits bounds, `info`/`dumpuser` length
  checks, userinfo sanitisation and validation, stateless `getchallenge`,
  rcon/connectionless rate limiting, 512-byte client-command cap, callvote
  and gc-order bounds, movement-axis clamp, `writeconfig` filename validation.
  The README's built-in hardening list is unchanged, so the six-guard card
  stays as it is.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `9ad9c99`:
  **28** cvars (was 21). Add `proxy_sv_enableIgnore`, `proxy_sv_enableBanFile`,
  `proxy_sv_banFile`, `proxy_sv_enableGuidBan`, `proxy_sv_guidBanFile`,
  `proxy_sv_antiMacro` and `proxy_sv_antiMacroLog` with the upstream defaults
  and descriptions; existing rows are unchanged.
- Update the two cvar counts (hero stat, features heading) to **28**.
- Add **Player ignore**, **IP bans**, **GUID bans** and **Anti-macro guard**
  subsections to `#features`, each with the command/file/log samples taken
  from the README. IP bans includes the shipped blocklist and its generator.
- Hero tagline and features title now name ban lists and the anti-macro guard
  ("Hardening, bans, team tools"); the status pill names ban lists.
- Add a TFFA bullet for the instance-switch model/animation reload (D-006.14).
- No layout, dependency, asset or deploy changes, so 0001/0004 constraints
  still hold.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007/0008/0009/0010 stay historical).
- The target is `main` at `9ad9c99`, which contains v0.11.0 plus the post-tag
  ban/anti-macro work. There is no v0.12 tag yet; the site shows no version
  number, so nothing has to be renamed when one appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `9ad9c99`, update the cvar count if it changes, and re-check
  the four dropped C++ cvars are still absent from `main`.
