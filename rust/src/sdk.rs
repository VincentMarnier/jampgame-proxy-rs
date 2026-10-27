//! SDK ABI surface the proxy must reproduce exactly.
//!
//! Every number and layout here is either
//!   - an SDK enum ordinal (verified by compiling `g_public.h` with the i386
//!     toolchain, `tools/` oracle), or
//!   - a layout offset/size produced by the same compiler from the SDK headers
//!     (the pristine `original/jedi-academy-sdk/`; the former proxy `src/sdk/`
//!     subset was verified byte-identical to it for these types before the
//!     proxy source was removed — see `docs/architecture.md` and U-003).
//!
//! Do not change a value without re-running the layout oracle and updating the
//! corresponding unit test.
#![allow(dead_code)] // version-locked reference table; the cfg(test) pins gate it

use core::ffi::c_char;

// ---------------------------------------------------------------------------
// Constants (SDK `q_shared.h` / `g_local.h` / `qcommon.h`)
// ---------------------------------------------------------------------------

pub const MAX_CLIENTS: usize = 32;
pub const MAX_NETNAME: usize = 36;
pub const MAX_INFO_STRING: usize = 1024;
pub const MAX_STRING_CHARS: usize = 1024;
pub const MAX_TOKEN_CHARS: usize = 1024;
pub const MAX_CVAR_VALUE_STRING: usize = 256;
pub const MAX_OSPATH: usize = 4096; // PATH_MAX on Linux
pub const CVAR_ARCHIVE: u32 = 0x0000_0001;
/// `CVAR_SERVERINFO` (`q_shared.h:1786`): included in `CS_SERVERINFO`.
pub const CVAR_SERVERINFO: u32 = 0x0000_0004;
/// `CVAR_ROM` (`q_shared.h:1794`): display only, set by code with a forced
/// `Cvar_Set` but never from the console.
pub const CVAR_ROM: u32 = 0x0000_0040;
pub const Q_COLOR_ESCAPE: u8 = b'^';
/// `BIG_INFO_STRING` (`q_shared.h:360`) — bounds the tokenizer copy.
pub const BIG_INFO_STRING: usize = 8192;

// server.h / q_shared.h / qcommon.h constants the hook layer reads.
pub const MAX_GENTITIES: usize = 1024;
pub const MAX_STRING_TOKENS: usize = 1024;
pub const PACKET_BACKUP: usize = 32;
pub const PACKET_MASK: usize = PACKET_BACKUP - 1;
pub const MAX_RELIABLE_COMMANDS: usize = 128;
pub const MAX_QPATH: usize = 64;
pub const MAX_NAME_LENGTH: usize = 32;

// clientState_t (server.h:96-103).
pub const CS_FREE: i32 = 0;
pub const CS_ZOMBIE: i32 = 1;
pub const CS_CONNECTED: i32 = 2;
pub const CS_PRIMED: i32 = 3;
pub const CS_ACTIVE: i32 = 4;

/// `clc_move` (binary: `SV_ExecuteClientMessage` dispatches `c == 2` to
/// `SV_UserMove` with `delta = 1`).
pub const CLC_MOVE: i32 = 2;
/// `clc_moveNoDelta` (dispatched with `delta = 0`).
pub const CLC_MOVE_NO_DELTA: i32 = 3;

// team_t (bg_public.h:997-1003).
pub const TEAM_FREE: i32 = 0;
pub const TEAM_RED: i32 = 1;
pub const TEAM_BLUE: i32 = 2;
pub const TEAM_SPECTATOR: i32 = 3;
pub const TEAM_NUM_TEAMS: i32 = 4;

// gametype_t (bg_public.h:183-198): the team modes the proxy features apply to.
pub const GT_TEAM: i32 = 6;
pub const GT_CTF: i32 = 8;

// clientConnected_t (g_local.h:366-371): `pers.connected` values.
pub const CON_DISCONNECTED: i32 = 0;
/// Fully connected (pristine `numPlayingClients` counts only this state).
pub const CON_CONNECTED: i32 = 2;

// PERS_* (server.h:16-18), STAT_* (q_shared.h) used by the stats printer.
pub const PERS_SCORE: usize = 0;
pub const PERS_TEAM: usize = 3;
pub const PERS_KILLED: usize = 8;
pub const STAT_HEALTH: usize = 0;
pub const STAT_ARMOR: usize = 5;

// angles indices (SDK `q_shared.h:346-348`)
pub const ROLL: usize = 2;

// force powers (SDK `q_shared.h:590-612`)
pub const FP_LEVITATION: u8 = 1;
pub const NUM_FORCE_POWERS: u8 = 18;

/// Version string the engine must report in its `version` cvar
/// (`Proxy_Header.hpp:31`, matches `linuxjampded`'s `JAmp: v1.0.1.1 …`).
pub const ORIGINAL_ENGINE_VERSION: &str = "JAmp: v1.0.1.1 linux-i386 Nov 10 2003";

/// Name the pristine game module is installed under next to the proxy.
pub const ORIGINAL_LIBRARY_NAME: &str = "jampgame_original.so";
/// Subdirectory the engine loads game modules from when `fs_game` is unset.
pub const DEFAULT_BASE_GAME_FOLDER_NAME: &str = "base";

// ---------------------------------------------------------------------------
// System trap ordinals (SDK `gameImport_t`, `g_public.h:102-276`)
//
// Consecutive from `G_PRINT = 0` up to the `G_MEMSET = 100` re-anchor; the
// values below were produced by compiling the SDK header (oracle), not by hand.
// Only the traps the proxy itself uses are listed.
// ---------------------------------------------------------------------------

pub const G_CVAR_REGISTER: i32 = 5;
pub const G_CVAR_UPDATE: i32 = 6;
pub const G_CVAR_SET: i32 = 7;
pub const G_CVAR_VARIABLE_INTEGER_VALUE: i32 = 8;
pub const G_CVAR_VARIABLE_STRING_BUFFER: i32 = 9;
pub const G_ARGV: i32 = 11;
pub const G_LOCATE_GAME_DATA: i32 = 17;
pub const G_DROP_CLIENT: i32 = 18;
pub const G_SEND_SERVER_COMMAND: i32 = 19;
pub const G_SET_CONFIGSTRING: i32 = 20;
pub const G_GET_CONFIGSTRING: i32 = 21;
pub const G_GET_USERINFO: i32 = 22;
pub const G_SET_USERINFO: i32 = 23;
pub const G_GET_USERCMD: i32 = 40;
pub const G_NAV_FREE: i32 = 201;
/// `G_NAV_LOAD` (`g_public.h:298-300`, right after `G_NAV_FREE`): the game's
/// single nav load (`g_main.c:1034`).
pub const G_NAV_LOAD: i32 = 202;

