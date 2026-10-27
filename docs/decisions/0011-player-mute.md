# 0011 — Admin `mute` / `unmute` console commands

Date: 2026-10-27

## Context

Server admins need to silence a spamming or abusive player without kicking them.
Jedi Academy has no server-side mute: the game module only relays a player's
`say`/`say_team`/`tell`/`voice_cmd` client commands to the other players, so a
mute has to drop those commands before the game sees them. The admin command must
be usable both over `rcon` and from the dedicated server's own console, and must
accept the player number the scoreboard/`status` shows as well as the player's
name.

## Evidence

Paths are relative to the source trees named by `AGENTS.md`.

- An rcon packet is `SVC_RemoteCommand`: it validates the password, opens
  `Com_BeginRedirect`, rebuilds the payload from `Cmd_Argv(2..)` separated by
  single spaces, and runs it with `Cmd_ExecuteString`
  (`original/jedi-academy/codemp/server/sv_main.cpp:464-515`). The re-join loses
  the original quoting, so `rcon mute "Darth Vader"` arrives as
  `mute Darth Vader`.
- `Cmd_ExecuteString` tries the engine command table, then cvars, then the game
  module: `if (com_sv_running && com_sv_running->integer && SV_GameCommand())
  return;` (`original/jedi-academy/codemp/qcommon/cmd_console.cpp:110-129`).
- `SV_GameCommand` is `VM_Call(gvm, GAME_CONSOLE_COMMAND)`, and its qtrue answer
  tells the engine the command was consumed
  (`original/jedi-academy/codemp/server/sv_game.cpp:1755-1775`).
- The pristine `ConsoleCommand` knows `entitylist`, `forceteam`, `game_memory`,
  `addbot`, `botlist`, `addip`, `removeip`, `listip`; on a dedicated server
  **every other line is echoed to all clients** as `server: <text>` and consumed
  (`original/jedi-academy-sdk/codemp/game/g_svcmds.c:409-475`). An intercepted
  command must therefore answer qtrue, not forward.
- `GAME_CONSOLE_COMMAND` is 9 in `gameExport_t` (`g_public.h:758`; 0–8 are
  `GAME_INIT … GAME_RUN_FRAME`), the same ordinal the module's `vmMain` already
  switches on.
- The dedicated console goes through the same path: lines typed at the engine's
  console are executed with `Cmd_ExecuteString` (`qcommon/cmd_console.cpp`), so
  the handler is shared by rcon and the local console.
- Player communication is four client commands in the game's `ClientCommand`:
  `say` (3093), `say_team` (3097), `tell` (3108) and the voice menu's `voice_cmd`
  (3113) (`original/jedi-academy-sdk/codemp/game/g_cmds.c:3080-3120`). The engine
  delivers them to the module as `GAME_CLIENT_COMMAND` (`sv_client.cpp`,
  `SV_ClientCommand`).
- A game module answers the admin with `trap_Print` (`G_PRINT`,
  `g_public.h:105`); the engine routes it to `Com_Printf`
  (`sv_game.cpp:515-517`), which an open rcon redirect captures and returns to
  the remote admin (`sv_main.cpp`, `SV_FlushRedirect`). `trap_Print` is the only
  way to answer the rcon client without going through the game's chat.
- Bots are added by the game module itself: `G_AddBot` calls
  `trap_BotAllocateClient`, `trap_SetUserinfo` and then the game's own
  `ClientConnect`/`ClientBegin`/`ClientUserinfoChanged`
  (`original/jedi-academy-sdk/codemp/game/g_bot.c:890-1010`), so the engine never
  raises `GAME_CLIENT_BEGIN` for them. Observed live (2026-10-27): with two bots
  in game and `status` listing them, `rcon mute 0` answered "client slot 0 has
  no player" while the roster came from the proxy's connect mirror. The live
  roster therefore has to be read from the game state
  (`g_entities[n]->client->pers.connected`, `pers.netname`), exactly like
  `tffa::connected_clients` already does.
- `pers.connected == CON_DISCONNECTED` (0) marks an empty slot; `pers.netname` is
  a `char[MAX_NETNAME]` (`g_local.h`, `OFFSET_PERS_NETNAME == 48`, pinned in
  `sdk.rs`).
