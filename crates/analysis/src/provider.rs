//! The inference boundary. Domain code depends on [`InferenceProvider`], not on
//! llama.cpp, so the model and the backend can be replaced independently.

use std::future::Future;

use serde::Serialize;

pub struct ChatRequest {
    pub system: String,
    pub user: String,
    /// JSON schema the reply must satisfy; providers that support it enforce
    /// it with a grammar, the caller validates either way.
    pub schema: serde_json::Value,
    pub schema_name: &'static str,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub prompt_ms: f64,
    pub completion_ms: f64,
    pub wall_ms: u64,
}

pub struct ChatResponse {
    pub content: String,
    pub stats: ChatStats,
}

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("inference backend unavailable: {0}")]
    Unavailable(String),
    #[error("inference request failed: {0}")]
    Request(String),
}

pub trait InferenceProvider {
    /// Identifier stored with every analysis (model file and backend).
    fn model_id(&self) -> &str;
    fn chat(
        &self,
        req: &ChatRequest,
    ) -> impl Future<Output = Result<ChatResponse, InferenceError>> + Send;
}