/// Trace-family traps (`g_public.h:182-185,235`; compiled-oracle verified —
/// `G_TRACE` 27, `G_G2TRACE` 28, `G_TRACECAPSULE` 49, all forwarding to
/// `SV_Trace(results, start, mins, maxs, end, passEntityNum=args[6],
/// contentmask=args[7], …)` per `sv_game.cpp:599-606`). All share the same
/// word layout, so the parallel-TFFA physics isolation intercepts them
/// together.
pub const G_TRACE: i32 = 27;
pub const G_G2TRACE: i32 = 28;
pub const G_TRACECAPSULE: i32 = 49;
/// `G_LINKENTITY` (`g_public.h:199`; enum-counted 34) — links a gentity into
/// the engine's world/area lists. The clone pass links each copy through the
/// engine rather than hand-rolling movement code.
pub const G_LINKENTITY: i32 = 34;
/// `G_UNLINKENTITY` (`g_public.h:204`; enum-counted 35).
pub const G_UNLINKENTITY: i32 = 35;
/// `G_ENTITIES_IN_BOX` (`g_public.h:207`; enum-counted 36, right after
/// `G_UNLINKENTITY` 35 and before `G_GET_USERCMD` 40): the engine fills the
/// game's `int *list` with the linked entities whose bounding box overlaps
/// `mins..maxs` and returns the count. Filtering keeps only the acting scope's
/// entities (S9 world-query gate).
pub const G_ENTITIES_IN_BOX: i32 = 36;
/// `G_ENTITY_CONTACT` (`g_public.h:211`; enum-counted 37): exact brush-model
/// contact test against one gentity.
pub const G_ENTITY_CONTACT: i32 = 37;
/// `G_ENTITY_CONTACTCAPSULE` (`g_public.h:236`; enum-counted 50, right after
/// `G_TRACECAPSULE` 49): the capsule variant of `G_ENTITY_CONTACT`.
pub const G_ENTITY_CONTACTCAPSULE: i32 = 50;

/// `trap_Milliseconds` ordinal (`g_public.h:110`).
pub const G_MILLISECONDS: i32 = 2;

/// `svc_download` byte written to download-error messages (`qcommon.h:141`).
pub const SVC_DOWNLOAD: i32 = 6;

/// `errorParm_t::ERR_DROP` (`q_shared.h:425`).
pub const ERR_DROP: i32 = 1;

// ---------------------------------------------------------------------------
// vmMain commands (SDK `gameExport_t`, `g_public.h:735+`)
// ---------------------------------------------------------------------------

pub const GAME_INIT: i32 = 0;
pub const GAME_SHUTDOWN: i32 = 1;
pub const GAME_CLIENT_CONNECT: i32 = 2;
pub const GAME_CLIENT_BEGIN: i32 = 3;
pub const GAME_CLIENT_USERINFO_CHANGED: i32 = 4;
pub const GAME_CLIENT_DISCONNECT: i32 = 5;
pub const GAME_CLIENT_COMMAND: i32 = 6;
pub const GAME_RUN_FRAME: i32 = 8;

// ---------------------------------------------------------------------------
// Layout oracle: sizes/offsets produced by compiling the SDK headers with the
// i386 toolchain (g++ 12, Debian bookworm i386). The `cfg(test)` constants are
// the regression-gate pins consumed by the unit tests at the bottom of this
// file; the ones used by production code sit next to the fields they describe.
// Do not "optimise" the structures below away from these.
// ---------------------------------------------------------------------------

/// `sizeof(gentity_t) == 1516`, `offsetof(gentity_t, client) == 864`
/// (SDK `g_local.h:133`).
pub const SIZEOF_GENTITY: usize = 1516;
pub const OFFSET_GENTITY_CLIENT: usize = 864;

/// `sizeof(gclient_t) == 7232`, `offsetof(gclient_t, pers) == 1552`,
/// `offsetof(clientPersistant_t, netname) == 48` (SDK `g_local.h:440,535`).
#[allow(dead_code)] // sizeof pin; offsets below are what production code reads
pub const SIZEOF_GCLIENT: usize = 7232;
pub const OFFSET_GCLIENT_PERS: usize = 1552;
pub const OFFSET_PERS_NETNAME: usize = 48;

/// `offsetof(gentity_t, s)` — entityState_t.
pub const OFFSET_GENTITY_S: usize = 0;
/// `offsetof(gentity_t, playerState)` — `playerState_t*`.
pub const OFFSET_GENTITY_PLAYER_STATE: usize = 532;
/// `offsetof(gentity_t, damageRedirect)`.
pub const OFFSET_GENTITY_DAMAGE_REDIRECT: usize = 1488;

/// `entityState_t` field offsets inside `gentity_t.s` (`s` at 0, SDK
/// `q_shared.h:2670-2832` PC layout; `clientNum` at 232 matches the existing
/// `OFFSET_ENTITY_CLIENT_NUM` pin, which anchors the whole table).
pub const OFFSET_GENTITY_S_ETYPE: usize = 4;
/// `offsetof(entityState_t, eFlags)` (`q_shared.h` field order: `number` @0,
/// `eType` @4, `eFlags` @8 — the `OFFSET_GENTITY_S_ETYPE` pin anchors it).
pub const OFFSET_GENTITY_S_EFLAGS: usize = 8;
pub const OFFSET_GENTITY_S_OTHER_ENTITY_NUM: usize = 188;
pub const OFFSET_GENTITY_S_OTHER_ENTITY_NUM2: usize = 192;
/// `entityState_t.owner` (crosshair owner).
pub const OFFSET_GENTITY_S_OWNER: usize = 280;
/// `entityShared_t.r.ownerNum`: `r` starts at 560
/// (`sizeof(entityState_t)` 532 + playerState* 4 + Vehicle* 4 + ghoul2 4 +
/// localAnimIndex 4 + modelScale 12), `ownerNum` 100 bytes into `r`
/// (linked 0, linkcount 4, svFlags 8, singleClient 12, bmodel 16, mins 20,
/// maxs 32, contents 44, absmin 48, absmax 60, currentOrigin 72,
/// currentAngles 84, mIsRoffing 96, ownerNum 100).
pub const OFFSET_GENTITY_R_OWNER: usize = 660;
/// `entityShared_t.contents` (`r` at 560 + `contents` 44). The parallel-TFFA
/// trace isolation zeroes this field for cross-match entities for the
/// duration of a single `SV_Trace`.
pub const OFFSET_GENTITY_R_CONTENTS: usize = 604;
/// `entityShared_t.linked` (`r` at 560, first field).
pub const OFFSET_GENTITY_R_LINKED: usize = 560;

/// `offsetof(entityState_t, number)` — the entity's own slot index.
pub const OFFSET_GENTITY_S_NUMBER: usize = 0;
/// `entityState_t.pos` (`trajectory_t`) — mover world position.
pub const OFFSET_GENTITY_S_POS: usize = 16;
/// `entityState_t.apos` — mover world angles.
pub const OFFSET_GENTITY_S_APOS: usize = 52;
/// `entityState_t.time` (trajectory timing).
pub const OFFSET_GENTITY_S_TIME: usize = 88;
/// `entityState_t.time2`.
pub const OFFSET_GENTITY_S_TIME2: usize = 92;
/// `entityState_t.modelindex` — `*N` bmodel for movers.
pub const OFFSET_GENTITY_S_MODELINDEX: usize = 224;
/// `entityState_t.modelindex2`.
pub const OFFSET_GENTITY_S_MODELINDEX2: usize = 228;
/// `entityState_t.solid`.
pub const OFFSET_GENTITY_S_SOLID: usize = 268;

