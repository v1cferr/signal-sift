//! Child-process control shared by the media pipeline (yt-dlp, ffmpeg,
//! whisper.cpp) and the analysis pipeline (llama.cpp server).
//!
//! Heavy and untrusted work runs in child processes so the daemon never links
//! inference libraries or keeps their memory resident (ADR 0002).

pub mod service;
pub mod subprocess;
pub mod usage;
