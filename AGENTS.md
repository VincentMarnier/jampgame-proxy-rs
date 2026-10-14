# AGENTS.md — website branch (Next.js + GitHub Pages)

This branch is the project website for `jampgame-proxy-rs`. It is web-only.
The Rust proxy lives on `main` and is never built or modified from here.

## Branch model

- `main` (`origin/main`): Rust proxy source of truth. Do not merge it
  into this branch, do not copy `rust/`, `original/`, or `tools/` here.
- `website` (this branch, currently unborn/no commits): Next.js static site,
  deployed to GitHub Pages.
- Remote: `git@github.com:VincentMarnier/jampgame-proxy-rs.git` → Pages URL is
  the project-page form `https://vincentmarnier.github.io/jampgame-proxy-rs/`
  (no custom domain configured). All asset/route paths must work under that
  subpath.

## Content source of truth (read, don't guess)

Proxy facts (features, cvar defaults, install steps) come from `main`.
Read them without switching branches:

```sh
git show origin/main:README.md
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

## Agent skills (install before starting, never commit)

Agent skills are environment-local: install/load them at the start of a
session if the task matches, and never commit them to this branch. Whatever
directory your agent tool uses for local skills (e.g. `.opencode/skills/`)
must stay git-ignored — see `.gitignore`; extend it if your tool uses a
different path.

- UI/UX design, review, or fix (pages, components, theme, accessibility,
  responsive layout): load the `ui-ux-pro-max` skill first and follow its
  workflow (design-system search → implement → pre-delivery checklist).
- No other skill is required for routine work on this branch. If a task needs
  another skill (e.g. slides, branding), note which one in the session and
  keep it uncommitted all the same.

## Agent memory

- Everything future agents need lives in `docs/` on this branch: guides plus
  ADRs under `docs/decisions/` (`0001-*.md`, `0002-*.md`, …). Read them before
  changing stack, layout, deploy, or content sourcing. Record every
  structural decision there — do not rely on chat history.
- Proxy facts still come from `main` (see above); website decisions live here.

## Current workdir hygiene

- This checkout has no scaffold yet: no root `package.json`, no `next.config`.
  Local agent-tooling directories (skill installs, plugins, caches),
  `tests/` is empty, `rust/target/` is leftover Rust build output — none of
  these get committed to this branch.
- Website `.gitignore` must cover `node_modules/`, `.next/`, `out/`,
  `rust/target/`, plus local agent-skills directories (currently
  `.opencode/node_modules/`, `.opencode/skills/` — extend for other tools).
- Verify with a clean static build producing `out/` (e.g. `npm run build`)
  before pushing; a dev server running is not verification.
