# 0002 — Current-thread Tokio; heavy work in child processes

**Status:** Accepted (2026-10-15)

## Context

The top requirement is near-zero cost when idle: target < 50 MiB RSS and
< 1 % of one core, with no model resident in RAM or VRAM. The daemon's own
async work is waiting on sockets, timers and child processes. Linking
whisper.cpp or llama.cpp into the daemon would keep their code and, worse,
their allocators and GPU contexts in the long-lived process.

## Decision

- The binary builds a **current-thread** Tokio runtime. Blocking work (SQLite)
  goes to the blocking pool, capped at 4 threads that exit after 10 s idle.
- **Every heavy or untrusted tool runs as a child process** (`media::subprocess`):
  cleared environment with an allow-list, own process group (timeouts and
  cancellation kill grandchildren), wall-clock timeout, stdout cap, stderr
  tail, and procfs sampling of the child's RSS, CPU time and DRM fdinfo VRAM.
- Models are loaded per job by the child and released when it exits.

## Consequences

- Measured idle footprint: ~7 MiB RSS, 2 threads (see
  `docs/benchmarks/idle-daemon.md`).
- Per-call process start-up and model load cost: ~1–2 s for whisper.cpp,
  negligible against 30 s per 10 min chunk.
- VRAM is provably zero between jobs: the daemon never opens a render node.
- Concurrency of heavy jobs is a scheduler decision (default 1), not a
  thread-pool size.
