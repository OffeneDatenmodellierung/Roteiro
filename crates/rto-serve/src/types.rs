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
    pub messages: Vec<RequestMessage>,
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

/// One incoming chat turn, whose content may be a plain string or an array of
/// OpenAI content parts (text + `image_url`) for multimodal requests.
#[derive(Debug, Clone, Deserialize)]
pub struct RequestMessage {
    /// `system` | `user` | `assistant`.
    pub role: String,
    /// The turn's content. A missing or `null` value is read as empty text.
    #[serde(default)]
    pub content: Option<MessageContent>,
}

/// A message's content: a string, or an array of parts (OpenAI multimodal).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text content.
    Text(String),
    /// Multimodal content parts (text and/or images).
    Parts(Vec<ContentPart>),
}

/// One content part of a multimodal message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    /// A text span.
    #[serde(rename = "text")]
    Text {
        /// The text.
        #[serde(default)]
        text: String,
    },
    /// An image reference (only `data:` URIs are accepted — see [`decode_image_url`]).
    #[serde(rename = "image_url")]
    ImageUrl {
        /// The image URL wrapper.
        image_url: ImageUrlPart,
    },
}

/// The `image_url` object of an image content part.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageUrlPart {
    /// The image URL — a `data:<mime>;base64,<data>` URI.
    pub url: String,
}

/// One chat turn on the response side (assistant messages): always plain text.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageDto {
    /// `assistant`.
    pub role: String,
    /// The generated text.
    pub content: String,
}

/// Max base64 payload length for an image (~20 MiB once decoded) — a guard
/// against a request decoding an unbounded blob into memory.
const MAX_IMAGE_B64_LEN: usize = 28 * 1024 * 1024;

/// Max images per request — bounds total decode work/allocation so a flood of
/// small images cannot exhaust memory or CPU.
const MAX_IMAGES: usize = 8;

/// Decode an `image_url` into raw (still-encoded, e.g. PNG/JPEG) image bytes.
/// Only `data:image/*;base64,…` URIs are accepted — a local server does not fetch
/// remote URLs (avoids SSRF), the payload must be a base64 image, and it is size-
/// capped; the image decoder downstream handles the actual format.
///
/// # Errors
/// Returns a message if the URL is not a base64 `image/*` `data:` URI, is over
/// the size cap, or fails to decode.
fn decode_image_url(url: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let rest = url
        .strip_prefix("data:")
        .ok_or("only `data:` image URIs are supported (remote URLs are not fetched)")?;
    let (meta, data) = rest
        .split_once(',')
        .ok_or("malformed data URI (no comma)")?;
    // MIME types are case-insensitive; normalise before matching.
    let meta = meta.to_ascii_lowercase();
    if !meta.starts_with("image/") {
        return Err("`image_url` must be an `image/*` data URI".to_owned());
    }
    if !meta.ends_with(";base64") {
        return Err("only base64-encoded image data URIs are supported".to_owned());
    }
    let data = data.trim();
    if data.len() > MAX_IMAGE_B64_LEN {
        return Err("image is too large".to_owned());
    }
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("base64 decode: {e}"))
}

impl ChatCompletionRequest {
    /// Validate and normalise into an [`ChatRequest`] for the engine, extracting
    /// text (per message) and any images (across the whole request).
    ///
    /// # Errors
    /// Returns a human-readable message if there are no messages or an
    /// `image_url` cannot be decoded.
    pub fn into_engine_request(self) -> Result<ChatRequest, String> {
        if self.messages.is_empty() {
            return Err("`messages` must not be empty".to_owned());
        }
        // Images are placed at the last `user` turn (where the vision path inserts
        // the media markers), so images may only appear there — anywhere else the
        // ordering relative to the text would be ambiguous.
        let last_user = self.messages.iter().rposition(|m| m.role == "user");
        let mut messages = Vec::with_capacity(self.messages.len());
        let mut images: Vec<Vec<u8>> = Vec::new();
        for (i, m) in self.messages.into_iter().enumerate() {
            let is_last_user = Some(i) == last_user;
            let text = match m.content.unwrap_or(MessageContent::Text(String::new())) {
                MessageContent::Text(s) => s,
                MessageContent::Parts(parts) => {
                    let mut text = String::new();
                    for part in parts {
                        match part {
                            ContentPart::Text { text: t } => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(&t);
                            }
                            ContentPart::ImageUrl { image_url } => {
                                if !is_last_user {
                                    return Err(
                                        "images are only supported in the last user message"
                                            .to_owned(),
                                    );
                                }
                                if images.len() >= MAX_IMAGES {
                                    return Err(format!(
                                        "too many images (max {MAX_IMAGES} per request)"
                                    ));
                                }
                                images.push(decode_image_url(&image_url.url)?);
                            }
                        }
                    }
                    text
                }
            };
            messages.push(Message {
                role: m.role,
                content: text,
            });
        }
        Ok(ChatRequest {
            model: self.model,
            messages,
            images,
            // The `/v1` wire does not accept audio attachments yet; audio is an
            // internal ingestion path (`roteiro sync`), not a served endpoint.
            audio: Vec::new(),
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
