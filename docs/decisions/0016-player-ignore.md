# 0016 — Player `ignore` / `unignore` commands and `proxy_sv_enableIgnore`

Date: 2026-11-02

## Context

The admin `mute` command (D-011) silences a player for the whole server. Players
also need a *personal* mute: one player wants to stop seeing another player's
chat without asking an admin, and without affecting anyone else's chat. The
command must accept the same kinds of target an admin already knows from
`rcon mute` (the client number or part of the name), and the whole feature must
be switchable by a server cvar.

## Evidence

Paths are relative to the source trees named by `AGENTS.md`.

- The client console forwards a command it does not handle itself: after the
  engine, cvar, cgame, server-game and UI command tables decline it,
  `Cmd_ExecuteString` calls `CL_ForwardCommandToServer`
  (`original/jedi-academy/codemp/qcommon/cmd_console.cpp:130-135`). The function
  requires `cls.state >= CA_CONNECTED` and adds the line as a reliable client
  command (`original/jedi-academy/codemp/client/cl_main.cpp:507-538`).
- A leading `/` is stripped by the console before the line is queued, so
  `/ignore Darth` executes as `ignore Darth`
  (`original/jedi-academy/codemp/client/cl_keys.cpp:756-770`).
- The engine delivers a reliable client command to the game module as
  `GAME_CLIENT_COMMAND`; the proxy already claims commands there (the `tffa_*`
  family and the muted player's chat, D-006/D-011). A command the proxy returns
  from is never seen by the pristine `ClientCommand`
  (`original/jedi-academy-sdk/codemp/game/g_cmds.c:3073-3207`).
- `G_Say` builds one `chat`/`tchat`/`lchat`/`ltchat` line and delivers it to
  each recipient individually with `trap_SendServerCommand(other, …)`, the
  speaker included (`original/jedi-academy-sdk/codemp/game/g_cmds.c:1475-1597`).
  `tell` uses the same line names: the target receives one copy and the sender
  a second echo (`g_cmds.c:1627-1658`).
- The proxy already intercepts `G_SEND_SERVER_COMMAND` and knows the recipient
  (`a0`) and the raw command bytes (`rust/src/syscall.rs`), the same place the
  silent mute and the TFFA team-chat filter work.
- The chat command is emitted synchronously while the issuing client's
  `GAME_CLIENT_COMMAND` forward runs, which is what the silent mute's shadow
  scope already relies on (`mute::SHADOW_CLIENT`, D-011).
- A game module's client-visible `print` is one `trap_SendServerCommand`
  (`print "…"`), the mechanism the `tffa_*` commands already use for replies.

## Decision

1. `ignore` and `unignore` are claimed in `GAME_CLIENT_COMMAND` before the
   game's `ClientCommand`; the proxy answers the player with `print` lines and
   forwards nothing. A player may use them while muted (the mute covers chat
   only). Every other client command keeps the pristine path.
2. The target is exactly the `mute` target (D-011), shared through
   `crate::player_target`: a whole decimal word is the client slot; anything
   else is a player name matched against the colour-stripped live
   `pers.netname`, case-insensitively, first by exact match, then prefix, then
   the fragment anywhere in the name. The whole argument tail is joined with
   `ConcatArgs`, so `/ignore Darth Vader` names the player even though the
   client command tokeniser loses the quotes. An ambiguous fragment is answered
   with the candidate slots and names rather than guessed.
3. `ignore <target>` sets the target slot in the issuing player's mask;
   `unignore <target>` clears it. A bare `ignore`/`unignore` prints the usage
   and the players currently ignored, capped at eight names plus a `+N more`
   count. Ignoring yourself is refused. No notice is sent to the ignored
   player: the list is personal and invisible to its subject.
4. The mask is a `u32` bit per client slot on
   `state::ClientEntry::ignored`, keyed by slot like the admin mute:
   - it is cleared on `GAME_CLIENT_DISCONNECT` for both directions — the
     leaving player's own list, and their slot from every other player's list,
     so the next player in the reused slot is not silently ignored;
   - it survives a `map_restart` (players keep their slots);
   - a full map change resets it with the rest of the session state.
5. Enforcement is in the `G_SEND_SERVER_COMMAND` trap. The speaker is the
   client whose `GAME_CLIENT_COMMAND` forward is in flight
   (`ignore::ACTING_SENDER`, opened around the forward in `lib.rs`). A
   `chat`/`tchat`/`lchat`/`ltchat` line is dropped for a recipient whose mask
   holds the speaker; everyone else keeps receiving it, and the speaker always
   keeps their own echo. A broadcast (`-1`) cannot be split per recipient and is
   left alone (the stock game never broadcasts chat).
6. Replies are `print` server commands. The client re-tokenizes each one
   (`Cmd_TokenizeString`, `qcommon/cmd_common.cpp:541-634`) and a quoted token
   ends at the first inner `"`, so the payload never contains one: quotes around
   names and the echoed target are written as `'`, backslashes become `/`, and
   control characters become spaces. The trailing newline therefore stays
   inside the closed token.
7. The feature is gated by `proxy_sv_enableIgnore` (default `0`):
   - `0` — off: `ignore`/`unignore` are still claimed (so the game does not
     answer an unknown client command) but only reply
     `proxy: ignore is disabled (proxy_sv_enableIgnore 0).`; no chat is filtered,
     and masks already set are left dormant until the feature is turned on;
   - non-zero — on.
   Like every proxy feature it also needs `proxy_sv_enable 1`; with the master
   switch off the commands are not claimed at all.

## Consequences and limits

- Replies go to the issuing player's console only. The ignored player is never
  told, so a player cannot tell they are being ignored.
- Player replies quote names and the echoed target with `'`, not `"`: a `print`
  server command is re-tokenized client-side and cannot carry an inner double
  quote without losing its tail. The admin `mute` replies keep `"` because they
  are written with `trap_Print` straight to the console/rcon redirect.
- Only client chat is covered (`say`, `say_team`, `tell`, including the localized
  `lchat`/`ltchat` shapes). `voice_cmd` is a broadcast sound event, not a chat
  line, so an ignore cannot silence it per recipient; that remains the admin
  mute's job.
- Chat not emitted from a client's `ClientCommand` (bot chat, a mod's own
  server-generated line) has no speaker in the acting scope and is therefore not
  filtered. A mod that introduces a different chat client command also bypasses
  the filter.
