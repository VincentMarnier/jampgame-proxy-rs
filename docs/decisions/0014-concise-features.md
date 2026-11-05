# 0014 — Concise feature overview

Date: 2026-11-05. Status: accepted. Extends 0004 (UX redesign) and 0013
(content snapshot); changes presentation only.

## Context

Each feature had grown its own subsection under `#features` — sixteen
headings, several long prose paragraphs and one or more code blocks each —
plus eleven bullets in `#tffa`. The user asked for a more concise site:
the one-section-per-feature shape reads as overcharged, even though every
fact is available in the upstream README.

## Decision

- Keep the full, filterable 32-cvar explorer (reference material that is
  expected to be complete). Replace the six headline hardening cards with four
  themed cards — flood/query, parsing/memory bounds, client/content integrity,
  crash containment — because the always-on set is much wider than the README's
  six bullets. The hero stat reads **20+ always-on guards**.
- Replace the per-feature subsections with one **Feature highlights** card
  grid (eight cards, two to three lines each): team control, chat moderation,
  IP/GUID bans, network tools, anti-macro guard, FOV cap, scoreboards and
  spectating, crash diagnostics. Each card names the relevant cvars and the
  one behaviour that matters.
- Drop the feature-level code blocks (`server.cfg`, ban files, log lines,
  netStatus table, rule syntax). The cvar explorer still gives every default,
  and the section ends with a link to the README's Features section for
  command reference, edge cases and samples.
- Compress `#tffa` to a four-step lifecycle with one sentence per step, three
  short cards, and six bullets covering commands, passwords, team handling,
  finish/restart, chat/scoreboard and spawns.
- Move the command chips into `#features`, split into an exhaustive **Player
  commands** set (TFFA including the bare `tffa` alias, `/ignore`,
  `/unignore`, `netStatus`, `showNet`) and an **Admin commands** set (`mute`,
  `unmute`, `ban`, the IP and GUID list commands, `reloadbans`). Notes explain
  the `rcon` prefix and what `ban`/`reloadbans` cover.
- Shorten the hero tagline to one sentence. No nav, layout, dependency or
  deploy changes.
- No removed subsection ids were linked elsewhere, so the anchor cleanup is
  internal (the only surviving anchors are the section-level ones).

## Consequences

- The page no longer duplicates the README's long-form documentation; it is a
  scannable overview that links out for depth. Facts and defaults still come
  from `main` (0002), and the 0012/0013 content snapshot remains the source
  for what the overview summarises.
- Re-adding a detail block later is a content change; keep new features to one
  card unless the user asks for a dedicated section.
- `app/page.jsx` remains the single source of page copy.
