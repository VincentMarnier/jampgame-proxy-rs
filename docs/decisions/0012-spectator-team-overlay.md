# 0012 — Spectator team overlay (HP/armor)

Date: 2026-11-01

## Context

TaystJK's `cg_drawTeamOverlay` styles (and the stock cgame's `CG_TEAMINFO` HUD
element) draw their rows — name, health, armor, weapon, powerups — from
`cgs.clientinfo[]` and `sortedTeamPlayers`, which the server fills with the
`tinfo` reliable command. The pristine game module only sends `tinfo` to
players who are on RED/BLUE and only lists their teammates, so a spectating
client has an empty (`numSortedTeamPlayers == 0`) or stale roster and the
overlay shows nothing useful while spectating — even though the cgame is
already willing to draw it. The request: give spectators the roster (every
in-game player's HP) so `cg_drawTeamOverlay` works for a spectator with the
latest TaystJK cgame, while staying compatible with stock/original clients.

## Evidence

Paths are relative to the source trees named by `AGENTS.md`.

- **What the cgame draws.** `CG_DrawUpperRight` calls
  `CG_DrawTeamOverlay`/`2`/`3` only when `cgs.gametype >= GT_TEAM` and
  `cg_drawTeamOverlay.integer` (`TaystJK/codemp/cgame/cg_draw.c:6910-6919`).
  All three styles return early unless
  `cg.snap->ps.persistant[PERS_TEAM]` is `TEAM_RED`/`TEAM_BLUE`
  (`cg_draw.c:6108-6110`, `6570-6574`). They read `ci->health`/`ci->armor`
  from `cgs.clientinfo` (`CG_TeamOverlayStats`, `cg_draw.c:6048-6075`),
  except for the row of `cg.snap->ps.clientNum`, whose values come from the
  live `cg.snap->ps.stats`; the overlay only ever considers
  `min(8, numSortedTeamPlayers)` entries (`cg_draw.c:6117`). jaPRO's packed
  `armor*100 + forcePower` field is decoded only when
  `cgs.serverMod == SVMOD_JAPRO` (`cg_draw.c:6055-6066`), and `serverMod` is
  derived from the serverinfo `gamename` (`cg_servercmds.c:252-302`):
  the shipped pristine module advertises `basejka` (`strings
  original/jalinuxded_1.011/jampgamei386.so`), i.e. `SVMOD_BASEJKA`, so its
  `tinfo` armor is raw.
- **What the stock cgame parses.** `CG_ParseTeamInfo` reads
  `tinfo <count>` then a 6-int stride per player — `clientNum location health
  armor weapon powerups` — with no bounds checks on `count` or on the client
  number (`original/jedi-academy/codemp/cgame/cg_servercmds.c:82-97`);
  `sortedTeamPlayers` is `int[TEAM_MAXOVERLAY]` (`TEAM_MAXOVERLAY` 32,
  `bg_public.h:1031`). The TaystJK cgame uses the same stride and adds range
  checks that drop the client on a bad count (`cg_servercmds.c:104-127`).
- **What the pristine game sends.** `TeamplayInfoMessage` returns unless
  `ent->client->pers.teamInfo`, selects up to `TEAM_MAXOVERLAY` players whose
  `sess.sessionTeam` equals the recipient's, sorts them by client number and
  writes `"%i %i %i %i %i %i"` per player with
  `ps.stats[STAT_HEALTH]`, `ps.stats[STAT_ARMOR]` (both clamped at 0),
  `ps.weapon`, `s.powerups` and `pers.teamState.location`
  (`original/jedi-academy-sdk/codemp/game/g_team.c:1094-1150`).
  `CheckTeamStatus` calls it once per second (`TEAM_LOCATION_UPDATE_TIME`,
  `bg_public.h:1028`) and only for in-use clients on RED/BLUE
  (`g_team.c:1156-1191`). `pers.teamInfo` is forced on for
  `g_gametype >= GT_TEAM`, otherwise read from the `teamoverlay` userinfo key
  (`g_client.c:2158-2168`).
- **The shipped binary matches that source.** Disassembly of
  `TeamplayInfoMessage` (`0x1656a4`) shows `pers.teamInfo` at `+0x6a8`
  (1704 = `pers` 1552 + 152), the team comparison on
  `client + 0x6ac` (1708 = `sess.sessionTeam`), health at `[client+0xf4]`
  (244), armor at `[client+0x108]` (264) with no `*100` packing, weapon at
  `[client+0xb0]` (176), powerups at `[gentity+0x124]` (292) and location at
  `[client+0x674]` (1652 = `pers` + 100). It is the SDK behaviour, so the
  overlay gap is real for the pristine module.
- **Why a following spectator is otherwise ready.** `SpectatorClientEndFrame`
  copies the followed player's `playerState` into the spectator and sets
  `PMF_FOLLOW` (`original/jedi-academy/codemp/game/g_active.c:3655-3680`), so
  the spectator's `cg.snap->ps.persistant[PERS_TEAM]` is the followed player's
  team and the cgame's gate passes. `sess.spectatorClient == -1`/`-2` resolves
  through `level.follow1`/`follow2` (`g_active.c:3660-3668`).
- **Reliable-command budget.** `SV_AddServerCommand` copies into a
  `char[MAX_STRING_CHARS]` slot with `Q_strncpyz`
  (`original/jedi-academy/codemp/server/sv_main.cpp`), i.e. a longer command is
  truncated; the proxy's own send helper drops anything above 1022 bytes.
- **Bots have no proxy client mirror entry.** The proxy's per-client
  `is_connected` flag is fed by the `GAME_CLIENT_CONNECT` event; bots are
  added by the game itself and never arrive through it (observed live
  2026-11-01: the mirror was empty with three bots in game while the game's
  own `pers.connected == CON_CONNECTED`). Recipient selection therefore reads
  `pers.connected` from the game module.

## Decision

Add `proxy_sv_spectatorTeamOverlay` (default `1`; archived like every proxy
cvar), active only while `proxy_sv_enable` is `1` and `g_gametype >= GT_TEAM`.
`crate::teamoverlay` runs after every pristine `G_RunFrame`:

1. Every second (`TEAM_LOCATION_UPDATE_TIME`) — and immediately when the
   followed player's team or parallel-TFFA instance changes — one `tinfo`
   reliable command is sent to each spectator that is
   `sess.spectatorState == SPECTATOR_FOLLOW`, follows a RED/BLUE player, has
   `pers.connected == CON_CONNECTED` and has `pers.teamInfo` set. The target
   for `sess.spectatorClient == -1`/`-2` is `level.follow1`/`follow2`, exactly
   like `SpectatorClientEndFrame`.
2. The roster is the followed player's team only, ascending by client number,
   capped at `TEAM_MAXOVERLAY` and at the reliable-command byte budget (whole
   entries only, header count rewritten to what was written). One team only:
   the cgame looks at the first 8 entries, so a mixed roster would hide players
   of one side; the roster of the other team arrives on the next follow change.
3. The payload is the stock six-int entry
   `clientNum location health armor weapon powerups` with raw
   `STAT_ARMOR` (the pristine module's `gamename` makes the cgame treat it as
   base JKA), health and armor clamped at 0 like the game clamps them.
4. Under parallel TFFA the roster is restricted to the followed player's match
   (`TffaState::client_match`), since the game module has no notion of the
   instances.
5. The game's own `tinfo` for teammates is left completely untouched; the
   proxy only adds messages the pristine game never sends.

Compatibility is by construction rather than by a capability probe: the
message is the stock cgame's own format (a spectator's clientinfo entries
would otherwise simply stay at their initial values), the count stays within
the array size stock 1.01 declares, client numbers are always real slots, and
health/armor are the same raw values the game sends to teammates. Stock
clients cannot be worse off; a stock client that does show a team overlay
(e.g. the `CG_TEAMINFO` HUD element) gets the same populated roster.

