//! OpenAI-compatible wire types for the `/v1` endpoints. Only the fields Roteiro
//! reads or emits are modelled; unknown request fields are ignored so standard
//! OpenAI clients work unchanged.

use serde::{Deserialize, Serialize};

use crate::engine::{ChatRequest, Message};

/// Default token budget when a request omits `max_tokens`.
const DEFAULT_MAX_TOKENS: u32 = 512;

/// A `POST /v1/chat/completions` request body.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    /// The model id to run (must be one of `/v1/models`).
    pub model: String,
    /// The conversation turns.
    pub messages: Vec<ChatMessageDto>,
    /// Sampling temperature; `0` (or omitted) is greedy.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Server-sent-events streaming. Not yet supported; a `true` value is
    /// rejected with a clear error (added in a later PR).
    #[serde(default)]
    pub stream: Option<bool>,
}

/// One chat turn on the wire.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessageDto {
    /// `system` | `user` | `assistant`.
    pub role: String,
    /// The turn's text. Defaults to empty (OpenAI allows null content).
    #[serde(default)]
    pub content: String,
}

impl ChatCompletionRequest {
    /// Validate and normalise into an [`ChatRequest`] for the engine.
    ///
    /// # Errors
    /// Returns a human-readable message if the request has no messages or asks
    /// for streaming (not yet supported).
    pub fn into_engine_request(self) -> Result<ChatRequest, String> {
        if self.messages.is_empty() {
            return Err("`messages` must not be empty".to_owned());
        }
        if self.stream == Some(true) {
            return Err("streaming (`stream: true`) is not yet supported".to_owned());
        }
        Ok(ChatRequest {
            model: self.model,
            messages: self
                .messages
                .into_iter()
                .map(|m| Message {
                    role: m.role,
                    content: m.content,
                })
                .collect(),
            temperature: self.temperature.unwrap_or(0.0).max(0.0),
            max_tokens: self.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1),
        })
    }
}

/// The `GET /v1/models` response.
#[derive(Debug, Clone, Serialize)]
pub struct ModelList {
    /// Always `"list"`.
    pub object: &'static str,
    /// One entry per served model.
    pub data: Vec<ModelObject>,
}

/// One model in [`ModelList`].
#[derive(Debug, Clone, Serialize)]
pub struct ModelObject {
    /// The model id.
    pub id: String,
    /// Always `"model"`.
    pub object: &'static str,
    /// Who owns the model — always `"roteiro"` for locally-served models.
    pub owned_by: &'static str,
}

/// A `POST /v1/chat/completions` response body.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    /// A unique id for this completion.
    pub id: String,
    /// Always `"chat.completion"`.
    pub object: &'static str,
    /// Unix seconds when the completion was created.
    pub created: u64,
    /// The model that produced it.
    pub model: String,
    /// The generated choices (always exactly one for Roteiro).
    pub choices: Vec<ChatChoice>,
    /// Token accounting.
    pub usage: Usage,
}

/// One generated choice.
#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    /// Choice index (always `0`).
    pub index: u32,
    /// The assistant message.
    pub message: ChatMessageDto,
    /// `stop` | `length`.
    pub finish_reason: &'static str,
}

/// Prompt/completion token counts.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Usage {
    /// Tokens in the templated prompt.
    pub prompt_tokens: u32,
    /// Tokens generated.
    pub completion_tokens: u32,
    /// Their sum.
    pub total_tokens: u32,
}

/// An OpenAI-shaped error body: `{ "error": { "message", "type" } }`.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    /// The error payload.
    pub error: ErrorBody,
}

/// The inner error object.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    /// Human-readable description.
    pub message: String,
    /// A coarse error class (e.g. `invalid_request_error`).
    pub r#type: &'static str,
}

impl ErrorResponse {
    /// Build an error body with the given message and type.
    #[must_use]
    pub fn new(message: impl Into<String>, r#type: &'static str) -> Self {
        Self {
            error: ErrorBody {
                message: message.into(),
                r#type,
            },
        }
    }
}
