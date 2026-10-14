# 0004 — Full UI/UX redesign: marketing-page IA on the JKA theme

Date: 2026-10-14. Status: accepted. Supersedes nothing; extends 0001–0003.

## Context

The previous site was a single linear column (hero → how → install → features →
build) with one dense cvar table, no search, no copy affordances, and a weak
hero (title + tagline + buttons, no proof, no quickstart). Content was
accurate but hard to scan; server admins — the primary audience — could not
filter cvars or copy commands. The JKA deep-space identity (0003) tested well
and stays.

## Decision

- Keep the JKA theme tokens (deep-space blue, Jedi blue, crawl-gold kept
  scarce, bundled Audiowide/Michroma, starfield canvas, saber dividers) and
  the static-export constraints from 0001 (single pre-rendered page, no new
  dependencies — still only `next`/`react`).
- New information architecture: split hero (copy + stat proof left,
  quickstart terminal card right) → architecture pipeline diagram (engine →
  proxy → original) + forward/hook/memory cards → 3-step install → features
  (hardening cards with SVG icons, filterable cvar explorer, team-size rules)
  → dedicated TFFA section with 4-step lifecycle → build → sitemap footer.
  TFFA is promoted to its own anchor (`#tffa`) since it is the flagship
  feature.
- New client islands in `app/interactive.jsx` (all static-export safe,
  SSR-rendered, progressive enhancement only): `CopyButton` (code blocks,
  clipboard fallback + "Copied" feedback), `CmdChip` (tap-to-copy player
  commands), `CvarExplorer` (live filter over the 13 cvars with category tags
  and result count). Search input has a real `<label>`; copy buttons use
  `aria-live`.
- Accessibility pass: skip link, `aria-labelledby` sections pointing at the
  `h2` ids, labelled nav, table `<caption>`, visible focus rings, 44px
  minimum touch targets on nav/links/inputs/buttons, `prefers-reduced-motion`
  disables starfield animation, saber pulse, and smooth scroll.
- Content re-verified against `origin/main` at redesign time (README):
  same 13 cvars/defaults, same install steps,
  same TFFA semantics, same four dropped C++ cvars.

## Consequences

- `app/page.jsx` owns content + layout, `app/interactive.jsx` owns the three
  client islands, `app/globals.css` owns the token system (~700 lines). No
  new routes: deep links still resolve to anchors on the single page.
- First-load JS unchanged (~105 kB, same chunk shape as before) — the islands add
  no dependencies.
- Future content edits stay in `page.jsx`; if the
  cvar table grows past ~20 rows, revisit 0002's build-time generation idea.
