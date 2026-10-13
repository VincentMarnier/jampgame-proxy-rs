# docs/ — website branch memory

Everything future agents need lives here. Read this directory before changing
stack, layout, deploy, or content sourcing.

- `decisions/` holds ADRs (`0001-*.md`, …). Record every structural decision
  there; do not rely on chat history.
- Proxy facts (features, cvar defaults, install steps, proxy version) come
  from `main` — see `AGENTS.md`. Re-verify against `origin/main` before
  publishing; never invent cvar defaults.
