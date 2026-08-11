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

/// Token accounting and stop reason for a completion (the non-text result of
/// generation — the text is either accumulated into a [`Completion`] or streamed
/// token-by-token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionStats {
    /// Tokens in the (templated) prompt.
    pub prompt_tokens: u32,
    /// Tokens generated in the completion.
    pub completion_tokens: u32,
    /// Why generation stopped: `stop` (natural end) or `length` (hit the cap).
    pub finish_reason: FinishReason,
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
    /// Images attached to the request (decoded, still-encoded PNG/JPEG bytes),
    /// in order — non-empty only for multimodal requests to a vision model.
    pub images: Vec<Vec<u8>>,
    /// Audio clips attached to the request (raw encoded WAV/MP3/FLAC bytes), in
    /// order — non-empty only for requests to an audio-capable model. The
    /// projector (`mmproj`) decodes and resamples them via miniaudio, so the
    /// caller passes the original file bytes, not PCM. Mutually exclusive with
    /// [`Self::images`]: a request that sets both is rejected as an invalid
    /// request (a projector splices one media stream).
    pub audio: Vec<Vec<u8>>,
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
    /// The request is malformed for the chosen model (e.g. images sent to a
    /// text-only model) — a client error (400), not an internal failure.
    #[error("{0}")]
    InvalidRequest(String),
    /// The operation is unsupported by the active engine (e.g. embeddings on
    /// a chat-only engine) — a 501, not an internal failure.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// Loading a model or running inference failed.
    #[error("inference failed: {0}")]
    Inference(String),
}

/// An inference backend the `/v1` server can drive. Implementors serialise their
/// own concurrency as needed; the server may call these from multiple request
/// tasks.
pub trait Engine: Send + Sync + 'static {
    /// The models this engine serves (installed only; never downloads).
    fn models(&self) -> Vec<ModelInfo>;

    /// Generate a completion, invoking `on_token` with each decoded text piece
    /// as it is produced (for streaming), and returning the final token
    /// accounting. Blocks until generation finishes.
    ///
    /// # Errors
    /// [`EngineError::UnknownModel`] if `req.model` is not served;
    /// [`EngineError::Inference`] if model load or decoding fails.
    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError>;

    /// Run a chat completion to completion, accumulating the streamed text into a
    /// single [`Completion`]. The non-streaming path is this default.
    ///
    /// # Errors
    /// As [`Engine::chat_stream`].
    fn chat(&self, req: &ChatRequest) -> Result<Completion, EngineError> {
        let mut content = String::new();
        let stats = self.chat_stream(req, &mut |piece| content.push_str(piece))?;
        Ok(Completion {
            content,
            prompt_tokens: stats.prompt_tokens,
            completion_tokens: stats.completion_tokens,
            finish_reason: stats.finish_reason,
        })
    }

    /// Produce one embedding vector per input string, using `model`. Defaults to
    /// unsupported; an embedding-capable engine overrides it.
    ///
    /// # Errors
    /// [`EngineError::UnknownModel`] if `model` is not served, or
    /// [`EngineError::Inference`] on failure (including engines with no embedding
    /// support).
    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
        let _ = (model, inputs);
        Err(EngineError::Unsupported(
            "this engine does not support embeddings".to_owned(),
        ))
    }
}
