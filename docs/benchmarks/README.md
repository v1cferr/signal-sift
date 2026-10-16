# Benchmarks

Measured values only. Every result names the machine, the date, the exact
command and the conditions. If a number is not here, it has not been measured.

| Document | What |
|----------|------|
| [idle-daemon.md](idle-daemon.md) | Idle RSS / CPU of the daemon |
| [whisper-arc-b580.md](whisper-arc-b580.md) | Speech recognition: models, GPU vs CPU, full 4 h podcast, timestamp accuracy |
| [llm-arc-b580.md](llm-arc-b580.md) | LLM relevance scoring: four models, recall/AUC on labelled segments, speed, VRAM |

Reference machine: Intel Core i5-11400 (6C/12T), Intel Arc B580 12 GB (xe
driver, Mesa ANV Vulkan 1.4.354), 16 GB RAM, NixOS 26.05, Hyprland session in
use during all runs (not an idle lab machine; load average noted per run).
