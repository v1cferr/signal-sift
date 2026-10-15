# 0004 — Captions first; whisper.cpp (Vulkan) as fallback

**Status:** Accepted (2026-10-15)

## Context

The core use case is finding the five relevant minutes in a multi-hour video
and linking to them. Captions, when they exist, are free and already timed.
They do not always exist: the reference podcast (N3eU-2VIByQ, 4h17m, ended
livestream) had **no captions of any kind** on the day it was published.

## Decision

1. **Captions via yt-dlp**, `json3` format (structured, no rolling-line
   duplication). Manual subtitles in preferred languages first, then automatic
   captions in the *spoken* language (`*-orig`). Machine-translated captions
   are never used.
2. **Local ASR otherwise**: audio-only download at low bitrate (Opus ≤ 70
   kb/s, ~127 MB for 4h17m), decoded by ffmpeg into 10 min chunks with 30 s
   overlap, transcribed by `whisper-cli` with Silero VAD. Each chunk owns a
   keep-range; ranges tile the timeline, duplicates at boundaries are dropped.
   Every chunk is a checkpoint; interruption leaves a resumable partial.
3. **Default model: `ggml-large-v3-turbo-q5_0` on the Vulkan backend**, chosen
   by benchmark on the reference machine (Arc B580): RTF 0.056 with VAD,
   900 MiB VRAM, ~270 MiB RSS; CPU-only is ~11× slower (RTF 0.60). `base` is
   faster but loses words and mangles phrases. Full numbers in
   `docs/benchmarks/whisper-arc-b580.md`.
4. yt-dlp is only ever given a URL rebuilt from a validated 11-character id,
   with `--ignore-config`, `--no-playlist` and `--` before the URL.

## Consequences

- A 4h17m podcast costs ~14 min of GPU time once; captions cost seconds.
- Transcripts record origin and engine; a transcript is never presented as
  complete unless its coverage reaches the media duration.
- Platform access rules apply: only public media, only what yt-dlp can fetch
  without authentication; the daemon stores text, not media (audio is deleted
  after a complete transcription by default).
- Open question: YouTube may add automatic captions to the livestream later.
  A re-ingest prefers captions (cheaper), but an existing complete ASR
  transcript is kept; choosing between them is a later policy decision.
