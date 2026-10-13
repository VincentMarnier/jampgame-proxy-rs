# AGENTS.md — website branch (Next.js + GitHub Pages)

This branch is the project website for `jampgame-proxy-rs`. It is web-only.
The Rust proxy lives on `main` and is never built or modified from here.

## Branch model

- `main` (`origin/main`, v0.3.0): Rust proxy source of truth. Do not merge it
  into this branch, do not copy `rust/`, `original/`, or `tools/` here.
- `website` (this branch, currently unborn/no commits): Next.js static site,
  deployed to GitHub Pages.
- Remote: `git@github.com:VincentMarnier/jampgame-proxy-rs.git` → Pages URL is
  the project-page form `https://vincentmarnier.github.io/jampgame-proxy-rs/`
  (no custom domain configured). All asset/route paths must work under that
  subpath.

## Content source of truth (read, don't guess)

Proxy facts (features, cvar defaults, install steps, version) come from `main`.
Read them without switching branches:

```sh
git show origin/main:README.md
git show origin/main:rust/Cargo.toml   # crate version
git show origin/main:docs/decisions/0006-parallel-tffa.md
```

Trust `README.md` + `docs/` over memory. Notably the Rust port deliberately
drops `proxy_sv_pingFix`, `proxy_sv_antiWallHack`, `proxy_sv_sabersFps`,
`proxy_sv_disableKillCmd` — never document those as available.

## Pages constraints (non-negotiable)

- Next.js must use static export (`output: "export"`, output dir `out/`).
  No API routes, no Server Actions, no ISR, no `next/image` server
  optimization — Pages serves static files only.
- Must set `basePath`/`assetPrefix` to `/jampgame-proxy-rs` (or read it from
  CI env) or all assets and client navigation 404 on Pages.
- Must emit `.nojekyll` in `out/` (files under `_next/` are dropped otherwise).
- Client-side routing only; no rewrites/redirects. Deep links must either be
  pre-rendered or fall back to a `404.html`.

## Agent memory

- Everything future agents need lives in `docs/` on this branch: guides plus
  ADRs under `docs/decisions/` (`0001-*.md`, `0002-*.md`, …). Read them before
  changing stack, layout, deploy, or content sourcing. Record every
  structural decision there — do not rely on chat history.
- Proxy facts still come from `main` (see above); website decisions live here.

## Current workdir hygiene

- This checkout has no scaffold yet: no root `package.json`, no `next.config`.
  `.opencode/` is only the OpenCode plugin (`@opencode-ai/plugin`), `tests/`
  is empty, `rust/target/` is leftover Rust build output — none of these get
  committed to this branch.
- Website `.gitignore` must cover `node_modules/`, `.next/`, `out/`,
  `rust/target/`, `.opencode/node_modules/`.
- Verify with a clean static build producing `out/` (e.g. `npm run build`)
  before pushing; a dev server running is not verification.
