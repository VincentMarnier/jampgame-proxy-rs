# D-027 — Server-side demos and publication

Date: 2026-11-12. Status: Phases 1–4 implemented; Phase 5 (retention UX beyond
the boundary block, web UI, quotas) not started. Scope: `rust/src/demo/`,
`rust/src/http/`, `rust/src/state.rs` demo cvars, `rust/src/shared_api.rs`
(`/demos`), `csrc/syscall_shim.c` (`statvfs`), `rust/build.rs` unchanged.

Evidence and the wire-format/gate details live in `docs/demo-format.md`
(§10–§13). This record fixes the decisions that are visible to server owners.

## What is recorded

The proxy records the server side of every instance (`main` for non-instanced
gametypes, `tffa<N>`/`duel<N>` per parallel match/court), following a human
player in the world, and writes the Phase 1 `.dm_26` container to
`proxy_sv_demoDir`. A file is closed with `-1/-1` at every boundary
(`GAME_INIT`: full map change and `map_restart`), when its instance empties,
when the size guard fires, on free-space exhaustion, at shutdown, and when the
recording switch goes off.

## Owner decisions

1. **The proxy hosts the HTTP server.** `proxy_sv_enableHttp` +
   `proxy_sv_enableDemoDownload` start a listener (`proxy_sv_httpBind`/
   `proxy_sv_httpPort`); with the download switch off no route is enabled and
   the listener is stopped, so OFF really means no socket. No external-hosting
   mode exists.
2. **`proxy_sv_demoUrl` is a link base, not a hosting switch.** The proxy
   always serves the files; the cvar only changes the links printed at
   boundaries and by `/demos` (empty → the listener's own
   `http://<bind>:<port>`, wildcard bind shown as `127.0.0.1`).
3. **Downloads are opt-in** (`proxy_sv_enableDemoDownload 0` by default) and
   the bind defaults to loopback.
4. **Chat in demos is broadcast-only.** Team chat, private `tell` and
   per-client `print` are never recorded (U-013, resolved).
5. **Accepted guards**: `proxy_sv_demoRetentionHours 24` (0 = keep forever),
   `proxy_sv_demoMaxFileMB 128` (0 = no cap), `proxy_sv_demoMinFreeMB 512`
   (0 = off). Retention and free-space checks run off the game thread.
6. **End of game**: the recorder keeps recording the intermission for
   `proxy_sv_demoIntermissionSecs` (default 3, `0` = close immediately) so the
   demo shows the final scores, then closes the files and sends their download
   links to the clients as console text (`print`, the same mechanism as the
   end-game stats; never `svsay`), one capped block (header, 12 links, then a
   `/demos` hint), only when downloads are configured. If the intermission ends
   before that delay, the block waits for the next game start.
7. **`/demos`** has two surfaces: the HTTP `GET /demos` is a JSON array of
   `{name, url, size}` for the servable files; the in-game `/demos` command
   prints the same links to the requesting client, or one "disabled" line when
   downloads are off.

## Implementation constraints recorded for future work

- The HTTP and maintenance threads never call engine functions or read engine
  memory; they hold only a plain configuration snapshot and the proxy-memory
  open-segment set (`C-1` below).
- All std setup reached from the game thread (socket bind, thread spawn, mutex,
  `statvfs`) goes through the C realign shim (D-001.1/D-018). Filesystem scans
  run on the maintenance thread.
- A still-open segment is published in the open set and additionally must end
  with the `-1/-1` terminator before it is served (404 otherwise).
- Only names accepted by `DemoName::parse` are servable; the resolved path must
  canonicalise inside the demo directory (symlink escape rejected). The
  canonicalise → open TOCTOU window is accepted.
- The size cap is inclusive MiB, checked after a written frame; the next
  segment starts on the next tick so the rest of the game is kept.
- Boundary announcements are `print` broadcasts to the clients (console text,
  like the end-game stats), not `svsay`, and they are not echoed to the server
  console. A console `set proxy_sv_demoUrl http://…` truncates at `//` (the
  engine's tokenizer); use a cfg, as `run-server.sh` does.
