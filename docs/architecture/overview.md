# Architecture overview

## Data flow

```
sources (RSS/Atom, YouTube, podcasts)          clients (GLANCE, CLI, curl)
        │                                                ▲
        ▼                                                │ HTTP/JSON, loopback
  ingestion ──► items ──► media pipeline ──► transcripts ─┤
                          captions | ASR     + cues       │
                                  │                       │
                                  ▼                       │
                              segments ──► analysis ──► analyses ──► /v1/feed
                                          (llama.cpp)
```

Everything persists in one SQLite database (ADR 0003). Heavy steps (ASR, LLM)
run in child processes, one at a time by default (ADR 0002).

## Resource model

| State | Daemon | Children | VRAM |
|-------|--------|----------|------|
| Idle | ~7 MiB RSS, 2 threads | none | 0 |
| Fetching captions | + yt-dlp (~80 MiB, seconds) | yt-dlp | 0 |
| Transcribing | unchanged | ffmpeg + whisper-cli (~270 MiB) | ~900 MiB |
| Analysing | unchanged | llama-server, gemma-4-E4B (~5 GiB RSS incl. mapped model) | ~3.4 GiB |

Measured values and methods: [`docs/benchmarks`](../benchmarks).

## Main technical risks

| Risk | Impact | Mitigation |
|------|--------|-----------|
| YouTube changes break yt-dlp | caption/audio fetch fails (seen: HTTP 403 on an audio download, 404 on a channel feed, both on 2026-10-16) | yt-dlp is a replaceable subprocess; pin in Nix but allow a newer path in config (`media.yt_dlp`); failures are recorded and retried with backoff, never fatal |
| Livestreams lack captions for days | ASR cost on every new podcast | ASR on GPU is ~14 min for 4 h; re-check for captions before transcribing later items |
| ASR errors on names and acronyms ("OWASP" became "OASP" in YouTube's own captions) | missed references | keep raw cues; reference detection (V1C-105) must match fuzzily, not just extract URLs |
| LLM false negatives on relevance | relevant segments hidden | score per segment, keep low scores, rank by recall-oriented eval; never delete |
| Prompt injection in transcripts/descriptions | manipulated summaries | content only as data, JSON-schema-constrained output, no tool use, nothing executed |
| SSRF through user-added feed URLs | access to LAN/metadata services | resolve-then-connect checks against private ranges, redirects re-checked, size caps (V1C-99) |
| Mesa/Vulkan regressions on Arc | ASR slows to CPU speed (~11×) | `doctor` checks render nodes; CPU fallback with a smaller model; benchmarks are re-runnable |
| Partial transcripts presented as complete | wrong conclusions | storage refuses `complete` below full coverage; API exposes `partial` and `coverage` |
