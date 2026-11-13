# 0020 — Content refresh for the v0.19.0 line

Date: 2026-11-13. Status: accepted. Extends 0002 (content from `main`), 0014
(concise feature overview) and 0019 (v0.18 refresh).

## Context

The site was last content-synced against `main` at `acb817d` (`v0.18.1`,
ADR 0019). `main` has since moved to `17fa454` (`v0.19.0`) through the
ping-equalisation work (D-028):

- `db2bd14` — server-side ping equalisation: the fastest players' inbound
  usercmds are delayed so everyone plays at the slowest in-game player's ping,
  capped by `proxy_sv_pingEqualize` (`0..=250` ms).
- `2c76a71` — zero-ping localhost/LAN players join the padded population
  instead of being mistaken for a missing reading.
- `55e295d` — the first mechanism shifted `ucmd.serverTime`; it was replaced by
  a per-client command queue (`ps.commandTime` and the state stay consistent,
  so client prediction no longer replays applied moves).
- `5227a4c`, `4c51976`, `e776b5c`, `37e34d2` — the final split: half the delay
  is a snapshot-clock (view) shift, half a command queue, so a high-frame-rate
  client stays inside its 64-command prediction ring; the view shift ramps one
  millisecond per snapshot and is subtracted from the `cl_timeNudge` estimate.
- `17fa454` — `v0.19.0` release commit.

`origin/main:README.md` now lists **59** top-level proxy cvars (57 + the two
ping-equalisation cvars) and the net status table gains an `eff` column
(`ping + applied delay`, `-` without a ping reading).

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `17fa454`:
  **59** cvars. Add `proxy_sv_pingEqualize` (default `0`) and
  `proxy_sv_pingEqualizeLog` (default `0`) with tag `network` after
  `proxy_sv_timeNudgeMargin`.
- Add a **Ping equalisation** feature-highlight card after **Network tools**
  (one card, per ADR 0014): the cap, the slowest-player target, the view-clock
  / command-queue split and the input-only scope.
- Update the **Network tools** card and the `netStatus` cvar row to mention the
  effective ping (`eff`).
- Update the cvar explorer hints to suggest `ping`.
- Do **not** touch the hero stat row (frozen by ADR 0015).

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0019 stay historical).
- The target is `main` at `17fa454` (`v0.19.0`). The site shows no version
  number in its own copy, so nothing has to be renamed when `v0.20` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `17fa454`, update the cvar count if it changes, and re-check the
  three dropped C++ cvars are still absent from `main`.
