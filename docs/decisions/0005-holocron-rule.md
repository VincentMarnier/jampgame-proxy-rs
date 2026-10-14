# 0005 — Replace lightsaber dividers with holocron rule

Date: 2026-10-14. Status: accepted. Supersedes the divider part of 0003–0004.

## Context

The `Saber` divider between sections (white 5px blade + grey hilt, five
blade colors, infinite opacity pulse) read as cheap: single-layer glow with
hard edges, toy-like hilt proportions, five competing colors with no
semantic meaning, and six constantly animating elements distracting from
content. The h2 saber underline already carries the Star Wars nod.

## Decision

- Replace `Saber` with a static `Divider` ("holocron rule"): 1px hairline
  fading to transparent on both sides with a centered `◆` glyph in
  `--faint`, reusing the brand-dot motif. No color variants, no animation.
- Keep the h2 underline and eyebrow glow untouched — one glow accent per
  section is enough.
- Remove the `saber-pulse` keyframes and the reduced-motion carve-out for
  the blade; the divider needs no motion handling.

## Consequences

- `app/page.jsx` owns `Divider`; `app/globals.css` owns `.rule`.
- Dividers are decorative (`aria-hidden`), visible in dark mode via
  `--border` / `--faint`, no JS, no new dependencies.