// `gentity_t` fields the per-instance world-entity clone pass reads/copies
// (i386 layout oracle, 2026-10-18: `sizeof == 1516`, `r == 560`, `client == 864`
// anchor the table). Only enough to decide the "interactive" predicate, copy a
// source entity, and re-point team/train links.
pub const OFFSET_GENTITY_TARGETNAME: usize = 796;
/// `gentity_t.classname` — `char*`.
pub const OFFSET_GENTITY_CLASSNAME: usize = 800;
/// `gentity_t.spawnflags`.
pub const OFFSET_GENTITY_SPAWNFLAGS: usize = 888;
/// `gentity_t.flags` (FL_*).
pub const OFFSET_GENTITY_FLAGS: usize = 960;
/// `gentity_t.neverFree` (`qboolean`).
pub const OFFSET_GENTITY_NEVER_FREE: usize = 956;
/// `gentity_t.freetime` — `level.time` when the slot was freed.
pub const OFFSET_GENTITY_FREETIME: usize = 972;
/// `gentity_t.clipmask`.
pub const OFFSET_GENTITY_CLIPMASK: usize = 996;
/// `gentity_t.inuse` (`qboolean`) — slot occupancy.
pub const OFFSET_GENTITY_INUSE: usize = 880;
/// `gentity_t.moverState` (`moverState_t`).
pub const OFFSET_GENTITY_MOVER_STATE: usize = 1012;
/// `gentity_t.parent` — mover owner (buttons/plats).
pub const OFFSET_GENTITY_PARENT: usize = 1036;
/// `gentity_t.nextTrain` (`func_train` chain).
pub const OFFSET_GENTITY_NEXT_TRAIN: usize = 1040;
/// `gentity_t.prevTrain`.
pub const OFFSET_GENTITY_PREV_TRAIN: usize = 1044;
/// `gentity_t.pos1` — mover rest position.
pub const OFFSET_GENTITY_POS1: usize = 1048;
/// `gentity_t.pos2` — mover activated position.
pub const OFFSET_GENTITY_POS2: usize = 1060;
/// `gentity_t.target` — `targetname` of entities this one triggers.
pub const OFFSET_GENTITY_TARGET: usize = 1096;
/// `gentity_t.target2`.
pub const OFFSET_GENTITY_TARGET2: usize = 1100;
/// `gentity_t.team` — shared team name for teamchain movers/doors.
pub const OFFSET_GENTITY_TEAM: usize = 1120;
/// `gentity_t.target_ent` — the resolved entity a turret/missile shoots or
/// mirrors (`g_turret.c:787`). A clone-set-internal pointer like the rest.
pub const OFFSET_GENTITY_TARGET_ENT: usize = 1132;
/// `gentity_t.nextthink`.
pub const OFFSET_GENTITY_NEXTTHINK: usize = 1188;
/// `gentity_t.think` — `void (*)(gentity_t*)`.
pub const OFFSET_GENTITY_THINK: usize = 1192;
/// `gentity_t.reached`.
pub const OFFSET_GENTITY_REACHED: usize = 1196;
/// `gentity_t.blocked`.
pub const OFFSET_GENTITY_BLOCKED: usize = 1200;
/// `gentity_t.touch` — `void (*)(gentity_t*, gentity_t*, trace_t*)`.
pub const OFFSET_GENTITY_TOUCH: usize = 1204;
/// `gentity_t.use` — `void (*)(gentity_t*, gentity_t*, gentity_t*)`.
pub const OFFSET_GENTITY_USE: usize = 1208;
/// `gentity_t.die`.
pub const OFFSET_GENTITY_DIE: usize = 1216;
/// `gentity_t.takedamage` (`qboolean`).
pub const OFFSET_GENTITY_TAKEDAMAGE: usize = 1236;
/// `gentity_t.count`.
pub const OFFSET_GENTITY_COUNT: usize = 1360;
/// `gentity_t.chain`.
pub const OFFSET_GENTITY_CHAIN: usize = 1372;
/// `gentity_t.teamchain` — next entity in a mover team.
pub const OFFSET_GENTITY_TEAMCHAIN: usize = 1388;
/// `gentity_t.teammaster` — team captain of a mover team.
pub const OFFSET_GENTITY_TEAMMASTER: usize = 1392;
/// `gentity_t.wait` — mover wait time.
pub const OFFSET_GENTITY_WAIT: usize = 1408;
/// `gentity_t.delay`.
pub const OFFSET_GENTITY_DELAY: usize = 1416;
/// `gentity_t.genericValue1`.
pub const OFFSET_GENTITY_GENERIC_VALUE1: usize = 1420;
/// `gentity_t.m_pVehicle` (`void*`, a Ghoul2/NPC-only field) — one of the
/// "cannot be duplicated" markers the eligibility scan looks for.
pub const OFFSET_GENTITY_M_P_VEHICLE: usize = 536;
/// `gentity_t.ghoul2` (`void*`) — the server-side Ghoul2 model instance of an
/// NPC/actor. Shared with a naive copy, so a cloned NPC would animate as one
/// entity and free the other's model (`G_FreeEntity`,
/// `g_utils.c:960-966`).
pub const OFFSET_GENTITY_GHOUL2: usize = 540;
/// `gentity_t.enemy` — runtime "who is using/blocking me" (a player usually),
/// or the training chain link. Remapped inside a clone set only.
pub const OFFSET_GENTITY_ENEMY: usize = 1376;
/// `gentity_t.lastEnemy`.
pub const OFFSET_GENTITY_LAST_ENEMY: usize = 1380;
/// `gentity_t.activator`.
pub const OFFSET_GENTITY_ACTIVATOR: usize = 1384;

/// `clientSnapshot_t` viewer: `areabytes(0) + areabits[32](4) + ps(36)`,
/// `playerState_t.clientNum` at +172 → frame+208 (SDK `server.h:96-114`,
/// `MAX_MAP_AREA_BYTES` 32 from `q_shared.h:416`).
pub const OFFSET_SNAPSHOT_FRAME_PS_CLIENT_NUM: usize = 208;
/// `snapshotEntityNumbers_t` (`sv_snapshot.cpp:264-268`):
/// `numSnapshotEntities` first, then `snapshotEntities[1024]`.
pub const OFFSET_SNAPSHOT_ENUMS_COUNT: usize = 0;
pub const OFFSET_SNAPSHOT_ENUMS_LIST: usize = 4;
pub const MAX_SNAPSHOT_ENTITIES: usize = 1024;

