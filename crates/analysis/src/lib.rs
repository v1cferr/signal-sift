//! Local LLM analysis: summaries, topics and relevance per segment.
//!
//! [`provider::InferenceProvider`] is the only thing the domain logic needs
//! from a model backend; [`llama::LlamaServer`] implements it with a
//! llama.cpp server started on demand (ADR 0005).

pub mod analyze;
pub mod interests;
pub mod llama;
pub mod prompt;
pub mod provider;

pub use analyze::{Analyzer, item_relevance, split_text};
pub use interests::InterestProfile;
pub use prompt::PROMPT_VERSION;
