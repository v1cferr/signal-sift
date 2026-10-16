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
## After a collection round

The first measurement is a daemon that never did anything. The realistic
idle state is *after* it collected, so the script can import sources first:

```sh
SIGNALSIFT_BENCH_SOURCES=config/sources.example.toml scripts/bench-idle.sh 600
```

Build at commit `29894e6` (feed polling, before the job workers). The daemon
polled the 4 validation sources at startup (54 items, one 767 KB feed), then
the 600 s window was measured.

| Metric | Value |
|--------|-------|
| RSS average | 15.8 MiB (16173 KiB) |
| RSS max | 16.3 MiB (16680 KiB) |
| Threads at end | 1 |
| CPU time over window | 0.00 s |

The ~10 MiB over the never-used daemon is allocator retention after parsing
and TLS state; glibc does not return it to the kernel. It is well inside the
target, so no allocator tuning was done. The polling loop wakes at most every
15 minutes when nothing is due, which did not register as CPU time.

With the job workers (commit `9ac764d`), `/v1/status` reported 12.5 MiB RSS
after completing a 13-minute transcription job, i.e. the daemon does not
keep anything from the whisper.cpp run.
