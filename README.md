# SignalSift

Local-first content intelligence: SignalSift follows blogs, newsletters,
YouTube channels and podcasts, extracts what was said (with timestamps),
and surfaces the parts that match your interests, including the five relevant
minutes buried in a four-hour podcast.

It is a single Rust daemon built to stay out of the way: idle, it holds a few
MiB of RAM and no CPU; speech recognition and language models run only while
there is work, as child processes that release all RAM and VRAM when they exit.

> **Status: early development.** Working today: the daemon with feed
> collection (RSS/Atom/JSON Feed, YouTube channels), a persistent job queue,
> captions or local transcription (whisper.cpp) with resumable checkpoints,
> local LLM summaries and per-segment relevance (llama.cpp), the ranked
> `/v1/feed`, live execution profiles, and the read/write API.
> See [Roadmap](#roadmap).

## Quick start

Requires Nix with flakes. Everything else comes from the flake.

```sh
nix develop                      # Rust toolchain, yt-dlp, ffmpeg, sqlite
cargo build --release
./target/release/signalsift doctor
./target/release/signalsift serve          # API on http://127.0.0.1:7878
curl -s localhost:7878/v1/status | jq
```

### Follow sources

```sh
./target/release/signalsift sources import config/sources.example.toml
./target/release/signalsift sources add https://www.youtube.com/channel/UCLW51-XEzuOm5RwPMChHBMw --creator "Augusto Galego"
./target/release/signalsift serve
```

The daemon polls each source on its own schedule (hourly by default, with
conditional GET and failure backoff). New videos from the last 7 days get
their captions fetched automatically; videos without captions are transcribed
locally if a speech model is configured, after a grace period in which
YouTube often adds captions by itself.

### Ingest a video

```sh
./target/release/signalsift media ingest https://youtu.be/BZxHviFY_YY
./target/release/signalsift media show BZxHviFY_YY
curl -s localhost:7878/v1/items/1/segments | jq '.segments[] | {start, link}'
```

Captions are used when the video has them. Otherwise, local transcription runs
if a whisper.cpp model is configured. Models are never downloaded
automatically; fetch one yourself and point the config at it:

```sh
nix develop .#inference          # adds whisper.cpp and llama.cpp (Vulkan)
mkdir -p ~/.local/share/signalsift/models && cd $_
curl -LO https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin
curl -LO https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin
```

```toml
# ~/.config/signalsift/config.toml
[asr]
model = "/home/you/.local/share/signalsift/models/ggml-large-v3-turbo-q5_0.bin"
vad_model = "/home/you/.local/share/signalsift/models/ggml-silero-v5.1.2.bin"
```

Interrupting a transcription (Ctrl-C) keeps what was done, marked *partial*;
running the same command again resumes from the last 10-minute chunk. With the
daemon running, `signalsift media queue <url>` hands the work to it instead.

### Analyse with a local LLM

Each segment is summarized and scored 0–100 against your interest profile;
an item scores as its best segment, so the five relevant minutes of a long
video surface with a link to them. Like the speech model, the LLM is never
downloaded automatically. The benchmarked default
([docs/benchmarks/llm-arc-b580.md](docs/benchmarks/llm-arc-b580.md)):

```sh
cd ~/.local/share/signalsift/models
curl -LO https://huggingface.co/google/gemma-4-E4B-it-qat-q4_0-gguf/resolve/main/gemma-4-E4B_q4_0-it.gguf
cp config/interests.example.toml ~/.config/signalsift/interests.toml   # then edit it
```

```toml
[analysis]
model = "/home/you/.local/share/signalsift/models/gemma-4-E4B_q4_0-it.gguf"
```

```sh
./target/release/signalsift analyze BZxHviFY_YY       # foreground, one item
curl -s 'localhost:7878/v1/feed?limit=5' | jq         # ranked highlights
```

With the daemon running, complete transcripts and new articles are analysed
automatically. `llama-server` is started for a batch and stopped when the
queue drains: no model stays in RAM or VRAM while idle.

## Configuration

All keys are optional; see [`config/signalsift.example.toml`](config/signalsift.example.toml).
Unknown keys are rejected. Paths follow XDG:

| What | Default |
|------|---------|
| Config | `$XDG_CONFIG_HOME/signalsift/config.toml` |
| Database | `$XDG_STATE_HOME/signalsift/signalsift.db` |
| Temporary audio | `$XDG_CACHE_HOME/signalsift/media/` |

Execution profiles (`runtime.profile`): `balanced` (default), `gaming` (API
and light collection only, no transcription or inference), `eco` (API and
state only). Switch the running daemon with `signalsift profile set gaming`:
a transcription in progress stops at once (its processes are killed, the
GPU memory is released) and resumes from its checkpoint when you switch back.

## Running as a service

NixOS with Home Manager:

```nix
imports = [ inputs.signal-sift.homeManagerModules.default ];
services.signalsift.enable = true;
```

Other systems: [`deploy/systemd/signalsift.service`](deploy/systemd/signalsift.service)
(a user unit). Both run at low CPU/IO priority with one memory cap for the
daemon and its inference children.

## API

Loopback only by default. Versioned under `/v1`; errors use
`{"error": {"code", "message"}}`. Contract: [`contracts/openapi.yaml`](contracts/openapi.yaml).
Writes require `Authorization: Bearer $(cat ~/.local/state/signalsift/api-token)`;
the token is created on first start with mode 0600.

| Endpoint | |
|----------|---|
| `GET /health` | liveness |
| `GET /v1/status` | version, profile, database counts, own RSS/CPU |
| `GET /v1/feed?limit=&days=` | ranked highlights with the moment that earned each |
| `GET /v1/sources` | sources, creators, poll state and errors |
| `GET /v1/items?limit=&cursor=` | items, newest first |
| `GET /v1/items/{id}` | item, transcript coverage, references |
| `GET /v1/items/{id}/segments` | segments with `?t=` deep links |
| `PUT /v1/profile` | switch profile live (token) |
| `POST /v1/feedback` | interest feedback on an item or segment (token) |
| `POST /v1/media` | queue a video for captions or transcription (token) |

## Development

```sh
nix develop -c cargo test --workspace
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo deny check
nix flake check                  # builds and tests inside the Nix sandbox
scripts/bench-idle.sh 600        # idle RSS/CPU of the release binary
```

Layout: `apps/signalsift` (CLI and daemon), `crates/core` (config, profiles,
timestamps), `crates/storage` (SQLite), `crates/ingestion` (feeds, SSRF-safe
HTTP), `crates/media` (yt-dlp, ffmpeg, whisper.cpp), `crates/analysis`
(llama.cpp, prompts, scoring), `crates/process` (bounded child processes),
`crates/api` (HTTP). Design decisions are recorded in
[`docs/adr`](docs/adr), measurements in [`docs/benchmarks`](docs/benchmarks).

## Roadmap

- [x] Daemon, CLI (`serve`, `status`, `doctor`), SQLite with migrations, CI, Nix
- [x] Captions and local transcription with checkpoints and partial coverage
- [x] RSS/Atom/JSON Feed ingestion with conditional GET and SSRF protection
- [x] Persistent job queue and profile-aware workers in the daemon
- [x] `/v1/sources`, `PUT /v1/profile`, `POST /v1/feedback`, `POST /v1/media`
- [x] Local LLM summaries and per-segment relevance (llama.cpp, model chosen by benchmark)
- [x] `/v1/feed` (ranked highlights) and the interest profile file
- [ ] Preferences that adapt from like/dislike feedback ([ADR 0009](docs/adr/0009-feedback-driven-preferences.md))
- [ ] Owner-reviewed, larger relevance evaluation set (built from that feedback)
- [ ] GLANCE dashboard integration ([design](docs/integration/glance.md))
- [ ] Article text extraction for sites without feeds

## Responsible use

SignalSift processes public content for personal use. It only fetches what
is publicly accessible without authentication, stores text rather than media
(audio is deleted after transcription by default), and never redistributes
transcripts. Respect the terms of the platforms you follow. Transcripts and
the database are local data and must not be committed.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
