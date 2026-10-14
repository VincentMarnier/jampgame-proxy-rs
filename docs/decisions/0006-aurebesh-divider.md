# 0006 — Aurebesh inscription in the section dividers

Date: 2026-10-14. Status: accepted. Extends 0005 (holocron rule).

## Context

The minimal `◆` divider from 0005 tested well but felt anonymous. Request:
spell "jampgame proxy rs" in Aurebesh (or Mandalorian) glyphs. Aurebesh
was chosen over Mando'a: it is the better-documented, instantly
recognizable Star Wars script with a canonical one-glyph-per-letter
mapping, and a public-domain reference chart exists.

## Decision

- New `app/aurebesh.jsx` renders "JAMPGAME PROXY RS" as inline SVG stroke
  glyphs (24-grid, round joins, `currentColor`) inside the existing `.rule`
  divider between the hairlines. Only the 11 needed letters are drawn
  (A E G J M O P R S X Y); each is an original simplification checked
  against the public-domain chart "Star-Wars-aurebesh-alphabet-chart-vert.svg"
  (Wikimedia Commons, PD-self), so there is no font to license, no download,
  and no offline/static-export risk. No new dependencies.
- Stays decorative: bare paths with no `<title>`, inside the already
  `aria-hidden` divider — screen readers skip it, same as 0005.
- Responsive: 19px glyphs on desktop, 13px under 640px so the 14-glyph
  inscription never crowds out the hairlines on small phones.

## Consequences

- `Divider` in `app/page.jsx` now renders `<Aurebesh>`; `.rule span` keeps
  only layout, glyph styling lives in `.aurebesh` / `.ab-space`.
- Letterforms are a homage, not a font — documented as such in the file
  header with the full transliteration so future editors can verify them.
- Mandalorian glyphs remain an option if a matching moment needs them
  (e.g. a TFFA badge), but are out of scope here.