## Consequences and limits

- The overlay appears while the spectator **follows a player**. In free-fly
  (`SPECTATOR_FREE`) the spectator's own `playerState` is sent and its
  `PERS_TEAM` is `TEAM_SPECTATOR`, so the cgame returns before drawing; that
  gate is client-side and cannot be changed from the server.
- The followed player's own row is live at snapshot rate (the cgame reads it
  from `cg.snap->ps`); every other row is up to one second old, exactly like a
  teammate's overlay.
- One reliable command per second per following spectator (at most ~1 KB with
  32 players), acked normally. A client that stops acking accumulates them
  like any other reliable command; the engine's existing 128-slot handling
  applies unchanged.
- Bots are served too (they have no cgame, so the messages are simply ignored).
- `proxy_sv_antiHpTeller` is unrelated: it quantises `EV_PAIN` events, while
  teammates (and now spectators) already receive exact `tinfo` health.
- TaystJK's `CG_UpdateFlagStatus` scans `cgs.clientinfo[i]` for
  `i < numSortedTeamPlayers` (`cg_draw.c:3961-3985`) — a pre-existing client
  bug that now also runs for spectators with a populated roster. It can only
  affect the CTF HUD's carrier name; it never indexes outside
  `cgs.clientinfo`.
- The feature is a server-side information disclosure (spectators see exact
  HP/armor); an admin can turn it off with
  `proxy_sv_spectatorTeamOverlay 0`.

