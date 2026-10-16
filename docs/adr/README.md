# Architecture Decision Records

Short records of decisions that shape the code, written when the decision is
made. Format: context, decision, consequences. A superseded ADR stays, with a
pointer to the one that replaces it.

| # | Decision | Status |
|---|----------|--------|
| [0001](0001-modular-monolith.md) | Modular monolith in a Cargo workspace | Accepted |
| [0002](0002-runtime-and-subprocesses.md) | Current-thread Tokio; heavy work in child processes | Accepted |
| [0003](0003-sqlite.md) | SQLite via rusqlite: WAL, STRICT, two connections, user_version migrations | Accepted |
| [0004](0004-media-pipeline.md) | Captions first; whisper.cpp (Vulkan) as fallback | Accepted |
| [0005](0005-local-llm.md) | llama.cpp server on demand; gemma-4-E4B chosen by benchmark | Accepted |
| [0006](0006-api-contract.md) | API v1: loopback, one error envelope, shared read models | Accepted |
| [0007](0007-license.md) | MIT OR Apache-2.0, enforced with cargo-deny | Accepted |
| [0008](0008-nix.md) | Toolchain from nixpkgs; opt-in inference shell; user service | Accepted |
| [0009](0009-feedback-driven-preferences.md) | Preferences adapt from like/dislike feedback | Proposed |
