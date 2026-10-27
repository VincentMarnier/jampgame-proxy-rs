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
| `proxy_sv_enableRconCmdCooldown`      | 0       | set a cooldown on RCON commands to avoid it being flooded                                       |
| `proxy_sv_maxCallVoteMapRestartValue` | 60      | maximum value allowed on a `callvote map_restart`                                               |
| `proxy_sv_modelPathLength`            | 64      | maximum model path length accepted from clients                                                 |
| `proxy_sv_enableNetStatus`            | 0       | compute and make accessible the players network status (`netStatus`/`showNet` commands print the [net status](#net-status) table); when `0` the feature is completely inert — no collection, no computation, no table |
| `proxy_sv_enableEndGameStats`         | 1       | print the personal/global/best-player stats tables at the end of a game                         |
| `proxy_sv_antiHpTeller`               | 0       | prevents HP tellers usage                                                                       |
| `proxy_sv_minJumpTime`                | 0       | minimum time (in ms) a player should be allowed to jump (anti low-jump scripting)               |
| `proxy_sv_lockTeamsAfterXSecs`        | 0       | grace delay in seconds (`0` = off) before a TDM/CTF round's teams lock: teams stay open for that long; a join in an instance cancels its lock for the round, otherwise the even roster is capped at its size; with parallel TFFA every instance is handled per its own roster (see [Team lock](#team-lock)) |
| `proxy_sv_teamSizeRules`              | (empty) | per-team-size `timelimit`/`fraglimit`/`capturelimit` override, reconciled as the roster changes, independent of `proxy_sv_lockTeamsAfterXSecs` (see below); with parallel TFFA it uses the per-match sizes and applies one rule only when every populated match agrees |
| `proxy_tffa_enable`                   | 0       | run parallel self-organized TFFA matches on one GT_TEAM map (`tffa_create`/`tffa_join`/`tffa_leave`/`tffa_list`); damage/scoreboard/visibility/team chat isolated per match, global chat stays server-wide (see below) |
| `proxy_tffa_maxMatches`               | 8       | maximum parallel TFFA matches |
| `proxy_tffa_maxGroupSize`             | 12      | maximum players per TFFA match (both teams combined) |

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
voice menu's `voice_cmd` are dropped, and they are told they are muted. Mutes
are per client slot, cleared when the player leaves, and kept across a
`map_restart`; a full map change starts with nobody muted. Like every proxy
feature they need `proxy_sv_enable 1`, and the same commands work in the
dedicated server's own console.

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

### Net status

With `proxy_sv_enableNetStatus 1`, the `netStatus` and `showNet` commands print
a per-player table (rate-limited to once per 500 ms, bots excluded):

```text
ping    rtt   jit  fps  pps     timeNudge snaps   rate name
---- ------ ----- ---- ---- ------------- ---- ------ ---------------
   0    0.0   0.0   60   30 [-17.9..-10.5]   20  25000 client0
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
* **pps** — how many packets per second the player's game sends. A packet is
  a bundle of those moves, so this is how often the game flushes them: a
  player on `cl_maxpackets 30` sends ~30, while a player on the same machine
  (where the game skips the packet limiter) sends one per frame, i.e. pps = fps.
* **timeNudge** — the player's `cl_timeNudge` setting, which the client never
  tells the server, so it is **estimated** from the clock its moves are
  stamped with and reported as a range: the likely interval, e.g.
  `[-17.9..-10.5]` means "between -17.9 and -10.5" (true value -15 here).
  `-` means there is not enough data yet. Values are never clamped to ±30:
  a reading outside that range is evidence of a modified client.
* **snaps** — how many times per second the server sends this player a view of
  the game (their `snaps` setting, capped by the server's `sv_fps`).
* **rate** — the player's bandwidth setting (bytes per second the game is
  allowed to use).
* **name** — the player.

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
They work while playing, not only from spectator: a travel command puts
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
match.
`timelimit` is global: at the limit each match ends unless it is tied, and a
tied match keeps playing (sudden death) until a frag separates the teams
(an untied match ends through the same one-second damage-void window).
Once no tied match with players remains, the map ends and restarts normally.
A `map_restart` keeps the matches: every player reconnects into the match and
team they were in, with that match's doors and triggers rebuilt, while the
scores start from zero. A full map change starts everyone in the main game.
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
lights, static models and a player's own projectiles stay as they were. A map
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