- The ignore is per slot, not per identity: a player who reconnects starts with
  an empty list, and a list pointing at a slot follows whoever occupies that
  slot until one of them disconnects. The disconnect cleanup makes that window
  end at the slot's reuse.
- The server log and the dedicated console still record the ignored player's
  lines (`G_Say` logs before delivery), so moderation remains auditable.
- While the feature is off, existing masks are inert and come back when it is
  switched on again; the mask data is not erased by the cvar.

## Verification

- `cargo test` — `ignore` tests cover command recognition (exact names, case,
  non-matches), chat-line recognition on raw bytes (`chat`/`tchat`/`lchat`/
  `ltchat`, non-chat untouched), the per-player bit set/clear (idempotence,
  isolation between players and slots, out-of-table refusal), the reply
  decisions (set/clear wording, self refusal, empty/unknown/ambiguous targets,
  usage and ignored list, the eight-name cap), disconnect cleanup in both
  directions, and the recipient filter (ignoring recipient dropped, other
  recipient kept, speaker kept, broadcast left alone, feature off inert, no
  acting scope inert). `player_target` tests cover the shared numeric/name/
  colour-code/prefix/substring/ambiguity resolution moved out of `mute`, and
  `mute` keeps its own flag, reply, mode and shadow-chat tests. `state` tests
  cover the `map_restart` carry and the full-map reset of the masks, plus the
  new cvar default.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` — clean.
- `rust/dev.sh ./scripts/build.sh` — i386 cdylib builds and stages as a 32-bit
  ELF shared object.
- **Live against the shipped 2003 engine** (2026-11-02, `mp/ffa1`, `g_gametype
  0`, no bots, in the `jampgame-investigation:i386-bookworm` container) with
  the committed harness `tools/runtime/a4_ignore.sh` (`--mode enabled` and
  `--mode disabled`). It reuses the A2 synthetic client
  (`tools/runtime/a2_netstatus_groundtruth.py` `ClientModel`, three clients:
  Ignorer, Speaker, Witness), records every `svc_serverCommand` and sends client
  commands through the real reliable command path:
  - `proxy_sv_enableIgnore 1`, with two players sharing the prefix `Speak`
    (Speaker, SpeakerTwo): `ignore Speak` was answered with
    `proxy: 'Speak' matches #1 'Speaker', #2 'SpeakerTwo'; use the client
    number.\n`, proving the whole ambiguity payload (and its newline) arrives
    after the client-side tokenizer. `ignore Speaker` then answered
    `proxy: you are now ignoring #1 'Speaker'\n`. The Speaker sent
    `say live-ignore-check`: the Speaker kept its own `chat` echo and the
    Witness received it, the Ignorer received nothing. After
    `unignore Speaker`, the next `say after-unignore` reached the Ignorer again
    (`proxy: you are no longer ignoring #1 'Speaker'\n`).
  - Every captured proxy `print` was run through a port of
    `Cmd_TokenizeString` and the harness requires its argument to end with the
    newline: an inner `"` would truncate it (the bug this check guards).
  - Feature off (the cvar absent): the Ignorer's `ignore Speak` was answered
    with `print "proxy: ignore is disabled (proxy_sv_enableIgnore 0).\n"`, and
    the Speaker's next `say` reached the Ignorer, the Speaker and the Witness —
    no filtering and no dormant mask.
- **Not yet exercised live**: a real two-client game (the run above uses the
  synthetic protocol clients, which do enter the world and chat through the
  real client-command path); `map_restart` carry and the disconnect cleanup are
  unit-tested only.

## Alternatives considered

- **Filtering in the client (cgame)** — the proxy has no cgame control, and a
  client-side filter would be trivially bypassed and only work on mods that ship
  the matching cgame.
- **Reusing the admin mute flag** — a server-wide mute is an admin decision;
  a player's ignore list must not affect anyone else, so it needs its own
  per-client mask.
- **Parsing the speaker out of the chat text** — `G_Say` prefixes the line with
  the speaker's `pers.netname`, but names may contain colour codes and duplicate
  each other, so text matching is ambiguous. The synchronous acting-sender scope
  used by the silent mute (D-011) names the speaker exactly.
- **Fanning a broadcast chat out per client** to apply the per-recipient mask —
  the stock game delivers chat individually, so this would add complexity (and
  a second copy of each line) only for mods that broadcast chat; leaving a
  broadcast untouched matches the stock path.
- **Persisting ignore lists by name/GUID** — out of scope: they are a per-session
  client preference and would need a policy for renames and reconnects.