## Verification

- `cargo test` — 206 tests, including the new `teamoverlay` ones: stock six-int
  layout and field order, health/armor clamping, empty roster, `TEAM_MAXOVERLAY`
  cap, reliable-buffer budget (whole entries, header count), team-only roster,
  parallel-match filter, `followed_player` resolution (follow target,
  `follow1`/`follow2`, free-fly, non-spectator slots, `pers.connected`,
  `pers.teamInfo`) and the roster read against a synthetic `g_entities`/
  `g_client_t` image laid out at the SDK offsets.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `rust/dev.sh ./scripts/build.sh` — 32-bit i386 ELF shared object staged as
  `jampgamei386.so`.
- **Live check** (2026-11-01, `jampgame-investigation:i386-bookworm`,
  `linuxjampded` with the proxy as `base/jampgamei386.so`, `g_gametype 6`,
  `bot_minplayers 3`, map `mp/duel1`): gdb patched a bot's `client` into
  `sess.sessionTeam = TEAM_SPECTATOR`, `spectatorState = SPECTATOR_FOLLOW`,
  `spectatorClient = 0`, `pers.teamInfo = 1`. Dumping the engine
  `client_t.reliableCommands` ring for that slot showed fresh one-per-second
  messages in the stock layout, e.g.
  `tinfo 2 0 0 105 25 3 0 2 0 125 25 3 0` — clients 0 and 2 (the followed
  BLUE players), viewer slot excluded, raw armor 25, health tracking the game
  state. `rcon proxy_sv_spectatorTeamOverlay 0` froze that client's reliable
  sequence (280 → 280 over 3 s); setting it back to `1` resumed it
  (283 → 286 over 3 s).

## Alternatives considered

- **A TaystJK capability bit + serverinfo key** (like `taystJKinfo` bit 7 /
  `taystJKcgflags` bit 0 for scoreboard deaths): rejected — the message format
  is unchanged stock `tinfo`, so there is nothing for a cgame to negotiate,
  and this works with the latest TaystJK cgame as shipped.
- **Listing both teams in one roster** so a follow switch needs no new
  message: rejected — the cgame only inspects the first
  `min(8, numSortedTeamPlayers)` entries, so with more than eight players
  total one team's players would silently disappear. Team-scoped rosters plus
  an immediate resend on scope change give the same responsiveness.
- **Hooking the game's `TeamplayInfoMessage`/`CheckTeamStatus`** to extend
  them: rejected — `CheckTeamStatus` never calls the function for spectators,
  and the pristine body builds an empty roster for a `TEAM_SPECTATOR`
  recipient; synthesising the command from the same fields is simpler and
  needs no new detour.
- **Gating on the proxy's `is_connected` client mirror**: rejected after the
  live check showed bots never populate it.
