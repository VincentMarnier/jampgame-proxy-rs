# D-026 — Parallel-TFFA cvar rename (`proxy_parallel_tffas_*`)

## Context

D-006 introduced the parallel self-organized TFFA feature under three cvars
named after the feature as it was first described:

| Old name (pre-rename)    | Default |
| ------------------------ | ------- |
| `proxy_tffa_enable`      | `0`     |
| `proxy_tffa_maxMatches`  | `8`     |
| `proxy_tffa_maxGroupSize`| `12`    |

D-025 then added the sibling feature — parallel duels — as
`proxy_parallel_duels_enable`. Both features are the same instancing core with a
different gametype policy, so the naming should say *what* runs in parallel
(matches, duels), not just the gametype acronym. The TFFA feature runs
*several TFFA matches at once*, hence the plural "TFFAs".

## Decision

1. The canonical cvars are renamed:

   | Canonical name                        | Default |
   | ------------------------------------- | ------- |
   | `proxy_parallel_tffas_enable`         | `0`     |
   | `proxy_parallel_tffas_maxMatches`     | `8`     |
   | `proxy_parallel_tffas_maxGroupSize`   | `12`    |

2. The old `proxy_tffa_*` spellings stay **registered**, with the same defaults
   and `CVAR_ARCHIVE`, as aliases. An existing server config, command line
   (`+set proxy_tffa_enable 1`) or script therefore keeps working unchanged, and
   the old names still show up in `cvarlist`.

3. **Resolution rule** (`state::cvar_integer`, `state::legacy_alias`): the
   canonical cvar wins once its value differs from the default it was registered
   with; while it still holds that default, a non-default legacy value applies.
   If both hold non-default values, the canonical one wins. Only the three
   renamed cvars have an alias; every other index resolves to `None`.

   The code never reads the legacy mirror directly — all three call sites go
   through the resolver: `state::instance_mode`, `tffa::reset_round` /
   `tffa::on_frame_start` (the enable latch, including the non-zero → zero
   edge that closes every created match) and `tffa::max_matches` /
   `tffa::max_group_size`.

## Why not "last write wins"

The obvious alternative — let whichever spelling was changed most recently
win — needs a way to tell "the admin set this" from "this is the registered
default". The engine offers none at `GAME_INIT`:

- `Cvar_Register` → `Cvar_Get` + `Cvar_Update`
  (`original/jedi-academy/codemp/qcommon/cvar.cpp:901-953`). The mirror's
  `modificationCount` is forced to `-1` and then copied from the engine scalar.
- A cvar created by the config/command line (`Cvar_Set2` with no existing cvar →
  `Cvar_Get(..., CVAR_USER_CREATED)`) starts at `modificationCount = 1`
  (`cvar.cpp:264-272`), and so does a cvar this module creates at registration.
  Both mirrors therefore hold `1` at startup whether or not the user configured
  them, so a modification-count snapshot cannot distinguish the two.
- `Cvar_Set2` only bumps the count when the value string actually changes
  (`cvar.cpp:330-332`, `391-395`), so a config that sets a cvar to its default
  leaves no trace at all.

The default-vs-value comparison used by the resolver is the only signal
available through the game-module traps without reaching into engine memory.

## Documented limits

- An explicit `set proxy_parallel_tffas_enable 0` cannot be told apart from the
  untouched default: while a non-default legacy value is configured, the legacy
  value keeps applying. To force the feature off on such a server, clear the
  legacy cvar as well (`set proxy_tffa_enable 0`) or remove it from the config.
- The legacy mirror is never written back, so `cvarlist` may show a stale legacy
  value (e.g. `1`) while the canonical one decides. The proxies' own log lines
  and behaviour always follow the resolver.

## Verification

- `state::tests::parallel_tffa_cvars_carry_their_legacy_alias` pins the six
  registered names, the shared defaults and the `legacy_alias` mapping (and that
  no other cvar has an alias).
- `state::tests::legacy_tffa_alias_is_read_only_as_a_fallback` exercises the
  resolution rule: both default, legacy-only set, canonical overrides legacy,
  and the documented fall-back when the canonical returns to its default.
- `cargo test` (443 passing), `cargo clippy --all-targets`, `cargo fmt --check`
  and the i686 release build (`./scripts/build.sh`).
