# 0001 — Static Next.js export on GitHub Pages project site

Date: 2026-10-13. Status: accepted.

## Context

The `website` branch hosts the project site for `jampgame-proxy-rs`. The repo
publishes at the project-page URL
`https://vincentmarnier.github.io/jampgame-proxy-rs/` (no custom domain).
Pages serves static files only, and files under `_next/` are dropped unless
Jekyll processing is disabled.

## Decision

- Next.js App Router with `output: "export"` (`out/`), single-page site
  (`app/page.jsx` + shared layout), `not-found.jsx` so the export emits a
  `404.html` fallback.
- `basePath` + `assetPrefix` set to `/jampgame-proxy-rs` when
  `GITHUB_ACTIONS=true` (local dev stays unprefixed). `trailingSlash: true`,
  `images.unoptimized`.
- `public/.nojekyll` so the exported `_next/` assets survive on Pages.
- Deploy via `.github/workflows/pages.yml` (build `out/`, upload + deploy
  Pages) on pushes to `website`. No API routes, Server Actions, ISR, or
  server-side `next/image` — none work on Pages.

## Consequences

- Any new route must be pre-renderable or it 404s behind the static fallback.
- External links and in-page anchors need no prefix; asset imports must go
  through Next so the prefix applies.
