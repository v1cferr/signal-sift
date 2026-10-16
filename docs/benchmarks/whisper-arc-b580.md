# Speech recognition on the Intel Arc B580

**Date:** 2026-10-15 · **Engine:** whisper.cpp 1.9.2 (nixpkgs, `vulkanSupport = true`)
· **Machine:** see [README](README.md) · **Audio:** YouTube `N3eU-2VIByQ`
(Flow Podcast #663, Portuguese, 4h17m, format 250 Opus ~66 kb/s).

Models (downloaded manually into `~/.local/share/signalsift/models`, never by
SignalSift):

| File | SHA-256 |
|------|---------|
| ggml-base.bin | `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe` |
| ggml-large-v3-turbo-q5_0.bin | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` |
| ggml-silero-v5.1.2.bin (VAD) | `29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf` |

## 1. Model and device comparison (one chunk)

```sh
nix develop .#inference -c cargo run --release -p signalsift-media --example bench_chunk -- \
  AUDIO MODEL START_S LEN_S [--vad VAD_MODEL] [--cpu --threads 6] --lang pt
```

Same code path as the daemon (ffmpeg decode to 16 kHz mono, then
`whisper-cli`), resources sampled from procfs and DRM fdinfo every 250 ms.
Load average during the runs: 3.9–4.5.

| Model | Device | VAD | Audio | Wall | RTF | Cues | Words | Peak RSS | Peak VRAM | CPU time |
|-------|--------|-----|-------|------|-----|------|-------|----------|-----------|----------|
| large-v3-turbo-q5_0 | GPU | no | 600 s | 32.4 s | 0.054 | 93 | 1893 | 263 MiB | 900 MiB | 26.7 s |
| large-v3-turbo-q5_0 | GPU | yes | 600 s | 33.9 s | 0.056 | 520 | 1851 | 273 MiB | 900 MiB | 31.8 s |
| base | GPU | yes | 600 s | 20.1 s | 0.033 | 46 | 1687 | 302 MiB | 376 MiB | 25.5 s |
| large-v3-turbo-q5_0 | GPU | yes | 120 s | 6.5 s | 0.054 | 76 | 336 | 191 MiB | 900 MiB | 6.0 s |
| large-v3-turbo-q5_0 | CPU ×6 | yes | 120 s | 71.7 s | 0.597 | 75 | 331 | 967 MiB | 0 | 422.8 s |
| base | CPU ×6 | yes | 120 s | 9.4 s | 0.078 | 6 | 323 | 453 MiB | 0 | 54.3 s |

Qualitative check on the same 30 s (00:30:00):

- turbo without VAD emits 25–30 s segments: too coarse for deep links.
- turbo with VAD emits sentence-level cues ("responsável de checar se você
  pediu." 1800.00–1802.48 s).
- base mangles phrases ("quando dá outra a gente ver") and drops ~10 % of words.

**Decision (ADR 0004):** large-v3-turbo-q5_0 + Silero VAD on Vulkan. CPU-only
is ~11× slower and uses ~70× the CPU time; `base` is the CPU fallback.

## 2. Full podcast, with interruption and resume

```sh
signalsift -c poc.toml media ingest --language pt N3eU-2VIByQ   # SIGTERM after 80 s
signalsift -c poc.toml media ingest --language pt N3eU-2VIByQ   # resumes
```

Chunks of 600 s with 30 s overlap (26 chunks).

| | Value |
|---|---|
| First run | stopped by SIGTERM after chunk 1: status `partial`, coverage 7.7 % (1 185 000 ms), 545 cues, no leftover child processes |
| Second run | resumed at 1 185 000 ms, 24 chunks, finished `complete`, coverage 100 % |
| Second run wall time | 805.8 s for 14 262 s of audio (RTF 0.0565) |
| whisper-cli, 24 runs | 779.2 s total, 32.5 s mean per chunk, peak RSS 293 MiB, peak VRAM 900 MiB, 734.8 s CPU |
| ffmpeg decode, 24 runs | 24.6 s total, peak RSS 32 MiB |
| Audio download | 127 MB in ~10 s; deleted after completion |
| Result | 8 653 cues, 80 segments, ~2 MB database |
| Load average | 3.6 at start, 10.6 at end (desktop in use) |

Boundary check over all 8 653 cues: 0 out of order, 0 overlaps above 1 s,
text continuous across every keep boundary, including 1 185 000 ms where the
interrupted run stopped and the resumed run started. Largest gaps are
VAD-skipped silence: 31 s of intro, then at most 8 s.

## 3. Timestamp accuracy

`BZxHviFY_YY` (13:41, Portuguese) has YouTube automatic captions, so ASR was
forced on it and both transcripts were compared:

```sh
signalsift -c poc.toml media ingest --asr force BZxHviFY_YY
python3 -I scripts/eval-timestamps.py ~/.local/state/signalsift/signalsift.db 1
```

| Metric | Value |
|--------|-------|
| ASR cues / matched to captions (3-word phrase within ±15 s) | 257 / 221 (34 unmatched, 2 too short) |
| Median signed offset (ASR − captions) | +18 ms |
| Median absolute offset | 120 ms |
| p90 absolute offset | 370 ms |
| Max absolute offset | 713 ms |
| Within 2 s | 100 % |

Caption word times are interpolated inside caption lines, so the method itself
carries about ±1 s of error; the true alignment is at least this good. Deep
links round down to the second, so ASR timestamps are adequate for `?t=`.
The ASR run took 57.0 s wall for 821 s of audio, including the download.

## 4. Profile switch during a daemon transcription

Daemon running (`signalsift serve`, chunks of 120 s for a short test),
`signalsift media queue BZxHviFY_YY --force-asr`, then after 20 s
`signalsift profile set gaming`, then `signalsift profile set balanced`.

| Check | Result |
|-------|--------|
| whisper-cli running before the switch | 1 process |
| whisper-cli 3 s after `gaming` | 0 processes (killed with its process group) |
| Transcript after `gaming` | `partial`, 230 000 ms covered (28 %), 50 cues, 2 segments already readable |
| DRM file descriptors held by the daemon | 0 (no GPU memory attributable to the daemon) |
| Job state | released to `queued`, attempt not counted |
| After `balanced` | resumed at 230 000 ms, completed: 821 000 ms, 256 cues, 5 segments |

## Not measured yet

- Word error rate against a human transcript (no reference transcript available).
- Power draw: no sensor reading was taken.
- Behaviour with another GPU workload (e.g. a game) running concurrently.
