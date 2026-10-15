# 0003 — SQLite via rusqlite

**Status:** Accepted (2026-10-15)

## Context

Single-user, single-machine state: sources, items, transcripts (thousands of
cues per long video), analyses, a job queue that must survive reboots.
PostgreSQL would be a second always-on service. Async SQLite crates (sqlx)
add a large dependency tree and compile-time database requirements.

## Decision

- `rusqlite` with the `bundled` feature: the same SQLite version everywhere
  (CI, Nix sandbox, desktop), independent of the host library.
- WAL journal, `synchronous = NORMAL`, `foreign_keys = ON`, 5 s busy timeout.
- **Two connections**: one writer, one read-only reader, each behind a mutex
  and used only from `spawn_blocking`. WAL lets the API read while the worker
  writes; a pool would only add memory.
- Every table is `STRICT`. Instants are Unix milliseconds (INTEGER).
- Migrations are embedded SQL files applied in order inside a transaction
  with `PRAGMA user_version`; no migration framework. Applied files are never
  edited (0001 was edited once, before it ever left the development machine).
- Idempotency through constraints: unique canonical URLs, unique
  `(kind, external_id)`, job `dedup_key`, `INSERT OR IGNORE` for references.

## Consequences

- No vector store: relevance is computed per segment and stored as rows.
  A vector index is added only if a measured retrieval use case needs it.
- Long writes (a transcript chunk) hold the writer lock for milliseconds; the
  reader is never blocked.
- A database written by a newer build is refused instead of silently misread.
