# 0007 — MIT OR Apache-2.0, enforced with cargo-deny

**Status:** Accepted (2026-10-15)

## Context

The repository is public from day one. The Rust ecosystem convention is the
dual MIT/Apache-2.0 license, which lets downstream users pick either.

## Decision

- License the project `MIT OR Apache-2.0`.
- `deny.toml` allows permissive licenses only (MIT, Apache-2.0, BSD, ISC,
  Unicode-3.0, Zlib). CI runs `cargo deny check`.
- The first check found `option-ext` (MPL-2.0) via `directories`. Instead of
  widening the policy, the dependency was replaced by a dozen lines of XDG
  resolution.
- External tools (yt-dlp: Unlicense; FFmpeg: LGPL/GPL depending on build;
  whisper.cpp and llama.cpp: MIT) are executed as separate programs, never
  linked, so they do not affect the license of this code. Model weights have
  their own licenses and are never distributed with the project.

## Pending

Nothing blocks the choice today. If a future dependency needs a copyleft
license, that is decided in a new ADR, not by editing `deny.toml` silently.
