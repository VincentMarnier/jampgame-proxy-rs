# JKA_YBEProxy 2026 changes: what the Rust rewrite is exposed to

Date: 2026-11-02. Status: **audit only** — no fix was implemented. One pure
helper was extracted for testability and unit tests were added (see
[Tests added](#tests-added)); no observable behaviour changed.

This document answers one question: among the 2026 work of the C++ proxy (now
developed as **JKA_YBEProxy**), which fixes matter to *this* Rust rewrite, and
which of them are still missing here?

## 1. What was reviewed

Upstream repository: `/home/vincent/Documents/JKA_YBEProxy` (same early history
as the original `jampgame_proxy`; the 2019 commits are identical). Reviewed:

- the 115 commits from `3582193` (2026-10-24, "docs: move the docs into the
  repository + audit") to `c7067bf` (2026-11-01, "docs: add the release
  procedure");
- the two audit rounds they produced:
  - `docs/audit-2026-10-25/` — findings **F1–F11**, **O1–O12**, **A1–A9**;
  - `docs/audit-2026-11-01/` — findings **F1–F8**, **H1**;
- the fix commits themselves (`d9daa68` … `013c7a7` for the second audit;
  `e54cb88` … `ed11d28` for the O-series), so that the *implemented* rule — not
  only the write-up — is what this comparison uses.

Our baseline: the C++ snapshot pinned by D-002 is
`687997412ea6e5ead93f5c7b25db552590a2eeb1` (2026-02-21, tag `0.0.7`). The 2026
audits are from October–November 2026, **after** that snapshot, so none of them
is in the Rust port by construction. Some concerns are nevertheless already
covered because the Rust port has its own equivalent (D-007 module pinning,
bounds-checked `SV_SvEntityForGentity`, bounded tokenizer, q3infoboom guard).

Evidence labels in this document:

- **AUDIT** — stated by the upstream audit (source and, where noted there,
  binary evidence; not re-run here);
- **SOURCE (ours)** — read in this repository (`rust/src`, `original/…`);
- **TESTED** — covered by a `cargo test` in this repository;
- **INFERRED** — a reasonable reading that was not verified by running a
  server.

Nothing in this document was reproduced on a running server.

## 2. Verdict at a glance

Legend: **LIVE** = the Rust rewrite is exposed; **FIXED** = addressed in
`rust/src` on 2026-11-02; **COVERED** = an equivalent mitigation exists here;
**N/A** = not applicable (different design, out of scope, or not present);
**ACCEPTED** = known and deliberately left open upstream.

| Upstream ID | Subject | Upstream severity | Our state |
|---|---|---|---|
| 11-01 F1 | command line length → `G_LogPrintf` stack overflow | Critical | **FIXED** 2026-11-02 |
| 11-01 F2 | `callvote clientkick <n>` out of client slots | High | **FIXED** 2026-11-02 |
| 11-01 F3 | `gc <player> 7` reads past `gc_orders` | High (Linux) | **FIXED** 2026-11-02 |
| 11-01 F4 | `forcepowers` with 16/17 digits or digits 4–9 | Medium | **FIXED** 2026-11-02 |
| 11-01 F5 | `color1`/`color2` fill the gamestate | Medium | **FIXED** 2026-11-02 |
| 11-01 F6 | `vote` parsing / `callvote kick <name>` lookup | Low, not security | **LIVE** |
| 11-01 F7 | `modelFullPath` overflow with `d_perPlayerGhoul2` | Low | **ACCEPTED** (unreachable on retail) |
| 11-01 F8 | kick vote can kick the next client of the slot | Low | **FIXED** 2026-11-02 |
| 11-01 H1 | refuse `"`/control characters in chat | — | **N/A** (refuted) |
| 10-25 F1 | crash on `rcon map` / `rcon map_restart` | Critical | **COVERED** (D-007) |
| 10-25 F2 | userinfo not sanitised on connect | High | **FIXED** 2026-11-02 |
| 10-25 F3 | anti-cheat false positive / bypass | Critical | **COVERED** (minor model gaps) |
| 10-25 F4 | OpenJK API path | High | **N/A** |
| 10-25 F5 | wrong cvars read | High | **N/A** (not ported) |
| 10-25 F6 | rcon/getstatus/getinfo without rate limiting | High | **FIXED** 2026-11-02 |
| 10-25 F7 | `writeconfig` filter bypass | Medium | **FIXED** 2026-11-02 |
| 10-25 F8 | unbounded index in `SV_SvEntityForGentity` | Low | **COVERED** (stricter) |
| 10-25 F9 | `netStatus` modifies engine state | Medium | **N/A** (different design) |
| 10-25 F10 | low-priority bugs | Low | **N/A / one cosmetic gap** |
| 10-25 F11 | `dumpuser` crash on a long userinfo value | Medium | **FIXED** 2026-11-02 |
| O1 | `ip` userinfo key not maintained | High | **FIXED** 2026-11-02 |
| O2 | out-of-band rate limiting | High | **FIXED** 2026-11-02 (= F6) |
| O3 | userinfo sanitised before the game sees it | High | **LIVE** (= F2) |
| O4 | `writeconfig` | Medium | **FIXED** 2026-11-02 (= F7) |
| O5 | connections per IP address | Medium, optional | **MISSING** (availability) |
| O6 | server commands > 1022 chars | Do not port | **N/A** |
| O7 | tokenizer reads its bounded copy | Low | **COVERED** |
| O8 | userinfo validation bypass | High | **FIXED** 2026-11-02 |
| O9 | movement events that lag every player | Medium | **ACCEPTED** (not fixed upstream) |
| O10 | `forwardmove` of −128 | Low | **FIXED** 2026-11-02 |
| O11 | `MSG_ReadBits` reads past the message | Low | **FIXED** 2026-11-02 |
| O12 | `Info_SetValueForKey` writes one byte past | Medium | **FIXED** 2026-11-02 |

Additional 2026 fixes that are not in either audit are listed in
[Section 4](#4-crash-and-availability-fixes-outside-the-two-audits).

## 3. Live security concerns, by priority

### S1. Command line longer than 512 characters at intermission — Critical — **FIXED 2026-11-02**

Upstream: 11-01 F1, commit `d9daa68` (*"fix: drop client commands longer than
512 characters"*).

**Cause (AUDIT).** At intermission the retail `ClientCommand` turns every
command outside its known list into public chat via `Cmd_Say_f(ent, qfalse,
qtrue)`. With `arg0` set, that calls `ConcatArgs(0)` — the whole command line —
and `G_Say` logs it *before* cutting it to 150 characters:
`G_LogPrintf("say: %s: %s\n", netname, chatText)`. `G_LogPrintf` writes with
`vsprintf` into `char string[1024]` with no size, and even calls `G_Printf`
(another unbounded `vsprintf` in 1024 bytes) on a dedicated server. Upstream
counts a maximum of 1073 bytes written from a 1024-byte buffer: 49 bytes past
the end, over the saved registers and the return address, with text chosen by
the player. Any client, spectator included, can trigger it at every map end.

This is confirmed in our own `original/jedi-academy-sdk/codemp/game` copy
(**SOURCE (ours)**): `g_main.c:2197-2223` (`G_LogPrintf`, `vsprintf(string + 7,
…)`) and `g_cmds.c:3216-3229` (the intermission fallback to `Cmd_Say_f`).

**Our code before the fix (SOURCE (ours)).** `shared_api::client_command`
(`rust/src/shared_api.rs:174-369`) is the same choke point as the C++
`Allowed`. It read the command and `args_concat = concat_args(1)` and only
bounded chat (256 characters). There was **no whole-command-line limit**, so
every other command name could carry up to the engine's 1022-character
`ConcatArgs(0)`, exactly the length the 2026 fix removes. The `\r\n`/`;`
filters do not shrink it.

**Impact (INFERRED from AUDIT).** Remote stack corruption / crash of the
server at every intermission, on our exact target (`linuxjampded` i386, the
platform where upstream measured the overflow). A crash is certain when the
function returns; code execution is plausible but was not checked upstream.

**Why the existing `Com_Printf` hardening does not cover it.** The Rust proxy
retargets the *engine's* `Com_Printf` to a `vsnprintf` shim
(`patch.rs`/`hooks/mod.rs:300-301`). The overflow is inside the **game module's**
`G_LogPrintf`/`G_Printf`, which the proxy does not patch. The only safe place
is the command filter, as upstream chose.

#### Applied (2026-11-02)

A direct port of `d9daa68`:

- `MAX_COMMAND_LENGTH = 512` (`shared_api.rs:32`) and the pure predicate
  `command_line_too_long(cmd, args)` (`shared_api.rs:733-735`), which is
  literally `cmd.len() + 1 + args.len() > 512` — the separating space is
  counted even with no arguments, as upstream.
- The check runs in `client_command` right after `args_concat` is computed
  (`shared_api.rs:223`). That is after the `jkaDST_` anti-cheat check and after
  the mute filter (which runs in `lib.rs` before this function), and before the
  chat and command checks, matching upstream's placement. A dropped line is
  logged as `command line longer than 512 bytes` when `proxy_sv_debugLog` is
  active; otherwise it is dropped silently, as upstream.
- 512 leaves a wide margin: the overflow starts at about 970 characters
  (upstream's measurement). Legitimate chat is cut at 150 by the game and at
  256 by the existing rule; no base-game command needs more.

**Test (TESTED).** `command_line_is_capped_at_512_bytes`
(`shared_api.rs`, `mod tests`) pins the boundary: 512 accepted, 513 rejected,
the unconditional `+ 1` space, and a 1022-character attack line rejected. The
filter itself needs the game module (`concat_args`); the runtime check is in
[Section 6](#6-tests-and-test-plan).

### S2. `callvote clientkick <n>` outside the client slots — High — **FIXED 2026-11-02**

Upstream: 11-01 F2, commit `94c89b0` (*"fix: drop callvote clientkick outside
the client slots"*).

**Cause (AUDIT; our SDK confirms).** `Cmd_CallVote_f` compares the number only
with `MAX_CLIENTS` (32), then reads `g_entities[n].client->pers.connected`
(`g_cmds.c:1993-2001` in our SDK copy). `G_InitGame` clears `g_entities` and
sets `client` only for `i < level.maxclients`, so from `sv_maxclients` to 31
`client` is `NULL` and the read crashes the server. Triggered by any voter when
`g_allowVote` is on (default) and `sv_maxclients < 32`.

**Our code before the fix (SOURCE (ours)).** The `callvote` block of
`client_command` validated only `capturelimit`, `fraglimit`, `g_doWarmup`,
`map_restart` and `timelimit`, plus a `MAX_CVAR_VALUE_STRING` length guard.
`clientkick` was not in the table, so the value reached the game unchanged.

**Impact.** Remote crash, same conditions as upstream.

#### Applied (2026-11-02)

A direct port of `94c89b0`, using the range table the other votes already go
through:

- `clientkick_max_slot(sv_maxclients) = sv_maxclients - 1`
  (`shared_api.rs:755-757`).
- The `clientkick` entry in the `callvote` table (`shared_api.rs:308-317`)
  sets `min = 0`, `max = clientkick_max_slot(sv_maxclients)`, so the existing
  final test `vote_value_out_of_range(value, min, max)`
  (`shared_api.rs:762-764`, also extracted) drops both directions:
  `value < 0` and `value >= sv_maxclients`. Upstream's applied code uses the
  same `min = 0`, so a negative number is refused by the proxy as well, even
  though the game would also refuse it.
- `sv_maxclients` is read through the engine at the moment of the vote, as the
  `gc` guard does. It is a latched cvar: the game reads the same value.
- The command name argument is still compared by prefix
  (`q_stricmpn(arg1, "clientkick", 10)`), as the rest of the 2019 table; the
  game compares it whole. This only over-blocks unknown vote names.

**Tests (TESTED).** Three vectors in `mod tests`, composed from the two
production helpers:
`clientkick_accepts_slots_below_sv_maxclients` (0 and the last slot for 1, 8
and 32 slots), `clientkick_drops_the_slot_at_or_above_sv_maxclients`
(`sv_maxclients`, `MAX_CLIENTS - 1`, 32 on a 32-slot server) and
`clientkick_drops_a_negative_slot`. The runtime check (`callvote clientkick
31` with `sv_maxclients 8`) is in [Section 6](#6-tests-and-test-plan).

### S3. `gc <player> 7` reads past `gc_orders` — High on Linux (our target) — **FIXED 2026-11-02**

Upstream: 11-01 F3, commit `85403cd` (*"fix: drop gc orders past the end of the
game's list"*).

**Cause (AUDIT; our SDK confirms).** `Cmd_GameCommand_f` tests
`order > sizeof(gc_orders)/sizeof(char *)` (`g_cmds.c:1744` in our SDK copy).
The array has 7 entries, so order 7 passes and `gc_orders[7]` — one pointer
past the end — is passed to `G_Say`. On the **Linux retail module** that entry
is not a pointer but the bytes `"prin"` (0x6e697270), so `G_Say` reads at an
unmapped address and the server crashes. Upstream reproduced the crash on
Linux; on Windows the neighbouring array yields the harmless text `Free For
All`.

**Our code before the fix (SOURCE (ours)).** The `gc` guard in
`client_command` only bounded the *client number*:
`utils::atoi(cmd_arg1) >= sv_maxclients`. `cmd_arg2` (the order) was never
inspected, so any in-game client could send `gc <valid-player> 7`.

**Impact.** Remote crash on the platform this rewrite targets. Upstream rates
it High because it is Linux-only; here Linux is the only target.

#### Applied (2026-11-02)

A direct port of `85403cd`, with the same choices:

- `GC_ORDERS_COUNT = 7` (`shared_api.rs:34-36`), from `g_cmds.c:1721-1729`.
- The pure predicate `gc_command_denied(player, order, sv_maxclients)`
  (`shared_api.rs:747-749`, Helpers) is
  `atoi(player) >= sv_maxclients || atoi(order) >= GC_ORDERS_COUNT`. The
  `client_command` guard (`shared_api.rs:252-269`) calls it and drops the
  command with a debug-log line.
- **Negative player and order are left to the game**, as upstream does: the
  game already returns on `player < 0` and `order < 0`. Only the two holes the
  game does not cover are closed: `order == 7` (one past `gc_orders`) and
  `player >= sv_maxclients` (where the slot's `client` pointer is `NULL`).
- The command name is still compared by prefix (`q_stricmpn(cmd, "gc", 2)`),
  as in the 2019 port; the game compares it whole. This over-blocks unknown
  commands that start with `gc`, which the game ignores anyway. Switching the
  whole command table to exact comparisons is the separate 10-25 F10.4 item.

**Tests (TESTED).** Four vectors in `mod tests`:
`gc_accepts_a_real_slot_and_a_listed_order`,
`gc_drops_the_order_past_the_end`,
`gc_drops_a_player_outside_the_configured_slots` and
`gc_leaves_negative_player_and_order_to_the_game`. They cover the last valid
order (6), order 7/8/999, a player at `sv_maxclients` and `MAX_CLIENTS - 1`,
the same player on a 32-slot server, `atoi` treating text as 0, and the
negative cases the game owns. The runtime reproduction on Linux is in
[Section 6](#6-tests-and-test-plan).

### S4. `forcepowers` with 16 or 17 power levels, or levels above 3 — Medium — **FIXED 2026-11-02**

Upstream: 11-01 F4, commit `2b96214` (*"fix: require 18 force power levels from
0 to 3 in forcepowers"*).

**Cause (AUDIT; our SDK confirms).** `BG_LegalizedForcePowers` declares
`int final_Powers[NUM_FORCE_POWERS]` uninitialised (`bg_misc.c:452` in our SDK
copy). The read loop stops at the end of the string, so with fewer than 18
digits after the second `-` the tail keeps stack garbage, which is later used as
a loop counter and as the column of `bgForcePowerCost[i][countDown]`. A large
value reads far past the table and can crash the server. A digit 4–9 is also
used as a column before being clamped to 3 (20 bytes past the table, no crash).
The retail client always sends `<rank>-<side>-` plus 18 digits in `0..=3`
(22 characters).

The old proxy rule accepted 18 shapes of 22–24 characters; **8 of them carry
only 16 or 17 powers**. The 2026 fix requires at least 18 digits after the
second `-` and rejects digits outside `0..=3`.

**Our code before the fix (SOURCE (ours)).** `forcepowers_is_valid` kept the
old shape rule: 22–24 characters, digits only, exactly two `-` at positions
1–5, never adjacent. It did **not** require 18 digits and accepted `4`–`9`.

**Impact.** A bad `forcepowers` string could leave 16/17 `final_Powers[]`
entries uninitialised, or use a `4`–`9` column, on every connect and every
`userinfo` command.

#### Applied (2026-11-02)

A direct port of `2b96214` in `forcepowers_is_valid`
(`shared_api.rs:682-723`):

- after the second `-`, require at least `NUM_FORCE_POWERS` (18) digits — the
  number `BG_LegalizedForcePowers` reads;
- require every power digit to be in `0..=3`;
- the rank and side digits (before the second `-`) stay unrestricted: the game
  replaces the rank and checks the side itself;
- the rest of the shape (22–24 characters, exactly two `-` at positions 1–5,
  never adjacent, digits only) is unchanged.

The invalid string still gets the default
`7-1-030000000000003332` written in its place.

**Tests (TESTED).**
`f4_requires_18_power_digits` enumerates the old 18-shape window and asserts
only the **10 long shapes** pass, each with at least 18 powers;
`f4_rejects_power_digits_above_three` covers a `9` in the power list;
`f4_leaves_rank_and_side_digits_unrestricted` covers `9-9-…`;
`retail_forcepowers_value_is_accepted` and
`length_and_separator_shape_is_enforced` guard the rest. The runtime check
(connect with `123-1-` + 16 digits, and with a `9`) is in
[Section 6](#6-tests-and-test-plan).

### S5. `color1` and `color2` are copied whole into `CS_PLAYERS` — Medium — **FIXED 2026-11-02**

Upstream: 11-01 F5, commit `01adba1` (*"fix: cut color1 and color2 to 15
characters"*).

**Cause (AUDIT).** `ClientUserinfoChanged` copies both keys as they are into
the player's `CS_PLAYERS` configstring (`g_client.c:2187-2188` in our SDK
copy), which is part of the gamestate every client holds in
`MAX_GAMESTATE_CHARS` (16000). Once the total exceeds it, every client is
dropped (`ERR_DROP` in `cl_cgame.cpp`) and nobody can join. Several players
must cooperate (about 10–15 by upstream's estimate). The 2026 fix cuts each at
15 characters, keeping the TaystJK/EternalJK hat/cape names (`<color><name>`,
1 + 13 characters).

**Our code before the fix (SOURCE (ours)).** `sanitize_userinfo` touched
`name`, `model` and `forcepowers` only; the engine-side `SV_UserinfoChanged`
wrapper touched `model` only. `color1`/`color2` were never read or rewritten.

**Impact.** Multi-client denial of service (gamestate overflow) on the retail
client. Not a server crash.

#### Applied (2026-11-02)

A direct port of `01adba1` in the sanitizer core, after the model fix and
before `forcepowers`:

- `truncate_colors` (`shared_api.rs:771-779`) reads `color1` and `color2` and,
  when a value is longer than 15 characters, writes back its first 15 with the
  checked writer. Failure (impossible for a shorter rewrite of a validated
  value, but handled) refuses with `Userinfo string length exceeded.`
- 15 keeps `<colour><name>`: 1 digit plus up to 13 characters of a TaystJK /
  EternalJK hat or cape name.

**Tests (TESTED, 1 new).** `sanitizer_cuts_color1_and_color2_to_15` sends
27-character values and asserts the output keeps exactly the first 15
characters for both keys. The runtime check (long colours, then the game's
`CS_PLAYERS`) is in [Section 6](#6-tests-and-test-plan).

### S6. Userinfo validation (duplicate keys, failed `Info_SetValueForKey`) — High — **FIXED 2026-11-02**

Upstream: O8, commits `3b3b252` (*"fix: O8"*) and the F2 work.

**Cause (AUDIT).** The proxy reads the userinfo with `Info_ValueForKey`, which
matches **without case**, but `Info_SetValueForKey` removes keys **with case**
and silently adds nothing when the result would reach `MAX_INFO_STRING`. A
userinfo close to 1024 characters with a capitalised `Forcepowers` (or two
`forcepowers` keys) therefore passes the sanitizer unchanged: the bad value is
never replaced, and the game reads it case-insensitively. OpenJK's
`G_ValidateUserinfo` (size, slashes, control characters, per-field counts) plus
a result-returning `Info_SetValueForKey` close it.

**Our code before the fix (SOURCE (ours)).** There was no userinfo validation:
no `validate_userinfo`/`g_userinfoValidate` equivalent anywhere under
`rust/src`. `sanitize_userinfo` called the game's `info_set_value_for_key`,
which returns `void` and silently adds nothing when the result does not fit;
the Rust binding could not see the failure. The engine-side model write did the
same.

**Impact.** Bypassed every userinfo-based fix (F4 forcepowers, model rules,
name cleaning) with a crafted 1024-byte userinfo; also the F11 `dumpuser` crash
below.

#### Applied (2026-11-02)

A port of `3b3b252` with two parts:

1. **Own info-string helpers** in `rust/src/userinfo.rs`, because the game's
   `Info_SetValueForKey` returns `void`:
   - `value_for_key` (`userinfo.rs:145-159`) — first pair, key compared without
     case, as `Info_ValueForKey`;
   - `set_value_for_key` / `set_value_for_key_vec`
     (`userinfo.rs:177-230`) — the fixed writer: forbids `\`, `;`, `"`,
     removes the key **with case** (`Info_RemoveKey`) and prepends the pair,
     returning `false` when the result would reach `MAX_INFO_STRING`. The bound
     is `>=`, not the retail `>`, which also closes O12 for the proxy's own
     writes.
   - `validate_userinfo` (`userinfo.rs:286-338`) — OpenJK's
     `G_ValidateUserinfo` with its default settings and the same 21-field
     table: length, leading/trailing slash, even slash count, `\n \r ; "`,
     and the per-field counts compared without case. Extended ASCII is not
     checked. No cvar: the default is hardcoded, as upstream.
2. **Sanitiser integration** (`shared_api.rs`):
   - `sanitize_userinfo_bytes` (`shared_api.rs:453-576`) is the pure core:
     validation first (when `validate`), then name, model and forcepowers, with
     every write checked. A failed write returns
     `"Userinfo string length exceeded."` instead of leaving the old value.
   - `sanitize_userinfo` (`shared_api.rs:586-632`) is the FFI wrapper: reads
     the engine userinfo, calls the core, stores `clean_name`, writes back. A
     refusal returns the reason instead; nothing is written.
   - `GAME_CLIENT_USERINFO_CHANGED` validates every non-bot client: the
     `is_bot` flag is now tracked in `state::ClientEntry` (set on every
     `client_connect`, cleared on disconnect). A validation failure drops the
     client with `Failed userinfo validation: <detail>` and no anti-cheat chat;
     a `darksidetools` model keeps the chat.
   - The engine-side `SV_UserinfoChanged` wrapper uses the checked writer for
     its `model` → `kyle` rewrite and calls `SV_DropClient` when it does not
     fit (`hooks/engine_sv.rs:746-750`), as JK2MV and upstream do.
   - The unused game `Info_SetValueForKey` binding was removed from
     `jampgame.rs`; nothing calls the unchecked game writer anymore.

**Tests (TESTED, 10 new).** In `userinfo.rs`: the info helpers (case-sensitive
remove, case-insensitive read, forbidden characters, the `>=` bound) and the
validation shapes (missing, duplicate/capitalised, `cl_guid`, two `ip`,
slashes, control characters). In `shared_api.rs`, the full core with no engine:
bad name/model/forcepowers are rewritten; a duplicate key is refused; the O8
shape (990 bytes + capitalised `Forcepowers`) is refused with
`Userinfo string length exceeded.`; a `darksidetools` model is refused without
writing; bots skip validation.

The runtime reproduction is in [Section 6](#6-tests-and-test-plan).

### S7. `dumpuser` crashes on a long userinfo value — Medium — **FIXED 2026-11-02**

Upstream: 10-25 F11, applied as an `Info_Print` rewrite (option 2).

**Cause (AUDIT).** `Info_Print` copies each value into `char value[512]` with no
bound. A client can plant a 512+ character value in its userinfo (the
userinfo command accepts up to `MAX_INFO_STRING` = 1024); the next routine
`rcon dumpuser` on that client overflows the stack and kills the server. OpenJK
uses `BIG_INFO_VALUE` (8192).

**Our code before the fix (SOURCE (ours)).** There was no `Info_Print` hook;
the engine's pristine 512-byte key/value buffers remained.

**Impact.** Remote-triggered admin crash: the client plants the value, the
server dies on the next `dumpuser` (a normal admin command).

#### Applied (2026-11-02)

The 2026 option 2, as a full rewrite (the original is never called):

- `info_print_lines` (`hooks/engine_sv.rs:471-504`) reproduces the retail
  parsing and layout: a key shorter than 20 characters is padded with spaces
  to 20; a value is everything up to the next `\`; a key without a value is
  `MISSING VALUE`. The buffers are growable `Vec`s, so no 512-byte limit.
- `info_print` (`hooks/engine_sv.rs:600-625`) prints each line through the
  engine's `Com_Printf` with the retail formats (`"%s"` for the padded key,
  the value plus newline, or `MISSING VALUE\n`).
- A new `Info_Print` hook (`patch.rs`, `0x8073504`, prologue
  `55 8b ec 81 ec 20 04 00 00`), with no trampoline use.
- The ordinals/helpers: `engine::functions::INFO_PRINT`.

**Tests (TESTED, 3 new).** `info_print_pads_short_keys_and_keeps_long_values`
(a 600-character value is kept, the retail 512 buffer would overflow),
`info_print_reports_a_missing_value`, and
`info_print_leaves_a_long_key_unpadded`. The live `rcon dumpuser` check is in
[Section 6](#6-tests-and-test-plan).

### S8. `Info_SetValueForKey` writes one byte past the buffer — Medium — **FIXED 2026-11-02**

Upstream: O12, commit `ed11d28` (*"fix: O12"*).

**Cause (AUDIT).** The retail `Info_SetValueForKey` refuses a pair only when
`strlen(newi) + strlen(s) > MAX_INFO_STRING`. A sum of exactly 1024 passes,
then `strcat` + `strcpy` write 1024 characters plus the terminator into a
1024-byte buffer. A `connect` packet with a 1007-character userinfo reaches it
through `SV_DirectConnect`'s local `userinfo[1024]` after the engine appends
`\ip\<address>` (17 characters). The fix flips `jbe` to `jb` at four
immediate-byte sites, on both functions and platforms.

**Our code before the fix (SOURCE (ours)).** The Rust port had no such inline
patch: the engine patches applied at attach did not touch the length test.

**Impact.** One-byte stack overflow on connect (crash/corruption depending on
what follows the local buffer). Triggerable unauthenticated with a valid
challenge.

#### Applied (2026-11-02)

A direct port of `ed11d28`, for the Linux engine:

- `engine::patches` now holds the two sites and the byte values
  (`INFO_SET_VALUE_FOR_KEY_LENGTH_JBE_ADDR = 0x807fe24`,
  `..._BIG_LENGTH_JBE_ADDR = 0x8080024`, pristine `0x76`, patched `0x72`),
  verified on `linuxjampded` (`cmp $0x400,%ebx; jbe` and
  `cmp $0x2000,%ebx; jbe`).
- `attach_all` (`hooks/mod.rs`) flips both bytes to `jb`; `revert_inline_patches`
  restores `jbe` when the master switch is off, so `proxy_sv_enable 0` gives
  the pristine engine.
- This covers every engine caller, not only `SV_DirectConnect`: `connect`
  userinfo, `serverinfo`/`systeminfo` (the `_Big` variant) and anything else
  that writes an info string near the boundary. The proxy's own writes already
  use the fixed `>=` in `userinfo::set_value_for_key`.

**Tests (TESTED, 1 new).**
`info_set_value_for_key_length_sites_are_pinned` pins both addresses, the
pristine/patched bytes and their presence in the engine text range. The
behavioural check remains the runtime one in
[Section 6](#6-tests-and-test-plan).

### S9. `ip` userinfo key is not maintained — High — **FIXED 2026-11-02**

Upstream: O1, commit `e54cb88` (*"fix: O1"*).

**Cause (AUDIT).** JKA sets `ip` once in `SV_DirectConnect` and never again.
The `userinfo` command replaces the whole userinfo, so a client can send a fake
`ip` or omit it. `G_FilterPacket` and the game's ban check then read the fake
address: an `addip` ban (which is only evaluated again in `ClientConnect`, run
for every client at each map change) does not apply to a player already on the
server. The fix re-derives `ip` from `netchan.remoteAddress` in
`SV_UserinfoChanged` and refuses a connect without room for it, as OpenJK.

**Our code before the fix (SOURCE (ours)).** `sv_userinfo_changed` checked
only the `model` key and then called the original. There was no `ip` repair
and no length refusal; `client_connect` sent the banner only.

**Impact.** IP-ban bypass on map change; misleading game logs. Note this also
undermined any future IP-based feature (O5).

#### Applied (2026-11-02)

Both halves of `e54cb88`:

- `hooks/engine_sv::refresh_ip` (`hooks/engine_sv.rs:646-683`), called at the
  start of `sv_userinfo_changed` (`:556-619`) before the model fix and the
  original:
  - `ip = "localhost"` for `NA_LOOPBACK`, otherwise the engine's own
    `NET_AdrToString(remoteAddress)` (`a.b.c.d:port`, `bot`, …);
  - writes it with the checked `userinfo::set_value_for_key`;
  - on failure, `SV_DropClient(cl, "userinfo string length exceeded")` and the
    original is not called.
- The S6 pure core (`shared_api.rs:453-576`) refuses a **validated** userinfo
  with no `ip` (`Userinfo string length exceeded.`). That covers the connect
  packet too long for the engine to append `ip` in `SV_DirectConnect`. The
  userinfo-change path is already repaired by `refresh_ip` before
  `GAME_CLIENT_USERINFO_CHANGED` runs.
- Bots keep an `ip` too (`NET_AdrToString` gives `bot`); the connect
  requirement is skipped for them, as upstream.

**Tests (TESTED, 1 new).**
`sanitizer_requires_ip_for_a_validated_connect` pins the connect refusal and
the bot exemption. `refresh_ip`'s address formatting and `SV_DropClient` path
need the runtime check (fake `userinfo`, then `map_restart`, then read the
game's `ip`).

### S10. Userinfo is not sanitised on connect — High — **FIXED 2026-11-02**

Upstream: 10-25 F2, applied in the connect path (`GAME_CLIENT_CONNECT`).

**Cause (AUDIT).** The engine calls `GAME_CLIENT_CONNECT` **before**
`SV_UserinfoChanged` (`server/sv_client.cpp:517-530`), and the game's
`ClientConnect` calls `ClientUserinfoChanged` directly (not through `vmMain`).
The proxy's sanitizer only ran on the later `userinfo` command, so a malicious
client could set every bad value in the connect packet and never send a
`userinfo` command afterwards. The fixes bypassed this way were forcepowers,
model length and name cleaning. The fix runs the sanitizer at the start of
`GAME_CLIENT_CONNECT` for non-bots, before forwarding, returning the reason as
the game's own "Banned." refusal.

**Our code before the fix (SOURCE (ours)).** `GAME_CLIENT_CONNECT` called
`shared_api::client_connect`, `tffa::on_client_connect` and
`teamlock::on_client_connect`, then forwarded. There was **no** sanitizer
call there; the only one ran on `GAME_CLIENT_USERINFO_CHANGED`. The engine-side
`SV_UserinfoChanged` wrapper is installed but runs after the game's connect
(and only fixes the model).

**Impact.** The 2019 sanitizer we ported was bypassable on first spawn. This
made S4 (forcepowers) and the model rules effective only after a subsequent
`userinfo` command, and compounded S6.

#### Applied (2026-11-02)

The upstream design, split into a sanitiser and its two callers:

- `sanitize_userinfo_bytes` (`shared_api.rs:453-576`) is the old
  `client_userinfo_changed` body as a pure core. It stops with the
  `darksidetools` anti-cheat reason on that model, **without producing a
  userinfo to write**; otherwise it sanitises and returns the new bytes. The
  FFI wrapper `sanitize_userinfo` (`shared_api.rs:586-632`) reads the engine
  userinfo, calls the core, and writes the result with `SetUserinfo`, turning a
  rejection into `Option<SanitizeRejection>`.
- `GAME_CLIENT_CONNECT` (`lib.rs:153-195`) runs it for every non-bot connect
  (first connection and the map-change reconnect) via
  `connect_userinfo_sanitized` (`shared_api.rs:375-377`), **before**
  `client_connect` and before forwarding. On a reason it returns the reason
  pointer from `vmMain`; the engine treats a non-null `GAME_CLIENT_CONNECT`
  return as a refusal (its own `Banned.` path), prints the reason to the client
  and never enters the game. No banner, no TFFA/teamlock bookkeeping, no game
  call.
- `client_userinfo_changed` (`shared_api.rs:641-666`) keeps the change-path
  behaviour: on the same reason it sends the anti-cheat chat and calls
  `DropClient`, then returns `false`; `vmMain` (`lib.rs:301-308`) returns 0
  instead of forwarding `GAME_CLIENT_USERINFO_CHANGED`.
- That last point also finishes the 10-25 F3 part "after the drop, return the
  reason; do not call `SetUserinfo` and do not forward": the game no longer
  rewrites `CS_PLAYERS` for a slot that is being cleared (the ghost-player
  configstring).
- Writing the userinfo during `GAME_CLIENT_CONNECT` is safe: the engine copied
  the connect string into `svs.clients[n].userinfo` before the call
  (`SV_DirectConnect`, `server/sv_client.cpp:548`), and `SV_SetUserinfo` only
  copies it back (`sv_init.cpp:173-184`). `DropClient` is **not** used on this
  path, because the slot is still `CS_FREE`.
- Bots are skipped (`connect_userinfo_sanitized`): their userinfo comes from
  the bot files and has no `forcepowers`, and the game takes their powers from
  `botstates[]`.

**Tests (TESTED).** `connect_sanitiser_skips_bots_only` pins the bot exemption;
the policy core now has its own S6 tests. The engine read/write and the
`GAME_CLIENT_CONNECT` return value still need the runtime check (connect with a
`darksidetools` model, and with a bad `forcepowers` string, then confirm the
game sees the sanitised userinfo), in
[Section 6](#6-tests-and-test-plan).

### S11. No out-of-band rate limiting; rcon flood gate disabled — High — **FIXED 2026-11-02**

Upstream: 10-25 F6 / O2.

**Cause (AUDIT).** The original engine has a global 500 ms timer before the
rcon password check, and no limit at all on `getstatus`/`getinfo` (DRDoS
amplification). The 2019 proxy NOPs the timer and offers only an optional,
global cooldown that runs before the password check (so an attacker can lock
admins out). The 2026 fix deletes the cvar and adds ioq3-style leaky buckets:
valid rcon is never limited; bad passwords share one global `(10, 1000)`
bucket; `getstatus`/`getinfo` get a per-address `(10, 1000)` bucket plus a
global outbound `(10, 100)`; `getchallenge`/`connect` get a per-address limit.

**Our code before the fix (SOURCE (ours)).**

- The engine's 500 ms timer was NOP'd unconditionally at attach.
- `svc_remote_command` applied only `proxy_sv_enableRconCmdCooldown`, whose
  default was `"0"` and which was a **global** gate updated before the password
  check.
- `svc_status`/`svc_info` kept the q3infoboom length guard but had no rate
  limit.
- The Rust `pps` module limits *sequenced, known-client* packets at
  `SV_Netchan_Process`; it never touched connectionless traffic.

**Impact.** Unlimited rcon password guessing (global gate off by default) and
unlimited query amplification. No `getchallenge`/`connect` limit either.

#### Applied (2026-11-02)

A port of the 2026 `Engine/RateLimit.cpp` into `rust/src/ratelimit.rs`, called
from the connectionless entry wrappers:

- `limit` (`ratelimit.rs:61-83`) is OpenJK's `SVC_RateLimit` with unsigned
  time: a `Sys_Milliseconds` wrap looks like one period elapsed instead of
  freezing the bucket. One bucket `(10, 1000)` per IPv4 address
  (`address_allowed`, `ratelimit.rs:129-140`), shared by `getstatus`,
  `getinfo`, `getchallenge` and `connect`; a global outbound `(10, 100)` for
  `getstatus`/`getinfo` (`status_allowed`, `:142-161`); a global `(10, 1000)`
  for a bad rcon password (`rcon_allowed`, `:163-186`). A valid rcon password
  is never limited.
- The address table is a `HashMap` capped at 65536 entries, garbage-collected
  once per second; a new address beyond the cap is dropped. Only `NA_IP`
  sources are limited (loopback, bots and bad addresses are not).
- `svc_status`/`svc_info` (`hooks/engine_sv.rs:366-426`) apply
  `status_allowed` after the q3infoboom length check.
- `svc_remote_command` (`hooks/engine_sv.rs:301-331`) computes the password
  match once, logs the attempt (redacted), calls `rcon_allowed(valid)`, and
  only then applies the `writeconfig` `.cfg` filter (so a bad password still
  gets the engine's `Bad rconpassword.` reply).
- New entry wrappers `sv_get_challenge` / `sv_direct_connect`
  (`hooks/engine_sv.rs:439-461`) apply `address_allowed`; two new hooks
  (`SV_GetChallenge`, `SV_DirectConnect`) with pinned prologues. A dropped
  packet gets no `challengeResponse`/`connectResponse`. The getchallenge
  wrapper then answers statelessly (S18); the connect wrapper admits the
  challenge before the original runs.
- `proxy_sv_enableRconCmdCooldown` is **deleted** (`state.rs` cvar table now
  21 entries, later indices shifted); the engine's 500 ms timer stays NOP'd.
  This matches the 2026 decision; the old cvar was the broken global gate.
- Drops are logged only with `proxy_sv_debugLog` (`diagnostics`), the local
  equivalent of upstream's `com_developer`.
- Difference from upstream: our buckets live in the pinned module (D-007), so
  they survive a map change instead of resetting. That can only make the limit
  stricter across a map change, never looser.

**Tests (TESTED, 5 new).** `ratelimit.rs`: burst then one token per period,
the `u32` wrap, per-address sharing/keying and the non-IP exemption, the
65536-entry cap, and the per-second garbage collection. The engine wrappers
themselves need the runtime.

**Runtime check.** `rcon` guessing, a `getstatus` flood, and a `getchallenge`
flood from one address (and from many spoofed addresses, for the challenge
table behaviour) in [Section 6](#6-tests-and-test-plan).

### S12. `writeconfig` `.cfg` filter is bypassable — Medium — **FIXED 2026-11-02**

Upstream: 10-25 F7 / O4, commit `5c3899a`/`e54cb88` series.

**Cause (AUDIT).** The 2019 filter lives in `SVC_RemoteCommand` and only
rejects a name at least 5 characters long whose last dot is at `len-4` and is
not `.cfg`. It does not see `vstr`/`exec` (the command runs later from the
Cbuf), allows `x.so` (4 characters), and does not block `..`. The fix hooks
`Com_WriteConfig_f` itself and accepts only `.cfg`, no `..`, not
`mpdefault.cfg`/`default.cfg`, copying OpenJK.

**Our code before the fix (SOURCE (ours)).** The old filter lived in
`SVC_RemoteCommand`: a name at least 5 characters long whose last dot is at
`len-4` and is not `.cfg` was dropped. `x.so` (4 characters), `..`, `vstr`,
`exec` and the dedicated console all got through.

**Impact.** An rcon admin (already trusted) could create/overwrite any file
under `fs_homepath/fs_game`, including `..` traversal, and on Linux could
truncate the loaded proxy `.so` if `fs_homepath` and `fs_basepath` coincide
(SIGBUS). Lower severity than the player-triggered items, but it is the wrong
layer.

#### Applied (2026-11-02)

The upstream design, a full rewrite of the command itself:

- `com_write_config_f` (`hooks/engine_sv.rs:538-587`) replaces
  `Com_WriteConfig_f`: `Cmd_Argc() != 2` → usage; copy `Cmd_Argv(1)` into a
  `MAX_QPATH` buffer and append `.cfg` (`compat::default_extension`); then
  refuse unless the suffix is `.cfg` (`compat::compare_extension`), there is no
  directory traversal (`compat::check_dir_traversal`, the proxy's existing
  hardened version) and the name is not `mpdefault.cfg`/`default.cfg`
  (`compat::filename_compare`); finally print `Writing <name>.` and call
  `Com_WriteConfigToFile`.
- The new `Com_WriteConfig_f` hook (`0x8074d64`, prologue
  `55 8b ec 83 ec 50`) makes the check apply to **every** caller — rcon, the
  dedicated console, `vstr` and `exec` — instead of the rcon packet path.
- The old `SVC_RemoteCommand` filter was removed; `svc_remote_command` now only
  does the password/rate-limit policy.
- `compat.rs` gained `default_extension` and `compare_extension`
  (OpenJK ports).

**Tests (TESTED, 2 new).** `default_extension_appends_only_without_one`
(dot in a directory part, existing extension, empty name) and
`compare_extension_is_case_insensitive`. The command's engine calls
(`Cmd_Argv`, `Com_Printf`, `Com_WriteConfigToFile`) remain runtime-only; the
check is in [Section 6](#6-tests-and-test-plan).

### S13. `MSG_ReadBits` reads past the message — Low (CVE-2017-11721) — **FIXED 2026-11-02**

Upstream: O11, commit `331f29c` (*"fix: O11"*).

**Cause (AUDIT).** JKA's `MSG_ReadBits` has no bound; a malformed message can
read past the OOB/Huffman data. In JKA the receive buffer is a local
`MAX_MSGLEN` array in `Com_EventLoop`, so it reads the stack, not unmapped
memory. ioq3/OpenJK added bounds; the 2026 proxy wraps `MSG_ReadBits` with
before/after checks (4-argument retail signature).

**Our code before the fix (SOURCE (ours)).** No `MSG_ReadBits` hook; the
retail read path was unchanged.

**Impact.** Remote out-of-bounds read (stack) from a malformed packet. Low.

#### Applied (2026-11-02)

A direct port of `331f29c`:

- `msg_read_bits` (`hooks/engine_sv.rs:1734-1772`) is an entry wrapper around
  the original (`MSG_ReadBits`, `0x8077834`). Before the read:
  - `readcount > cursize` → return 0;
  - out-of-band: `readcount + bits/8 > cursize` → set `readcount = cursize + 1`
    and return 0;
  - Huffman: `bit + bits%8 > cursize*8` → same.
  After the original: a Huffman read whose `bit` went past `cursize*8` gets 0
  and the same marker. The one-symbol overshoot stays inside the receive
  buffer.
- The two decisions are pure helpers (`readbits_over_bound`,
  `readbits_post_over`, `:1776-1790`) and are unit tested.
- `OFFSET_MSG_OOB = 8` added to `sdk.rs` (the SDK `msg_t` has `allowoverflow`,
  `overflowed`, `oob` before `data`), pinned with the other message offsets.
- New `MSG_ReadBits` hook with its site prologue
  (`55 8b ec 83 ec 38`, verified on `linuxjampded`).

**Tests (TESTED, 1 new).** `msg_read_bits_bounds_match_ioq3` covers the OOB
whole-byte bound, the Huffman bit bound (including the exact end), negative
bit counts and the post-read overshoot. The live malformed-packet check is in
[Section 6](#6-tests-and-test-plan).

### S14. A kick vote can kick the next client of the slot — Low — **FIXED 2026-11-02**

Upstream: 11-01 F8, commit `013c7a7` (*"fix: cancel a vote kick when its target
left the slot"*).

**Cause (AUDIT).** A vote stores a slot number, not a client. It lasts up to
30 s plus a 3 s delay, then `clientkick <slot>` runs. If the target leaves and
another client (or a `bot_minplayers` bot) takes the slot, that client is
kicked. The 2026 fix snapshots each slot's client address on
`G_SET_CONFIGSTRING` of `CS_VOTE_TIME` and drops the `EXEC_APPEND clientkick`
when the slot changed hands.

**Our code before the fix (SOURCE (ours)).** The syscall forwarder intercepted
`G_SET_CONFIGSTRING` for `CS_SCORES1/2`/`CS_PLAYERS` only and did not intercept
`G_SEND_CONSOLE_COMMAND`; there was no vote tracking.

**Impact.** A player could get someone else kicked by leaving during the vote.
Not a crash.

#### Applied (2026-11-02)

A port of `013c7a7` into `rust/src/votes.rs`:

- `on_configstring` (`votes.rs`) runs from the `G_SET_CONFIGSTRING` arm when
  `CS_VOTE_TIME` becomes non-empty and copies every slot's 20-byte `netadr_t`
  from `svs.clients[slot].netchan.remoteAddress`
  (`engine_sv::client_address_bytes`) into a static table. The engine
  allocates only `sv_maxclients` slots, so the loop stops there.
- `console_command` runs from the `G_SEND_CONSOLE_COMMAND` arm: only
  `EXEC_APPEND` with the `clientkick ` prefix (case-insensitive — the game
  keeps the case the player typed) is screened; the slot is parsed with
  `atoi`; a slot outside `sv_maxclients` is left to the engine; if the slot's
  current address differs from the snapshot, the command is dropped and
  `print "Vote kick canceled: the player left the server."` goes to every
  player.
- The team-killer automatic kick uses `EXEC_INSERT` and is untouched.
- Limits, as upstream: a target who reconnects into the same slot with the
  same address is still kicked; bots have an `NA_BOT` address, so a bot
  replacing a bot is still kicked.
- New constants: `CS_VOTE_TIME = 8`, `G_SEND_CONSOLE_COMMAND = 16`,
  `EXEC_APPEND = 2` (`sdk.rs`).

**Tests (TESTED, 3 new).** `only_exec_append_clientkick_is_the_vote`,
`the_slot_is_parsed_and_ranged` (including `atoi` text/negative/32), and
`a_changed_slot_address_drops_the_kick`. The engine address read and the
broadcast need the runtime check in [Section 6](#6-tests-and-test-plan).

### S15. `forwardmove`/`rightmove`/`upmove` of −128 — Low — **FIXED 2026-11-02**

Upstream: O10, commit `3f1fdcd`.

**Cause (AUDIT).** `MSG_ReadDeltaUsercmdKey` can produce −128 for a movement
axis; because negative values then have one more magnitude step than positive
ones (+127 max), a negative move is very slightly longer. ioq3/OpenJK clamp
−128 to −127.

**Our code before the fix (SOURCE (ours)).** `shared_api::get_usercmd` reset
`forcesel` and `ROLL` but did not clamp movement axes.

**Impact.** A subtle movement asymmetry, not a server compromise. Low.

#### Applied (2026-11-02)

A direct port of `3f1fdcd` in `get_usercmd` (`shared_api.rs:73-91`): each of
`forwardmove`, `rightmove` and `upmove` equal to `-128` is set to `-127`; every
other value, including `-127` and `+127`, is untouched. This runs in the
`G_GET_USERCMD` trap, so the game never sees `-128`; the engine's own copy is
unchanged, as OpenJK.

**Tests (TESTED, 1, inverted).** `get_usercmd_clamps_minus_128` checks the
three axes at `-128` and that `-127`/`0`/`127` are preserved, next to
`get_usercmd_sanitises_forcesel_and_roll`.

### S16. Vote parsing and `callvote kick <name>` lookup — Low, not security

Upstream: 11-01 F6, commit `1f2ea59`.

**Cause (AUDIT).** `Cmd_Vote_f`/`Cmd_TeamVote_f` test `msg[0] == 'y' ||
msg[1] == 'Y' || msg[1] == '1'` (a Quake 3 typo), so `vote Y` and `vote 1`
count as no. `G_ClientNumberFromName` walks the first
`level.numConnectedClients` slots (a count, not the highest slot) and does not
test `pers.connected`, so a name after an empty slot is not found and a
departed player's old name can target an empty slot. The 2026 fix replaces the
game's arguments through `G_ARGV` (`vote` → `yes`/`no`, `kick <name>` →
`clientkick <n>` after its own lookup).

**Our code (SOURCE (ours)).** No `G_ARGV` interception in
`jampgame_syscall_forward` (`syscall.rs`), no name lookup, no argument
replacement. Functional gap, no security impact.

**Test.** The name lookup is pure string work once implemented.

### S17. `modelFullPath` overflow — accepted upstream, not reachable on retail

Upstream: 11-01 F7, **not fixed** by decision of the author.

`SetupGameGhoul2Model` builds `models/players/<name>/model.glm` with `strcpy`
into `char modelFullPath[MAX_QPATH]`; a model base longer than 38 characters
writes past it. It is only reached when `d_perPlayerGhoul2` (a `CVAR_CHEAT`,
default 0) is set or `G_PlayerHasCustomSkeleton` returns true — the latter is
`xor eax, eax; ret` in the retail Linux module (AUDIT), i.e. always false.
Nobody on a normal server can trigger it. Our model check uses
`proxy_sv_modelPathLength` (default 64, `state.rs:26`), so a 39–63 character
base still passes; it is the same accepted risk. Keep upstream's decision
unless a custom-skeleton build is used.

## 4. Crash and availability fixes outside the two audits

These came from the same 2026 commit range and are worth tracking even though
they are not in the audit files.

### S18. Challenge table can be evicted by a spoofed `getchallenge` flood — **FIXED 2026-11-02**

Upstream: commit `6c8ce8c` (*"fix: renew the challenge at each getchallenge"*)
then `ae62510` (*"feat: stateless challenges"*).

**Cause (AUDIT/A1 — A9).** `SV_GetChallenge` stores each new address in
`svs.challenges` (1024 slots) and evicts the oldest; `SV_DirectConnect` accepts
only a challenge still in the table. More than ~1024 spoofed addresses during a
player's round trip evict that player's challenge, and they cannot connect. The
2026 fix answers `getchallenge` with a stateless keyed hash (SipHash) of the
address and a timestamp and only inserts the challenge at `connect`, so nothing
can be evicted.

**Our code before the fix (SOURCE (ours)).** No `SV_GetChallenge` /
`SV_DirectConnect` hook was installed; the retail stateful table was used.

**Impact.** A spoofed flood could keep new players out (availability), and the
related ping trick (getchallenge immediately before connect) bypassed
`sv_minPing`/`sv_maxPing`. Medium/High for a public server.

#### Applied (2026-11-02)

A port of `ae62510` (which supersedes the `6c8ce8c` renewal) into
`rust/src/challenge.rs`:

- `getchallenge` is answered by the proxy with
  `challengeResponse %i` (`answer`), and the original engine handler is not
  called. The challenge is
  `SipHash-2-4(key, ip | port << 32, timestamp)` with 31 bits of the hash and
  the timestamp parity in the top bit; nothing is stored.
- `connect` is validated in the `SV_DirectConnect` wrapper (`admit`): a valid
  challenge (current or previous 16.384 s timestamp) writes its entry into
  `svs.challenges` just before the original looks it up; a repeated connect
  reuses its existing entry even after expiry. An invalid challenge inserts
  nothing and the engine replies `No or bad challenge for address.`
- The entry's `pingTime` is `svs.time - ceil(sv_minPing)`, so the original
  `sv_minPing`/`sv_maxPing` checks pass; neither can be enforced on a stateless
  challenge, as in OpenJK, TaystJK and Quake3e.
- The key is a process-lifetime `LazyLock<[u64; 2]>` seeded from std's
  `RandomState` (OS entropy). Upstream keeps it in a hidden cvar because the
  engine unloads the module on every map change; our module is pinned (D-007),
  so a challenge sent just before a map change stays valid without a cvar.
- New offsets in `sdk.rs`: `OFFSET_SVS_CHALLENGES = 32` (confirmed on the
  retail binary: `SV_GetChallenge` uses `0x8312200` = `svs` + 32),
  `SIZEOF_CHALLENGE_T = 40`, `MAX_CHALLENGES = 1024`, pinned by a layout test.
- The `SV_GetChallenge`/`SV_DirectConnect` hooks were added with S11; the
  getchallenge trampoline is now unused (the original is never called).

**Tests (TESTED, 5 new).** The SipHash-2-4 reference vector, the
current/previous-timestamp validity and the future-timestamp rejection, the
address+port binding, the base-address comparison, and the `sdk.rs` layout
pins. The engine write into `svs.challenges` and the `challengeResponse` on the
wire need the runtime check in [Section 6](#6-tests-and-test-plan).

### S19. Ghoul2 calls with a null instance — **FIXED 2026-11-02**

Upstream: commit `99f5599`.

**Cause.** The engine dereferences the Ghoul2 instance without a test; the game
passes one when a model failed to load. The proxy returns the engine's
empty-instance value for the `G_G2_*` traps when the pointer is null.

**Our code before the fix (SOURCE (ours)).** The syscall forwarder did not
intercept any `G_G2_*` trap; a null instance from a failed model load reached
the engine unchanged.

**Impact.** Possible remote/indirect server crash when a model load fails. Not
reproduced. Medium.

#### Applied (2026-11-02)

A port of `99f5599` in the syscall forwarder:

- `g2_null_result` (`syscall.rs:141-203`) runs at the entry of
  `jampgame_syscall_forward` (`:205-579`) when the master switch is on. It
  answers the exact groups of the upstream commit:
  - instance in `args[0]` → `0` (31 traps, `G_G2_HAVEWEGHOULMODELS` …
    `G_G2_GETSURFACENAME`);
  - instance in `args[0]` → `-1` (`G_G2_ADDBOLT`, `G_G2_GETSURFACERENDERSTATUS`);
  - `args[0]`/`args[1]` null → `0` (`COPYGHOUL2INSTANCE`,
    `DUPLICATEGHOUL2INSTANCE`);
  - `args[0]`/`args[2]` null → `0` (`COPYSPECIFICGHOUL2MODEL`);
  - `args[1]` null → `0` (`COLLISIONDETECT`, `COLLISIONDETECTCACHE`);
  - `args[0]` or `*args[0]` null → `0` (`HASGHOUL2MODELONINDEX`,
    `REMOVEGHOUL2MODEL`, `REMOVEGHOUL2MODELS`).
  `G_G2_INITGHOUL2MODEL` (creates the instance), `G_G2_CLEANMODELS` and the
  attachment cleanups are left alone, as upstream. The bolt matrix of
  `G_G2_GETBOLT` is left untouched.
- The ordinals (585–629, after `BOTLIB_PC_SOURCE_FILE_AND_LINE` = 581) are in
  `sdk.rs:176-226`, with the null-return rule documented per group.

**Tests (TESTED, 1 new).** `null_ghoul2_instances_are_answered` covers every
group, the `void**` dereference (both the pointer and its target), and the traps
deliberately left alone. **Not reproduced at runtime**: triggering it needs a
model that fails to load, so it stays in the runtime list as a `dG2` scenario
without a reproduction.

### S20. Sub-BSP buffers leak on every map load — **FIXED 2026-11-02**

Upstream: commit `23e84f1`.

**Cause.** `CM_LoadMap_Actual` reads each `misc_bsp`/model sub-BSP into a
`TAG_BSP_DISKIMAGE` buffer and never frees it (the main map's buffer is freed).
The proxy calls `Z_TagFree(TAG_BSP_DISKIMAGE)` after `GAME_INIT`.

**Our code before the fix (SOURCE (ours)).** No `Z_TagFree`/BSP handling. The
nav-graph leak was already handled (D-008 / `G_NAV_FREE`), but not this one.

#### Applied (2026-11-02)

A port of `23e84f1`: after forwarding `GAME_INIT` (`lib.rs`), when the master
switch is on, `free_bsp_disk_images` calls the engine's
`Z_TagFree(TAG_BSP_DISKIMAGE)` (`0x80804e4`, verified on `linuxjampded`). The
tag is `TAG_BSP_DISKIMAGE = 9` (`tags.hpp` order); `GAME_INIT` has loaded the
sub-BSPs at that point. The main map's buffer is already freed by the engine.

**Impact.** Long-running servers with sub-BSP maps (`misc_bsp`, model
`#name`) no longer leak a disk-image buffer per map change.

**Tests (TESTED, 1 new).** `bsp_disk_image_tag_and_address_are_pinned` pins the
tag value and the `Z_TagFree` address. The leak itself is a runtime memory
measurement; see [Section 6](#6-tests-and-test-plan).

### S21. `map_restart` puts connecting clients back in the world — **FIXED 2026-11-02**

Upstream: commit `48d4da5`.

**Cause.** `SV_MapRestart_f` sets `CS_ACTIVE` for every client from
`CS_CONNECTED` up, including one still downloading or without the map's
gamestate. Combined with the proxy's donedl/update-state guards (which we
ported), such a client can wait forever. The fix makes `map_restart` skip
non-active clients, as Quake3e.

**Our code before the fix (SOURCE (ours)).** We did not patch
`SV_MapRestart_f`'s client-state store. We **do** port the
`SV_ExecuteClientMessage` upstream gate (see `hooks/engine_sv.rs:1307-1390`),
which is the other half of this interaction.

#### Applied (2026-11-02)

A byte-for-byte port of `48d4da5` with no new hook. On `linuxjampded` the
pristine store at `0x804f23f` is `c7 04 0e 04 00 00 00`
(`client->state = CS_ACTIVE`), followed by the argument setup and the
`call SV_ClientEnterWorld` at `0x804f253`. The patch replaces the store with
`83 3c 0e 04 75 13 90`:

```
cmpl $4,(%esi,%ecx,1)   ; state == CS_ACTIVE?
jne  +0x13              ; no: skip EnterWorld
nop
```

The `jne` lands at `0x804f258`, right after the 5-byte call, so only clients
already `CS_ACTIVE` are re-entered. A client in `CS_CONNECTED`/`CS_PRIMED`
keeps its state, finishes connecting, and receives the gamestate through the
normal path. The site is applied in `attach_all` via `patch::write_code`
(`hooks/mod.rs`) and restored to the pristine 7-byte store in
`revert_inline_patches` (`patch.rs`), so `proxy_sv_enable 0` gives back the
pristine engine (`engine.rs:170-176`, `patch.rs:574`, `:1023-1026`).

**Impact.** Functional/availability only: a client connecting while an admin
or a vote issues `map_restart` no longer gets stuck on a black screen.

**Tests (TESTED, 1 new).** `sv_map_restart_set_active_site_is_pinned`
(`patch.rs`) pins the address, both byte sequences, the 7-byte length, the
engine text range, and that `jne +0x13` lands exactly after the
`SV_ClientEnterWorld` call. The end-to-end behaviour needs a paused/slow
downloading client during `map_restart`; see
[Section 6](#6-tests-and-test-plan).

### S22. Engine counter wrap (`svs.time`, `nextSnapshotEntities`)

Upstream: commit `a0d322b`.

**Cause.** `SV_Frame` stops the server before `svs.time` passes `0x70000000`
(~21.7 days) or `nextSnapshotEntities` nears `0x7FFFFFFE`, calling
`SV_Shutdown` + `map_restart 0` — but `map_restart` then refuses ("Server is
not running"), dropping everyone and leaving the server map-less. The proxy
reloads the map shortly before the limits (with `killserver` first for the time
wrap).

**Our code (SOURCE (ours)).** No equivalent. The Rust `pps`/netStatus code is
careful about *its own* unsigned wrap (`pps.rs`, `svc_remote_command` uses
`wrapping_sub`), but the engine's shutdown bug remains. Availability after
~21 days, or on a busy single map. Low/Medium.

### S23. NPC navigation data / other fixed items

- `4b3d1b2` free NPC navigation data before each map load: **COVERED** by
  D-008 / `G_NAV_FREE` (`syscall.rs`, `test nav_load_frees_the_previous_nav_graph_first`).
- `001ac04` IP bans no longer treat 0 as a wildcard: **N/A** — the Rust proxy
  has no ban subsystem (no `ybeproxy_bans.txt` equivalent).
- `81664df`, `05b990d`, `adffc2b`, `ac1212c`, `56f519d`, `9ee15e2`,
  `f074131`, `eb1099f`, `0148257`, `630d610`: features (bans, proxyconfig,
  netStatus columns, serverinfo version). Not security; out of the D-002
  baseline.

## 5. Covered or not applicable

| Item | Why it is not a concern here |
|---|---|
| 11-01 H1 (quote/control characters in chat) | **REFUTED upstream**: the tokenizer never keeps a `"` inside an argument, so `ConcatArgs` cannot contain one. Our `cmd_tokenize_string2` follows the same tokenizer. Nothing to do. |
| 10-25 F1 (`rcon map` crash) | **COVERED by D-007**: the Rust module pins itself (`original::pin_own_module()` at `dllEntry`) so the engine's `dlclose` cannot unmap code with live frames. Upstream chose a packet filter stub instead and rejected pinning; both remove the crash. Our approach also removes the hot-reload-on-map-change property, documented in D-007. |
| 10-25 F3 (anti-cheat) | The 2026 corrected rule is largely what we have: `darksidetools` is a fixed-length prefix (kicks every suffixed model), `rancor`/`wampa` are prefixes (skins caught). S10 also implemented the "after the drop, return the reason; do not call `SetUserinfo` and do not forward" part, so the game no longer rewrites `CS_PLAYERS` for a dropped slot. Two minor differences remain: (a) we still only catch `jedi_/red` and `jedi_/blue`, not `jedi_` alone or other skins; (b) an empty model is left empty instead of forced to `kyle` (upstream says the retail game handles it, and we did not reproduce a problem). Neither is a bypass of the *kick*; they are cosmetic/robustness. |
| 10-25 F4 (OpenJK API) | The Rust module exports only `dllEntry` and `vmMain` (`lib.rs:87-120`), i.e. upstream's chosen option A. No `GetModuleAPI`. |
| 10-25 F5 (wrong cvars) | The Rust proxy does not register `sv_gametype`/`sv_fps` at all (`state.rs:22-43`); it reads what it needs through the engine. The bogus cvar cannot be created. |
| 10-25 F8 (`SV_SvEntityForGentity`) | **COVERED, stricter**: `hooks/engine_sv.rs:1075-1135` validates `s.number`, the array base, the element size/alignment and the final index, then `Com_Error(ERR_DROP)`. Unlike JK2MV/2026 it does not fall back to `s.number` when the pointer is outside `g_entities`; it errors instead. Safe, but a behaviour difference if a game ever passes such a pointer (the retail game passes `&g_entities[n]`). |
| 10-25 F9 (`netStatus` engine state) | Our netStatus only **reads** `cl->snapshotMsec` (`hooks/netstatus.rs:276`) and derives fps/packets itself; it does not write `snapshotMsec`. The bug (a display command changing a player's snapshot rate) does not exist. |
| 10-25 F10 | F10.1/F10.5/F10.8 are refuted or engine-specific and not present. F10.2 (`1000 / sv_fps`): our netstatus/pps arithmetic is hardened (D-010). F10.4: we still compare command names/arguments by prefix (`shared_api.rs`), so we over-block (`teamvote`, `saber_*` NPCs, `gc…`) exactly like the 2019 proxy; the game compares whole names, so this is not a bypass. Cosmetic, Low. F10.6/F10.7 are about C++ code we do not have. |
| O6 (server commands > 1022) | **Do not port**, upstream's own conclusion: the retail `scores` command legitimately exceeds 1022 and retail clients handle the truncation. |
| O7 (tokenizer) | **COVERED**: `hooks/compat.rs:123-152` (`cmd_tokenize_string2`) copies into a `BIG_INFO_STRING` bound and tokenizes the copy, as Quake3e. |
| O9 (movement events) | **ACCEPTED**: upstream did not fix it either (design written, then removed). Our `SV_AddEntitiesVisibleFromPoint` hook exists for other reasons but has no event filter. |
| A1 (hook relocation) | Our `attach` copies the stolen bytes verbatim (`patch.rs:452-463`). All fixed hook sites have relocatable-free prologues, pinned by the `SITES` tests (`patch.rs:1100+`), so today this is safe. It would become a hazard only for a new hook whose first bytes contain `E8/E9`/`Jcc` (e.g. the Windows `Com_WriteConfig_f` upstream needed relocation for). Keep the site table pinned if S12 is implemented. |

## 6. Tests and test plan

### Tests added

`rust/src/userinfo.rs` (new, 5 tests):

- `value_for_key` / `set_value_for_key` / `validate_userinfo` are pure and
  tested directly: case-insensitive read vs case-sensitive remove, forbidden
  characters, the `>= MAX_INFO_STRING` bound, and the OpenJK validation shapes
  (missing, duplicate and capitalised keys, `cl_guid`, two `ip`, slashes,
  control characters).

`rust/src/shared_api.rs`:

- the forcepowers shape check was moved **verbatim** out of the userinfo
  sanitiser into the pure `forcepowers_is_valid` (`shared_api.rs:682-723`).
- `command_line_too_long` (`shared_api.rs:733-735`) is the S1 predicate.
- `gc_command_denied` (`shared_api.rs:747-749`) is the S3 predicate; the
  `client_command` guard only reads the cvar and calls it.
- `clientkick_max_slot` (`shared_api.rs:755-757`) and
  `vote_value_out_of_range` (`shared_api.rs:762-764`) are the S2 seam.
- `connect_userinfo_sanitized` (`shared_api.rs:375-377`) is the S10 bot
  exemption.
- `sanitize_userinfo_bytes` (`shared_api.rs:453-576`) is the S6 pure core. The
  whole userinfo path is tested without an engine: name/model/forcepowers
  rewrites, duplicate-key refusal, the O8 write that cannot fit, the
  `darksidetools` refusal, and the bot path.
- 23 new tests in this module: `connect_sanitiser_skips_bots_only`,
  `command_line_is_capped_at_512_bytes`,
  `gc_accepts_a_real_slot_and_a_listed_order`,
  `gc_drops_the_order_past_the_end`,
  `gc_drops_a_player_outside_the_configured_slots`,
  `gc_leaves_negative_player_and_order_to_the_game`,
  `clientkick_accepts_slots_below_sv_maxclients`,
  `clientkick_drops_the_slot_at_or_above_sv_maxclients`,
  `clientkick_drops_a_negative_slot`,
  `retail_forcepowers_value_is_accepted`,
  `length_and_separator_shape_is_enforced`,
  `f4_requires_18_power_digits`,
  `f4_rejects_power_digits_above_three`,
  `f4_leaves_rank_and_side_digits_unrestricted`,
  `get_usercmd_sanitises_forcesel_and_roll`,
  `get_usercmd_clamps_minus_128`,
  `sanitizer_rewrites_a_bad_name_model_and_forcepowers`,
  `sanitizer_refuses_a_duplicate_key`,
  `sanitizer_refuses_the_o8_write_that_cannot_fit`,
  `sanitizer_refuses_darksidetools_without_writing`,
  `sanitizer_skips_validation_for_bots` and
  `sanitizer_requires_ip_for_a_validated_connect` and
  `sanitizer_cuts_color1_and_color2_to_15`. The `*_known_gap_*` tests are
  characterisation tests: they pin a live gap and say, in their doc comments,
  that they must be inverted when the finding is fixed.

`rust/src/ratelimit.rs` (new, 5 tests): the burst/refill boundary, the `u32`
time wrap, per-address buckets keyed by IP with the non-IP exemption, the
65536-entry cap, and the per-second garbage collection. `address_allowed_at`
takes the time as a parameter, so the table is tested without the engine.

`rust/src/challenge.rs` (new, 4 tests) and `sdk.rs` (1 layout test): the
SipHash-2-4 reference vector, a challenge valid at its timestamp and the next
one but not later, the address+port binding, the base-address comparison, and
the `challenge_t`/`serverStatic_t` offsets.

`rust/src/syscall.rs` (1 new test): `null_ghoul2_instances_are_answered` covers
every null-instance group and the `void**` dereference.

`rust/src/hooks/engine_sv.rs` (4 new tests): the `Info_Print` formatter —
20-character key padding, a 600-character value kept intact (the retail 512
buffer would overflow), a long key left unpadded, `MISSING VALUE` — and the
`MSG_ReadBits` bounds.

`rust/src/patch.rs` (1 new test): the `Info_SetValueForKey` length sites and
the pristine/patched `jbe`/`jb` bytes.

`rust/src/hooks/compat.rs` (2 new tests): `default_extension` (existing
extension, dot in a directory part, empty name) and `compare_extension`
(case-insensitive suffix).

`rust/src/votes.rs` (new, 3 tests): the `EXEC_APPEND clientkick` recognition,
the slot parse/range, and the changed-address decision.

`rust/src/lib.rs` (1 new test): the `TAG_BSP_DISKIMAGE` value and the
`Z_TagFree` address.

Run:

```sh
rust/dev.sh cargo test --offline
rust/dev.sh cargo clippy --offline --all-targets -- -D warnings
rust/dev.sh cargo fmt --check
rust/dev.sh cargo build --offline --target i686-unknown-linux-gnu --release
```

Result: 316 unit tests pass; the i686 release cdylib builds. No server was
started.

### Why the other findings have no unit test yet

Most of the rules above are not reachable in a host-architecture `cargo test`:

- `client_command` calls the engine (`syscall::argv`,
  `cvar_variable_integer_value`) and the original game module
  (`q_stricmpn`, `concat_args`). Both are process-global function pointers that
  only exist in a running 32-bit server. The crate exposes no test double for
  them today (the engine syscall has one, `syscall::TEST_ENGINE_LOCK`; the game
  module does not).
- `sanitize_userinfo` itself is now split: the policy core
  (`sanitize_userinfo_bytes`) is pure and tested, and only the FFI wrapper
  needs the engine.
- The remaining engine-side findings (S7, S8, S13, S20, S21, S22) are byte
  patches, engine calls or entry wrappers over `linuxjampded`; they need the
  container runtime under `tools/runtime`. S11, S18 and S19 have pure tests for
  their policy (buckets, SipHash, null groups), but their live paths do too.

To make the missing rules testable without duplicating them, the later work
should split the decision logic from the I/O the way `forcepowers_is_valid`
now does. The natural seams:

1. `fn command_denied(cmd, arg1, arg2, args_concat, cfg) -> Option<&'static str>`
   covering the chat length, `gc`, `npc spawn`, `team follow`, `callteamvote`,
   the `callvote` table **and the new 512-character / `clientkick` rules**;
2. `fn validate_userinfo(info: &[u8]) -> Result<(), Reason>` (O8) and a
   `fn set_info_value(...) -> bool` that mirrors the result-returning
   `Info_SetValueForKey`;
3. `fn rate_limit(bucket: &mut LeakyBucket, burst: i32, period: i32, now: u32) -> bool`
   and the `getstatus`/`getinfo`/rcon policies (S11);
4. `fn challenge_accept(address, challenge, now) -> bool` (S18), with SipHash
   reference vectors;
5. `fn clamp_negative_move(v: i32) -> i32` in `get_usercmd` (S15).

Each extraction is behaviour-preserving and gives a table of vectors. The
connect-path (S10), vote-slot (S14) and engine-patch items stay integration
tests.

### Proposed integration checks (`tools/runtime`)

None of these has been run yet; they are the runtime half of the fixes.

- S1: connect a client, complete a map, send a 1000+ character unknown command
  at intermission; the pristine server must crash, the fixed one must stay up.
- S2: `callvote clientkick 31` with `sv_maxclients 8`.
- S3: `gc 1 7` on Linux (upstream reproduced the crash).
- S4/S10: connect with `forcepowers` = `123-1-` + 16 digits and with a 9 in the
  power list; with and without a following `userinfo` command. The game's
  `WP_InitForcePowers` must see the default
  `7-1-030000000000003332` on the first spawn and after the command.
- S5: connect with `color1`/`color2` far longer than 15 characters (e.g. 500
  each) and inspect the game's `CS_PLAYERS`; each value must be cut to its
  first 15 characters, and the TaystJK/EternalJK `<colour><name>` shape must
  survive.
- S10: connect with a `darksidetools` model; the engine must print the
  anti-cheat reason and refuse the client, and no `CS_PLAYERS` configstring may
  remain for the slot. Also send a `userinfo` command with that model to a
  connected client and confirm it is dropped with no ghost configstring.
- S1/S11: rcon password guessing and `getstatus` amplification rate.
- S18: flood `getchallenge` from more than 1024 spoofed addresses while a real
  client connects; the client must still connect (the stateless answer stores
  nothing). Also: a valid challenge must survive a map change, an invalid or
  expired one must get `No or bad challenge for address.`, and a `connect` the
  client retries must reuse its entry.
- S7: plant a 600-character userinfo value, then `rcon dumpuser <client>`.
- S8: `connect` with a 1007-character userinfo (the engine's `\ip\<address>`
  pair brings the sum to exactly 1024): the request must be refused by the
  length test, with no corruption/crash. Also exercise `serverinfo`/`systeminfo`
  near `BIG_INFO_STRING`.
- S12: `rcon writeconfig x.so` and `rcon writeconfig ../escape.cfg` must be
  refused; `rcon writeconfig good.cfg` must write; `rcon writeconfig
  mpdefault.cfg`/`default.cfg` must be refused; and the same command reached
  through `set x "writeconfig evil.so"; vstr x` (or `exec`) must be refused
  from the command itself, not the rcon packet.
- S13: truncate a client message inside a bit-packed field (or fuzz the message
  readers); `MSG_ReadBits` must return 0 past the end and the server must stay
  up. Best run against an ASan build of the engine if one is available.
- S9: `userinfo` with a fake `ip`, then `map_restart`; the game must still see
  the real address (the engine wrapper overwrote it). Also `connect` with a
  userinfo too long for `ip` (1007 characters plus the engine's pair): the
  proxy must refuse the client instead of letting the game check bans against
  no address.
- S14: start a `callvote kick <name>`, let the target leave during the vote,
  and have another client take the slot before it executes. The new client
  must not be kicked, and every player must see
  `Vote kick canceled: the player left the server.`. Separately, the automatic
  team-killer kick (`EXEC_INSERT`) must still kick the current client of the
  slot.
- S20: load a map with `misc_bsp`/model sub-BSPs several times (`map`/
  `map_restart`) and check `Z_MemSize(TAG_BSP_DISKIMAGE)` (or RSS) does not
  grow per load.
- S21: have a client still downloading (or one whose packets are paused) when
  `rcon map_restart` is issued. It must stay `CS_CONNECTED`, finish the
  download and enter the world; without the patch it waits on a black screen.
  A second, fully active client must still re-enter and play.

## 7. Order of work (most urgent first)

The order weighs exploitability (who can trigger it), consequence, and
dependencies. It is the order the fixes are being applied in.

1. ✅ **S1** — 512-character command line (Critical, any client, every map
   end). *Fixed 2026-11-02.*
2. ✅ **S3** — `gc` order bound (High, any client, crashes the Linux module,
   which is our only target). *Fixed 2026-11-02.*
3. ✅ **S2** — `callvote clientkick` range (High, any voter, any platform).
   *Fixed 2026-11-02.*
4. ✅ **S10** — sanitise the userinfo at `GAME_CLIENT_CONNECT` (High; makes the
   existing forcepowers/model/name fixes effective on first spawn). *Fixed
   2026-11-02.*
5. ✅ **S6** — userinfo validation and a checked `Info_SetValueForKey` (High;
   closes the long/capitalised-key bypass, and is the prerequisite for S7/S8
   and a reliable S4). *Fixed 2026-11-02.*
6. ✅ **S11** — out-of-band rate limits and the rcon policy (High,
   unauthenticated DoS). *Fixed 2026-11-02.*
7. ✅ **S18** — stateless/renewed challenges (availability companion of S11).
   *Fixed 2026-11-02.*
8. ✅ **S9** — re-derive the `ip` userinfo key (High; IP-ban bypass, only
   observable when bans are used). *Fixed 2026-11-02.*
9. ✅ **S4** — forcepowers 18 digits / 0..3 (Medium). *Fixed 2026-11-02.*
10. ✅ **S5** — cut `color1`/`color2` at 15 (Medium, multi-client DoS).
    *Fixed 2026-11-02.*
11. ✅ **S19** — null Ghoul2 instance (Medium crash, trigger not reproduced).
    *Fixed 2026-11-02.*
12. ✅ **S7** — `Info_Print` / `dumpuser` (Medium). *Fixed 2026-11-02.*
13. ✅ **S8** — `Info_SetValueForKey` one-byte overflow (Medium, connect path).
    *Fixed 2026-11-02.*
14. ✅ **S12** — `Com_WriteConfig_f` (Medium). *Fixed 2026-11-02.*
15. **O5** — connections per IP address (Medium, optional availability).
16. ✅ **S13** — `MSG_ReadBits` bounds (Low, CVE-2017-11721).
    *Fixed 2026-11-02.*
17. ✅ **S14** — kick vote slot reuse (Low). *Fixed 2026-11-02.*
18. ✅ **S15** — `forwardmove` −128 (Low). *Fixed 2026-11-02.*
19. **S16** — vote parsing / name lookup (Low, not security).
20. ✅ **S20** — sub-BSP buffer leak (Low/Medium availability). *Fixed
    2026-11-02.*
21. ✅ **S21** — `map_restart` and connecting clients (Low functional).
    *Fixed 2026-11-02.*
22. **S22** — engine counter wrap (Low/Medium availability).

Items to leave alone, matching upstream: S17 / 11-01 F7 (unreachable without
the cheat cvar), H1 (refuted), O6 (do not port), O9 (not fixed upstream).

## 8. Evidence index — where the claims above live

Upstream (paths relative to `/home/vincent/Documents/JKA_YBEProxy`):

- `docs/audit-2026-10-25/README.md`, `fixes.md`, `improvements.md`,
  `other-engines.md`
- `docs/audit-2026-11-01/README.md`, `fixes.md`
- fix commits: `d9daa68`, `94c89b0`, `85403cd`, `2b96214`, `01adba1`,
  `1f2ea59`, `013c7a7` (second audit); `e54cb88` (O1), `5c3899a` (O5),
  `181b16c` (O7), `3b3b252` (O8), `3f1fdcd` (O10), `331f29c` (O11),
  `ed11d28` (O12); plus `99f5599`, `23e84f1`, `a0d322b`, `4b3d1b2`,
  `48d4da5`, `6c8ce8c`, `ae62510`, `8b3e51e`.

Game behaviour (our authoritative copy for the game module):
`original/jedi-academy-sdk/codemp/game/g_main.c:2197-2223`,
`g_cmds.c:1718-1748`, `g_cmds.c:1993-2001`, `g_client.c:2187-2188`,
`bg_misc.c:439-560`.

Rust rewrite (this repository):
`rust/src/shared_api.rs:73-91` (`get_usercmd`), `:100-131` (`client_connect`),
`:174-369` (`client_command`), `:375-377` (`connect_userinfo_sanitized`),
`:453-576` (`sanitize_userinfo_bytes`, pure core), `:586-632`
(`sanitize_userinfo`, FFI wrapper), `:641-666` (`client_userinfo_changed`),
`:682-723` (`forcepowers_is_valid`), `:733-735` (`command_line_too_long`),
`:747-749` (`gc_command_denied`), `:755-757` (`clientkick_max_slot`),
`:762-764` (`vote_value_out_of_range`), `:771-779` (`truncate_colors`), and
its `mod tests`;
`rust/src/userinfo.rs:145-338` (`value_for_key`, `set_value_for_key*`,
`validate_userinfo`) and its `mod tests`;
`rust/src/lib.rs:153-195` and `:301-308`, `:342-350`
(`free_bsp_disk_images`);
`rust/src/hooks/engine_sv.rs:301-331` (rcon), `:366-426` (status/info),
`:439-461` (challenge/connect), `:471-504` (`info_print_lines`), `:1734-1790`
(`msg_read_bits` bounds),
`:538-587` (`com_write_config_f`), `:600-625` (`info_print`), `:646-683`
(`refresh_ip`), `:693-756` (`SV_UserinfoChanged`),
`:1075-1135` (`SV_SvEntityForGentity`);
`rust/src/hooks/mod.rs:284-401` (intra patches), `rust/src/engine.rs:166-176`
(S21 site constants), `rust/src/patch.rs:705-822`
(engine hook table), `:452-489` (trampoline copy), `:574-579` (`write_code`),
`:1023-1026` (S21 revert) and `:1376-1401` (the S21 site pin);
`rust/src/ratelimit.rs:61-186` (bucket arithmetic and policies) and its tests;
`rust/src/challenge.rs:85-265` (SipHash, create/verify, answer/admit) and its
tests; `rust/src/sdk.rs:582-593` (challenge layout constants);
`rust/src/state.rs:22-43` (proxy cvars), `rust/src/syscall.rs:141-203`
(`g2_null_result`), `:205-602` (intercepted traps), `rust/src/pps.rs`
(sequenced-packet limiter);
`rust/src/votes.rs:43-129` (S14 vote-slot snapshot and screening);
`rust/src/sdk.rs:133` (`G_SEND_CONSOLE_COMMAND`), `:241`
(`TAG_BSP_DISKIMAGE`), `:544` (`CS_VOTE_TIME`), `:176-226` (Ghoul2 ordinals);
`rust/src/hooks/compat.rs:42-83` (`default_extension`, `compare_extension`),
`:123-152` (tokenizer).
