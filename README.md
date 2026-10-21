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
| `proxy_sv_enable`                     | 1       | master switch: when `0`, detach every hook and forward everything to the original game module   |
| `proxy_sv_enableRconCmdCooldown`      | 0       | set a cooldown on RCON commands to avoid it being flooded                                       |
| `proxy_sv_maxCallVoteMapRestartValue` | 60      | maximum value allowed on a `callvote map_restart`                                               |
| `proxy_sv_modelPathLength`            | 64      | maximum model path length accepted from clients                                                 |
| `proxy_sv_enableNetStatus`            | 0       | compute and make accessible the players network status (`netStatus`/`showNet` commands)         |
| `proxy_sv_enableEndGameStats`         | 1       | print the personal/global/best-player stats tables at the end of a game                         |
| `proxy_sv_antiHpTeller`               | 0       | prevents HP tellers usage                                                                       |
| `proxy_sv_minJumpTime`                | 0       | minimum time (in ms) a player should be allowed to jump (anti low-jump scripting)               |
| `proxy_sv_lockTeams`                  | 0       | lock both teams at their round-start size when a TDM/CTF round starts even and populated (>=2 per team) |
| `proxy_sv_teamSizeRules`              | (empty) | per-team-size `timelimit`/`fraglimit`/`capturelimit` override, reconciled as the roster changes, independent of `proxy_sv_lockTeams` (see below) |
| `proxy_tffa_enable`                   | 0       | run parallel self-organized TFFA matches on one GT_TEAM map (`tffa_create`/`tffa_join`/`tffa_leave`/`tffa_list`); damage/scoreboard/visibility/team chat isolated per match, global chat stays server-wide (see below) |
| `proxy_tffa_maxMatches`               | 8       | maximum parallel TFFA matches |
| `proxy_tffa_maxGroupSize`             | 12      | maximum players per TFFA match (both teams combined) |

Note: this Rust version deliberately drops some cvars present in the original
C++ proxy (`proxy_sv_pingFix`, `proxy_sv_antiWallHack`, `proxy_sv_sabersFps`,
`proxy_sv_disableKillCmd`).

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
`proxy_sv_lockTeams` is enabled.

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
it, its members are moved to spectator and may join another match.
`timelimit` is global: at the limit each match ends unless it is tied, and a
tied match keeps playing (sudden death) until a frag separates the teams.
Once no tied match with players remains, the map ends and restarts normally.
A `map_restart` keeps the matches: every player reconnects into the match and
team they were in, with that match's doors and triggers rebuilt, while the
scores start from zero. A full map change starts everyone in the main game.
`proxy_sv_lockTeams` /
`proxy_sv_teamSizeRules` stay off while TFFA is on. HUD team scores stay
global (per-match HUD would need a client mod).

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

