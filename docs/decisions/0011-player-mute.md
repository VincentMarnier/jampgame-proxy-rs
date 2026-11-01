# 0011 — Admin `mute` / `unmute` console commands and `proxy_sv_muteMode`

Date: 2026-10-27 (mode cvar added 2026-11-02)

## Context

Server admins need to silence a spamming or abusive player without kicking them.
Jedi Academy has no server-side mute: the game module only relays a player's
`say`/`say_team`/`tell`/`voice_cmd` client commands to the other players, so a
mute has to drop those commands before the game sees them. The admin command must
be usable both over `rcon` and from the dedicated server's own console, and must
accept the player number the scoreboard/`status` shows as well as the player's
name.

Some servers also want a *silent* mute: the player keeps chatting and sees
their own lines normally, so they cannot tell they are muted, while nobody else
receives them. The same command therefore has three modes, selected by
`proxy_sv_muteMode` (D-011).

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
- `G_Say` sends a `say`/`say_team` line to every appropriate client, **the
  speaker included** (`g_cmds.c:1573-1595`); that echo is what makes the line
  appear in the speaker's own chat. `G_SayTo` delivers it as `chat`/`lchat`
  (global) or `tchat`/`ltchat` (team) through `trap_SendServerCommand`
  (`g_cmds.c:1511-1522`). A `tell` goes to its target and then a second
  `G_Say(ent, ent, …)` echoes it to the sender.
- The proxy already intercepts `G_SEND_SERVER_COMMAND` (`rust/src/syscall.rs`,
  the same arm that isolates parallel-TFFA team chat), so it can drop a chat
  line per recipient while leaving the speaker's own echo alone. `voice_cmd`
  instead spawns a broadcast `EV_VOICECMD_SOUND` temp entity
  (`g_cmds.c:1688-1719`), not a server command: there is no line to show its
  author, so a silent mute still drops the command.
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
8. `proxy_sv_muteMode` (default `1`) selects the behaviour:
   - `0` — off: `mute`/`unmute` are still claimed so the pristine module does
     not broadcast `server: mute …`, but they only answer
     `proxy: mute is disabled (proxy_sv_muteMode 0).`; no chat is filtered and
     no notice is sent. Flags already set are left dormant and take effect
     again when the mode returns to `1`/`2`.
   - `1` — the stock mute described in item 6.
   - `2` — silent: the three text commands (`say`, `say_team`, `tell`) are
     forwarded to the game under a *shadow scope* (the speaking client number,
     held in `mute::SHADOW_CLIENT` for the duration of that synchronous
     `GAME_CLIENT_COMMAND` forward). The game builds and sends its normal chat
     line, and the `G_SEND_SERVER_COMMAND` trap drops every `chat`/`tchat`/
     `lchat`/`ltchat` recipient except the speaker — the speaker sees their own
     line, nobody else does. No mute, unmute or per-message notice is sent, so
     the mute is invisible. `voice_cmd` is dropped without a word (it has no
     chat line to echo). Values outside `0..=2` behave like `1`.

## Consequences and limits

- Replies appear in the rcon reply buffer (and on the dedicated console). In
  mode `1` other players are not told about a mute; only the muted player is.
  In mode `2` nobody — the muted player included — is told.
- In silent mode the game's own `G_Say` still logs `say:`/`sayteam:`/`tell:` to
  the server log and the dedicated console echoes the global line
  (`g_dedicated`), so the admin keeps a moderation trail; the line is only kept
  from the other *clients*.
- Silent mode goes through the game, so anything the game derives from the
  message before delivery — location prefixes, team colouring, the game log —
  is preserved for the speaker's own echo, unlike mode `1` which never lets the
  command reach the game.
- A `voice_cmd` in silent mode is dropped before the game, so the speaking
  player hears/sees nothing from it — the same as being ignored, and the absence
  of the expected voice line could still hint at a mute. Text chat has no such
  tell.
- While a shadow scope is open only that client's chat delivery is narrowed.
  `G_Say` addresses each recipient individually, so a broadcast chat (`-1`) is
  dropped as well; the stock code never broadcasts chat, and a mod that did
  would lose its author's echo rather than leak to everyone.
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

- `cargo test` — the `mute` tests cover: chat-command recognition (exact names,
  case, prefixes rejected), numeric/name/colour-code/prefix/substring/
  ambiguity/empty-name resolution, the exact→prefix→substring precedence, the
  ambiguity reply cap, disconnected slots not being players, the mute flag write
  (including a bot-style slot the mirror never saw), the reply lines
  (`plan`/`decide`), `mute_direction`, the usage/muted-list text, the mode
  mapping (`0`/`1`/`2`, unknown values → `1`), the per-mode command decision
  (stock drop-with-notice, silent text shadow / silent voice drop, off
  forwards everything), notice suppression in silent/off mode, the shadow-chat
  predicate (speaker kept, every other recipient and broadcast dropped,
  `chat`/`tchat`/`lchat`/`ltchat` recognised on raw bytes, non-chat untouched,
  no scope or another mode untouched), and the `state` restart carry /
  map-change reset.
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
- **Not yet exercised end to end**: a real client sending `say` while muted, in
  either mode. The enforcement path (`GAME_CLIENT_COMMAND` → drop or shadow,
  then the `G_SEND_SERVER_COMMAND` recipient filter) is unit-tested for the
  command-name and mode decisions and reviewed against `g_cmds.c:1471-1597`; a
  live two-client check remains open, and the live server above ran with bots
  only. Also not yet checked live: `proxy_sv_muteMode 0` on a running server.

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
- **Synthesising the muted speaker's chat echo in the proxy** (drop the command,
  `trap_SendServerCommand` a `chat` line back) — the wording, colour codes, `EC`
  markers and team/location variants would have to be duplicated, and would
  drift from the game's `G_Say`. Forwarding the real command under a shadow
  scope lets the game produce the exact line its client expects and only
  narrows who receives it.
- **Persisting mutes by name/GUID across sessions or maps** — out of scope: this
  is a session-level admin action, and a persistent store would need a policy for
  name changes and reconnects.
- **Muting by userinfo name from `trap_GetUserinfo` instead of `pers.netname`** —
  the game's `pers.netname` is what the scoreboard and `status` show; reading it
  from the live client keeps the reply consistent with what the admin sees.
