# 0003 — Jedi Academy visual theme

Date: 2026-10-13. Status: accepted.

## Context

The site should feel like Jedi Academy, not a generic docs page, while
staying fast, offline-safe, and static-export compatible.

## Decision

- Deep-space palette (near-black blue, Jedi blue `#5ec8ff`, Sith red,
  crawl-gold `#ffe81f` reserved for the hero title, h3s, and primary CTA).
- Canvas starfield (`app/starfield.jsx`, client-only via `useEffect`,
  twinkle + occasional meteor, static frame + no animation under
  `prefers-reduced-motion`); nebula wash via body background gradients
  (canvas stays transparent so no z-index juggling).
- Lightsaber divider component (`Saber`, five blade colors) between
  sections; corner-cut primary buttons; plain bordered code blocks for
  console snippets.
- System fonts only for body text — no webfont fetches, so the theme
  works fully offline from `out/`. Display type is two bundled OFL fonts
  via `next/font/local` (`app/fonts/`: Audiowide for the hero title,
  Michroma for headings/brand/buttons), with OFL license texts kept
  alongside the woff2 files. Local bundling keeps asset URLs basePath-aware
  for Pages; SW feel otherwise comes from letterspacing, glow, and color.
- Only the hero keeps a kicker line; sections use plain headings.

## Consequences

- Console snippets are plain bordered code blocks (no window chrome).
- Section kickers were removed per review; headings are plain h2s with a
  saber underline. Only the hero keeps a kicker line.
- Keep gold scarce (titles, primary actions, key numbers) or it stops
  reading as Star Wars and starts reading as warning UI.
