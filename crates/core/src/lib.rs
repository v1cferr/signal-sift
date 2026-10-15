//! Domain types and configuration shared by every SignalSift crate.
//!
//! This crate stays free of async runtimes, databases and network clients so
//! that the rules it encodes (profiles, timestamps, URL canonicalization) can be
//! unit-tested in isolation and reused by any front end.

pub mod config;
pub mod profile;
pub mod timestamp;

pub use config::Config;
pub use profile::Profile;