/// Entity numbers (`q_shared.h:1991-2016`, `GENTITYNUM_BITS` 10 on PC).
pub const ENTITYNUM_NONE: i32 = 1023;
pub const ENTITYNUM_WORLD: i32 = 1022;
/// `ENTITYNUM_MAX_NORMAL` (`q_shared.h:2016`) — the last ordinary slot is
/// `ENTITYNUM_MAX_NORMAL - 1`; `G_Spawn` `G_Error`s at this index
/// (`g_utils.c:835-845`), so usable entity slots are `[MAX_CLIENTS,
/// ENTITYNUM_MAX_NORMAL)`.
pub const ENTITYNUM_MAX_NORMAL: i32 = 1022;

/// `entityState_t.eType` base for temp events (`bg_public.h`: `ET_EVENTS`,
/// 18 on PC: GENERAL 0 .. FX 17). Temp entities have
/// `eType = ET_EVENTS + event`.
/// `entityType_t::ET_ITEM` (`bg_public.h`): an item entity. `LaunchItem`
/// (`g_items.c:2653`) marks a *dropped* item with `s.modelindex2 = 1`, which is
/// how the proxy tells a player's dropped item from a map item (S9).
pub const ET_ITEM: i32 = 2;
pub const ET_EVENTS: i32 = 18;

/// Map mover (`bg_public.h:1189-1208`: GENERAL 0, PLAYER 1, ITEM 2, MISSILE 3,
/// SPECIAL 4, HOLOCRON 5, **MOVER 6**): doors, plats, elevators, `func_rotating`.
pub const ET_MOVER: i32 = 6;

/// `ET_GENERAL` (`bg_public.h:1189`) — the default `eType` of entities that are
/// not players/items/movers/temp events (spawn points, `misc_model_static`,
/// lights, most `func_*` helpers).
pub const ET_GENERAL: i32 = 0;

/// `ET_NPC` (`bg_public.h:1202`) — a Ghoul2 actor. NPCs carry a debt the clone
/// pass cannot pay (a shared `ghoul2` model instance and a dynamically
/// allocated `client`), so a map holding an *interactive* NPC is not eligible
/// for per-instance worlds at all (all-or-nothing, D-006.6).
pub const ET_NPC: i32 = 13;

/// `CONTENTS_TRIGGER` (`surfaceflags.h:20`) carried in `r.contents` by every
/// `SP_trigger_*`/item (`g_trigger.c:13`, `g_items.c:1313`) — the mask
/// `G_TouchTriggers` passes to `trap_EntitiesInBox` (`g_active.c:516-548`, the
/// `0x400` immediate at `0x119c95` in the shipped module).
pub const CONTENTS_TRIGGER: i32 = 0x0000_0400;

/// `CONTENTS_SOLID` (`surfaceflags.h:10`) — the bit every brush mover (door,
/// plat, elevator) carries in `r.contents`, and the base of the movement masks
/// (`MASK_PLAYERSOLID`, `bg_public.h:1170`). Used by the trace gate: an entity
/// whose contents do not intersect the trace's `contentmask` is already skipped
/// by the engine (`sv_world.cpp:637`), so it needs no flip.
pub const CONTENTS_SOLID: i32 = 0x0000_0001;

/// Event numbers (`bg_public.h` `entity_event_t`, oracle-counted 2026-09-15;
/// the sound set re-counted 2026-10-18 and cross-checked against these pins).
pub const EV_SABER_HIT: i32 = 30;
pub const EV_SABER_BLOCK: i32 = 31;
pub const EV_SABER_CLASHFLARE: i32 = 32;
/// `G_PreDefSound`'s event (`w_force.c:40-49`: force jump, protect and absorb
/// sounds). Built by `G_TempEntity`, so it carries no owner of its own; the
/// proxy stamps it with the acting scope's instance (see `tffa`).
pub const EV_PREDEFSOUND: i32 = 40;
pub const EV_MUTE_SOUND: i32 = 74;
pub const EV_GENERAL_SOUND: i32 = 76;
pub const EV_GLOBAL_SOUND: i32 = 77;
pub const EV_GLOBAL_TEAM_SOUND: i32 = 78;
pub const EV_ENTITY_SOUND: i32 = 79;
pub const EV_OBITUARY: i32 = 93;

/// `EF_PLAYER_EVENT` (`bg_public.h:570`, `1<<5`): the temp entity carries a
/// predicted player event (`SendPendingPredictableEvents`, `g_active.c:1058`),
/// whose generating player is `s.otherEntityNum` (`CG_CheckEvents`
/// `cg_event.c:3533` rewrites `number` to that player on the client).
pub const EF_PLAYER_EVENT: i32 = 1 << 5;

/// Configstring indices (`bg_public.h`): `CS_SCORES1/2` drive the mini HUD
/// (`cgs.scores1/2`), `CS_PLAYERS + n` carries client `n`'s `n\t\…` info.
pub const CS_SCORES1: i32 = 6;
pub const CS_SCORES2: i32 = 7;
/// `CS_PLAYERS = CS_ICONS + MAX_ICONS = 1131` (`bg_public.h` chain:
/// `CS_AMBIENT_SET 37 + MAX_AMBIENT_SETS 256 → … → CS_SOUNDS 811 +
/// MAX_SOUNDS 256 → CS_ICONS 1067 + MAX_ICONS 64`).
pub const CS_PLAYERS: i32 = 1131;

#[cfg(test)]
mod layout_pins {
    /// `sizeof(usercmd_t) == 28` (SDK `q_shared.h:2524`).
    pub const SIZEOF_USERCMD: usize = 28;
    /// `offsetof(usercmd_t, angles)`.
    pub const OFFSET_USERCMD_ANGLES: usize = 4;
    /// `offsetof(usercmd_t, forcesel)`.
    pub const OFFSET_USERCMD_FORCESEL: usize = 21;

    /// `sizeof(vmCvar_t) == 272` (SDK `q_shared.h:1412-1418`).
    pub const SIZEOF_VMCVAR: usize = 272;
    pub const OFFSET_VMCVAR_HANDLE: usize = 0;
    pub const OFFSET_VMCVAR_MODIFICATION_COUNT: usize = 4;
    pub const OFFSET_VMCVAR_VALUE: usize = 8;
    pub const OFFSET_VMCVAR_INTEGER: usize = 12;
    pub const OFFSET_VMCVAR_STRING: usize = 16;
}

// ---------------------------------------------------------------------------
// Engine struct offsets (SDK `server.h`, `qcommon.h`, `g_public.h`), produced
// by the i386 layout oracle (`g++ -m32` over the SDK headers — see
// `tools/layout/`). The proxy's shared structs are layout-tested against the
// pristine SDK (U-003), and the shipped binary's `g_entities`/`level`/`g_clients`
// sizes match these layouts (R-004/R-013); the offsets below are the pins the
// hooks read through raw pointers.
// ---------------------------------------------------------------------------

