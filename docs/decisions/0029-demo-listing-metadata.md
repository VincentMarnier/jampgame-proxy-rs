# D-029 — Demo metadata in the `/demos` listing

Date: 2026-11-25. Status: implemented (`rust/src/demo/meta.rs`,
`rust/src/demo/msg.rs`, `rust/src/http/server.rs`). Scope: the `/demos` JSON
listing; the demo container, recorder and retention are unchanged.

## 1. What changed

`GET /demos` now returns, for each servable file, the existing
`{name, url, size}` fields plus a `metadata` object:

```json
{
  "name": "20261112-112854_mp_ffa3_main_s4.dm_26",
  "url": "https://example.com/demos/20261112-112854_mp_ffa3_main_s4.dm_26",
  "size": 123456,
  "metadata": {
    "map": "mp/ffa3",
    "gametype": 0,
    "players": [
      {"num": 2, "name": "Desann", "team": 0, "score": 20}
    ],
    "teamScores": {"red": 2, "blue": 1},
    "durationMs": 68984,
    "snapshots": 8624
  }
}
```

`metadata` is `null` when the file has no readable stream (a truncated
leftover, a non-demo file that somehow matches the name pattern, or an empty
file). Fields that a particular file does not carry are `null` (`map`,
`gametype`, `teamScores`); `players` is `[]`.

## 2. Where each field comes from

The scan reads the file exactly as the demo client does, but it does not
replay the game:

| field | source |
| --- | --- |
| `map` | `mapname` key of the recorded `CS_SERVERINFO` (gamestate or a later `cs 0`) |
| `gametype` | `g_gametype` key of the same info string, parsed as an integer |
| `players` | the last `scores` command in the file, joined with the latest `CS_PLAYERS` infos |
| `players[].name` / `team` | `CS_PLAYERS` `n` and `t` keys for that slot |
| `players[].score` | the `scores` entry's `PERS_SCORE` int |
| `teamScores` | the last `scores` header (`red`, `blue`), team gametypes only |
| `durationMs` | last minus first snapshot `serverTime`, clamped at 0 |
| `snapshots` | number of `svc_snapshot` messages seen |

`players` keeps the scoreboard order. It excludes spectators (`t = 3`) so a
parallel-TFFA demo lists its own match (the per-viewer `CS_PLAYERS` demotion
is exactly this marker), and it excludes entries whose `CS_PLAYERS` was
cleared (players who left before the last board). Team gametypes
(`GT_TEAM`, `GT_SIEGE`, `GT_CTF`, `GT_CTY`) additionally require `team` 1 or
2. FFA and duel players carry `team` 0 and are kept.

`teamScores` is reported only for those team gametypes: `GT_DUEL` stores the
court pair in `CS_SCORES1/2`, not RED/BLUE totals, and FFA has no teams.

## 3. Why the HTTP side parses the file

The recorder knows the same values at close time, but a sidecar would only
describe files recorded after the change, while the listing also publishes
files already on disk (for example after a module reload), and it would make
retention and rotation responsible for a second file. The demo format is
already documented and written by this crate, so a read-only scan gives old
and new files the same answer from one source of truth.

The scan is deliberately small:

- the file container (`int32 sequence, int32 length, payload`) is walked
  frame by frame;
- every payload starts with `lastClientCommand`, then a sequence of `svc_*`
  ops; server commands precede the snapshot in the engine's own message
  order, so the scan handles `svc_serverCommand`, `svc_configstring`,
  `svc_setgame`, `svc_download` and the XBOX position ops, reads
  `svc_snapshot`'s `serverTime`, and stops the frame there;
- the gamestate scan stops at the first `svc_baseline`: configstrings precede
  baselines, so the entity field table is never needed;
- the bit reader mirrors `MSG_ReadBits` over the static `msg_hData` tree
  (`MsgReader` in `demo/msg.rs`), the exact inverse of the existing writer.

This means a full 128 MiB segment still costs one sequential read per file,
but only a handful of decoded bits per frame; it never decodes the snapshot
bodies. Parsed results are cached by path, size and mtime, so a listing does
not rescan unchanged files. The cache is bounded (1024 entries, cleared when
full) and lives in the HTTP process only; the game thread never calls it.

## 4. Safety

- The scan runs on the HTTP worker threads, never on the game thread, and
  reads only the demo directory it already serves.
- Every read is bounds-checked: a truncated payload or a corrupt length field
  ends the scan (or the frame) instead of panicking. Frame payloads above
  64 KiB are refused (the engine's `MAX_MSGLEN` is 16 KiB).
- Commands and configstrings are decoded with explicit caps (`1024`/`4096`
  bytes for a command, `8192` for a gamestate configstring), matching the
  engine's `MAX_STRING_CHARS`/`BIG_INFO_STRING` limits.
- JSON text still goes through `json_escape`; names with colour codes or
  quotes are safe.

## 5. Evidence

- **TESTED** — `demo::meta` builds a synthetic demo with the crate's own
  `encode_gamestate`/`DemoWriter`, then checks map, gametype, final scores,
  a later `cs` rename, spectator exclusion, duration and `teamScores`;
  separate tests cover a non-team FFA, `bcs0/1/2` chunk reassembly,
  malformed input and the real Phase 0 fixture
  (`tests/fixtures/demo/server-ffa3-phase0.dm_26` → `mp/ffa3`, gametype 0).
- **TESTED** — `MsgReader` round-trips `MsgWriter` values and decodes every
  one of the 256 symbols.
- **RUNTIME (analysis)** — the `artifacts/` demo corpus (FFA, stock duel,
  parallel duels, TFFA, CTF, chat, guards, 32-slot, size-cap segments) was
  scanned and compared field by field with `tools/demo_inspect.py`'s
  independent full-message decoder: map, gametype, final players (num,
  name, team, score), team scores, duration and snapshot count matched on
  every compared file. The fixture and corpus test was manual because
  `artifacts/` is not tracked.
- **RUNTIME (HTTP)** — the end-to-end listing test now asserts the metadata
  object and the `null` case for a file with no readable stream.

## 6. Limits

- `durationMs` is the recorded segment's time, not the whole match clock: a
  size-capped rotation starts a new file. The base 1.01 game module does not
  put `CS_LEVEL_START_TIME` in these demos, so the match clock cannot be
  reconstructed from the recording alone.
- Names are the recorded bytes (colour codes included), not the client's
  rendered text.
- A file's final board is the last `scores` command. If a segment was closed
  before the recorder queued a board (for example immediately after the
  first snapshot), `players` is empty even though `CS_PLAYERS` may be
  populated.
