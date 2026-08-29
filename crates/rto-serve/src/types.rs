//! OpenAI-compatible wire types for the `/v1` endpoints. Only the fields Roteiro
//! reads or emits are modelled as named fields; everything else a client sends
//! is captured by [`ChatCompletionRequest::extra`] and checked against the
//! declared boundary in [`crate::openai_params`], so a parameter that would
//! return a silently wrong answer is refused rather than dropped (#488).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::{ChatRequest, Message};
use crate::tools::ToolDef;

/// Default token budget when a request omits `max_tokens` — which the Ask panel
/// does (`crates/roteiro/src/assets/app.js`'s `submitAsk` sends neither `max_tokens` nor
/// `temperature`), so this is the budget every Ask question actually runs under.
///
/// **512 was chosen when a context was a fixed 4,096, and it is small for the
/// models this registry now serves.** A reasoning model is charged for its
/// `<think>` block out of this same budget before it writes a token of answer or
/// of tool call — Stage 35b measured `qwen3.8-27b` spending an entire
/// 1,200-token budget inside `<think>` and emitting no answer at all. 2,048
/// clears that with room for an answer after it; the longest answer measured
/// over the Ask questions was 359 tokens on that model.
///
/// **This is a raised ceiling, not a measured fix, and the difference matters.**
/// #489's live refusals were attributed to generations truncated at this cap.
/// Re-measured, they were not: across two models, two temperatures, two round
/// budgets and 512/1,024/2,048, every response came back with
/// `finish_reason: "stop"` and byte-identical content. The cap was never the
/// binding constraint — `server::MAX_TOOL_ROUNDS` was, and it gates this one,
/// because a loop that never reaches a final generation cannot truncate one.
/// The refusal this constant governs (`tools::Unfinished::CutAtTokenCap`) stays
/// reachable in principle and was not observed in practice.
///
/// **The cost is per request, and was measured rather than reasoned.**
/// `max_tokens` is an input to `rto_llama::window_for_request` (#496), so this
/// default sizes the context of every request that does not override it. On
/// `qwen3.8-27b` at 64 KiB/token (`rto-llama/tests/context_window.rs`), at a
/// realistic 8,775-token prompt the allocation moves from 742 MiB at 512 to
/// 838 MiB at 2,048 — **+96 MiB** against ~18 GiB of weights.
///
/// **And that cost is charged more than once.** Every tool round appends the
/// call the model generated — a generation bounded by this budget — to the next
/// prompt, so a request that spends its round budget pays this
/// `MAX_TOOL_ROUNDS + 1` times, not once. [`crate::budget`] is where that sum
/// lives and where the build fails if it outgrows what a request may allocate
/// (#556). The paragraph above already asked a reader to remember the coupling
/// while #555 raised two of the three, which is why the total is now a fact the
/// compiler checks rather than a note.
pub(crate) const DEFAULT_MAX_TOKENS: u32 = 2048;

/// Max entries in a client's `tools` array. OpenAI's own documented ceiling, so
/// a client written against their API cannot trip this without already having
/// tripped theirs.
const MAX_CLIENT_TOOLS: usize = 128;

