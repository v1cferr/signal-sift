# Idle daemon footprint

**Date:** 2026-10-15 · **Build:** release (`lto = "thin"`, `codegen-units = 1`) at
commit `a82e507` · **Machine:** see [README](README.md).

## Method

```sh
nix develop -c cargo build --release
scripts/bench-idle.sh 600
```

The script starts `signalsift serve` on a throwaway state directory and a
spare port, waits for `/health`, then samples `/proc/<pid>` every 5 s for the
window. CPU is the utime+stime delta over wall time (percent of one core).
Nothing is read from the daemon's own reporting.

## Result

| Metric | Value | Target |
|--------|-------|--------|
| Window | 601 s, 120 samples | |
| RSS average | 6.3 MiB (6404 KiB) | < 50 MiB (ideal), < 100 MiB (limit) |
| RSS max | 7.6 MiB (7788 KiB) | |
| Threads at end | 1 | |
| CPU time over window | 0.00 s (below the 10 ms tick resolution) | < 1 % of a core |

Notes:

- Threads drop from 2 to 1 because the blocking pool's thread exits after
  10 s idle (ADR 0002).
- Under systemd (transient unit with the Home Manager unit's properties), the
  cgroup charged 1.6 MiB `MemoryCurrent` and 2 tasks right after start: shared
  library pages are charged elsewhere.
- The daemon currently has no background scheduler. When feed polling lands,
  this benchmark must be re-run: periodic timers are the main risk to the
  CPU target.
