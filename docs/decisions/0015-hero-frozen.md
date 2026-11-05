# 0015 — Hero section is frozen

Date: 2026-11-05. Status: accepted.

## Context

The hero copy has been revised repeatedly — title, tagline, status pill and
stat row — across content and concision passes. Those changes were driven by
explicit user feedback, not by new proxy facts, and each pass risks undoing
wording the user has already settled.

## Decision

Treat the whole hero block in `app/page.jsx` as frozen:

- the status pill and its themes;
- the `h1` title;
- the tagline;
- the CTA row and terminal quickstart;
- the stat row and its three metrics.

Do not change its copy, structure or metrics as part of a content refresh, a
feature addition, a concision pass, a redesign or any other general edit. Edit
it only when the user explicitly asks for a hero change in the current
request. If a new feature seems to require a hero update, ask first and wait
for an explicit instruction; do not infer permission from an unrelated task.

This decision has no expiry. `docs/decisions/` is the source of truth, and the
hero marker in `app/page.jsx` points here.

## Consequences

- New features and refreshes update their own sections only; the hero stays as
  the user last approved it.
- A requested hero change is allowed and should record any lasting new wording
  here or in a later ADR; a one-off copy tweak needs no new ADR.
- Agents must not "fix", "refresh" or "align" the hero opportunistically.
