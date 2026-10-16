# 0006 — API v1: loopback, one error envelope, shared read models

**Status:** Accepted (2026-10-15)

## Decision

- Bind `127.0.0.1:7878` by default; non-loopback addresses are refused unless
  `server.allow_non_loopback = true` (the API has no TLS).
- `/health` is unversioned liveness (no database access). Everything else is
  under `/v1`. Within v1 changes are additive; clients ignore unknown fields.
- Errors always use `{"error": {"code", "message"}}`. `code` is stable;
  internal details are logged, never returned.
- Read models live in `api::views` and are also used by the CLI, so there is
  one contract for every client. Nothing is shaped for QML specifically.
- Lists use keyset cursors (`next_cursor`), not offsets.
- Write endpoints (`PUT /v1/profile`, `POST /v1/feedback`, `POST /v1/media`)
  require a bearer token even on loopback, because any local process or
  browser page could otherwise write. The token is generated on first start
  into the state directory with mode 0600 and read by trusted clients (the
  CLI, GLANCE's fetch script) from there. Bodies are capped at 16 KiB.
- The OpenAPI document is versioned in `contracts/openapi.yaml`; a test checks
  that every documented path is routed.
