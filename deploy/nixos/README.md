# Deploying SignalSift on NixOS with Home Manager

SignalSift runs as a systemd **user** service (ADR 0008). This flake exports
a Home Manager module, `homeManagerModules.default`, that builds the daemon
from this repository's pinned nixpkgs and puts the benchmarked Vulkan builds
of whisper.cpp and llama.cpp on the service's PATH, so a host on another
channel (e.g. nixos-26.05) still runs the measured versions.

With `runtime.require_gpu` (the default, ADR 0011) speech recognition and the
LLM run on the GPU or not at all: a run that falls back to the CPU is killed
and its job retried later.

## 1. Flake input

```nix
# flake.nix, inputs
signal-sift.url = "github:v1cferr/signal-sift";
```

No `inputs.nixpkgs.follows`: the package and the engines must come from
SignalSift's own lock (the versions in docs/benchmarks/).

## 2. A module that enables the service

```nix
# modules/home/services/signalsift.nix
{ inputs, config, ... }:

let
  models = "${config.home.homeDirectory}/.local/share/signalsift/models";
in
{
  imports = [ inputs.signal-sift.homeManagerModules.default ];

  services.signalsift = {
    enable = true;
    settings = {
      asr.model = "${models}/ggml-large-v3-turbo-q5_0.bin";
      asr.vad_model = "${models}/ggml-silero-v5.1.2.bin";
      analysis.model = "${models}/gemma-4-E4B_q4_0-it.gguf";
      # Versioned with the rest of the configuration; edit it to change what
      # counts as relevant. Copy config/interests.example.toml to start.
      analysis.interests_file = "${./signalsift-interests.toml}";
    };
  };
}
```

and add `./signalsift.nix` to the imports of `modules/home/services/default.nix`.

Models are not managed by Nix (they are large, and SignalSift never downloads
them): fetch them once into `~/.local/share/signalsift/models`, see the
README and docs/benchmarks/ for the exact files and checksums.

## 3. After the first switch

```sh
systemctl --user status signalsift
signalsift doctor                       # every line should be ok
signalsift sources import config/sources.example.toml
xdg-open http://127.0.0.1:7878/
```

Sources live in the database, not in Nix, because feedback, poll state and
history are attached to them.

## What was verified (2026-10-16)

- The module evaluated inside Home Manager `release-26.05` on the system
  nixpkgs (a throwaway flake, not the dotfiles), producing the expected unit:
  `MemoryMax=10G`, `Nice=10`, CPU/IO weight 20, hardening, and a PATH with
  yt-dlp, ffmpeg-headless, whisper-cpp 1.9.2 (Vulkan) and llama-cpp b10408
  (Vulkan).
- `doctor` and a real LLM analysis ran inside a transient user unit with the
  same properties and PATH: Vulkan device found, 3.4 GiB of VRAM used, so the
  hardening does not hide the GPU.
- With Vulkan hidden (simulated broken driver), whisper-cli was killed after
  30 s and llama-server refused right after loading; nothing ran on the CPU.