- The engine calls `GAME_CLIENT_DISCONNECT` when a client (bot included) is
  dropped (`SV_DropClient`, `sv_client.cpp`), which is the hook that must clear a
  mute: slots are reused by the next player.

## Decision

1. `mute`/`unmute` are claimed at `GAME_CONSOLE_COMMAND` before the pristine
   module sees them, and the proxy answers qtrue. They work over rcon and from
   the dedicated console; every other console command keeps the pristine path.
2. `mute <target>` silences, `unmute <target>` restores. `<target>` is:
   - a whole decimal word → the client slot (`rcon mute 0`, the clientNum
     `status` and the scoreboard show);
   - anything else → a player name, matched against the colour-stripped
     `pers.netname` in three tiers, the first one that matches deciding:
     exact (case-insensitive, colour codes ignored on both sides), then a
     **prefix**, then the fragment **anywhere** in the name (`mute ade` finds
     "Darth Vader"). Several players in the winning tier are refused with their
     slots and names rather than guessed.
   - A bare `mute`/`unmute` prints the usage and the players currently muted.
3. The target is the concatenation of every argument from 1 on (the game's own
   `ConcatArgs`, already used for chat), because rcon loses the quotes. This is
   what makes `rcon mute darth vader` name "Darth Vader"; it is a no-op for
   `mute 0`.
4. The roster (who occupies which slot, and their names) is read from the live
   game state (`g_entities`), with the proxy's connect mirror as the fallback
   before the world is located. Slots with `pers.connected == CON_DISCONNECTED`
   are not players.
5. The mute is a flag on the proxy's per-client entry
   (`state::ClientEntry::muted`), keyed by client slot:
   - it is cleared on `GAME_CLIENT_DISCONNECT` even when the connect mirror never
     saw the slot begin (bots), so it can never be inherited by the next player
     in that slot;
   - it survives a `map_restart` (players keep their slots);
   - a full map change resets it with the rest of the session state
     (`state::reset_session`), since every slot is handed to a fresh connection.
6. Enforcement is in `GAME_CLIENT_COMMAND`: a muted client's `say`, `say_team`,
   `tell` and `voice_cmd` are dropped (the game never sees them) and the player
   gets a `print` saying they are muted — once per dropped command, so a player
   who keeps trying keeps being told. Every other client command is forwarded
   untouched. The admin's reply and the player notices go through `trap_Print` /
   `trap_SendServerCommand`.
7. The feature follows the master switch: while `proxy_sv_enable` is `0` the
   command is not claimed (the pristine dedicated server answers `server: mute …`)
   and chat is not filtered.

## Consequences and limits

- Replies appear in the rcon reply buffer (and on the dedicated console). Other
  players are not told about a mute; only the muted player is.
- Client numbers are slots, so they change as players come and go — the `mute`
  list always shows the current number and name.
- A player whose name is a plain number cannot be targeted by name (the word is
  read as a slot); use the slot number or rename.
- Prefix and substring matching make `mute dar` and `mute ade` work, but a
  fragment shared by several players is refused with the candidates
  (`#0 "Darth"`, `#3 "Darker"`). The reply lists at most 8 of them plus
  `+N more (see status)`: each printed line is its own rcon packet and the
  engine's redirect buffer is 1008 bytes (`SVC_RemoteCommand`'s `sv_outputbuf`,
  the proxy keeps that patched length — `docs/inventory.md`), so a full 32-slot
  list would be truncated mid-name.
- The mute is per slot, not per identity: a player who reconnects is unmuted, and
  a mute is not written to disk. This is deliberate (an admin action belongs to
  the session), and it is what keeps a reused slot from carrying a stranger's
  mute.
- Only the four stock client commands are covered. A mod that adds its own chat
  command (a different client command name) would bypass the mute; base JKA's
  `voice_cmd` is included because it is the voice menu's communication channel.
- Bots can be muted by name/number, but they never issue these client commands
  (`bot_chat` is not `GAME_CLIENT_COMMAND`), so the flag has no visible effect on
  them. A `map_restart` keeps the flag on the slot, which may belong to a
  different bot after the restart re-adds them; harmless but worth knowing.
- The rcon payload is capped by the engine's 1024-byte `remaining` buffer and the
  tokenizer, so an extremely long target is truncated before it reaches the
  command.

