# Local LLM for relevance scoring on the Intel Arc B580

**Date:** 2026-10-15 · **Engine:** llama.cpp build 10408 (nixpkgs,
`vulkanSupport = true`), `llama-server` started by SignalSift · **Machine:** see
[README](README.md) · **Load average** during runs: 1.1–3.2 (desktop in use).

## Candidates

All Apache-2.0, all quantized to ~4 bits, all fully offloaded to the GPU.
Downloaded manually (never by SignalSift) into `~/.local/share/signalsift/models`,
SHA-256 checked against the published LFS hashes.

| File | Source | Size | SHA-256 |
|------|--------|------|---------|
| Qwen3.5-4B-Q4_K_M.gguf | unsloth/Qwen3.5-4B-GGUF | 2.55 GiB | `00fe7986ff5f6b46…` |
| gemma-4-E4B_q4_0-it.gguf | google/gemma-4-E4B-it-qat-q4_0-gguf (official QAT) | 4.8 GiB | `676c35070db6dbe5…` |
| Qwen3.5-9B-Q4_K_M.gguf | unsloth/Qwen3.5-9B-GGUF | 5.29 GiB | `03b74727a860a563…` |
| gemma-4-12b-it-qat-q4_0.gguf | google/gemma-4-12B-it-qat-q4_0-gguf (official QAT) | 6.5 GiB | `93567e57a8fe10b2…` |

## Method

24 segments (~1 500 prompt tokens each): all 4 of `BZxHviFY_YY` (captions)
and every 4th of the 80 segments of `N3eU-2VIByQ` (whisper transcript), scored
against `config/interests.example.toml` through the daemon's own code path:

```sh
signalsift media show BZxHviFY_YY > galego.json   # likewise for the podcast
nix develop .#inference -c cargo run --release -p signalsift-analysis --example bench_llm -- \
    MODEL.gguf config/interests.example.toml out.json galego.json akita-sample.json
python3 -I scripts/eval-relevance.py tests/eval/relevance-labels.toml out.json
```

Labels (`tests/eval/relevance-labels.toml`): 10 segments labelled 2 (would
watch), 9 labelled 1 (relevant), 5 labelled 0. **They are provisional**, made
by the coding assistant from the raw text before seeing model output, and
need the owner's review. With only 5 negatives, AUC differences of a few
hundredths are within noise: re-running one model with a slightly different
prompt moved its relevant-vs-not AUC from 0.953 to 0.905.

## Results: prompt `segment-v1`

| Model | Recall@50 | Recall@20 | Precision@50 | AUC relevant | AUC strong | Mean score (0/1/2) | s/segment | VRAM | Valid JSON |
|-------|-----------|-----------|--------------|--------------|------------|--------------------|-----------|------|------------|
| Qwen3.5-4B Q4_K_M | 0.47 | 0.74 | 1.00 | 0.87 | 0.83 | 4 / 27 / 48 | 5.1 | 3 070 MiB | 24/24 |
| gemma-4-E4B QAT q4_0 | **1.00** | 1.00 | 0.91 | 0.89 | 0.88 | 43 / 62 / 75 | 8.2 | 3 416 MiB | 24/24 |
| Qwen3.5-9B Q4_K_M | 0.16 | 0.95 | 1.00 | 0.72 | 0.65 | 12 / 25 / 34 | 6.8 | 5 349 MiB | 24/24 |
| gemma-4-12B QAT q4_0 | **1.00** | 1.00 | 0.86 | 0.95 | 0.86 | 42 / 68 / 80 | 16.8 | 7 970 MiB | 24/24 |

Throughput (server-reported): prompt processing 1 659 / 736 / 1 381 / 384
tokens/s, generation 65.1 / 50.9 / 46.2 / 28.6 tokens/s, model load 4.3 /
7.3 / 5.8 / 8.6 s, in the same order. Peak server RSS (5–9 GiB) counts the
memory-mapped GGUF file pages, which are file-backed and reclaimable, not heap.

Qualitative check (same segments, read side by side):

- Both Qwen models score conservatively: at the default threshold of 50,
  Qwen3.5-9B would have hidden 84 % of the segments labelled relevant. That
  is the false-negative failure the project ranks first.
- Both Gemma models produce grounded summaries: for the OWASP segment both
  name the OWASP cheat sheet, SAST, dependency and secret scanning; the 12B
  also caught the MCP mention.
- gemma-4-E4B answered several Portuguese segments **in English** under the
  instruction "same language as the content".
- gemma-4-12B listed one mention that was not in the text
  ("GPT-3a4O-mini (implied…)"), against the "explicitly named" rule.

## Results: prompt `segment-v2` (spoken language named explicitly)

| Model | Recall@50 | Precision@50 | AUC relevant | AUC strong | Mean score (0/1/2) | s/segment | VRAM | English summaries |
|-------|-----------|--------------|--------------|------------|--------------------|-----------|------|-------------------|
| gemma-4-E4B QAT q4_0 | 1.00 | 0.86 | 0.90 | 0.88 | 44 / 64 / 77 | 7.2 | 3 416 MiB | 0 of 24 |
| gemma-4-12B QAT q4_0 | 1.00 | 0.83 | 0.91 | 0.84 | 46 / 69 / 80 | 16.2 | 7 970 MiB | — |

## Decision

**Default: gemma-4-E4B-it QAT q4_0** (ADR 0005). Same recall as the 12B, the
same ranking quality within the noise of this set, 2.2× faster per segment
and 4.5 GiB less VRAM: a four-hour podcast (80 segments) costs about 10
minutes of GPU instead of 22, and leaves most of the 12 GiB card free.
gemma-4-12B is the documented option when quality matters more than time.

Both Gemma models score the labelled-irrelevant segments around 45, so the
feed should rank by score rather than filter at 50; the thresholds in the
API are a client choice.

## Not measured yet

- Summary faithfulness beyond the side-by-side reading above (needs a
  rubric and more than one rater).
- A larger, owner-labelled set; articles (only video segments so far).
- Power draw.
