//! Feed collection: fetch (SSRF-safe, conditional, bounded), parse, and
//! canonicalize. Persistence and scheduling belong to the caller.

pub mod canonical;
pub mod feed;
pub mod fetch;
pub mod html;
pub mod schedule;

pub use fetch::{Fetched, Fetcher, Validators};
