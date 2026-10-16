# 0011 — Heavy inference runs on the GPU or not at all

**Status:** Accepted (2026-10-16). Tracked in V1C-108.

## Context

whisper.cpp and llama.cpp both prefer the GPU, but both **silently fall back
to the CPU** when Vulkan cannot be initialized (driver update, device busy or
missing, a non-Vulkan build on the PATH). On the reference machine the CPU
path is ~11× slower for speech recognition and keeps all six cores busy
(docs/benchmarks/whisper-arc-b580.md). A background daemon doing that behind
the user's back defeats the project's first requirement. The owner asked
explicitly that these tools always run on the GPU.

## Decision

- `runtime.require_gpu` (default **true**) makes the GPU a precondition for
  heavy work:
  - configuration that asks for CPU inference (`asr.use_gpu = false`,
    `analysis.gpu_layers = 0`) is rejected at startup;
  - every whisper-cli run has a watchdog: if the process has not allocated
    VRAM (DRM fdinfo) within 30 s, its process group is killed and the job
    fails with "ran without the GPU";
  - llama-server must hold VRAM once it reports ready, or it is stopped and
    the job fails the same way;
  - failed jobs keep their checkpoints and are retried with backoff, so a
    transient GPU problem delays work instead of moving it to the CPU;
  - `doctor` fails when no DRM render node exists.
- ffmpeg (audio decoding) and yt-dlp (downloads) are not inference and keep
  running on the CPU; they are light and gain nothing from the GPU.
- The flake exports the Vulkan builds of whisper.cpp and llama.cpp from its
  own pinned nixpkgs (the benchmarked versions), and the Home Manager module
  puts those on the service's PATH by default.
- Machines without a usable GPU set `require_gpu = false` deliberately; the
  CPU path stays available, never implicit.

## Consequences

- A broken GPU stack shows up as failed jobs with a clear reason in the log
  and `/v1/status`, instead of a hot, slow machine.
- VRAM detection relies on DRM fdinfo, which the xe, i915 and amdgpu drivers
  expose; a driver without it would need `require_gpu = false`.
