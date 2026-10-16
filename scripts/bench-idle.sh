#!/usr/bin/env bash
# Measures an idle SignalSift daemon: RSS and CPU over a fixed window.
#
#   scripts/bench-idle.sh [SECONDS] [BINARY]
#   SIGNALSIFT_BENCH_SOURCES=config/sources.example.toml scripts/bench-idle.sh 600
#
# With SIGNALSIFT_BENCH_SOURCES, those sources are imported first, so the
# daemon polls them at startup and the window measures idle-after-collection
# (allocator retention, TLS state) instead of a never-used daemon. That run
# makes real HTTP requests.
#
# Starts the daemon on a throwaway state directory and a spare port, waits for
# /health, then samples /proc/<pid> every 5 s. CPU% is (utime+stime delta) /
# wall time, i.e. percent of ONE core. Only procfs and curl are used, so the
# numbers come from the kernel, not from the daemon's own reporting.
set -euo pipefail

duration=${1:-600}
bin=${2:-target/release/signalsift}
port=${SIGNALSIFT_BENCH_PORT:-17999}
work=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT

cat >"$work/config.toml" <<CONF
[server]
listen = "127.0.0.1:$port"
[paths]
state_dir = "$work/state"
cache_dir = "$work/cache"
CONF

if [[ -n ${SIGNALSIFT_BENCH_SOURCES:-} ]]; then
  "$bin" --config "$work/config.toml" sources import "$SIGNALSIFT_BENCH_SOURCES" 2>/dev/null
fi

"$bin" --config "$work/config.toml" serve 2>"$work/log" &
pid=$!
for _ in $(seq 50); do
  curl -sf "http://127.0.0.1:$port/health" >/dev/null && break
  sleep 0.1
done
if [[ -n ${SIGNALSIFT_BENCH_SOURCES:-} ]]; then
  # Let the startup poll finish before the window opens.
  sleep 15
  echo "startup polls: $(grep -c polled "$work/log")" >&2
fi

hz=$(getconf CLK_TCK)
ticks() { awk '{print $14 + $15}' "/proc/$pid/stat"; }
rss() { awk '/^VmRSS:/ {print $2}' "/proc/$pid/status"; }
threads() { awk '/^Threads:/ {print $2}' "/proc/$pid/status"; }

start_ticks=$(ticks)
start_ns=$(date +%s%N)
max_rss=0
sum_rss=0
n=0
end=$((SECONDS + duration))
while ((SECONDS < end)); do
  sleep 5
  r=$(rss)
  ((r > max_rss)) && max_rss=$r
  sum_rss=$((sum_rss + r))
  n=$((n + 1))
done
end_ticks=$(ticks)
end_ns=$(date +%s%N)

awk -v t0="$start_ticks" -v t1="$end_ticks" -v hz="$hz" -v w0="$start_ns" -v w1="$end_ns" \
    -v max="$max_rss" -v sum="$sum_rss" -v n="$n" -v thr="$(threads)" 'BEGIN {
  wall = (w1 - w0) / 1e9
  cpu = (t1 - t0) / hz
  printf "{\"window_s\": %.0f, \"samples\": %d, \"rss_avg_kib\": %d, \"rss_max_kib\": %d, ", wall, n, sum / n, max
  printf "\"threads\": %d, \"cpu_s\": %.2f, \"cpu_pct_one_core\": %.4f}\n", thr, cpu, 100 * cpu / wall
}'