/// `offsetof(client_t, state)` — the clientState_t.
pub const OFFSET_CLIENT_STATE: usize = 0;
/// `offsetof(client_t, userinfo)` — `char[MAX_INFO_STRING]`.
pub const OFFSET_CLIENT_USERINFO: usize = 4;
/// `offsetof(client_t, reliableCommands)` — `char[MAX_RELIABLE_COMMANDS][MAX_STRING_CHARS]`.
pub const OFFSET_CLIENT_RELIABLE_COMMANDS: usize = 1032;
/// `offsetof(client_t, reliableSequence)`.
pub const OFFSET_CLIENT_RELIABLE_SEQUENCE: usize = 132104;
/// `offsetof(client_t, reliableAcknowledge)`.
pub const OFFSET_CLIENT_RELIABLE_ACKNOWLEDGE: usize = 132108;
/// `offsetof(client_t, messageAcknowledge)`.
pub const OFFSET_CLIENT_MESSAGE_ACKNOWLEDGE: usize = 132116;
/// `offsetof(client_t, gamestateMessageNum)` (binary: `SV_ExecuteClientMessage`
/// compares the ack against `0x20418` for the dropped-gamestate resend).
pub const OFFSET_CLIENT_GAMESTATE_MESSAGE_NUM: usize = 132120;
/// `offsetof(client_t, lastClientCommandString)` — `char[MAX_STRING_CHARS]`
/// (binary: `SV_ClientCommand` keeps it at `cl+0x20444`, right before `gentity`;
/// also the original proxy's `strstr(..., "nextdl")` haystack).
pub const OFFSET_CLIENT_LAST_CLIENT_COMMAND: usize = 132164;
/// `offsetof(client_t, deltaMessage)` (binary: `SV_UserMove` writes
/// `messageAcknowledge` to `0x20908` for delta packets, `-1` for noDelta).
pub const OFFSET_CLIENT_DELTA_MESSAGE: usize = 133384;
/// `offsetof(client_t, lastPacketTime)` (binary: `SV_ReadPackets` stores
/// `svs->time` at `0x20910` for every accepted packet).
pub const OFFSET_CLIENT_LAST_PACKET_TIME: usize = 133392;
/// `offsetof(client_t, gentity)` — `sharedEntity_t*`.
pub const OFFSET_CLIENT_GENTITY: usize = 133188;
/// `offsetof(client_t, name)` — `char[MAX_NAME_LENGTH]`.
pub const OFFSET_CLIENT_NAME: usize = 133192;
/// `offsetof(client_t, downloadName)` — `char[MAX_QPATH]`.
pub const OFFSET_CLIENT_DOWNLOAD_NAME: usize = 133224;
/// `offsetof(client_t, download)` — `fileHandle_t`.
pub const OFFSET_CLIENT_DOWNLOAD: usize = 133288;
/// `offsetof(client_t, frames)` — `clientSnapshot_t[PACKET_BACKUP]`.
pub const OFFSET_CLIENT_FRAMES: usize = 133412;
/// `offsetof(client_t, ping)`.
pub const OFFSET_CLIENT_PING: usize = 234532;
/// `offsetof(client_t, rate)`.
pub const OFFSET_CLIENT_RATE: usize = 234536;
/// `offsetof(client_t, snapshotMsec)`.
pub const OFFSET_CLIENT_SNAPSHOT_MSEC: usize = 234540;
/// `offsetof(client_t, netchan)` — `netchan_t`.
pub const OFFSET_CLIENT_NETCHAN: usize = 234548;

/// `offsetof(clientSnapshot_t, messageSent)`.
pub const OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT: usize = 3148;
/// `offsetof(clientSnapshot_t, messageAcked)`.
pub const OFFSET_CLIENT_SNAPSHOT_MESSAGE_ACKED: usize = 3152;
/// `sizeof(clientSnapshot_t)` — stride of `client_t.frames[PACKET_BACKUP]`.
///
/// Binary-verified (R-0XX; gdb batch `disassemble /r` of the shipped
/// `SV_CalcPings` at `0x8057204`, the R-022 tooling note): the frames walk
/// steps `0xc58` bytes per slot and loads `messageSent` at `+0xc4c` and
/// `messageAcked` at `+0xc50` of each slot, over exactly `PACKET_BACKUP`
/// (32) slots — `(OFFSET_CLIENT_PING - OFFSET_CLIENT_FRAMES) / 32`.
pub const SIZEOF_CLIENT_SNAPSHOT_T: usize = 3160;

/// `offsetof(server_t, svEntities)` — `svEntity_t[MAX_GENTITIES]`.
pub const OFFSET_SV_SV_ENTITIES: usize = 8880;
/// `offsetof(server_t, gentities)` — `sharedEntity_t*`.
pub const OFFSET_SV_GENTITIES: usize = 647860;
/// `offsetof(server_t, gentitySize)`.
pub const OFFSET_SV_GENTITY_SIZE: usize = 647864;

/// `offsetof(serverStatic_t, time)`.
pub const OFFSET_SVS_TIME: usize = 4;
/// `offsetof(serverStatic_t, clients)` — `client_t[MAX_CLIENTS]` array start.
pub const OFFSET_SVS_CLIENTS: usize = 12;

/// `offsetof(sharedEntity_t, r.svFlags)` (binary: `SV_CalcPings` at
/// `0x8057273` loads `0x238(%gentity)` and tests bit 3 to identify bots).
pub const OFFSET_SHARED_SVFLAGS: usize = 0x238;
/// `SVF_BOT` (binary: `SV_CalcPings` tests `0x8` against `r.svFlags` and
/// writes `ping = 0` for bots).
pub const SVF_BOT: u8 = 8;

/// `offsetof(sharedEntity_t, s)` — the embedded `entityState_t`.
pub const OFFSET_SHARED_ENTITY_S: usize = 0;
/// `offsetof(entityState_t, number)`.
pub const OFFSET_ENTITY_NUMBER: usize = 0;
/// `offsetof(entityState_t, clientNum)`.
pub const OFFSET_ENTITY_CLIENT_NUM: usize = 232;

/// `offsetof(playerState_t, stats)` — `int[MAX_STATS]` (STAT_HEALTH/STAT_ARMOR).
pub const OFFSET_PS_STATS: usize = 244;
/// `offsetof(playerState_t, persistant)` — `int[MAX_PERSISTANT]` (PERS_SCORE/PERS_KILLED).
pub const OFFSET_PS_PERSISTANT: usize = 308;
/// `offsetof(playerState_t, clientNum)`.
pub const OFFSET_PS_CLIENT_NUM: usize = 172;

/// `offsetof(gclient_t, ps)` — playerState_t.
pub const OFFSET_GCLIENT_PS: usize = 0;
/// `offsetof(gclient_t, sess)` — clientSession_t.
pub const OFFSET_GCLIENT_SESS: usize = 1708;

/// `offsetof(clientPersistant_t, cmd)` — usercmd_t.
pub const OFFSET_PERS_CMD: usize = 4;
/// `offsetof(clientPersistant_t, connected)` — `clientConnected_t` (first
/// field, `g_local.h:441`); `TeamCount` skips `CON_DISCONNECTED` clients.
pub const OFFSET_PERS_CONNECTED: usize = 0;

/// `offsetof(clientSession_t, sessionTeam)` — team_t.
pub const OFFSET_SESS_SESSION_TEAM: usize = 0;
/// `offsetof(clientSession_t, spectatorState)` (`g_local.h`: `sessionTeam(0)`,
/// `spectatorTime(4)`, `spectatorState(8)`).
pub const OFFSET_SESS_SPECTATOR_STATE: usize = 8;
/// `offsetof(clientSession_t, spectatorClient)` (`spectatorState(8)` + 4).
pub const OFFSET_SESS_SPECTATOR_CLIENT: usize = 12;

