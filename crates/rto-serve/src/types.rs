//! OpenAI-compatible wire types for the `/v1` endpoints. Only the fields Roteiro
//! reads or emits are modelled; unknown request fields are ignored so standard
//! OpenAI clients work unchanged.

use serde::{Deserialize, Deserializer, Serialize};

use crate::engine::{ChatRequest, Message};

/// Deserialize a string field that OpenAI clients may send as `null` (an
/// assistant turn with a tool call carries `content: null`) — treat null and a
/// missing field alike as the empty string.
fn null_as_empty<'de, D>(de: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

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
    /// Server-sent-events streaming: when `true`, the response is a stream of
    /// `chat.completion.chunk` events terminated by `data: [DONE]`.
    #[serde(default)]
    pub stream: Option<bool>,
}

/// One chat turn on the wire.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessageDto {
    /// `system` | `user` | `assistant`.
    pub role: String,
    /// The turn's text. A missing or `null` value (OpenAI allows both) is read
    /// as the empty string.
    #[serde(default, deserialize_with = "null_as_empty")]
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

/// One `chat.completion.chunk` streamed over SSE when `stream: true`.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    /// The completion id (stable across the stream).
    pub id: String,
    /// Always `"chat.completion.chunk"`.
    pub object: &'static str,
    /// Unix seconds when the completion started.
    pub created: u64,
    /// The model producing the stream.
    pub model: String,
    /// The incremental choices (always exactly one for Roteiro).
    pub choices: Vec<ChunkChoice>,
}

/// One choice in a streamed [`ChatCompletionChunk`].
#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    /// Choice index (always `0`).
    pub index: u32,
    /// The incremental delta for this chunk.
    pub delta: Delta,
    /// Serialized as `null` on every chunk until the final one, which carries
    /// `stop` | `length` — matching OpenAI's streaming shape (intermediate
    /// chunks include an explicit `"finish_reason": null`).
    pub finish_reason: Option<&'static str>,
}

/// The incremental payload of a streamed chunk: a role on the first chunk, then
/// content pieces, then empty on the terminating chunk.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Delta {
    /// Present only on the first chunk (`"assistant"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    /// A piece of generated text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// A `POST /v1/embeddings` request. `input` accepts a single string or an array
/// of strings (both via [`EmbeddingInput`]).
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingRequest {
    /// The embedding model id (must be one of `/v1/models`).
    pub model: String,
    /// One or more texts to embed.
    pub input: EmbeddingInput,
}

/// The `input` field: OpenAI allows either a string or an array of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    /// A single text.
    One(String),
    /// A batch of texts.
    Many(Vec<String>),
}

impl EmbeddingInput {
    /// Normalise to a vector of input strings.
    #[must_use]
    pub fn into_vec(self) -> Vec<String> {
        match self {
            EmbeddingInput::One(s) => vec![s],
            EmbeddingInput::Many(v) => v,
        }
    }
}

/// The `POST /v1/embeddings` response.
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingResponse {
    /// Always `"list"`.
    pub object: &'static str,
    /// One entry per input, in request order.
    pub data: Vec<EmbeddingObject>,
    /// The model that produced the embeddings.
    pub model: String,
    /// Token accounting (`completion_tokens` is always 0 for embeddings).
    pub usage: Usage,
}

/// One embedding in an [`EmbeddingResponse`].
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingObject {
    /// Always `"embedding"`.
    pub object: &'static str,
    /// The embedding vector.
    pub embedding: Vec<f32>,
    /// The input's position in the request.
    pub index: usize,
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
