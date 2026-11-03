# jampgame-proxy-rs

Proxy between the server engine and the game module of the game STAR WARS™
Jedi Knight - Jedi Academy™. This is the Rust rewrite of
[jampgame_proxy](https://github.com/VincentMarnier/jampgame_proxy), which
itself extends yberion's original work
[JKA_YBEProxy](https://github.com/Yberion/JKA_YBEProxy).

The proxy is a drop-in replacement for the game module `jampgamei386.so`: the
engine loads it, it transparently forwards everything to the original module,
and adds features and security hardening on top.

Project website: <https://vincentmarnier.github.io/jampgame-proxy-rs/>.

## Features

**Built-in security hardening** (active unless the master switch
`proxy_sv_enable` is turned off):

- `getstatus` flood protection (`ipAuthorize`/DDoS removal)
- q3infoboom exploit guard
- bugged-model force and model path length clamp
- anti-cheat kicks, forcepowers validation
- `\r\n`/`;` command injection blocking in client messages and chat
- download path validation

**Optional features** (cvars):

| cvar                                  | default | description                                                                                   |
|---------------------------------------|---------|-----------------------------------------------------------------------------------------------|
| `proxy_sv_enable`                     | 1       | master switch: when `0`, detach every hook and forward everything to the original game module (RCON keeps working, with the engine's pristine 500 ms gate) |
| `proxy_sv_muteMode`                   | 1       | `mute`/`unmute` behaviour: `0` disabled, `1` drop the player's chat and tell them, `2` silent (their own line is echoed back to them, nobody else receives it, no notice); see [Player mute](#player-mute) |
| `proxy_sv_enableIgnore`               | 0       | let any player hide another player's chat from their own client with `/ignore`/`/unignore`; see [Player ignore](#player-ignore) |
| `proxy_sv_enableBanFile`              | 0       | file-backed IP ban list: `addip`/`removeip` edit `proxy_sv_banFile`, `listip` prints it, `reloadbans` re-reads it, and every connect is checked against it (IPv4 addresses and CIDR ranges; see [IP bans](#ip-bans)) |
| `proxy_sv_banFile`                    | `proxy_bans.txt` | ban file path, relative to the server's working directory or absolute; ignored while `proxy_sv_enableBanFile 0` |
| `proxy_sv_enableGuidBan`              | 0       | file-backed `ja_guid` ban registry (separate file and cvar from the IP list); `rcon ban <player>` adds the target's GUID, `addguid`/`removeguid`/`listguid` manage it (see [GUID bans](#guid-bans)) |
| `proxy_sv_guidBanFile`                | `proxy_guid_bans.txt` | GUID ban file path, relative to the server's working directory or absolute; ignored while `proxy_sv_enableGuidBan 0` |
| `proxy_sv_debugLog`                   | 0       | crash-diagnostics trace: `0` off, `1` suspicious/blocked events, `2` every client command/userinfo/lifecycle event; writes `proxy_debug.log` (see [Crash diagnostics](#crash-diagnostics)) |
| `proxy_sv_enableRconCmdCooldown`      | 0       | set a cooldown on RCON commands to avoid it being flooded                                       |
| `proxy_sv_maxCallVoteMapRestartValue` | 60      | maximum value allowed on a `callvote map_restart`                                               |
| `proxy_sv_modelPathLength`            | 64      | maximum model path length accepted from clients                                                 |
| `proxy_sv_enableNetStatus`            | 0       | compute and make accessible the players network status (`netStatus`/`showNet` commands print the [net status](#net-status) table); when `0` the feature is completely inert — no collection, no computation, no table |
| `proxy_sv_enableEndGameStats`         | 1       | print the personal/global/best-player stats tables at the end of a game                         |
| `proxy_sv_antiHpTeller`               | 0       | prevents HP tellers usage                                                                       |
| `proxy_sv_minJumpTime`                | 0       | minimum time (in ms) a player should be allowed to jump (anti low-jump scripting)               |
| `proxy_sv_lockTeamsAfterXSecs`        | 0       | grace delay in seconds (`0` = off) before a TDM/CTF round's teams lock: teams stay open for that long; a join in an instance cancels its lock for the round, otherwise the even roster is capped at its size; with parallel TFFA every instance is handled per its own roster (see [Team lock](#team-lock)) |
| `proxy_sv_teamSizeRules`              | (empty) | per-team-size `timelimit`/`fraglimit`/`capturelimit` override, reconciled as the roster changes, independent of `proxy_sv_lockTeamsAfterXSecs` (see below); with parallel TFFA it uses the per-match sizes and applies one rule only when every populated match agrees |
| `proxy_tffa_enable`                   | 0       | run parallel self-organized TFFA matches on one GT_TEAM map (`tffa_create`/`tffa_join`/`tffa_password`/`tffa_leave`/`tffa_list`); damage/scoreboard/visibility/team chat isolated per match, global chat stays server-wide (see below) |
| `proxy_tffa_maxMatches`               | 8       | maximum parallel TFFA matches |
| `proxy_tffa_maxGroupSize`             | 12      | maximum players per TFFA match (both teams combined) |
| `proxy_sv_spectatorTeamOverlay`       | 1       | send the team overlay's HP/armor roster to spectators following a player (works with TaystJK's `cg_drawTeamOverlay` and the stock `team info` HUD; see [Spectator team overlay](#spectator-team-overlay)) |
| `proxy_sv_enableClientPpsLimit`       | 0       | per-client incoming packet-rate limiter: the maximum allowed pps is an integer downscale of the client's measured FPS ± 2, capped by `proxy_sv_globalMaxPps`; excess packets are dropped evenly through the next second, not at its tail (see [Client packet rate](#client-packet-rate)) |
| `proxy_sv_globalMaxPps`               | 100     | global hard ceiling on accepted packets per second per client (`0` = no ceiling); the per-client FPS-derived limit is capped to it |
| `proxy_sv_clientPpsLog`               | 0       | log client packet-rate limit transitions (starts using too many packets / back under the limit); works on its own without the limiter |
| `proxy_sv_voidTimeNudge`              | 0       | neutralise a player's non-zero `cl_timeNudge` on the server: when the estimated range excludes 0, shift their usercmds' clock by the estimated setting (see [Time nudge neutralisation](#time-nudge-neutralisation)) |
| `proxy_sv_voidTimeNudgeLog`           | 0       | log `cl_timeNudge` non-zero / back-to-zero transitions; works on its own (reports the shift without applying it) |
| `proxy_sv_antiMacro`                  | 0       | freeze a usercmd channel while a burst of reversals runs faster than a hand can (movement every 1-3 commands, view ≥ 15/s, or frame-exact `wait` holds): the macro signature behind scripted ground-strafe acceleration and turn-key view wiggling; hand play is untouched and the player is never told (see [Anti-macro guard](#anti-macro-guard)) |
| `proxy_sv_antiMacroLog`               | 0       | `1` log anti-macro lock/release transitions, `2` add the statistics of locked channels, `3` the statistics of every channel with a reversal (diagnostic); works on its own (reports what would be frozen) |
| `proxy_sv_maxFov`                     | 0       | maximum `cg_fov` value a viewer can benefit from: players outside the corresponding view frustum are hidden for that viewer while their footsteps, swings and saber hum keep playing; `0` = off (see [FOV cap](#fov-cap)) |
| `proxy_sv_maxFovAspect`               | 1.7777778 | screen aspect used to turn `proxy_sv_maxFov` into the rendered view frustum (16:9 default, matching OpenJK/TaystJK's `cg_fovAspectAdjust 1`; use `1.3333334` for 4:3/stock clients or higher for ultrawide) |
| `proxy_sv_maxFovCameraRange`          | 100     | third-person camera setback (world units) assumed by the frustum test; `100` matches the stock `cg_thirdPersonRange`, `0` tests from the player's eye (first-person-strict) |

Note: this Rust version deliberately drops some cvars present in the original
C++ proxy (`proxy_sv_pingFix`, `proxy_sv_antiWallHack`, `proxy_sv_sabersFps`,
`proxy_sv_disableKillCmd`).

### Player mute

`rcon mute <player>` silences a player's chat; `rcon unmute <player>` gives it
back. `<player>` is either the client number `status` shows (`rcon mute 0`) or
any part of the player's name — case- and colour-code-insensitive, exact first,
then a prefix, then the fragment wherever it sits in the name (`rcon mute ade`
finds `Darth Vader`). Names with spaces work as typed (`rcon mute darth vader`);
a fragment shared by several players is answered with the candidates and their
numbers, and a bare `rcon mute` prints the usage and who is currently muted.

A muted player keeps playing normally: only `say`, `say_team`, `tell` and the
voice menu's `voice_cmd` are covered. Mutes are per client slot, cleared when
the player leaves, and kept across a `map_restart`; a full map change starts
with nobody muted. Like every proxy feature they need `proxy_sv_enable 1`, and
the same commands work in the dedicated server's own console.

`proxy_sv_muteMode` selects what a mute does:

- `1` (default) — the stock mute: the four commands are dropped before the game
  sees them and the player is told they are muted each time they try to chat;
- `2` — silent mute: the player's `say`/`say_team`/`tell` are still processed by
  the game, so their own chat line appears in their client as usual, but it is
  delivered to nobody else; no mute or per-message notice is sent, so they have
  no way to tell. `voice_cmd` is dropped silently (it has no chat line to echo);
- `0` — the feature is off: `mute`/`unmute` only answer that it is disabled,
  and no chat is filtered (a slot that still carries a flag is left untouched
  until the feature is turned back on).

In silent mode the server console/log still records the player's line
(`G_Say`'s own `say:` log), which keeps moderation auditable.


### Player ignore

Any player can hide another player's chat from their own client:
`/ignore <player>` silences that player **for them only**, and
`/unignore <player>` brings them back. `<player>` uses the same target shape
as `rcon mute`: the client number `status` shows (`/ignore 3`) or any part of
the name, case- and colour-code-insensitive, exact first, then a prefix, then
the fragment wherever it sits in the name (`/ignore ade` finds `Darth Vader`).
A fragment shared by several players is answered with the candidates and their
numbers instead of guessing; `/ignore` on its own prints the usage and who you
are currently ignoring.

The ignored player is not affected: they keep playing and their lines still
reach everyone else, and they are never told. Only client chat is covered
(`say`, `say_team` and `tell`, including the localized variants); the voice
menu's `voice_cmd` is a sound event, not a chat line, and stays with the admin
mute. Ignores are per client slot and last for the session: they are cleared
when either player leaves (so the next player in a reused slot is not silently
ignored) and kept across a `map_restart`; a full map change starts with nobody
ignored.

The feature is off by default: set `proxy_sv_enableIgnore 1` in the server
command line or at runtime with `rcon proxy_sv_enableIgnore 1`. While it is
off, `/ignore` answers that the feature is disabled and no chat is filtered;
lists already set stay dormant and work again when it is turned back on. Like
every proxy feature it also needs `proxy_sv_enable 1`.


### IP bans

The stock `addip` keeps its filters in memory and mirrors them into the
`g_banIPs` cvar. That cvar is a 256-byte `vmCvar_t` mirror, and the engine's
`Cvar_Update` treats a longer value as fatal: about 25 addresses fit, and
overrunning the limit logs `Cvar_Update: src g_banIPs length N exceeds
MAX_CVAR_VALUE_STRING`, shuts the game down, and leaves the over-long value in
`jampserver.cfg` so the next start fails too. The built-in list is therefore
only usable for a handful of exact addresses.

With `proxy_sv_enableBanFile 1` the proxy takes the ban list over:

- `addip <ip[/prefix]>` adds an exact address or a **CIDR range**
  (`addip 84.17.32.0/19`) and writes it to `proxy_sv_banFile`;
  `removeip <ip[/prefix]>` removes it; `listip` prints the loaded list;
  `reloadbans` re-reads the ban files after a hand edit. All four work over
  rcon and on the dedicated server's console.
- Every connect is checked at `GAME_CLIENT_CONNECT` against the engine's
  real `ip` (the value `SV_DirectConnect` forces into the userinfo), and a
  listed address is refused with the stock `Banned.` message before the player
  enters the game. Bots and LAN/loopback clients are never checked.
- The file is the only source of truth: `addip`/`removeip` write it before the
  change is committed, so a failed write leaves the file and the loaded list in
  agreement. The list is loaded at startup and whenever the two cvars change.

The file is plain text: one address or CIDR range per line, `#` starts a
comment. Host bits are masked (`84.17.32.5/19` is stored as `84.17.32.0/19`),
an exact address means `/32`, and a `/0` (ban everyone) is refused. Invalid
lines are skipped with a console warning naming the line. A missing file is an
empty list, not an error.

```
# proxy_sv_banFile — one entry per line
84.17.32.0/19
203.0.113.7
```

A ready-made cloud/hosting/VPN list ships at `rust/proxy_bans.txt` (tens of
thousands of ranges). Copy it next to your server, point `proxy_sv_banFile` at
it and enable `proxy_sv_enableBanFile`. It is a snapshot: regenerate it with
`tools/update_banlist.py` (Python 3, standard library only), which re-fetches
the AWS, Google Cloud, Azure, Oracle Cloud, DigitalOcean, Linode and Vultr
feeds plus the [X4BNet](https://github.com/X4BNet/lists_vpn) VPN+datacenter
aggregate, strips reserved space, merges adjacent ranges, and refuses to write
a list that would cover a consumer ISP wholesale. The proxy only enforces the
ranges, it does not detect VPNs by itself — so any other blocklist can be
dropped into the file as well.

Notes:

- `addip`/`removeip` rewrite the file in canonical (sorted, deduplicated) form,
  so hand-written comments are not preserved by those two commands. `reloadbans`
  never rewrites it.
- With `proxy_sv_enableBanFile 0` (the default) the whole feature is off: the
  pristine `addip`/`removeip`/`listip` handlers run exactly as before, and no
  connect is checked.
- A pre-existing `g_banIPs` from an older configuration is still enforced by the
  game module's own `ClientConnect`. When the proxy loads a ban file it prints a
  one-line warning if `g_banIPs` is non-empty; move those entries to the file and
  run `set g_banIPs ""`.
- IPv4 only (the engine is IPv4), like the rest of the ban path.

### GUID bans

Some modern clients add a `ja_guid` key to their userinfo. A GUID ban is a
separate, optional registry (`proxy_sv_enableGuidBan`, default `0`) with its own
file (`proxy_sv_guidBanFile`, default `proxy_guid_bans.txt`).

`ja_guid` is **client-asserted**: the player controls the value and can change
it. A GUID ban is therefore a deterrent (it keeps a known install out
cheaply), not a security boundary; a cheater can pick a fresh value and rejoin.
It is never stored in the IP ban file and never shares it:

- the IP file only ever receives IPv4 addresses/CIDR ranges (through `addip` or
  the `ban` command's IP half);
- a GUID line is a strict token (printable ASCII, no whitespace, `#`, `\` or
  `"`, 1-64 characters, compared and stored lower-case), so a crafted userinfo
  value can only ever add a GUID, never an address range.

```
# proxy_sv_guidBanFile — one token per line
0123456789abcdef0123456789abcdef
```

Commands (rcon or the dedicated console):

- `ban <clientNumber|playerName>` — the all-in-one admin command. It resolves
  the target like `mute` (slot, exact name, unique prefix, or name fragment),
  then:
  1. bans the target's **IP** through `proxy_sv_banFile` when
     `proxy_sv_enableBanFile 1`; when that is off it queues the pristine
     `addip <ip>` instead;
  2. bans the target's **`ja_guid`** when `proxy_sv_enableGuidBan 1` and the
     client sends one (a stock client has none: that is reported and the IP
     ban still happens);
  3. kicks the target with the stock `Banned.` reason.
- `addguid <guid>`, `removeguid <guid>`, `listguid` — manage the registry by
  hand (e.g. to undo a wrong `ban`, or add a value an admin collected outside
  the game).
- `reloadbans` — re-reads both ban files after a hand edit.

Enforcement happens at `GAME_CLIENT_CONNECT`, before the game sees the client:
a listed `ja_guid` is refused with `Banned.`. Like the IP list it applies to
the next connect (or the reconnect after a map change); a player already in the
game when you add their GUID is only kicked by `ban`, not by editing the file.
The reply to `ban` names what was added and why anything was skipped.

### Crash diagnostics

`proxy_sv_debugLog` turns on a trace meant to identify a player who crashes the
dedicated server. Set it at runtime with `rcon proxy_sv_debugLog 2` (or
permanently with `+set proxy_sv_debugLog 1` in the server command line) and
reproduce the crash:

- `0` (default) — off. Nothing is collected or formatted.
- `1` — suspicious events only: commands the proxy blocked (anti-cheat,
  crash guards, chat/command injection, oversized callvote), userinfo fields
  forced back to safe values, rcon attempts (password redacted), rejected
  downloads, oversized `getstatus`/`getinfo` queries and malformed client
  messages.
- `2` — everything in `1`, plus every client command with its full text and
  length, every `userinfo` update, and client connect/begin/disconnect events.

Each line records the server time (`t=`, the engine's `svs.time`), the wall
clock (`unix_ms=`), the client slot, its name and remote address where
available. Client bytes are escaped (`\n`, `\r`, non-printables as `\xNN`) so a
malicious command cannot forge log lines. Lines are written and flushed to
`proxy_debug.log` in the server's working directory *before* the command is
handed to the engine/game, so the last line survives a segmentation fault; they
are also printed to the server console (and therefore `qconsole.log` when
`logfile` is on). The rcon password is never written.

Use it only while investigating: level `2` is intentionally verbose, and a
busy server will produce a lot of lines. A typical report is the last few
`clientcmd`/`userinfo` lines before the server died, plus the client name and
IP. If the file cannot be created, the proxy says so once on the console and
keeps logging to the console only.


### TaystJK scoreboard deaths

While `proxy_sv_enable` is `1`, the proxy advertises `taystJKinfo 128`
(`TAYSTJK_INFO_SCORE_DEATHS`) in the server info and, for clients that opt in,
appends each player's death count as a 15th int to the `scores` command. That
is what [TaystJK](https://github.com/taysta/TaystJK)'s cgame uses to show an
up-to-date K/D on the tab scoreboard; the stock game module only sends 14 ints
and never advertises the key. The death count is the game's own
`ps.persistant[PERS_KILLED]`, read when the scoreboard is sent.

The deaths field is sent per client, and only to a client whose cgame
advertises it: the proxy appends the 15th int only when the recipient's
userinfo carries `taystJKcgflags` bit 0 (`TAYSTJK_CGFLAG_SCORE_DEATHS`, TaystJK
cgame builds from commit `30fc5979b`, 2026-10-23). Every other client — older
TaystJK builds, stock JKA, JA+, jaPRO — receives the pristine 14-int message,
so mixed rosters work: updated clients see K/D, older ones keep a correct
scoreboard instead of having the whole roster collapse onto one team. The
`taystJKinfo 128` server info key stays advertised because that is what tells a
capable cgame to parse the 15-int layout; cgames without support ignore it.

### Spectator team overlay

With `proxy_sv_spectatorTeamOverlay 1` (the default), a spectator who is
following a player receives the same team information the players themselves
get: name, health, armor, weapon and powerups for the followed player's team,
refreshed once per second. That is what makes
[TaystJK](https://github.com/taysta/TaystJK)'s `cg_drawTeamOverlay` (including
the bar styles 5/6) show live HP while spectating; the stock game module only
sends this data to teammates, so a spectator's overlay stays empty or keeps
stale values.

It is the stock `tinfo` message in its usual format, so it works with the
latest TaystJK cgame as shipped and with unmodified clients — nothing has to be
installed client-side. Two limits are worth knowing:

- the overlay is drawn while the spectator **follows a player** (the point of
  view the cgame uses comes from that player); in free-fly spectating the
  client hides it, which cannot be changed from the server;
- when the spectator switches to a player on the other team, that team's
  roster arrives immediately; otherwise it refreshes once per second, exactly
  like a teammate's overlay.

`proxy_sv_spectatorTeamOverlay 0` stops the feed; only the master switch
`proxy_sv_enable` and team gametypes are needed on top of it.

### Net status

With `proxy_sv_enableNetStatus 1`, the `netStatus` and `showNet` commands print
a per-player table (rate-limited to once per 500 ms, bots excluded):

```text
ping    rtt   jit  fps packets/s     timeNudge snaps   rate name
---- ------ ----- ---- --------- ------------- ----- ------ ---------------
   0    0.0   0.0   60        30 [-17.9..-10.5]    20  25000 client0
   0    0.0   0.0   60 ^1      100^7 [-17.9..-10.5]    20  25000 client0
   0    0.0   0.0   60 ^1   100^7/73 [-17.9..-10.5]    20  25000 client0
```

* **ping** — the player's ping as the game reports it on the scoreboard.
* **rtt** — round-trip time: how long a message needs to travel to the
  player's game and back, in milliseconds. This is the "real" ping — the one
  the server measures itself. Lower is better; on the same machine it reads 0.
* **jit** — jitter: how much that round-trip time wobbles between messages.
  A steady line to the player shows a small number; a flaky one (wifi,
  congestion, a bad route) shows a large one. 0 means rock steady.
* **fps** — how many moves per second the player's game produces (its frame
  rate). A typical player is 30-125; a very fast PC might show 200.
* **packets/s** — how many packets per second the player's game sends, counted
  from every incoming packet (the same counter the
  [packet-rate limiter](#client-packet-rate) uses), so it also sees
  empty/duplicate packets that carry no moves. A packet bundles those moves, so
  this is how often the game flushes them: a player on `cl_maxpackets 30` sends
  ~30, while a player on the same machine (where the game skips the packet
  limiter) sends one per frame, i.e. packets/s = fps. When the player uses more
  than the limiter allows, the value turns red. With
  `proxy_sv_enableClientPpsLimit 1` the value shown is the **effective** rate —
  what the limiter lets through (200 observed with a limit of 100 reads `100`)
  — and it is red whenever the player is being limited. With the limiter off
  there is no effective rate, so the red actual rate is followed by the rate
  that would be enforced, in the normal colour: `100/73` means "sending 100,
  limited to 73". Until the first second of traffic has been measured, the
  older count of packets carrying moves is shown instead.
* **timeNudge** — the player's `cl_timeNudge` setting, which the client never
  tells the server, so it is **estimated** from the clock its moves are
  stamped with and reported as a range: the likely interval, e.g.
  `[-17.9..-10.5]` means "between -17.9 and -10.5" (true value -15 here).
  `-` means there is not enough data yet. Values are never clamped to ±30:
  a reading outside that range is evidence of a modified client. The value is
  coloured:
  - **white** when 0 is inside the range (consistent with the default
    setting);
  - **yellow** when 0 is outside the range but the range is not entirely
    outside the legal clamp (the player is confidently using a non-zero
    setting);
  - **red** when the whole range is outside the client's ±30 clamp
    (`max < -30` or `min > 30`), i.e. the reading cannot come from a stock
    client.

  With `proxy_sv_voidTimeNudge` active, the column instead shows the value the
  server enforces: `0` for every player, in **red** on the players whose range
  excludes 0 (their timenudge is being voided), white on the rest.
* **snaps** — how many times per second the server sends this player a view of
  the game (their `snaps` setting, capped by the server's `sv_fps`).
* **rate** — the player's bandwidth setting (bytes per second the game is
  allowed to use).
* **name** — the player.

### Time nudge neutralisation

`cl_timeNudge` is a client-side setting that shifts the clock the player's game
stamps into its moves: a negative value makes the client run "ahead" of the
server. It is never transmitted, but the [net status](#net-status) estimator
recovers it. With `proxy_sv_voidTimeNudge 1`, the proxy uses that estimate to
put the game back on the neutral clock: when the reported range **excludes 0**
(so the player is confidently using a non-zero setting) every usercmd's
`serverTime` is shifted by the estimated setting before the game processes it.
The client keeps its own setting; the server stops accepting the timing
advantage.

Details worth knowing:

- The shift is the rounded estimate, clamped to ±30 ms — the 2003 release
  client's own `cl_timeNudge` clamp. A reading beyond that is left uncorrected
  rather than amplified.
- It engages only after two consecutive refreshed ranges exclude 0 (about one
  second), because the first window after a connect can still contain startup
  artifacts, and it needs the ~5 s estimation window to settle. A range that
  contains 0 disengages immediately.
- The applied shift moves by 1 ms per command in either direction, so engaging
  or disengaging never drops a move (a 30 ms jump would fail the game's
  `msec < 1` guard) or creates one oversized movement frame. The player's
  timeline is shifted by the same total amount either way.
- Only the server side is corrected: the player's own prediction and rendering
  still run on their shifted clock, but the game no longer processes their
  moves on it.
- While the feature is active the netStatus table's `timeNudge` column shows
  what the server enforces: `0` for every player, **red** on the players being
  voided (their estimated range excludes 0) and white on the rest (a player
  with no settled estimate keeps `-`). The estimator itself still reads the raw
  command stamps, so the red marks and the log follow the player's real
  setting.
- It needs `proxy_sv_enable 1` like every feature, but not
  `proxy_sv_enableNetStatus`.

`proxy_sv_voidTimeNudgeLog 1` logs transitions, like the packet-rate log:

```text
----- proxy-rs: timeNudge non-zero: estimate=[-17.0..-11.1] shift=-14 client=2 name="..." adr="..."
----- proxy-rs: timeNudge back to zero: estimate=[-1.2..3.1] client=2 name="..." adr="..."
```

It works on its own: with `proxy_sv_voidTimeNudge 0` it still reports the shift
the feature *would* apply, without rewriting any command. A client that stays
on the same side of zero is silent.

### Client packet rate

`proxy_sv_enableClientPpsLimit 1` activates the per-client incoming packet-rate
limiter. The maximum allowed pps is computed per client from their **estimated
FPS** and the global ceiling:

```text
limit = max over f in FPS-2 ..= FPS+2 of
            min(f / ceil(f / proxy_sv_globalMaxPps), proxy_sv_globalMaxPps)
```

The FPS estimate is uncertain by ±2, so the formula is evaluated for all five
values and the **highest** result is used; the uncertainty can only raise the
limit, never lower it. This makes the accepted rate an exact `1/K` fraction of
the client's rate (`K = ceil(f / globalMaxPps)`), so the eviction pattern is a
clean "drop one out of every K packets":

| FPS | global max | samples | limit |
|---|---|---|---|
| 60 | 100 | 58..62 → 58..62 | 62 |
| 100 | 100 | 98..102 → 98,99,100,50,51 | 100 |
| 144 | 100 | 142..146 → 71,71,72,72,73 | 73 |
| 250 | 100 | 248..252 → 82,83,83,83,84 | 84 |

The FPS is computed by the same function as the netStatus table
(`netstatus_est::fps_pps`), so both agree. `proxy_sv_globalMaxPps 0` removes
the ceiling and the highest sample (`FPS + 2`) is used; if the FPS is not yet
trustworthy, only the global ceiling is enforced (and `0` there means fail
open).

The netStatus table renders the same verdict: its `packets/s` cell comes from
this counter (all incoming packets, not only packets carrying moves) and an
over-limit client is red. With this feature on the cell shows the **effective**
rate the limiter lets through (200 observed with a limit of 100 reads `100`,
red); with it off there is no effective rate, so the limit follows in the
default colour (`100/73`, only `100` red), showing what would be enforced
without pretending it is. The counter runs while either this feature or
`proxy_sv_enableNetStatus` is on; packets are only actually dropped when this
feature is on.

A stock client sends at most one packet per generated usercmd (one per rendered
frame; `cl_maxpackets` can only make it send fewer), so a client sending more
packets than its frame rate can explain — duplicated/empty packets — is the
shape this catches. Genuinely new usercmds raise the measured FPS and therefore
the limit; limiting the command rate itself is a separate concern.

- The rate is measured over one-second windows and the limit follows the
  client's FPS refreshed once a second. Until the client's command stream
  covers a full window (`< 0.9 s`, e.g. just connected), the FPS is not
  trusted; only the global ceiling applies, and with no ceiling the limiter
  fails open.
- A client inside its limit is never touched. Above it, the excess is dropped
  **evenly through the next second**: with a limit of 63 and a client sending
  100, the proxy accepts 63 and drops 37 interleaved (about two accepted, one
  dropped), never "the last 37".
- The first second after a connect (and after a 2 s silence) is measurement
  only, so handshake/reconnect bursts are not filtered.
- Being dropped is exactly a lost UDP packet to the engine, so the netchan and
  the client's `cl_packetdup` redundancy handle it like any other loss.
- The feature does not require `proxy_sv_enableNetStatus`; it uses the same
  per-usercmd feed internally for the FPS estimate.

With `proxy_sv_clientPpsLog 1`, the server logs **transitions** for each client,
not every second:

- one line when the client is first seen above its limit;
- one line when a completed window brings it back under;
- another line if it starts again.

A client that stays on the same side of its limit is silent.

```text
----- proxy-rs: pps over limit: rate=100 limit=62 fps=60 window_ms=1000 client=2 name="..." adr="..."
----- proxy-rs: pps back under limit: rate=40 limit=62 fps=60 window_ms=1000 client=2 name="..." adr="..."
```

The log works on its own: it keeps the packet accounting and the FPS feed
running without `proxy_sv_enableClientPpsLimit`, so it reports the verdict the
limiter *would* apply (the same one the netStatus table shows). While the cvar
is `0` no transition state is kept, so enabling it reports the next over-limit
window. `proxy_sv_enable 0` detaches the hook, so the whole feature is off with
the master switch.

### Anti-macro guard

`proxy_sv_antiMacro` (default `0`, off) targets config-file macros that produce
input a hand cannot: a movement axis, the view, or the attack button reversing
faster than a person can. Two examples it is aimed at are the scripted ground
strafe (A/D alternation every few commands, which feeds `PM_Accelerate` a fresh
`wishdir` each frame) and the turn-key view wiggle used against the saber's
per-frame position update.

The server never sees "both keys held": `CL_CreateCmd` sums the movement keys
before sending, so `+moveleft` with `+moveright` arrives as `rightmove == 0`.
What it does see is the sum's **sign changing between consecutive commands**.
That is the signal the guard watches, on `forwardmove`, `rightmove`, `upmove`,
the `yaw`/`pitch` deltas and the attack/alt-attack bits.

The unit is a **burst**, not a time window: reversals are chained into a *run*
while the gap to the previous one is at most 200 ms, and a longer gap starts a
new run. This matters because a bind is usually a fixed sequence — six holds,
five reversals, then a human gap until the key is pressed again. A window that
spans those gaps averages the burst away; a run does not.

Within a run, two independent tests are used:

- **rate** — the mean interval is shorter than a hand can sustain: at most 3
  **commands** for a movement axis (the acceleration payoff is per command), 65
  **ms** for the view (≈15 reversals/s) or 60 ms for the attack button. Four
  reversals are enough: three frame-exact intervals is already far beyond a hand,
  and randomising the wait counts changes neither the mean nor the payoff.
- **spread** — after four reversals, the hold lengths in commands are confined
  to a range of at most 2. That is what a config `wait` produces at *any* rate
  and on any channel; a hand's timing spreads over several commands per hold.

Once a channel has fired it stays **wary** for 3 s: the next burst fires after
only three reversals, so repeated presses are neutralised earlier and earlier.

Amplitude is not a signal: at 200 fps the stock `cl_yawspeed 140` turns 0.7°
per command for a hand and a macro alike, and a movement axis is `±127` (run) or
`46` (walk) either way.

Frozen means *held*, not zeroed: the command keeps the value of the last
command before the run, so the player keeps moving in the direction they already
chose while the reversal (and its acceleration or wiggle) stops. The guard keeps
reading the raw command, so a script that keeps reversing stays locked, and the
lock is released 400 ms after the last reversal. Nothing is sent to the player.

`proxy_sv_antiMacroLog 1` writes lock and release transitions to the server
console and works without the guard being enabled, so an operator can check the
thresholds against their own players before freezing anything. `2` adds the
figures of a locked channel; `3` adds every channel with a run, which is the
diagnostic mode:

```text
jampgame-proxy-rs: antiMacro client=2 name="..." adr="..." yaw locked
jampgame-proxy-rs: antiMacro debug client=2 name="..." adr="..." cmds=6041 yaw rev=4 mean=55ms/11cmd holds=11..11 swing=6deg locked=1
```

`cmds` proves the command feed is alive, `rev` counts reversals in the run,
`mean` is the mean interval, `holds` the shortest/longest hold in commands and
`swing` the view swing in degrees.

Not covered: a single aim snap (one large angle jump is not a reversal), attack
timing that stays inside the game's own `weaponTime` gate, and a script that both
randomises its waits widely *and* runs at hand speed (it then looks like a hand
in both tests, and is correspondingly weaker).

### FOV cap

`proxy_sv_maxFov` (default `0`, off) caps the `cg_fov` value a client can
benefit from. Stock Jedi Academy never sends `cg_fov` to the server — it is a
cgame-only cvar — so the proxy cannot see who raised it. Instead it caps what
every viewer receives. A player outside the view frustum that corresponds to
the cap is hidden for that viewer; a client running a larger `cg_fov` sees no
more players than one at the cap, and the extra field is empty. Legitimate
values below the cap are unaffected, and the viewer's own entity is never
touched. The cap applies only to clients actually in the game: a free or
following spectator is never affected, and spectator entities are never
hidden. The test uses both the horizontal and vertical FOV, so looking up or
down does not hide players that are still on screen; above 45° of pitch the cap
pauses entirely, because the client's third-person camera pitch then diverges
too far from the server's aim for the server to know what is on screen.

`cg_fov` is not the rendered horizontal angle on modern clients: OpenJK and
TaystJK default to `cg_fovAspectAdjust 1`, which widens it for the screen
aspect (`fov_x = 2*atan(tan(cg_fov/2) * 0.75 * aspect)`). `proxy_sv_maxFov` is
therefore converted with `proxy_sv_maxFovAspect` (default 16:9) before the
frustum test, so `proxy_sv_maxFov 100` still covers `cg_fov 97` on a 16:9 screen
(where `cg_fov 97` renders as about 113°). Set it to `1.3333334` for 4:3
clients or stock behaviour, and to the actual aspect for ultrawide screens
(e.g. `2.3333334` for 21:9).

Third person matters too: the client renders from a camera up to
`cg_thirdPersonRange` (default 100) behind the player, so a close target at
the screen edge has a smaller bearing from the camera than from the player's
eye. `proxy_sv_maxFovCameraRange` (default `100`) shifts the test point back by
that many world units, which keeps close edge targets visible in third person,
and the frustum test uses the camera's pitch as well as its yaw;
set it to `0` for a first-person-strict server, or raise it if your players use
a larger `cg_thirdPersonRange`.

Hiding is instant: the player entity is marked `EF_NODRAW` for that viewer, so
cgame's `CG_Player` returns at the top instead of fading. The entity stays in
the snapshot, so its server events (footsteps, weapon/force sounds) still fire
at the true position. A per-viewer mind-trick bit and a 1% model scale remain
as layers for a client that ignores `EF_NODRAW`. `iModelScale` only scales the
model and its hand bolts, not the client-side saber blade length, so a drawn
saber is also marked `saberHolstered = 2` (hidden) and its native hum is
replaced by `s.loopSound` at the hilt's own sound index:

- walking/running footsteps (`EV_FOOTSTEP`, surface-aware) still play at the
  true position;
- the saber hum, swings, hits and unholsters still play (the hum is the
  entity loop at the hilt's own sound index, so a client that turns
  `cg_ambientSounds` off hears no entity loops at all, as with any other loop);
- a **thrown** saber is left visible and audible as-is.

The per-viewer changes are reverted immediately after the engine copies that
viewer's snapshot, so the server's shared game state is never left modified.

Two limits are worth knowing:

- a viewer with **Force Sight** active bypasses the stock mind-trick check; the
  entity is still `EF_NODRAW`, so nothing is drawn, and the 1% scale is there
  for a client that strips NODRAW too;
- a determined custom client can strip `EF_NODRAW`, ignore the mind-trick bit
  and normalise the model scale; removing the player entity from the packet
  would stop that, but it would also remove the client-side footsteps and saber
  hum;
- the replacement hum is an entity loop: TaystJK/OpenJK's `CG_EntityEffects`
  plays it even for `NODRAW`, but the stock 1.01 cgame skips loop sounds on
  `NODRAW`, so a stock client would lose the replaced hum (it never had the
  mind-trick instant hide either).

Set it at startup or at runtime:

```text
seta proxy_sv_maxFov 100
```

or `rcon proxy_sv_maxFov 100`. Like every proxy feature it needs
`proxy_sv_enable 1`.

### Team lock

`proxy_sv_lockTeamsAfterXSecs` is the grace delay, in seconds, before a round's
teams lock (default `0`, off). When a round starts (map load or `map_restart`),
teams stay open for that many seconds so players can still join. If a player
joins a team in an instance during the window, that instance is **not** locked
for the round. If nobody joins, the roster is locked when the window expires:
each team is capped at its current size, provided the roster is even at that
moment (a `2 vs 2` locks at 2; an uneven `2 vs 3` is left alone). A cap is a
reservation: it does not shrink when players leave, so the freed slots stay
open for the same team. A join to a full, locked team is refused with a message
(an admin's `forceteam` is subject to the same cap while the lock is active).
`0` disables the feature entirely. This cvar replaces the former
`proxy_sv_lockTeams` minimum-team-size setting; old configs need the new name.

With `proxy_tffa_enable 1` the lock runs **per instance**: only the matches that
existed at the round start are eligible, each is snapshotted against its own
roster, and a join in one match cancels only that match's lock; a join is
checked against the caps of the match the player belongs to. A match created
after the round started has no round-start roster and stays unlocked until the
next round (a `map_restart` keeps the matches, so they are eligible again then).
Without parallel TFFA the whole server is the one (main) instance.

### Per-team-size rules

`proxy_sv_teamSizeRules` overrides the limits for the size the round actually
started at. Rules are separated by spaces/commas and defined as
`size:timelimit:fraglimit:capturelimit`; an empty (or `-`) limit is left
untouched:

```text
set proxy_sv_teamSizeRules "2:20:30:5,3:15:40:0"
```

A round with no exact rule falls back to the nearest *smaller* defined size
(5 vs 5 → 4 vs 4 → 3 vs 3 → …). A roster below every defined size (e.g. 1 vs 1
with rules starting at 2) has no rule, so the server's original
`timelimit`/`fraglimit`/`capturelimit` values are restored. The effective rule
is reconciled every frame, so the limits adapt when the roster changes mid-match
(e.g. a game that becomes 2 vs 2 adopts the 2 vs 2 rules, and dropping back
below the smallest rule restores the server limits); they are only rewritten
when the effective decision actually changes. This works whether or not
`proxy_sv_lockTeamsAfterXSecs` is enabled.

With parallel TFFA matches, the rule follows the matches being played rather
than the sum of all players: empty/spectator-only matches are ignored, and a
rule applies only when every populated match is the same `N vs N` (two 2 vs 2
matches use the `2` rule, not the `4` one). If the matches differ in size, or a
populated match is itself uneven, the limits keep their previous rule until the
sizes agree again.

### Parallel TFFA matches

With `proxy_tffa_enable 1` (GT_TEAM only), players self-organize into
parallel matches on the same map: `tffa_create` opens a match, puts the
creator on its smaller side (red when it is empty) and joins it,
`tffa_join <id>` / `tffa_leave` switch matches (`tffa_join 0` returns to the
main game, same as `tffa_leave`) and `tffa_list` shows them.
`tffa_create <password>` opens a **password-protected** match,
`tffa_join <id> <password>` joins one to play, and `tffa_password <password>`
gives the password of the match you are already in (this is also the way in
after a match was promoted into the main game). A password only gates joining
a team (`/team red|blue`, `team free`): **anyone can spectate a protected
match without it** — `tffa_join <id>` with no password puts you in it as a
spectator, and `tffa_list` marks it with `[password]`. Passwords are
case-sensitive, up to 32 characters, and are never written to the proxy log.
These commands work while playing, not only from spectator: a travel command puts
the player in the spectator team first (their character is killed), then
switches the instance, so after `tffa_join` they re-pick a team with
`/team red|blue`. A created match with nobody in game there (everyone left
or went spectator) closes itself; the main game always stays. A
command that names the instance the player is already in changes nothing
(no needless death). Auto team picks (`team free`, the client's
auto-join, `g_teamAutoJoin`) balance the match being joined rather than
the whole server, so one match's sides stay even.
Each match is isolated: cross-match damage is
dropped, cross-match players (and their projectiles, sabers and kill feed)
are hidden from snapshots, and the scoreboard lists all matches' players — so
you can see who is where — with the other matches' players grouped as
spectators and the red/blue header plus each player's score taken from the
viewer's own match. Global chat is server-wide; team chat (`say_team`) stays
inside its own match, so blue in main cannot talk to blue in a created match
(spectators are split the same way). Every match shares the
server's own `fraglimit` independently (the global team scores are kept
neutralised so the pristine win check never fires early); when a match hits
it, the match keeps playing for one second with its damage void — the base
game's fraglimit delay, so the last kill is not followed by an instant
teleport — and then its members are moved to spectator and may join another
match. The finished match remembers its group (who played and for which team):
if those players do not start playing in another match, the next `map_restart`
brings the match back with them in it. A player who starts playing in another
match (red/blue there, main included) is forgotten by the old one.
`timelimit` is global: at the limit each match ends unless it is tied, and a
tied match keeps playing (sudden death) until a frag separates the teams
(an untied match ends through the same one-second damage-void window).
When a tied match keeps the map alive, a finished untied match's members are
moved to spectator as usual. When **no** tied match with players remains, the
whole map ends instead: every match keeps its players and tallies through the
final scoreboard — so each player reads their own instance's result instead of
being sent to main — and the map then restarts normally.
A `map_restart` keeps the matches: every player reconnects into the match and
team they were in — including a match that had already finished, which is
re-created from its remembered group and puts each player back on their old
team — with that match's doors and triggers rebuilt, while the
scores start from zero. A full map change starts everyone in the main game and
drops every remembered match.
`proxy_sv_lockTeamsAfterXSecs` works per match while TFFA is on: each instance
is evaluated against its own roster and a join cancels only its own lock (see
[Team lock](#team-lock)).
`proxy_sv_teamSizeRules`
keeps working: each match's own team size is used, and a rule applies when all
matches being played have the same size (see [Per-team-size rules](#per-team-size-rules)).
HUD team scores stay
global (per-match HUD would need a client mod). Setting `proxy_tffa_enable`
back to `0` closes every created match: its players are moved to spectator
with a message explaining why, the map's per-match copies go away, and
everyone keeps playing in the main game, whose score is kept instead of
being reset.

**The world is per match.** Doors, plats, elevators, buttons, triggers and map
items are duplicated for every match, so a door opened in one match is open only
there, a trigger fires only for its own match's players, and an item taken in one
match is still lying there in the others. The duplicate set is created with the
match (the map is measured at level start) and given back when it closes.
Entities the game creates *while the map runs* — a touch-openable door's trigger,
an asteroid field's rocks, a dropped weapon, a script's spawn — are attributed to
the match whose code created them, and the questions a match's world entities ask
about the world (collision, touch, `targetname` chains, blast radius and box
queries) see only their own match's copies plus everything shared: spawn points,
lights, static models and a player's own projectiles stay as they were. Spawn
points are shared, but a match picks among them without counting another match's
players: its spawn-in never runs out of spots because other matches are full, so
it never drops its players on top of each other. A map
whose interactive entities cannot be duplicated (an NPC or vehicle actor among
them) takes no parallel matches at all, and `tffa_create` is refused with a
message when the map's remaining entity slots cannot hold another whole set —
coverage is never traded for match count.

## Usage

1. Rename the original `jampgamei386.so` of your server's gamedir to
   `jampgame_original.so`.
2. Get or build the proxy (see below) and put it in the place of the original
   `jampgamei386.so`.
3. Start the server as usual — the proxy loads the original module itself.

## How it works?

`linuxjampded` loads the proxy and uses it as any `jampgamei386.so`. The proxy
forwards all `vmMain` calls and game traps to `jampgame_original.so`. It hooks
selected engine and game functions (entry detours, call-site retargets and
inline patches) to inject its features without rewriting the original game
module, and reads the engine's memory layer (`serverStatic_t`/`server_t`,
cvar slots) to implement cvars and the net-status table.

```
linuxjampded ──dlopen──> jampgamei386.so (proxy) ──dlopen──> jampgame_original.so
                vmMain / syscall traps forwarded both ways,
                selected functions hooked by the proxy
```

## Building

The artifact is a 32-bit (`i686`) Linux shared object; the recommended build
route is the containerised dev environment (Docker). From `rust/`:

```sh
./dev.sh sh ./scripts/build.sh      # -> ./jampgamei386.so
```

See [`rust/README.md`](rust/README.md) for the full build, test and run
instructions (end-to-end engine test, persistent bot server).

## Contributing

Issues and pull requests are welcome.

