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
{ inputs, pkgs, ... }:

let
  # Pinned by hash: the exact files benchmarked in docs/benchmarks/.
  hf = repo: file: hash: pkgs.fetchurl {
    url = "https://huggingface.co/${repo}/resolve/main/${file}";
    inherit hash;
  };
  whisper = hf "ggerganov/whisper.cpp" "ggml-large-v3-turbo-q5_0.bin" "sha256-OUIhcJzVrR9AxG5gMcphvOiJMebgiMGIKUxtWlX/p+I=";
  vad = hf "ggml-org/whisper-vad" "ggml-silero-v5.1.2.bin" "sha256-KZQNmNQrkfvQXOSJ8+z3xy8KQvAn5IdZGaKPtMBOos8=";
  llm = hf "google/gemma-4-E4B-it-qat-q4_0-gguf" "gemma-4-E4B_q4_0-it.gguf" "sha256-Z2w1Bw222+Uvk+nIZO4Puk7d6pS5yHXZyxDa/0U/uu4=";
in
{
  imports = [ inputs.signal-sift.homeManagerModules.default ];

  services.signalsift = {
    enable = true;
    settings = {
      runtime.require_gpu = true; # the default; GPU or nothing (ADR 0011)
      asr.model = "${whisper}";
      asr.vad_model = "${vad}";
      analysis.model = "${llm}";
      # Versioned with the configuration; start from config/interests.example.toml.
      analysis.interests_file = "${./signalsift-interests.toml}";
    };
  };
}
```

and add `./signalsift.nix` to the imports of your home services.

The models are declared, so a reinstall needs no manual step; it is still
the configuration that fetches them, never the daemon. If the files are
already on disk, add them to the store first so the build downloads nothing:

```sh
nix-store --add-fixed sha256 ~/.local/share/signalsift/models/gemma-4-E4B_q4_0-it.gguf   # and the other two
```

This is how the owner's dotfiles deploy it (commit `8ca8a221` there, 2026-10-16).

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
