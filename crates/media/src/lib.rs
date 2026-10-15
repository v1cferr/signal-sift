//! Video and podcast pipeline building blocks.
//!
//! Strategy, cheapest first: existing captions (manual, then automatic in the
//! spoken language) via yt-dlp; only when none exist, audio-only download and
//! local speech recognition with whisper.cpp. Every external tool runs as a
//! bounded child process (see [`subprocess`]).
//!
//! This crate has no database dependency: it turns media into [`cue::Cue`]s and
//! [`segment::Segment`]s, and the caller decides how to persist them.

pub mod cue;
pub mod json3;
pub mod links;
pub mod segment;
pub mod subprocess;
pub mod usage;
pub mod whisper;
pub mod youtube;
