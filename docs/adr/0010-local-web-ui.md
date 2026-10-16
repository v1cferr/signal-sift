# 0010 — A local web UI served by the daemon

**Status:** Accepted (2026-10-16). Tracked in V1C-107.

## Context

The owner wants to use SignalSift from a browser on this machine (feed,
moments, like/dislike) without depending on GLANCE. Remote access (phone,
outside the LAN) was explicitly left out for now.

## Decision

- The daemon serves one page at `/`, plus `/ui/app.js` and `/ui/app.css`,
  from the same loopback listener as the API. No new port, no new process.
- No frontend toolchain: hand-written HTML, CSS and JavaScript embedded in
  the binary with `include_str!`. No Node, no bundler, nothing to install;
  the cost at idle is a few kilobytes of static data in the binary.
- The page is a client of API v1 like any other: `/v1/feed`, `/v1/items`,
  `/v1/status`, `POST /v1/feedback`, `PUT /v1/profile`. No UI-only endpoints.
- Security:
  - reads stay open (public content, loopback only);
  - writes need the bearer token, which the user pastes once and the page
    keeps in `localStorage`. Other origins cannot read it, and since the API
    sends no CORS headers they cannot send an `Authorization` header to it
    either;
  - a strict Content-Security-Policy (`default-src 'self'`, no inline
    scripts or styles, `frame-ancestors 'none'`), `X-Content-Type-Options:
    nosniff` and `Referrer-Policy: no-referrer` on UI responses;
  - third-party text (titles, summaries, transcripts) is inserted with
    `textContent` only, never as HTML; links are rendered only for http(s)
    URLs.
- Remote access, if wanted later, goes through an authenticated tunnel
  (e.g. Tailscale serve with HTTPS), never by binding the daemon to a public
  address. That needs a change in the owner's NixOS configuration and a
  separate decision.

## Consequences

- The UI ships and versions with the daemon, so it can never drift from the
  API it calls.
- It stays deliberately small; a framework is reconsidered only if the UI
  outgrows a few hundred lines.
- The route-sync test documents `/health` and `/v1/*` in OpenAPI; UI
  routes are static assets and are excluded from it.
