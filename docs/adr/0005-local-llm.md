# 0005 — llama.cpp server on demand behind a provider trait

**Status:** Proposed (2026-10-15) — to be confirmed by the V1C-101 benchmark.

## Context

Summaries, topics and relevance per segment need an LLM. It must be local,
loaded only while there is work, and replaceable (model and backend).

## Decision (proposed)

- `llama-server` (llama.cpp, Vulkan build, verified to detect the Arc B580) is
  started by the worker when an analysis batch begins and stopped when the
  queue drains or the profile forbids heavy work. The daemon talks to it over
  its OpenAI-compatible HTTP API on a random loopback port.
- Domain code depends on a small `InferenceProvider` trait (prompt in,
  validated JSON out); llama.cpp is one implementation.
- Output is constrained with a JSON schema / grammar and validated; anything
  invalid is retried once and otherwise recorded as a failed analysis.
- Content is untrusted: transcripts go in a data section of the prompt, model
  output is only ever parsed as data, never executed or followed as instructions.
- Candidate models: small quantized instruct models (3–8 B, Q4/Q5) that fit
  comfortably beside nothing else in 12 GB VRAM. No model is downloaded
  automatically.

## Open points

- Which model: needs a labelled evaluation set (Recall@K first, since false
  negatives are the costlier error) before choosing.
- Whether one server per batch or per job is cheaper overall.