/// Max total bytes of client tool names, descriptions and schemas — the payload
/// that reaches the prompt verbatim via `tool_system_prompt`.
///
/// **This bound exists because of the interaction with per-request context
/// sizing (#496), not because large prompts are untidy.** That change sizes the
/// context window to the prompt (`window_for_request`: `prompt_tokens +
/// max_tokens + headroom`, capped at the model's `n_ctx_train`). Together, an
/// unbounded `tools` array would let a *caller* choose Roteiro's allocation: on
/// `qwen3.8-27b` the trained window is 262,144 tokens and KV runs ~64 KiB/token,
/// so driving the prompt to the ceiling reserves ~16.4 GiB for one request.
/// Neither change has that reach alone — before #496 the window is a fixed
/// 4,096, and before this one Roteiro only ever sizes to prompts it built
/// itself.
///
/// 32 KiB is roughly 8k tokens, which keeps the tool surface's contribution to
/// `prompt_tokens` about 32x below that ceiling. Raising it re-opens the same
/// hole in proportion, so treat it as a security bound rather than a tuning
/// knob.
const MAX_CLIENT_TOOL_BYTES: usize = 32 * 1024;

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
    ///
    /// OpenAI spells this budget two ways and this is the deprecated one; the
    /// current spelling `max_completion_tokens` names the *same* number and
    /// lands in [`Self::extra`], from where
    /// [`crate::openai_params::generation_budget`] resolves the two into this
    /// one value. It is not a `#[serde(alias)]` on purpose — see that
    /// function for the evidence that ruled the alias out.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Server-sent-events streaming: when `true`, the response is a stream of
    /// `chat.completion.chunk` events terminated by `data: [DONE]`.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Tools the **client** will execute (OpenAI `tools`). When present, Roteiro's
    /// own graph tools are **suppressed** for the request (see
    /// [`crate::tools::chat_with_client_tools`]) and a call to one of these ends
    /// the completion with `finish_reason: "tool_calls"` — Roteiro returns the
    /// call and never runs it.
    #[serde(default)]
    pub tools: Option<Vec<ToolSpec>>,
    /// OpenAI `tool_choice`. **Accepted and parsed, then discarded** — forcing a
    /// named function is grammar-constrained sampling, which lands with the
    /// grammar work (#485 PR 2). It is carried as far as
    /// [`NormalisedChat::tool_choice`] so the "accepted, not forced" claim is
    /// checkable at the type boundary, and [`crate::server`] explicitly drops it
    /// from there; nothing reports it back to the client. Declared
    /// `accepted, not enforced` in [`crate::openai_params`] — which is published
    /// as `docs/SERVING.md`'s parameter table — rather than half-implemented.
    /// Declared is the whole difference between this and the silent drop #488
    /// was filed over.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// OpenAI `parallel_tool_calls`. **Accepted and parsed, then discarded** —
    /// [`crate::tools`] parses at most one call per turn today, so a turn never
    /// carries more than one regardless of this field. Carried and dropped
    /// exactly as [`Self::tool_choice`] is. Declared `accepted, not enforced`
    /// in [`crate::openai_params`] alongside it.
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// **Every request key the fields above do not name.**
    ///
    /// Before #488 these were discarded by serde without ever reaching Roteiro,
    /// which is how `seed`, `stop`, `response_format` and `n` came to be
    /// accepted and silently ignored: a caller set one, believed it had taken
    /// effect, and got a `200` that said so. Capturing them is what makes the
    /// declaration in [`crate::openai_params`] enforceable at all —
    /// [`Self::normalise`] hands this map to
    /// [`crate::openai_params::check_declared`], which refuses the parameters
    /// whose being ignored would contradict the response, and passes everything
    /// else through untouched.
    ///
    /// A [`BTreeMap`] rather than a [`serde_json::Map`] so that when a request
    /// carries several refused parameters the one named is the alphabetically
    /// first, not whichever the client's serialiser happened to emit first — an
    /// error message that varies by client is a bug report nobody can reproduce.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One entry of the client's `tools` array. OpenAI wraps every tool in a
/// `{"type": "function", "function": {...}}` envelope; `type` defaults to
/// `"function"` because that is the only kind and some clients omit it.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolSpec {
    /// The tool kind — only `"function"` is meaningful.
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    /// The function's name, description and JSON-Schema parameters.
    pub function: FunctionSpec,
}

/// The `function` object of a [`ToolSpec`].
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionSpec {
    /// The function name the model emits to call it.
    pub name: String,
    /// What it does and when to use it (advertised to the model verbatim).
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema (an `object`) describing the arguments.
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

/// The default `type` of a tool envelope.
fn function_kind() -> String {
    "function".to_owned()
}

/// One `tool_calls` entry, on both wires: inbound on an assistant turn a client
/// replays, outbound on a completion Roteiro returns without executing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallDto {
    /// Correlation id — a `role: "tool"` result names it as `tool_call_id`.
    pub id: String,
    /// Always `"function"`.
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    /// The called function and its arguments.
    pub function: FunctionCallDto,
}

/// The `function` object of a [`ToolCallDto`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCallDto {
    /// The function name.
    pub name: String,
    /// The arguments as a **JSON string** (OpenAI's shape — not an object).
    pub arguments: String,
}