/// `spectatorState_t` (`g_local.h:373-378`).
pub const SPECTATOR_NOT: i32 = 0;
pub const SPECTATOR_FREE: i32 = 1;
pub const SPECTATOR_FOLLOW: i32 = 2;
pub const SPECTATOR_SCOREBOARD: i32 = 3;

/// `offsetof(level_locals_t, time)`.
pub const OFFSET_LEVEL_TIME: usize = 32;
/// `offsetof(level_locals_t, framenum)`.
pub const OFFSET_LEVEL_FRAMENUM: usize = 28;
/// `offsetof(level_locals_t, warmupTime)` (field order in SDK
/// `g_local.h:812-832`; the shipped binary's `AddScore`/`CheckExitRules` read
/// it at this offset).
pub const OFFSET_LEVEL_WARMUP_TIME: usize = 16;
/// `offsetof(level_locals_t, startTime)` — the time the map started, used by
/// the pristine timelimit check (`g_main.c:2667`).
pub const OFFSET_LEVEL_START_TIME: usize = 40;
/// `offsetof(level_locals_t, teamScores)` (base of `int[TEAM_NUM_TEAMS]`,
/// derived from the field order in SDK `g_local.h:812-832` and pinned by the
/// existing `OFFSET_LEVEL_FRAMENUM`/`OFFSET_LEVEL_TIME` anchors:
/// clients(0) gentities(4) gentitySize(8) num_entities(12) warmupTime(16)
/// logFile(20) maxclients(24) framenum(28) time(32) previousTime(36)
/// startTime(40) teamScores(44)).
pub const OFFSET_LEVEL_TEAM_SCORES: usize = 44;
/// `offsetof(level_locals_t, teamScores[TEAM_RED])`.
pub const OFFSET_LEVEL_TEAM_SCORES_RED: usize = 48;
/// `offsetof(level_locals_t, teamScores[TEAM_BLUE])`.
pub const OFFSET_LEVEL_TEAM_SCORES_BLUE: usize = 52;

/// `offsetof(msg_t, data)` — the message bytes (`SV_ExecuteClientMessage` peek).
pub const OFFSET_MSG_DATA: usize = 12;
/// `offsetof(msg_t, cursize)`.
pub const OFFSET_MSG_CURSIZE: usize = 20;
/// `offsetof(msg_t, readcount)`.
pub const OFFSET_MSG_READCOUNT: usize = 24;
/// `offsetof(msg_t, bit)`.
pub const OFFSET_MSG_BIT: usize = 28;

/// `offsetof(netchan_t, remoteAddress)` — netadr_t (binary: `SV_ReadPackets`
/// passes `cl+0x3943c` to `NET_CompareBaseAdr`, so netchan starts at 234544).
pub const OFFSET_NETCHAN_REMOTE_ADDRESS: usize = 234556;
/// `offsetof(netchan_t, qport)`.
pub const OFFSET_NETCHAN_QPORT: usize = 28;
/// `offsetof(netchan_t, unsentFragments)`.
pub const OFFSET_NETCHAN_UNSENT_FRAGMENTS: usize = 49200;

/// `offsetof(netadr_t, port)`.
pub const OFFSET_NETADR_PORT: usize = 18;

/// `sizeof(client_t)` — stride of the `svs->clients` array (oracle).
pub const SIZEOF_CLIENT_T: usize = 332920;
/// `sizeof(svEntity_t)` — stride of `sv->svEntities` (oracle).
pub const SIZEOF_SV_ENTITY_T: usize = 624;

// ---------------------------------------------------------------------------
// #[repr(C)] mirror of the SDK types the proxy passes to the engine
// ---------------------------------------------------------------------------

/// `usercmd_t` — the command the engine stores for a client
/// (SDK `q_shared.h:2524-2533`).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Usercmd {
    pub server_time: i32,
    pub angles: [i32; 3],
    pub buttons: i32,
    pub weapon: u8,
    pub forcesel: u8,
    pub invensel: u8,
    pub generic_cmd: u8,
    pub forwardmove: i8,
    pub rightmove: i8,
    pub upmove: i8,
}

/// `vmCvar_t` — engine↔game cvar mirror (SDK `q_shared.h:1412-1418`).
/// The engine writes every field through `G_CVAR_REGISTER`/`G_CVAR_UPDATE`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VmCvar {
    pub handle: i32,
    pub modification_count: i32,
    pub value: f32,
    pub integer: i32,
    pub string: [c_char; MAX_CVAR_VALUE_STRING],
}

