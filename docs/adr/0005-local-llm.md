# 0005 — llama.cpp server on demand behind a provider trait

**Status:** Accepted (2026-10-15). Model chosen by benchmark:
[docs/benchmarks/llm-arc-b580.md](../benchmarks/llm-arc-b580.md).

## Context

Summaries, topics and relevance per segment need an LLM. It must be local,
loaded only while there is work, and replaceable (model and backend).

## Decision

- `llama-server` (llama.cpp, Vulkan) runs as a child process on a random
  loopback port, all layers on the GPU, one slot, no reasoning tokens. A
  heavy worker starts it on the first analysis job, reuses it across
  consecutive ones, and stops it when the queue drains, before a
  transcription job, when the profile revokes heavy work, and after any
  error. Nothing stays loaded while idle.
- Domain code depends on `InferenceProvider` (prompt in, JSON out); llama.cpp
  is one implementation.
- Replies are constrained by a JSON schema (grammar) and validated again;
  invalid output is retried once, then recorded as a rejected segment, never
  replaced by a guessed score.
- Content is untrusted: it goes inside nonce-tagged markers and is described
  to the model as data; the model has no tools and its output is parsed only
  as data.
- Relevance per segment, 0–100, with a recall-biased rubric. Item relevance
  is the maximum over segments, computed in code. Scores carry the model,
  prompt version and interest-profile fingerprint.
- **Default model: gemma-4-E4B-it QAT q4_0** (Apache-2.0, 4.8 GiB): recall
  1.00 at score 50 on the labelled set, AUC ~0.9, 7.2 s per ~1 500-token
  segment, 3.4 GiB VRAM. gemma-4-12B QAT is the higher-cost alternative;
  the Qwen3.5 models scored too conservatively for a recall-first system.
- No model is downloaded by SignalSift; `analysis.model` is unset by default.

## Consequences

- A four-hour podcast costs about 10 minutes of GPU for analysis on top of
  ~14 minutes of transcription.
- Changing the prompt bumps `PROMPT_VERSION`; changing the interest profile
  changes the fingerprint; both make stale scores identifiable.
- The labelled set is small and provisionally labelled; model choice should
  be revisited once the owner reviews and extends it.