/// One incoming chat turn, whose content may be a plain string or an array of
/// OpenAI content parts (text + `image_url`) for multimodal requests.
#[derive(Debug, Clone, Deserialize)]
pub struct RequestMessage {
    /// `system` | `user` | `assistant` | `tool`.
    pub role: String,
    /// The turn's content. A missing or `null` value is read as empty text —
    /// which is what an assistant turn carrying only `tool_calls` sends.
    #[serde(default)]
    pub content: Option<MessageContent>,
    /// On an assistant turn a client replays: the calls Roteiro returned to it.
    /// Rendered back into the in-band `<tool_call>` form so the model sees the
    /// turn it actually produced (see [`ChatCompletionRequest::normalise`]).
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDto>>,
    /// On a `role: "tool"` turn: which call this result answers.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// The tool's name on a `role: "tool"` turn (OpenAI's legacy field).
    #[serde(default)]
    pub name: Option<String>,
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

/// One chat turn on the response side (assistant messages).
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageDto {
    /// `assistant`.
    pub role: String,
    /// The generated text, or `null` on a turn that carries [`Self::tool_calls`]
    /// instead — OpenAI's shape. Deliberately **not** `skip_serializing_if`: a
    /// client distinguishes "no content" from "field absent", and an explicit
    /// `null` is what OpenAI sends.
    pub content: Option<String>,
    /// Calls the model made against the **client's** tools. Roteiro returns them
    /// and stops; it never executes a client's tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDto>>,
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

/// A `POST /v1/chat/completions` body normalised for the engine and the tool
/// loop: the engine request itself, plus the client-supplied tool surface that
/// [`ChatRequest`] has no room for.
#[derive(Debug, Clone)]
pub struct NormalisedChat {
    /// The validated request handed to the engine.
    pub request: ChatRequest,
    /// The client's tools, in request order. Non-empty means Roteiro's graph
    /// tools are suppressed for this request.
    pub client_tools: Vec<ToolDef>,
    /// The request's `tool_choice`, parsed but **not** acted on. It reaches this
    /// struct so the accepted-not-forced boundary is visible and testable in one
    /// place; [`crate::server`] discards it. Nothing echoes it to the client.
    /// See [`ChatCompletionRequest::tool_choice`].
    pub tool_choice: Option<serde_json::Value>,
    /// The request's `parallel_tool_calls`, parsed but **not** acted on, exactly
    /// as [`Self::tool_choice`] is. See
    /// [`ChatCompletionRequest::parallel_tool_calls`].
    pub parallel_tool_calls: Option<bool>,
}

/// Validate a client's `tools` array and convert it to the loop's [`ToolDef`]s.
///
/// Enforces both bounds ([`MAX_CLIENT_TOOLS`], [`MAX_CLIENT_TOOL_BYTES`]) and the
/// tool kind. Every failure is a rejection rather than a repair: an oversized
/// array is refused, not trimmed, and an unknown `type` is refused, not coerced.
///
/// # Errors
/// Returns a human-readable message when the array is over either bound or
/// carries a tool whose `type` is not `function`.
fn client_tools_from(specs: Vec<ToolSpec>) -> Result<Vec<ToolDef>, String> {
    if specs.len() > MAX_CLIENT_TOOLS {
        return Err(format!(
            "too many tools: {} (max {MAX_CLIENT_TOOLS})",
            specs.len()
        ));
    }
    let mut client_tools: Vec<ToolDef> = Vec::with_capacity(specs.len());
    let mut advertised_bytes: usize = 0;
    for t in specs {
        // Only `function` exists in OpenAI's tool envelope. Anything else is
        // rejected rather than coerced: silently advertising a `retrieval` tool
        // as a function would tell the client it was understood.
        if t.kind != "function" {
            return Err(format!(
                "unsupported tool type `{}` (only `function` is supported)",
                t.kind
            ));
        }
        let name = t.function.name;
        let description = t.function.description.unwrap_or_default();
        // A tool with no schema still has to advertise an argument shape; an
        // empty object is the honest "takes no known arguments".
        let parameters = t
            .function
            .parameters
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        advertised_bytes = advertised_bytes
            .saturating_add(name.len())
            .saturating_add(description.len())
            .saturating_add(serde_json::to_string(&parameters).map_or(0, |p| p.len()));
        // Refused, never truncated: trimming a client's schemas would leave the
        // model calling tools whose arguments no longer match what the client
        // will execute — corrupting exactly the correlation the `tool_call_id`
        // handling elsewhere is careful to preserve.
        if advertised_bytes > MAX_CLIENT_TOOL_BYTES {
            return Err(format!(
                "`tools` is too large: over {MAX_CLIENT_TOOL_BYTES} bytes of tool \
                 names, descriptions and schemas"
            ));
        }
        client_tools.push(ToolDef {
            name,
            description,
            parameters,
        });
    }
    Ok(client_tools)
}

/// Render one replayed [`ToolCallDto`] back into the in-band `<tool_call>` form
/// the model emitted, so a multi-turn transcript reads to the model exactly as
/// it wrote it. `arguments` is a JSON *string* on the wire; it is re-parsed so
/// the rendered call is an object, falling back to the raw string when the
/// client sent something that is not JSON.
fn render_tool_call(call: &ToolCallDto) -> String {
    let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| serde_json::Value::String(call.function.arguments.clone()));
    // Built field-by-field rather than through `json!`, whose object is a sorted
    // map: `name` must come first, because that is the order the tool system
    // prompt shows the model and this turn is the model's own prior output.
    let name = serde_json::Value::String(call.function.name.clone());
    format!("<tool_call>{{\"name\":{name},\"arguments\":{arguments}}}</tool_call>")
}

impl ChatCompletionRequest {
    /// Validate and normalise into a [`NormalisedChat`]: an engine [`ChatRequest`]
    /// (text per message, images across the whole request) plus the client's tool
    /// surface.
    ///
    /// Two role mappings make the OpenAI tool protocol legible to the in-band
    /// `<tool_call>` convention the served models actually speak:
    ///
    /// - **`role: "tool"` becomes a `user` turn** carrying `<tool_response>`.
    ///   This is not a portability workaround — passing `tool` through would emit
    ///   a role token the models were never trained on (`apply_chat_template`
    ///   renders unknown roles literally), whereas a `<tool_response>` user turn
    ///   is what every Qwen template emits natively for a tool result.
    /// - **An assistant turn's `tool_calls` are rendered back** into
    ///   `<tool_call>…</tool_call>`, so the call the model made is present in the
    ///   transcript the client replays rather than silently dropped.
    ///
    /// A client's tool result is **not** truncated on the way in:
    /// [`crate::tools`]'s `MAX_TOOL_RESULT` caps the results of tools Roteiro
    /// *executes*, where the size is Roteiro's to control. A client's result is
    /// its own context budget to spend, and trimming it silently would corrupt
    /// the transcript it is correlating `tool_call_id`s against.
    ///
    /// # Errors
    /// Returns a human-readable message if there are no messages or an
    /// `image_url` cannot be decoded.
    pub fn normalise(self) -> Result<NormalisedChat, String> {
        if self.messages.is_empty() {
            return Err("`messages` must not be empty".to_owned());
        }
        // Before the request is normalised into something the engine will
        // cheerfully answer: a parameter whose being ignored would make that
        // answer contradict the request is refused here rather than dropped.
        // See `crate::openai_params` for the boundary, and for why a key the
        // table has never heard of is still allowed through.
        crate::openai_params::check_declared(&self.extra)?;
        // The generation budget has two OpenAI spellings for one number:
        // `max_tokens` is the field above, and `max_completion_tokens` — the
        // spelling OpenAI now prefers — arrives in `extra`. Resolved after
        // `check_declared` so the declared boundary keeps first refusal: a
        // request that is refused outright has no budget worth resolving.
        let max_tokens = crate::openai_params::generation_budget(self.max_tokens, &self.extra)?;
        let client_tools = client_tools_from(self.tools.unwrap_or_default())?;
        let (tool_choice, parallel_tool_calls) = (self.tool_choice, self.parallel_tool_calls);
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
            // `tool` results and replayed `tool_calls` are translated into the
            // in-band protocol; see this method's documentation for why.
            let (role, content) = if m.role == "tool" {
                (
                    "user".to_owned(),
                    format!("<tool_response>{text}</tool_response>"),
                )
            } else if let Some(calls) = m.tool_calls.filter(|c| !c.is_empty()) {
                let rendered = calls.iter().map(render_tool_call).collect::<Vec<_>>();
                let rendered = rendered.join("\n");
                let content = if text.is_empty() {
                    rendered
                } else {
                    format!("{text}\n{rendered}")
                };
                (m.role, content)
            } else {
                (m.role, text)
            };
            messages.push(Message { role, content });
        }
        Ok(NormalisedChat {
            request: ChatRequest {
                tools: None,
                model: self.model,
                messages,
                images,
                // The `/v1` wire does not accept audio attachments yet; audio is an
                // internal ingestion path (`roteiro sync`), not a served endpoint.
                audio: Vec::new(),
                temperature: self.temperature.unwrap_or(0.0).max(0.0),
                max_tokens: max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1),
            },
            client_tools,
            tool_choice,
            parallel_tool_calls,
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
    /// `stop` | `length` | `tool_calls`.
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
    /// Client tool calls. **Divergence:** every call arrives complete in a single
    /// chunk, where OpenAI fragments `arguments` across several. The `index` is
    /// the call's position and is not part of the divergence — a client
    /// accumulating by `index` gets a correct result either way, which is why
    /// one-shot is legal and works in mainstream clients. Declared in the crate
    /// README.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// One streamed `tool_calls` entry — a [`ToolCallDto`] plus the per-call `index`
/// OpenAI's streaming shape requires. `arguments` is always complete rather than
/// fragmented (see [`Delta::tool_calls`]); `index` is the call's position in the
/// turn, which is `0` for every call Roteiro emits today only because
/// `parse_tool_calls` yields at most one.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDelta {
    /// The call's position in the turn's `tool_calls` array — assigned from the
    /// call's actual position, not pinned to a constant.
    pub index: u32,
    /// Correlation id.
    pub id: String,
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The called function and its (complete) arguments.
    pub function: FunctionCallDto,
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

#[cfg(test)]
mod tests {
    use super::ChatCompletionRequest;

    fn parse(body: serde_json::Value) -> ChatCompletionRequest {
        serde_json::from_value(body).expect("a valid request")
    }

    #[test]
    fn client_tools_are_parsed_including_an_omitted_type() {
        // OpenAI wraps every tool in `{"type": "function", "function": {...}}`,
        // but clients omit `type` often enough that defaulting it is worth more
        // than rejecting them.
        let req = parse(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"type": "function", "function": {
                    "name": "get_weather",
                    "description": "current weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
                }},
                {"function": {"name": "no_type"}},
            ],
        }));
        let normalised = req.normalise().expect("normalised");
        let names: Vec<&str> = normalised
            .client_tools
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["get_weather", "no_type"]);
        assert_eq!(normalised.client_tools[0].description, "current weather");
        assert_eq!(
            normalised.client_tools[0].parameters["properties"]["city"]["type"],
            "string"
        );
        // A tool with no schema still advertises an argument shape.
        assert_eq!(
            normalised.client_tools[1].parameters,
            serde_json::json!({"type": "object"})
        );
    }

    fn with_tools(tools: &serde_json::Value) -> Result<super::NormalisedChat, String> {
        parse(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": tools.clone(),
        }))
        .normalise()
    }

    fn tool(name: &str, description: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {"name": name, "description": description},
        })
    }

    #[test]
    fn a_tool_type_other_than_function_is_rejected_not_coerced() {
        // `type` defaults to `function` when omitted, but an explicit unknown
        // kind is refused: advertising a `retrieval` tool to the model as a
        // function would tell the client it was understood.
        let err = with_tools(&serde_json::json!([{
            "type": "retrieval",
            "function": {"name": "lookup"},
        }]))
        .expect_err("must be rejected");
        assert!(err.contains("retrieval"), "{err}");
        assert!(err.contains("function"), "names what IS supported: {err}");
    }

    #[test]
    fn too_many_tools_is_rejected() {
        let many: Vec<serde_json::Value> = (0..=super::MAX_CLIENT_TOOLS)
            .map(|i| tool(&format!("t{i}"), ""))
            .collect();
        let err = with_tools(&serde_json::Value::Array(many)).expect_err("must be rejected");
        assert!(err.contains("too many tools"), "{err}");

        // The limit itself is fine — the bound rejects only past it.
        let at_limit: Vec<serde_json::Value> = (0..super::MAX_CLIENT_TOOLS)
            .map(|i| tool(&format!("t{i}"), ""))
            .collect();
        assert!(with_tools(&serde_json::Value::Array(at_limit)).is_ok());
    }

    #[test]
    fn an_oversized_tools_array_is_rejected_rather_than_truncated() {
        // A caller must not be able to choose Roteiro's context allocation by
        // sending arbitrarily large schemas — see `MAX_CLIENT_TOOL_BYTES` for why
        // that matters once the context window sizes to the prompt (#496).
        //
        // The refusal is the point: truncating would leave the model calling
        // tools whose arguments no longer match what the client will run.
        let huge = "x".repeat(super::MAX_CLIENT_TOOL_BYTES + 1);
        let err =
            with_tools(&serde_json::json!([tool("big", &huge)])).expect_err("must be rejected");
        assert!(err.contains("too large"), "{err}");

        // Spread across many tools, the *total* is what is bounded — not each one.
        let chunk = "y".repeat(super::MAX_CLIENT_TOOL_BYTES / 4);
        let spread: Vec<serde_json::Value> =
            (0..5).map(|i| tool(&format!("t{i}"), &chunk)).collect();
        assert!(
            with_tools(&serde_json::Value::Array(spread))
                .expect_err("the total is bounded, not the per-tool size")
                .contains("too large")
        );
    }

    #[test]
    fn tool_choice_and_parallel_tool_calls_are_carried_unenforced() {
        // Both are accepted and carried so the server *can* report what it
        // received; neither is enforced (see the crate README's divergence
        // table). Carrying them is what makes "accepted" a checkable claim
        // rather than "silently dropped by serde", which is the #488 defect.
        let req = parse(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
            "parallel_tool_calls": true,
        }));
        let normalised = req.normalise().expect("normalised");
        assert_eq!(
            normalised
                .tool_choice
                .as_ref()
                .and_then(|c| c["function"]["name"].as_str()),
            Some("get_weather")
        );
        assert_eq!(normalised.parallel_tool_calls, Some(true));
    }

    #[test]
    fn a_tool_turn_and_a_replayed_call_become_the_in_band_protocol() {
        // The wire→prompt half of the round trip, at the type boundary: a
        // `role: "tool"` turn is a `<tool_response>` USER turn (never a `tool`
        // role — the models were not trained on that token), and the assistant's
        // own `tool_calls` are rendered back into `<tool_call>` markup.
        let req = parse(serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_0",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Berlin\"}"},
                }]},
                {"role": "tool", "tool_call_id": "call_0", "content": "{\"temp\":21}"},
            ],
        }));
        let turns = req.normalise().expect("normalised").request.messages;
        assert!(turns.iter().all(|m| m.role != "tool"), "{turns:?}");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(
            turns[1].content,
            r#"<tool_call>{"name":"get_weather","arguments":{"city":"Berlin"}}</tool_call>"#
        );
        assert_eq!(turns[2].role, "user");
        assert_eq!(
            turns[2].content,
            r#"<tool_response>{"temp":21}</tool_response>"#
        );
    }

    fn budget_of(body: &serde_json::Value) -> Result<u32, String> {
        serde_json::from_value::<super::ChatCompletionRequest>(body.clone())
            .expect("the body parses; the budget is resolved after deserialisation, not during it")
            .normalise()
            .map(|n| n.request.max_tokens)
    }

    fn with_budget(extra: &serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let map = body.as_object_mut().expect("object");
        for (k, v) in extra.as_object().expect("object") {
            map.insert(k.clone(), v.clone());
        }
        body
    }

    /// The end of the wire: OpenAI's current spelling arrives as the engine's
    /// budget, which is the whole of what "supported" claims for it.
    ///
    /// Asserted on [`super::ChatRequest::max_tokens`] rather than on the
    /// resolver in isolation, because the resolver returning the right number
    /// and `normalise` then dropping it on the floor would be the same silent
    /// wrong answer wearing a passing unit test.
    #[test]
    fn the_current_spelling_of_the_budget_reaches_the_engine() {
        assert_eq!(
            budget_of(&with_budget(
                &serde_json::json!({"max_completion_tokens": 321})
            )),
            Ok(321)
        );
        assert_eq!(
            budget_of(&with_budget(&serde_json::json!({"max_tokens": 321}))),
            Ok(321)
        );
        assert_eq!(
            budget_of(&with_budget(
                &serde_json::json!({"max_tokens": 321, "max_completion_tokens": 321})
            )),
            Ok(321)
        );
    }

    /// Two names, two numbers: a `400` from `normalise` — which the handler
    /// renders as the same `{"error": …}` envelope as every other refusal —
    /// rather than serde's `duplicate field` dying inside the extractor as a
    /// `422` of plain text, which is what a `#[serde(alias)]` would have given.
    #[test]
    fn two_different_budgets_are_a_refusal_and_not_a_silent_pick() {
        let err = budget_of(&with_budget(
            &serde_json::json!({"max_tokens": 10, "max_completion_tokens": 20}),
        ))
        .expect_err("two budgets is ambiguous");
        assert!(err.contains("`max_completion_tokens`"), "{err}");
        assert!(err.contains("`max_tokens`"), "{err}");
    }

    /// Neither name is still the default budget the Ask panel runs under.
    #[test]
    fn neither_budget_name_leaves_the_default_in_place() {
        assert_eq!(
            budget_of(&with_budget(&serde_json::json!({}))),
            Ok(super::DEFAULT_MAX_TOKENS)
        );
    }
}
