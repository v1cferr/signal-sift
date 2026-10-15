# 0001 — Modular monolith in a Cargo workspace

**Status:** Accepted (2026-10-15)

## Context

SignalSift runs permanently on one desktop machine. The work splits into
collection (network-bound, cheap), media processing and inference (expensive,
bursty) and an HTTP API for clients such as GLANCE. Microservices, external
queues or a separate API process would add resident memory, IPC and
deployment surface without a scaling need: there is one user and one machine.

## Decision

One binary (`apps/signalsift`) and a few library crates with clear
dependency direction:

```
apps/signalsift ──► api ──► storage ──► (rusqlite)
       │             └────► core
       ├────────► media ──► core
       └────────► core
```

- `core`: configuration, profiles, timestamps. No I/O beyond reading config.
- `storage`: SQLite schema, migrations, repositories. No HTTP, no media.
- `media`: yt-dlp / ffmpeg / whisper.cpp wrappers producing cues and
  segments. No database dependency: the caller persists.
- `api`: Axum router and the JSON read models (also used by the CLI).
- `apps/signalsift`: CLI, wiring, orchestration (`media ingest`, `serve`).

The suggested `ingestion` and `processing` crates are not created yet. A crate
appears when there is code for it: feed ingestion lands in `ingestion` with
V1C-99, and LLM analysis gets its own crate with V1C-101 so the inference
client stays out of the media crate.

## Consequences

- Crate boundaries are enforced by the compiler: `media` cannot reach into
  the database, `storage` cannot make HTTP calls.
- A single process to supervise, measure and package.
- If one component ever needs isolation (e.g. a crashy extractor), the
  subprocess pattern of ADR 0002 provides it without splitting the daemon.
