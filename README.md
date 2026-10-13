# jampgame-proxy-rs — website

Project website for [jampgame-proxy-rs](https://github.com/VincentMarnier/jampgame-proxy-rs),
the Rust proxy for the Jedi Academy dedicated-server game module.
Lives at <https://vincentmarnier.github.io/jampgame-proxy-rs/>.

This is the `website` branch. It is web-only: the proxy itself lives on
`main` and is never built or modified from here. Proxy facts shown on the
site (features, cvar defaults, install steps, version) are sourced from
`main` — see `AGENTS.md` and `docs/decisions/0002-content-from-main.md`.

## Stack

Static Next.js site (App Router), deployed to GitHub Pages:

- `output: "export"` → static `out/` directory, no server needed.
- `basePath`/`assetPrefix` set to `/jampgame-proxy-rs` on CI so assets work
  under the project-page URL; local dev stays unprefixed.
- `public/.nojekyll` keeps the exported `_next/` assets alive on Pages.
- Single pre-rendered page plus a `404.html` fallback — no server routing.

## Develop

Requirements: Node 20+.

```sh
npm install
npm run dev      # local dev server, http://localhost:3000
npm run build    # clean static export into out/
```

A running dev server is not verification — always confirm with a clean
`npm run build` before pushing.

## Deploy

Pushes to the `website` branch trigger `.github/workflows/pages.yml`:
install → `next build` → upload `out/` → deploy to GitHub Pages.

## Layout

```text
app/
  page.jsx        # the whole site (single page: hero, features, install, TFFA, build)
  layout.jsx      # metadata + bundled display fonts
  fonts.js        # next/font/local wiring (Audiowide + Michroma, SIL OFL — see app/fonts/OFL-*.txt)
  starfield.jsx   # decorative canvas starfield (client-only, reduced-motion aware)
  globals.css     # Jedi Academy theme
  not-found.jsx   # static 404.html fallback
public/.nojekyll  # required for _next/ assets on Pages
docs/             # branch memory: guides + ADRs under docs/decisions/
```

## Notes for contributors

- Display fonts are bundled locally (`app/fonts/`, SIL Open Font License) —
  do not replace them with webfont links; the export must stay self-contained
  and work offline.
- New animated decoration must follow the starfield pattern: client-only via
  `useEffect`, `aria-hidden`, and a `prefers-reduced-motion` fallback.
- Record structural decisions (stack, layout, deploy, content sourcing) as
  ADRs in `docs/decisions/` — see `docs/README.md`.
