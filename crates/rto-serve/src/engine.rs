//! The inference-engine abstraction the `/v1` server is written against.
//!
//! Keeping the HTTP layer behind a trait lets it be exercised with a mock in
//! tests without building llama.cpp, and lets the real [`crate::llama`] engine
//! land behind the `llama` feature (ADR-0006).

/// A model the engine can serve, as surfaced by `/v1/models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// The model's public id (its registry name, e.g. `qwen3-0.6b`).
    pub id: String,
}

/// The outcome of a chat completion: the assistant text plus token accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The generated assistant message content.
    pub content: String,
    /// Tokens in the (templated) prompt.
    pub prompt_tokens: u32,
    /// Tokens generated in the completion.
    pub completion_tokens: u32,
    /// Why generation stopped: `stop` (natural end) or `length` (hit the cap).
    pub finish_reason: FinishReason,
}

/// Why a completion ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// The model emitted an end-of-generation token.
    Stop,
    /// The token budget (`max_tokens`) was reached first.
    Length,
}

impl FinishReason {
    /// The OpenAI wire string for this reason.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
        }
    }
}

/// A single chat turn handed to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// `system` | `user` | `assistant`.
    pub role: String,
    /// The turn's text.
    pub content: String,
}

/// A validated chat request, normalised from the wire
/// [`crate::types::ChatCompletionRequest`].
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// The requested model id.
    pub model: String,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// Sampling temperature (`0.0` = greedy).
    pub temperature: f32,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

/// A failure while serving a request.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The requested model is not one this engine serves.
    #[error("model `{0}` is not served (see GET /v1/models)")]
    UnknownModel(String),
    /// Loading a model or running inference failed.
    #[error("inference failed: {0}")]
    Inference(String),
}

/// An inference backend the `/v1` server can drive. Implementors serialise their
/// own concurrency as needed; the server may call [`Engine::chat`] from multiple
/// request tasks.
pub trait Engine: Send + Sync + 'static {
    /// The models this engine serves (installed only; never downloads).
    fn models(&self) -> Vec<ModelInfo>;

    /// Run a chat completion, blocking until the full response is generated.
    ///
    /// # Errors
    /// [`EngineError::UnknownModel`] if `req.model` is not served;
    /// [`EngineError::Inference`] if model load or decoding fails.
    fn chat(&self, req: &ChatRequest) -> Result<Completion, EngineError>;
}