## Verification

- `cargo test` — 14 new tests in `mute` plus one in `state`: chat-command
  recognition (exact names, case, prefixes rejected), numeric/name/colour-code/
  prefix/substring/ambiguity/empty-name resolution, the exact→prefix→substring
  precedence, the ambiguity reply cap, disconnected slots not being players, the
  mute flag write (including a bot-style slot the mirror never saw), the reply
  lines (`plan`/`decide`), `mute_direction`, the usage/muted-list text, and the
  `state` restart carry / map-change reset.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` — clean.
- `rust/dev.sh ./scripts/build.sh` — i386 cdylib builds and stages as a 32-bit
  ELF shared object.
- **Live against the shipped 2003 engine** (2026-10-27, `mp/ffa1`, bots, via
  `rust/scripts/run-server.sh`, driven over UDP with a one-off rcon client):
  - `rcon status` lists slots 0/1; `rcon mute 0` answers
    `proxy: muted #0 "Rosh Penin"`, `rcon mute` answers the usage plus
    `proxy: muted players: #0 "Rosh Penin"`.
  - Name targeting works case- and colour-insensitively, by unique prefix
    (`mute rosh`) and with spaces after rcon dropped the quotes
    (`mute Imperial Saboteur` → `proxy: muted #1 "Imperial Saboteur"`), and
    `unmute Rosh Penin` clears it.
  - A fragment from the middle of a name resolves too (the requested
    `mute ade` shape): `mute rgan` → `proxy: muted #0 "Morgan Katarn"`,
    `mute kat` → the same bot through the prefix tier, `mute twin` → `#1 "Reborn
    Twin"`, and `mute a` — a fragment every name contains — was answered with
    the candidate list (`#0 "Morgan Katarn", #2 "Rodian"`) instead of a mute.
  - Bad targets are refused: `mute nobody` → `no player named "nobody"`,
    `mute 9` → `client slot 9 has no player`, `mute 1 2` → `no player named
    "1 2"`.
  - `rcon kick allbots` (the engine's `kick` drops the bots through
    `GAME_CLIENT_DISCONNECT`) left `mute` reporting `no player is muted`, i.e.
    the flag was cleared on disconnect even though the connect mirror never saw
    the bot slot begin.
  - `rcon mute 0` followed by `rcon map_restart` kept `#0 "<bot>"` muted after
    the restart (the bot re-entered the same slot), as intended.
  - `rcon proxy_sv_enable 0` then `rcon mute 0` produced the pristine
    `broadcast: print "server: mute 0\n"` echo instead of a proxy reply, i.e. the
    command is left to the game while the master switch is off; `proxy_sv_enable
    1` restored the handler with the mute still in place.
  - No `server: mute …` line reached the clients while the proxy was enabled
    (checked in the server log), so the command was consumed by the proxy rather
    than echoed by the pristine `ConsoleCommand`.
- **Not yet exercised end to end**: a real client sending `say` while muted. The
  enforcement path (`GAME_CLIENT_COMMAND` → drop + notice) is unit-tested for the
  command-name decision and reviewed against `g_cmds.c:3093-3120`; a live
  two-client check remains open, and the live server above ran with bots only.

## Alternatives considered

- **Patching the game's `ClientCommand` / `G_Say`** — the proxy already runs
  `GAME_CLIENT_COMMAND` before forwarding, so a game-side hook would add an
  engine patch and a trampoline for no gain (D-002/minimal alteration).
- **Implementing the command in the game module's console path via a game hook**
  (`ConsoleCommand` detour) — `vmMain(GAME_CONSOLE_COMMAND)` is our own entry
  point; a trampoline into the game's `ConsoleCommand` would only add a failure
  mode when the original module is missing.
- **Broadcasting the mute/unmute to all players** — noisy, and it tells the muted
  player's target nothing useful; the admin and the player are the only parties
  acted on.
- **Persisting mutes by name/GUID across sessions or maps** — out of scope: this
  is a session-level admin action, and a persistent store would need a policy for
  name changes and reconnects.
- **Muting by userinfo name from `trap_GetUserinfo` instead of `pers.netname`** —
  the game's `pers.netname` is what the scoreboard and `status` show; reading it
  from the live client keeps the reply consistent with what the admin sees.
