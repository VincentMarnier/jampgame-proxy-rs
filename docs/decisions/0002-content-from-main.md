# 0002 — Website content sourced from `main`, verified at build time

Date: 2026-10-13. Status: accepted.

## Context

Proxy behavior (cvar table, install steps, TFFA commands, version) evolves on
`main`. The website branch is web-only and must not copy `rust/`, `original/`,
or `tools/` — stale duplicated content would silently drift.

## Decision

- Hand-maintained page content, but every fact re-verified against
  `origin/main` before publishing:
  `git show origin/main:README.md`, `rust/Cargo.toml` (version),
  `docs/decisions/*` for feature semantics.
- Current snapshot: proxy v0.3.0; 13 documented cvars; install =
  rename-to-`jampgame_original.so` + drop-in; TFFA commands
  `tffa_create` / `tffa_join <id>` / `tffa_leave` / `tffa_list`.
- Dropped C++ cvars (`proxy_sv_pingFix`, `proxy_sv_antiWallHack`,
  `proxy_sv_sabersFps`, `proxy_sv_disableKillCmd`) are explicitly marked as
  unavailable on the site.

## Consequences

- Content updates are manual but cheap (single `app/page.jsx`); the
  verification step is what keeps them honest.
- If the cvar table grows, consider generating it from `main` at build time
  instead of hand-editing.
