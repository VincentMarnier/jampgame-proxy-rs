# 0018 — Content refresh for the v0.16–v0.17 line

Date: 2026-11-10. Status: accepted. Extends 0002 (content from `main`), 0014
(concise feature overview) and 0017 (v0.15 refresh).

## Context

The site was last content-synced against `main` at `df526ac` (`v0.15.0`,
ADR 0017). `main` has since moved to `65ac4fc` (`v0.17.0`) through:

- `f3c8e11` — `callvote coinflip` (D-024): a normal 30-second vote whose pass
  announces `Heads` or `Tails` through `svsay`, drawn from `/dev/urandom`.
- `8de517e` — the shared instance slate grows to 16 created slots.
- `075a6d5` + duel fixes (`7c876b7` … `d2315ed`, `91386a7`) — parallel duel
  ladders on `GT_DUEL` (D-025): `proxy_parallel_duels_enable`, courts,
  carried rounds, W/L standings, `(Spec)`/`(Bench)`/`(Duel N)` labels.
- `a78a4f5` — the three TFFA cvars are canonically renamed
  `proxy_parallel_tffas_*`; the old `proxy_tffa_*` names stay registered as
  aliases (D-026).
- `83e9ee6` — a spectator following another TFFA match who asks for a team
  joins the match being watched, including `/join red|blue` (D-006.17).

The top cvar table therefore grows from 46 to **47** rows: one new cvar
(`proxy_parallel_duels_enable`), three renamed rows, no other table change.

## Decision

- Re-verify the cvar table against `origin/main:README.md` at `65ac4fc`:
  **47** cvars. Rename the three TFFA rows and name their legacy aliases; add
  `proxy_parallel_duels_enable` (default `0`, tag `duels`) after
  `proxy_parallel_tffas_maxGroupSize`. Update the `#features` count label to
  **47 · filterable** and the explorer hints to name `duel`.
- Add a `#duels` section after `#tffa` plus nav and footer links: court
  shuffle (court 1 in main, others in instances), per-round duels to
  `fraglimit`, carried ladder bump with the FIFO bench, champion at
  `duel_fraglimit`, W/L board and per-court HUD. Keep it to one lifecycle row,
  three hook cards and a short bullet list, per ADR 0014.
- Add the coinflip to the feature highlights and to the player commands as
  `callvote coinflip`.
- Update the TFFA copy to `proxy_parallel_tffas_enable`, note the legacy
  aliases, and add the spectate-then-join rule (including `/join red|blue` as
  a player command).
- Do **not** touch the hero stat row (frozen by ADR 0015); its `32` metric and
  the `20+`/`0` stats stay as-is.

## Consequences

- `app/page.jsx` remains the single source of page copy; this ADR is the
  current content snapshot (0002/0007–0017 stay historical).
- The target is `main` at `65ac4fc` (`v0.17.0`). The site shows no version
  number in its own copy, so nothing has to be renamed when `v0.18` appears.
- Next refresh: `git show origin/main:README.md`, diff `docs/decisions/` on
  `main` since `65ac4fc`, update the cvar count if it changes, and re-check the
  three dropped C++ cvars are still absent from `main`.