impl Default for VmCvar {
    fn default() -> Self {
        VmCvar {
            handle: 0,
            modification_count: 0,
            value: 0.0,
            integer: 0,
            string: [0; MAX_CVAR_VALUE_STRING],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::layout_pins::*;
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn usercmd_layout_matches_sdk() {
        assert_eq!(size_of::<Usercmd>(), SIZEOF_USERCMD);
        assert_eq!(align_of::<Usercmd>(), 4);
        assert_eq!(offset_of!(Usercmd, server_time), 0);
        assert_eq!(offset_of!(Usercmd, angles), OFFSET_USERCMD_ANGLES);
        assert_eq!(offset_of!(Usercmd, forcesel), OFFSET_USERCMD_FORCESEL);
    }

    #[test]
    fn vm_cvar_layout_matches_sdk() {
        assert_eq!(size_of::<VmCvar>(), SIZEOF_VMCVAR);
        assert_eq!(offset_of!(VmCvar, handle), OFFSET_VMCVAR_HANDLE);
        assert_eq!(
            offset_of!(VmCvar, modification_count),
            OFFSET_VMCVAR_MODIFICATION_COUNT
        );
        assert_eq!(offset_of!(VmCvar, value), OFFSET_VMCVAR_VALUE);
        assert_eq!(offset_of!(VmCvar, integer), OFFSET_VMCVAR_INTEGER);
        assert_eq!(offset_of!(VmCvar, string), OFFSET_VMCVAR_STRING);
    }

    #[test]
    fn trap_ordinals_match_sdk() {
        // Values verified by compiling g_public.h (oracle output committed in
        // the module docs); pin them so a header re-read cannot silently drift.
        assert_eq!(G_CVAR_REGISTER, 5);
        assert_eq!(G_CVAR_UPDATE, 6);
        assert_eq!(G_CVAR_SET, 7);
        assert_eq!(G_CVAR_VARIABLE_INTEGER_VALUE, 8);
        assert_eq!(G_CVAR_VARIABLE_STRING_BUFFER, 9);
        assert_eq!(G_ARGV, 11);
        assert_eq!(G_LOCATE_GAME_DATA, 17);
        assert_eq!(G_DROP_CLIENT, 18);
        assert_eq!(G_SEND_SERVER_COMMAND, 19);
        assert_eq!(G_SET_CONFIGSTRING, 20);
        assert_eq!(G_GET_CONFIGSTRING, 21);
        assert_eq!(G_GET_USERINFO, 22);
        assert_eq!(G_SET_USERINFO, 23);
        assert_eq!(G_GET_USERCMD, 40);
        // Trace-family ordinals (compiled-oracle verified 2026-09-15).
        assert_eq!(G_TRACE, 27);
        assert_eq!(G_G2TRACE, 28);
        assert_eq!(G_TRACECAPSULE, 49);
        // World-query ordinals (S9): `G_ENTITIES_IN_BOX`/`G_ENTITY_CONTACT` sit
        // between the pinned `G_UNLINKENTITY` 35 and `G_GET_USERCMD` 40, and
        // `G_ENTITY_CONTACTCAPSULE` follows the pinned `G_TRACECAPSULE` 49.
        assert_eq!(G_UNLINKENTITY, 35);
        assert_eq!(G_ENTITIES_IN_BOX, 36);
        assert_eq!(G_ENTITY_CONTACT, 37);
        assert_eq!(G_ENTITY_CONTACTCAPSULE, 50);
        // Nav traps (`g_public.h:298-300`): `G_NAV_INIT = 200` precedes them.
        assert_eq!(G_NAV_FREE, 201);
        assert_eq!(G_NAV_LOAD, 202);
    }

    #[test]
    fn vm_main_commands_match_sdk() {
        assert_eq!(GAME_INIT, 0);
        assert_eq!(GAME_SHUTDOWN, 1);
        assert_eq!(GAME_CLIENT_CONNECT, 2);
        assert_eq!(GAME_CLIENT_BEGIN, 3);
        assert_eq!(GAME_CLIENT_USERINFO_CHANGED, 4);
        assert_eq!(GAME_CLIENT_DISCONNECT, 5);
        assert_eq!(GAME_CLIENT_COMMAND, 6);
        assert_eq!(GAME_RUN_FRAME, 8);
    }

    #[test]
    fn engine_version_gate_string_is_ascii() {
        assert!(ORIGINAL_ENGINE_VERSION.is_ascii());
        assert_eq!(
            ORIGINAL_ENGINE_VERSION.as_bytes(),
            b"JAmp: v1.0.1.1 linux-i386 Nov 10 2003"
        );
    }

    #[test]
    fn team_mode_and_pers_connected_constants_match_sdk() {
        // gametype_t ordinals (bg_public.h) and the TeamCount skip value.
        assert_eq!(GT_TEAM, 6);
        assert_eq!(GT_CTF, 8);
        assert_eq!(CON_DISCONNECTED, 0);
        assert_eq!(CON_CONNECTED, 2);
        assert_eq!(OFFSET_PERS_CONNECTED, 0);
    }

    #[test]
    fn engine_struct_offsets_match_layout_oracle() {
        // Regression pins from the i386 layout oracle (g++ -m32, SDK headers).
        // Re-derive with tools/layout/ if the SDK surface is ever re-read.
        assert_eq!(OFFSET_CLIENT_USERINFO, 4);
        assert_eq!(OFFSET_CLIENT_RELIABLE_SEQUENCE, 132104);
        assert_eq!(OFFSET_CLIENT_RELIABLE_ACKNOWLEDGE, 132108);
        assert_eq!(OFFSET_CLIENT_MESSAGE_ACKNOWLEDGE, 132116);
        assert_eq!(OFFSET_CLIENT_GAMESTATE_MESSAGE_NUM, 132120);
        assert_eq!(OFFSET_CLIENT_LAST_CLIENT_COMMAND, 132164);
        assert_eq!(OFFSET_CLIENT_GENTITY, 133188);
        assert_eq!(OFFSET_CLIENT_DOWNLOAD_NAME, 133224);
        assert_eq!(OFFSET_CLIENT_DOWNLOAD, 133288);
        assert_eq!(OFFSET_CLIENT_FRAMES, 133412);
        assert_eq!(OFFSET_CLIENT_PING, 234532);
        assert_eq!(OFFSET_CLIENT_SNAPSHOT_MSEC, 234540);
        assert_eq!(OFFSET_CLIENT_NETCHAN, 234548);
        assert_eq!(OFFSET_SV_SV_ENTITIES, 8880);
        assert_eq!(OFFSET_SV_GENTITIES, 647860);
        assert_eq!(OFFSET_SV_GENTITY_SIZE, 647864);
        assert_eq!(OFFSET_SVS_CLIENTS, 12);
        assert_eq!(OFFSET_SHARED_SVFLAGS, 0x238);
        assert_eq!(SVF_BOT, 8);
        assert_eq!(OFFSET_GENTITY_CLIENT, 864);
        assert_eq!(OFFSET_GENTITY_DAMAGE_REDIRECT, 1488);
        assert_eq!(OFFSET_GCLIENT_PERS, 1552);
        assert_eq!(OFFSET_GCLIENT_SESS, 1708);
        assert_eq!(OFFSET_PERS_CMD, 4);
        assert_eq!(OFFSET_PERS_NETNAME, 48);
        assert_eq!(OFFSET_SESS_SESSION_TEAM, 0);
        assert_eq!(OFFSET_LEVEL_TIME, 32);
        assert_eq!(OFFSET_LEVEL_FRAMENUM, 28);
        assert_eq!(OFFSET_LEVEL_WARMUP_TIME, 16);
        assert_eq!(OFFSET_LEVEL_START_TIME, 40);
        assert_eq!(OFFSET_LEVEL_TEAM_SCORES, 44);
        assert_eq!(OFFSET_LEVEL_TEAM_SCORES_RED, 48);
        assert_eq!(OFFSET_LEVEL_TEAM_SCORES_BLUE, 52);
        assert_eq!(OFFSET_GENTITY_S_ETYPE, 4);
        assert_eq!(OFFSET_GENTITY_S_EFLAGS, 8);
        assert_eq!(OFFSET_GENTITY_S_OTHER_ENTITY_NUM, 188);
        assert_eq!(OFFSET_GENTITY_S_OTHER_ENTITY_NUM2, 192);
        assert_eq!(OFFSET_GENTITY_S_OWNER, 280);
        assert_eq!(OFFSET_GENTITY_R_OWNER, 660);
        assert_eq!(OFFSET_GENTITY_R_CONTENTS, 604);
        assert_eq!(OFFSET_GENTITY_R_LINKED, 560);
        assert_eq!(OFFSET_SNAPSHOT_FRAME_PS_CLIENT_NUM, 208);
        assert_eq!(OFFSET_SNAPSHOT_ENUMS_LIST, 4);
        assert_eq!(ENTITYNUM_NONE, 1023);
        assert_eq!(ENTITYNUM_WORLD, 1022);
        assert_eq!(ENTITYNUM_MAX_NORMAL, 1022);
        assert_eq!(ET_GENERAL, 0);
        assert_eq!(ET_ITEM, 2);
        assert_eq!(ET_MOVER, 6);
        assert_eq!(CONTENTS_TRIGGER, 0x400);
        assert_eq!(CONTENTS_SOLID, 1);
        assert_eq!(OFFSET_MSG_DATA, 12);
        assert_eq!(OFFSET_MSG_CURSIZE, 20);
        assert_eq!(OFFSET_NETCHAN_REMOTE_ADDRESS, 234556);
        assert_eq!(OFFSET_NETCHAN_QPORT, 28);
        assert_eq!(OFFSET_NETCHAN_UNSENT_FRAGMENTS, 49200);
    }

    #[test]
    fn client_snapshot_layout_matches_binary() {
        // `cl->frames[PACKET_BACKUP]` (R-0XX binary check: SV_CalcPings at
        // 0x8057204 walks 32 slots with a 0xc58 = 3160 stride and loads
        // messageSent +0xc4c / messageAcked +0xc50 per slot).
        assert_eq!(PACKET_BACKUP, 32);
        assert_eq!(PACKET_MASK, 31);
        assert_eq!(SIZEOF_CLIENT_SNAPSHOT_T, 3160);
        assert_eq!(OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT, 3148);
        assert_eq!(OFFSET_CLIENT_SNAPSHOT_MESSAGE_ACKED, 3152);
        assert_eq!(
            OFFSET_CLIENT_SNAPSHOT_MESSAGE_ACKED,
            OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT + 4
        );
        // the frames array ends exactly where cl->ping begins
        assert_eq!(
            OFFSET_CLIENT_PING - OFFSET_CLIENT_FRAMES,
            PACKET_BACKUP * SIZEOF_CLIENT_SNAPSHOT_T
        );
        // absolute addresses of frames[0].messageSent/messageAcked
        assert_eq!(
            OFFSET_CLIENT_FRAMES + OFFSET_CLIENT_SNAPSHOT_MESSAGE_SENT,
            136560
        );
        assert_eq!(
            OFFSET_CLIENT_FRAMES + OFFSET_CLIENT_SNAPSHOT_MESSAGE_ACKED,
            136564
        );
    }

    #[test]
    fn gentity_world_entity_offsets_match_layout_oracle() {
        // Per-instance world-entity clone pass (D-006 extension). i386 layout
        // oracle over the SDK headers, 2026-10-18; `sizeof(gentity_t) 1516`,
        // `r 560`, `client 864` are the same anchors as the pins above.
        assert_eq!(OFFSET_GENTITY_S_NUMBER, 0);
        assert_eq!(OFFSET_GENTITY_S_POS, 16);
        assert_eq!(OFFSET_GENTITY_S_APOS, 52);
        assert_eq!(OFFSET_GENTITY_S_TIME, 88);
        assert_eq!(OFFSET_GENTITY_S_TIME2, 92);
        assert_eq!(OFFSET_GENTITY_S_MODELINDEX, 224);
        assert_eq!(OFFSET_GENTITY_S_MODELINDEX2, 228);
        assert_eq!(OFFSET_GENTITY_S_SOLID, 268);
        assert_eq!(OFFSET_GENTITY_TARGETNAME, 796);
        assert_eq!(OFFSET_GENTITY_CLASSNAME, 800);
        assert_eq!(OFFSET_GENTITY_INUSE, 880);
        assert_eq!(OFFSET_GENTITY_SPAWNFLAGS, 888);
        assert_eq!(OFFSET_GENTITY_NEVER_FREE, 956);
        assert_eq!(OFFSET_GENTITY_FLAGS, 960);
        assert_eq!(OFFSET_GENTITY_FREETIME, 972);
        assert_eq!(OFFSET_GENTITY_CLIPMASK, 996);
        assert_eq!(OFFSET_GENTITY_MOVER_STATE, 1012);
        assert_eq!(OFFSET_GENTITY_PARENT, 1036);
        assert_eq!(OFFSET_GENTITY_NEXT_TRAIN, 1040);
        assert_eq!(OFFSET_GENTITY_PREV_TRAIN, 1044);
        assert_eq!(OFFSET_GENTITY_POS1, 1048);
        assert_eq!(OFFSET_GENTITY_POS2, 1060);
        assert_eq!(OFFSET_GENTITY_TARGET, 1096);
        assert_eq!(OFFSET_GENTITY_TARGET2, 1100);
        assert_eq!(OFFSET_GENTITY_TEAM, 1120);
        assert_eq!(OFFSET_GENTITY_NEXTTHINK, 1188);
        assert_eq!(OFFSET_GENTITY_THINK, 1192);
        assert_eq!(OFFSET_GENTITY_REACHED, 1196);
        assert_eq!(OFFSET_GENTITY_BLOCKED, 1200);
        assert_eq!(OFFSET_GENTITY_TOUCH, 1204);
        assert_eq!(OFFSET_GENTITY_USE, 1208);
        assert_eq!(OFFSET_GENTITY_DIE, 1216);
        assert_eq!(OFFSET_GENTITY_TAKEDAMAGE, 1236);
        assert_eq!(OFFSET_GENTITY_COUNT, 1360);
        assert_eq!(OFFSET_GENTITY_CHAIN, 1372);
        assert_eq!(OFFSET_GENTITY_TEAMCHAIN, 1388);
        assert_eq!(OFFSET_GENTITY_TEAMMASTER, 1392);
        assert_eq!(OFFSET_GENTITY_WAIT, 1408);
        assert_eq!(OFFSET_GENTITY_DELAY, 1416);
        assert_eq!(OFFSET_GENTITY_TARGET_ENT, 1132);
        assert_eq!(OFFSET_GENTITY_GENERIC_VALUE1, 1420);
        assert_eq!(OFFSET_GENTITY_M_P_VEHICLE, 536);
        assert_eq!(OFFSET_GENTITY_GHOUL2, 540);
        assert_eq!(OFFSET_GENTITY_ENEMY, 1376);
        assert_eq!(OFFSET_GENTITY_LAST_ENEMY, 1380);
        assert_eq!(OFFSET_GENTITY_ACTIVATOR, 1384);
    }

    #[test]
    fn temp_event_sound_constants_match_bg_public() {
        // `entity_event_t` (bg_public.h) and the `EF_PLAYER_EVENT` flag: the
        // numbers were re-counted 2026-10-18 and cross-checked against the
        // oracle-counted 2026-09-15 pins below.
        assert_eq!(ET_EVENTS, 18);
        assert_eq!(EV_SABER_HIT, 30);
        assert_eq!(EV_SABER_BLOCK, 31);
        assert_eq!(EV_SABER_CLASHFLARE, 32);
        // `G_PreDefSound` (`w_force.c:44`): the force-jump/protect/absorb event.
        assert_eq!(EV_PREDEFSOUND, 40);
        assert_eq!(EV_MUTE_SOUND, 74);
        assert_eq!(EV_GENERAL_SOUND, 76);
        assert_eq!(EV_GLOBAL_SOUND, 77);
        assert_eq!(EV_GLOBAL_TEAM_SOUND, 78);
        assert_eq!(EV_ENTITY_SOUND, 79);
        assert_eq!(EV_OBITUARY, 93);
        assert_eq!(EF_PLAYER_EVENT, 0x20);
    }
}
