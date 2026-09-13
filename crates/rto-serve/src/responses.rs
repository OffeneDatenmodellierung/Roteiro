//! `POST /v1/responses` — OpenAI's **Responses** wire protocol, as an adapter
//! over the one that is already here (issue #809).
//!
//! ## Why this is an adapter and not a second server
//!
//! `rto-serve` has exactly one tool loop and, since #582/#583, one set of rules
//! about what a generation is allowed to publish. Both live *below* the wire:
//! [`crate::types::ChatCompletionRequest::normalise`] turns a chat body into an
//! [`engine::ChatRequest`](rto_llama::engine::ChatRequest), and
//! [`crate::tools::chat_with_client_tools`] returns a
//! [`ToolLoopOutcome`](crate::tools::ToolLoopOutcome). Everything a client can
//! observe about an *answer* is decided in between.
//!
//! So this module does not reimplement any of it. It translates a Responses
//! request into a [`ChatCompletionRequest`] and calls `normalise`, and it
//! renders a `ToolLoopOutcome` into Responses events. The translation is the
//! whole module; the answer is unchanged.
//!
//! **This buys the tool-call round trip for free, and that is the point.**
//! `normalise` flattens tool history into the in-band `<tool_call>` /
//! `<tool_response>` text the served models actually speak. Re-deriving that
//! rendering here would be a second copy of a byte-exact format — the failure
//! mode being designed out is a Responses conversation that degrades silently
//! at turn three because one copy drifted. Instead a `function_call` item
//! becomes an assistant [`RequestMessage`] carrying a [`ToolCallDto`] and a
//! `function_call_output` item becomes a `role: "tool"` turn, and `normalise`
//! renders both exactly as it renders a chat client's.
//!
//! ## What this surface is, and what it is not
//!
//! Streaming-only, unscoped, and one item type each for `message` and
//! `function_call`. Everything else in `input` is refused with a `400` that
//! **names it**, on the rule [`crate::openai_params`] already established for
//! the chat wire: a parameter or item whose being ignored would make the `200`
//! contradict the request is refused; bookkeeping that changes nothing
//! observable is dropped. [`RESPONSES_PARAMS`] is that declaration, published
//! verbatim in `docs/SERVING.md` and enforced by [`check_declared`].
//!
//! Project- and workspace-scoped routes (`/v1/{project}/responses`) are
//! deliberately **not** here. `ChatScope` is the seam they would reuse
//! (ADR-0008) and reusing it is cheap, but nothing yet asks for it.
//!
//! ## What the wire shapes actually are
//!
//! Read off the wire from `codex-cli 0.147.0` on 2026-09-13 rather than from a
//! schema, because two of the differences from Chat Completions are easy to get
//! wrong from prose:
//!
//! * A Responses tool is **flat** — `{"type":"function","name":…,"parameters":…}`
//!   — where a chat tool nests under a `function` key.
//! * `input` items carry their own `type`, and a message's content is an array
//!   of `{"type":"input_text","text":…}` parts rather than a string (though a
//!   bare string is legal and accepted).
//!
//! The event sequence is likewise verified rather than assumed; see
//! [`ResponseWriter`].

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::FinishReason;
use crate::openai_params::{Forward, Mention, Param, Support};
use crate::types::{
    ChatCompletionRequest, FunctionCallDto, FunctionSpec, Limits, MessageContent, NormalisedChat,
    RequestMessage, ToolCallDto, ToolSpec,
};

/// A `POST /v1/responses` request body.
///
/// The fields named here are the ones this surface reads; every other key lands
/// in [`Self::extra`] and is judged by [`check_declared`], exactly as
/// [`ChatCompletionRequest::extra`] is judged by
/// [`crate::openai_params::check_declared`].
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    /// The model id to run (must be one of `/v1/models`).
    pub model: String,
    /// The conversation, as a bare string or an array of typed items.
    #[serde(default)]
    pub input: Option<ResponsesInput>,
    /// The system prompt, which Responses hoists out of the conversation into
    /// its own field. Mapped to a leading `system` turn.
    #[serde(default)]
    pub instructions: Option<String>,
    /// SSE streaming. Only `true` is served — see [`STREAM_ONLY`].
    #[serde(default)]
    pub stream: Option<bool>,
    /// The client's tools, in the Responses **flat** envelope. Read as raw JSON
    /// so that a hosted (non-`function`) tool can be named in a message rather
    /// than dying inside serde.
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    /// Responses' name for the generation budget.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature; `0` (or omitted) is greedy.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Accepted and parsed, then discarded — exactly as on the chat wire.
    #[serde(default)]
    pub tool_choice: Option<Value>,
    /// Accepted and parsed, then discarded — exactly as on the chat wire.
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// Every request key the fields above do not name. See
    /// [`ChatCompletionRequest::extra`] for why this is a [`BTreeMap`]: a
    /// request carrying several refused parameters must name the same one
    /// whichever order its client serialised them in.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// `input`, which Responses lets a client send either way.
///
/// Deliberately not `#[non_exhaustive]`: the set is closed by the wire rather
/// than by this crate. OpenAI's `input` is a string **or** an array, and a third
/// form would be a new protocol rather than a new variant here — so a caller
/// matching both arms exhaustively is matching something that cannot grow. The
/// growth this surface really will see is in the *items* of the array, and those
/// are read as untyped JSON precisely so that an unrecognised one is a refusal
/// naming it rather than a type change.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    /// A single user turn as a bare string.
    Text(String),
    /// The typed item array.
    Items(Vec<Value>),
}

/// The refusal for `stream: false`.
///
/// Stated as a constant because it is the one divergence a Responses client is
/// most likely to meet, and `docs/SERVING.md` quotes it verbatim.
pub const STREAM_ONLY: &str = "`stream` must be `true`: this endpoint serves the Responses API as a typed SSE event stream only, so a non-streaming request would have no body shape to return. Send `stream: true` and read `response.completed`, whose `response.output` carries exactly what a non-streaming body would have.";

/// Every parameter of OpenAI's `POST /v1/responses` request body this endpoint
/// has an answer for, and what it does with each.
///
/// Sorted by name, and carrying the same [`Param`] rows the chat table uses, so
/// the two declarations read alike and share one refusal shape.
///
/// **Read from the Responses request schema on 2026-09-13**, and cross-checked
/// against the top level of a captured `codex-cli` 0.147.0 request. Provenance
/// is recorded because a key in no row here passes through untouched — the same
/// rule the chat wire has, and the right one (see `docs/SERVING.md` on why
/// `deny_unknown_fields` is the wrong instrument) — which means **a stateful
/// parameter this table has not heard of is a silent wrong answer, not a
/// harmless unknown**. That is not hypothetical: `conversation` was missing
/// from the first version of this table and review caught it, so a request
/// naming a server-side conversation would have been answered from the turns
/// in `input` alone. When OpenAI adds a parameter that moves state or output
/// off the request, it belongs here.
///
/// It stays deliberately **shorter** than
/// [`crate::openai_params::OPENAI_CHAT_PARAMS`], which is exhaustive over a
/// frozen wire: this one carries the parameters a Responses client sends plus
/// every one whose absence would mislead.
pub const RESPONSES_PARAMS: &[Param] = &[
    Param {
        name: "background",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "there is no job store behind this endpoint, so a background response would be started and then never be retrievable by the id you were given",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("stream")],
                prose: "Send `stream: true` and read the events as they arrive; a loopback server has nothing to gain by deferring the work.",
            },
        },
        inert: &["false"],
    },
    Param {
        name: "conversation",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "a conversation object lives on the server that issued it and there is no such object here, so the turns it names are not in this request and the model would answer having never seen them",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("input")],
                prose: "Send the whole conversation in `input` each turn, which is what a stateless endpoint needs.",
            },
        },
        inert: &[],
    },
    Param {
        name: "include",
        served_by: None,
        note: "asks for optional output fields — encrypted reasoning, log probabilities — that this endpoint never produces; the items simply do not appear",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "input",
        served_by: None,
        note: "the conversation, including `function_call` and `function_call_output` items",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "instructions",
        served_by: None,
        note: "the system prompt; mapped to a leading `system` turn",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "max_output_tokens",
        served_by: None,
        note: "the generation budget, and the input that sizes the context window for the request",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "max_tool_calls",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the tool loop's own round budget is what bounds it, so a lower cap would not be applied and the model could call tools more times than you allowed",
            forward: Forward::Nothing(
                "There is no per-request tool-call cap on this endpoint; the server-side budget is fixed at build time.",
            ),
        },
        inert: &[],
    },
    Param {
        name: "metadata",
        served_by: None,
        note: "free-form labels for OpenAI's dashboard; never read, never echoed",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "model",
        served_by: None,
        note: "the model id to run; must be one of `/v1/models`",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "parallel_tool_calls",
        served_by: None,
        note: "at most one call is parsed per turn today, so a turn never carries more than one regardless",
        support: Support::AcceptedNotEnforced,
        inert: &[],
    },
    Param {
        name: "previous_response_id",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "nothing is stored here, so the turns that id names are not on the server and the model would answer having never seen them",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("input")],
                prose: "Send the whole conversation in `input` each turn — every prior `message`, `function_call` and `function_call_output` — which is what a stateless endpoint needs.",
            },
        },
        inert: &[],
    },
    Param {
        name: "prompt",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "a stored prompt template lives in OpenAI's dashboard and cannot be resolved from here, so the instructions you believe were applied were not",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("instructions")],
                prose: "Send the template's resolved text as `instructions`.",
            },
        },
        inert: &[],
    },
    Param {
        name: "prompt_cache_key",
        served_by: None,
        note: "a cache-bucketing hint for OpenAI's prompt cache; nothing about the response depends on it",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "prompt_cache_retention",
        served_by: None,
        note: "as `prompt_cache_key`",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "reasoning",
        served_by: None,
        note: "a model's `<think>` block is stripped on every Roteiro surface and no `reasoning` item is ever emitted, so neither the effort nor the summary setting has anything to act on — see the divergence table above",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "safety_identifier",
        served_by: None,
        note: "an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "service_tier",
        served_by: None,
        note: "selects OpenAI's processing tier for latency and billing; there is one tier here and the output is unaffected",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "store",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "Roteiro retains nothing and sends nothing anywhere, so the response you asked to keep is gone the moment the stream ends and the id you were given addresses nothing",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("input")],
                prose: "Send `store: false` and keep the turns yourself, replaying them in `input`; that is the only conversation state this endpoint has.",
            },
        },
        // `false` is both OpenAI's default and what this endpoint does, so a
        // client library sending it has asked for exactly what it gets. Only
        // `true` is a decision that would not be honoured — and the one real
        // client measured here sends `false`.
        inert: &["false"],
    },
    Param {
        name: "stream",
        served_by: None,
        note: "the typed SSE event sequence; **`true` is the only value served** and `false` is a `400` — see the divergence table above",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "stream_options",
        served_by: None,
        note: "its one field, `include_obfuscation`, pads events against traffic analysis on a network this endpoint does not cross — it is bound to loopback, so the padding would defend nothing and its absence changes no field you can read",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "temperature",
        served_by: None,
        note: "the one sampling control this endpoint honours; `0` (or omitted) is greedy",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "text",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "there is no grammar-constrained sampling on this endpoint, so a `json_schema` format would return prose that need not parse and a verbosity setting would not change the length",
            forward: Forward::Nothing(
                "Ask for the shape and the length you want in the prompt, and parse defensively.",
            ),
        },
        inert: &[],
    },
    Param {
        name: "tool_choice",
        served_by: None,
        note: "forcing a named function is grammar-constrained sampling, which lands with the grammar work; half-implementing it would tell a client it was honoured",
        support: Support::AcceptedNotEnforced,
        inert: &[],
    },
    Param {
        name: "tools",
        served_by: None,
        note: "`function` tools are advertised to the model and their calls returned, never run — bounded at 128 entries / 32 KiB. A hosted tool (`web_search`, `code_interpreter`, …) is dropped rather than refused; see the divergence table above",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "top_logprobs",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no log probabilities are computed, so no alternatives come back at any position",
            forward: Forward::Nothing(
                "This endpoint returns no log probabilities, and there is no flag that turns them on.",
            ),
        },
        inert: &[],
    },
    Param {
        name: "top_p",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "nucleus sampling is not wired to the sampler, so the sampling you configured is not the sampling that ran",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("temperature")],
                prose: "`temperature` is the one sampling control this endpoint honours.",
            },
        },
        inert: &["1"],
    },
    Param {
        name: "truncation",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "`auto` asks the server to drop turns from the middle of the conversation so that an over-long input still answers, and nothing here does that — an input past the context window is refused, which is the exact failure `auto` was set to avoid",
            forward: Forward::Do {
                mentions: &[Mention::Parameter("input")],
                prose: "Trim the conversation on your side and send the shortened `input`; only the caller knows which turns it can afford to lose.",
            },
        },
        inert: &["\"disabled\""],
    },
    Param {
        name: "user",
        served_by: None,
        note: "an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way",
        support: Support::Dropped,
        inert: &[],
    },
];

/// Look one Responses parameter up by wire name.
#[must_use]
pub fn param(name: &str) -> Option<&'static Param> {
    RESPONSES_PARAMS.iter().find(|p| p.name == name)
}

/// The whole published Responses parameter table, header and all.
///
/// `docs/SERVING.md` carries this verbatim, and
/// `tests::the_published_responses_table_is_this_table` compares the two byte
/// for byte — the same arrangement the chat table has had since #488, for the
/// same reason.
#[must_use]
pub fn published_table() -> String {
    let mut out = String::from("| parameter | status | what happens |\n|---|---|---|\n");
    for p in RESPONSES_PARAMS {
        out.push_str(&p.published_row());
        out.push('\n');
    }
    out
}

/// Enforce [`RESPONSES_PARAMS`] against the keys [`ResponsesRequest`] does not
/// name.
///
/// The chat twin of this function, and deliberately so — same rule, same
/// message shape, different table. See
/// [`crate::openai_params::check_declared`].
///
/// # Errors
/// The refusal message for the first refused parameter, in name order.
pub fn check_declared(extra: &BTreeMap<String, Value>) -> Result<(), String> {
    for (name, value) in extra {
        let Some(p) = param(name) else { continue };
        if crate::openai_params::is_inert(p, value) {
            continue;
        }
        if let Some(refusal) = p.refusal() {
            return Err(refusal);
        }
    }
    Ok(())
}

/// Read one `input` item's `type`, defaulting to `"message"` when it is
/// **absent**.
///
/// Responses' `EasyInputMessage` may omit `type` entirely, so an item with a
/// `role` and no `type` is a message rather than a malformed item.
///
/// A `type` that is *present* and not a string is a different thing and is
/// refused. Defaulting it would let `{"type": 123, "role": "user"}` be answered
/// as an ordinary message while every unsupported item type is documented as a
/// `400` — a malformed item admitted by the same fallback that exists for a
/// well-formed one that simply left the key out.
///
/// # Errors
/// A message naming the non-string value.
fn item_type(item: &Value) -> Result<&str, String> {
    match item.get("type") {
        None => Ok("message"),
        Some(Value::String(kind)) => Ok(kind),
        Some(other) => Err(format!(
            "an `input` item's `type` must be a string, and this one is `{other}`. \
             Omit `type` for an ordinary message, or send one of `message`, \
             `function_call` or `function_call_output`."
        )),
    }
}

/// Pull a required string field off an item, naming the item type in the
/// refusal so the caller knows which of its items is wrong.
///
/// The second sentence is per-field rather than one sentence for all of them.
/// `docs/REVIEW_CHECKLIST.md` asks a refusal to say why, and "it is what
/// correlates the call with its result" is true of `call_id` and false of
/// `name`, `arguments` and `role` — a reason that is wrong for three fields out
/// of four is worse than none, because a caller who reads it looks in the wrong
/// place.
fn required_str<'a>(item: &'a Value, field: &str, kind: &str) -> Result<&'a str, String> {
    item.get(field).and_then(Value::as_str).ok_or_else(|| {
        let why = match field {
            "call_id" => "it is what correlates the call with its result",
            "name" => "it is the tool the model asked for, and nothing else in the item names one",
            "arguments" => "it is the call's arguments, as a JSON string — an object is not accepted here, on either wire",
            "role" => "it is who is speaking, and the prompt cannot be assembled without it",
            _ => "the item cannot be read without it",
        };
        format!("an `input` item of type `{kind}` must carry a string `{field}`: {why}.")
    })
}

/// Read a message item's content into plain text.
///
/// Accepts a bare string or an array of text parts. A non-text part is refused
/// by name rather than skipped: an image or a file a client believed was read
/// would otherwise produce a confident answer about content the model never
/// saw.
fn message_text(content: Option<&Value>) -> Result<String, String> {
    // Absent or `null` is refused rather than read as an empty turn. A message
    // item's `content` is required on this wire, so a missing one is malformed
    // input — and answering it would delete a turn from the conversation the
    // model is reasoning over while still returning a `200`. An *explicitly*
    // empty string or array is a different thing: the caller said "no text",
    // and that is served.
    let Some(content @ (Value::String(_) | Value::Array(_))) = content else {
        return Err(
            "a `message` item must carry `content`: a string, or an array of \
             `input_text` / `output_text` parts. An absent or null `content` \
             would silently drop this turn from the conversation the model \
             answers from; send an empty string if the turn really is empty."
                .to_owned(),
        );
    };
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "input_text" | "output_text") {
                    return Err(format!(
                        "a message content part of type `{kind}` is not supported on this \
                         endpoint: only `input_text` and `output_text` parts are read, so \
                         anything else would be silently absent from what the model saw. \
                         Send the part's information as text."
                    ));
                }
                // Refused rather than read as empty. A part whose `text` is
                // missing or is not a string is part of the caller's prompt or
                // tool result, and defaulting it to `""` would delete exactly
                // that much of the conversation while still answering — the
                // silent-drop failure this whole surface refuses by name.
                let piece = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                    format!(
                        "a `{kind}` content part must carry a string `text`, and this one \
                         does not. Send the part's text as a string; reading it as empty \
                         would drop part of the turn the model is answering from."
                    )
                })?;
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(piece);
            }
            Ok(text)
        }
        // Unreachable: the binding above admits only the two shapes.
        other => Err(format!(
            "a message's `content` must be a string or an array of content parts, \
             and this one is `{other}`. Send the turn's text as a string."
        )),
    }
}

/// Read a `function_call_output` item's `output`.
///
/// A string on the wire every client this has been tested against writes, and
/// an array of parts in OpenAI's newer schema; both are read, because the one
/// that is not read would silently drop a tool result out of the transcript.
fn call_output_text(item: &Value) -> Result<String, String> {
    match item.get("output") {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(parts @ Value::Array(_)) => message_text(Some(parts)),
        _ => Err(
            "an `input` item of type `function_call_output` must carry an `output` string \
             (or an array of text parts). Send the tool's result as `output`; it is the \
             turn the model reads."
                .to_owned(),
        ),
    }
}

/// Map a Responses role onto the chat roles the served templates know.
///
/// `developer` is Responses' rename of `system`; every other role passes
/// through. An unknown role is refused rather than forwarded, because
/// `llama_chat_apply_template` renders an unknown role **literally** into the
/// prompt (the same reason `role: "tool"` is mapped rather than passed — see
/// [`ChatCompletionRequest::normalise`]).
fn map_role(role: &str) -> Result<String, String> {
    match role {
        "developer" => Ok("system".to_owned()),
        "system" | "user" | "assistant" => Ok(role.to_owned()),
        other => Err(format!(
            "a message `role` of `{other}` is not supported: the served chat templates render \
             an unknown role literally into the prompt, so the turn would reach the model as \
             text rather than as a turn. Use `system`, `developer`, `user` or `assistant`."
        )),
    }
}

/// Translate one Responses tool envelope into the chat one.
///
/// Returns `None` for a **hosted** tool — one OpenAI's own servers execute,
/// such as `web_search` — which is dropped rather than refused. Roteiro
/// executes no hosted tool and does not advertise one to the model, so the
/// model can never call it and no answer can be falsely attributed to it;
/// refusing instead would make every turn of a client that advertises one fail
/// for a tool it was not going to use. This is a deliberate divergence from the
/// chat wire, where a non-`function` tool **is** a `400` — there, `tools` is
/// function-only by construction, so a `retrieval` entry is a mistake rather
/// than a normal part of the protocol.
fn tool_spec(tool: &Value) -> Result<Option<ToolSpec>, String> {
    // Absent means `function` — some clients omit it — but a present non-string
    // is malformed, and coercing it would advertise `{"type": 123, "name": "x"}`
    // to the model as a function tool, slipping past both the hosted/function
    // split here and the chat wire's refusal of an unknown tool kind.
    let kind = match tool.get("type") {
        None => "function",
        Some(Value::String(kind)) => kind,
        Some(other) => {
            return Err(format!(
                "a tool's `type` must be a string, and this one is `{other}`. \
                 Send `\"type\": \"function\"` for a tool you will execute."
            ));
        }
    };
    if kind != "function" {
        return Ok(None);
    }
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a `function` tool must carry a string `name`. Responses spells a tool flat — `{\"type\":\"function\",\"name\":…,\"parameters\":…}` — rather than nesting it under a `function` key as Chat Completions does.")?;
    // Read as raw JSON, so a malformed field has to be refused here rather than
    // by serde. Dropping one would advertise a *different* tool to the model
    // than the caller described — a tool with no description, or with no schema
    // — and the call would come back looking like an answer to the tool that was
    // sent. The chat wire gets this from its typed deserialisation; this wire
    // has to say it.
    let description = match tool.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(d)) => Some(d.clone()),
        Some(other) => {
            return Err(format!(
                "tool `{name}` has a `description` that is not a string (`{other}`). \
                 Send the description as a string, or omit it — dropping it would \
                 advertise a tool the model knows less about than you described."
            ));
        }
    };
    let parameters = match tool.get("parameters") {
        None | Some(Value::Null) => None,
        Some(schema @ Value::Object(_)) => Some(schema.clone()),
        Some(other) => {
            return Err(format!(
                "tool `{name}` has `parameters` that are not a JSON-Schema object \
                 (`{other}`). Send an object with a `type` and `properties`, or omit \
                 `parameters` for a tool that takes none."
            ));
        }
    };
    Ok(Some(ToolSpec {
        kind: "function".to_owned(),
        function: FunctionSpec {
            name: name.to_owned(),
            description,
            parameters,
        },
    }))
}

/// Bound the `tools` array **as the caller sent it**, before hosted entries are
/// filtered out.
///
/// Both halves of the chat wire's bound, applied one step earlier. Filtering
/// first would leave the dropped entries unbounded in *both* directions: the
/// count never reaches [`crate::types::MAX_CLIENT_TOOLS`], and the bytes never
/// reach `client_tools_from`'s `max_client_tool_bytes` — so a request could
/// carry 128 `web_search` entries with multi-megabyte descriptions, have every
/// one of them deserialised and walked, and meet no limit at all. Raised twice
/// in review of #825, once per half.
///
/// **Measured on the raw JSON**, which is what the request actually made the
/// server hold, rather than on the `name`/`description`/`schema` sum the chat
/// path takes over surviving tools. The two differ by an entry's punctuation,
/// so this fires marginally earlier; against the client this was built for
/// there is room to spare — `codex-cli` 0.147.0's ten tools serialise to 18,448
/// bytes, 56% of the 32 KiB default.
///
/// # Errors
/// A message naming the bound that was crossed and what the count or size was.
fn bound_declared_tools(declared: &[Value], limits: Limits) -> Result<(), String> {
    if declared.len() > crate::types::MAX_CLIENT_TOOLS {
        return Err(format!(
            "too many tools: {} (max {}). The bound counts every entry of `tools`, \
             including hosted ones this endpoint drops — it caps what one request \
             may make the server read, not what the model ends up being shown.",
            declared.len(),
            crate::types::MAX_CLIENT_TOOLS
        ));
    }
    let bytes: usize = declared
        .iter()
        .map(|t| serde_json::to_string(t).map_or(0, |s| s.len()))
        .sum();
    if bytes > limits.max_client_tool_bytes {
        return Err(format!(
            "the `tools` array is {bytes} bytes, over the {} byte limit. The bound \
             measures every entry as sent, including hosted ones this endpoint \
             drops, because the cost is paid on the way in. Send fewer tools, or \
             shorten the longest descriptions and schemas.",
            limits.max_client_tool_bytes
        ));
    }
    Ok(())
}

impl ResponsesRequest {
    /// Translate into the chat request this server already serves, then
    /// normalise it.
    ///
    /// The translation is the only thing here. `normalise` does the validating,
    /// the image handling (none — Responses images are refused above), the tool
    /// bounds, and — the part that matters — the in-band `<tool_call>` /
    /// `<tool_response>` rendering of tool history.
    ///
    /// # Errors
    /// A human-readable `400` message naming the parameter, item type, content
    /// part or role that was refused.
    pub fn into_chat(self, limits: Limits) -> Result<ChatCompletionRequest, String> {
        check_declared(&self.extra)?;
        if self.stream != Some(true) {
            return Err(STREAM_ONLY.to_owned());
        }
        let mut messages = Vec::new();
        if let Some(instructions) = self.instructions.filter(|s| !s.is_empty()) {
            messages.push(RequestMessage {
                role: "system".to_owned(),
                content: Some(MessageContent::Text(instructions)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        match self.input {
            None => {}
            Some(ResponsesInput::Text(text)) => messages.push(RequestMessage {
                role: "user".to_owned(),
                content: Some(MessageContent::Text(text)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }),
            Some(ResponsesInput::Items(items)) => {
                for item in items {
                    messages.push(input_item(&item)?);
                }
            }
        }
        if messages.is_empty() {
            return Err(
                "`input` must not be empty. Send at least one item — a `message` with \
                        a `role` and `content`, or a bare string."
                    .to_owned(),
            );
        }
        let declared = self.tools.unwrap_or_default();
        bound_declared_tools(&declared, limits)?;
        let mut tools = Vec::new();
        for tool in declared {
            if let Some(spec) = tool_spec(&tool)? {
                tools.push(spec);
            }
        }
        Ok(ChatCompletionRequest {
            model: self.model,
            messages,
            temperature: self.temperature,
            max_tokens: self.max_output_tokens,
            stream: Some(true),
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: self.tool_choice,
            parallel_tool_calls: self.parallel_tool_calls,
            // Deliberately empty rather than `self.extra`: the two wires do not
            // share a parameter vocabulary, and handing Responses keys to the
            // chat table would refuse `reasoning` (a chat `reasoning_effort`
            // cousin this endpoint drops) and miss `previous_response_id`
            // entirely. `check_declared` above is the Responses table's own
            // enforcement, and it has already run.
            extra: BTreeMap::new(),
        })
    }

    /// Translate and normalise in one step.
    ///
    /// # Errors
    /// As [`Self::into_chat`], plus anything `normalise` refuses.
    pub fn normalise(self, limits: Limits) -> Result<NormalisedChat, String> {
        self.into_chat(limits)?.normalise(limits)
    }
}

/// Translate one `input` item into the chat turn that renders it.
///
/// The three supported types, and why each maps where it does:
///
/// * `message` — an ordinary turn.
/// * `function_call` — the assistant turn the model produced. Carried as a
///   [`ToolCallDto`] so `normalise` re-renders it through `render_tool_call`,
///   byte for byte as a replayed chat `tool_calls` entry.
/// * `function_call_output` — the result. Carried as a `role: "tool"` turn so
///   `normalise` wraps it in `<tool_response>`, byte for byte as a chat
///   `role: "tool"` message.
///
/// Anything else is named in the refusal. `reasoning` items are the ones a
/// client is most likely to replay, and dropping them silently would be worse
/// than refusing: the client would believe the model had been shown its own
/// prior deliberation.
fn input_item(item: &Value) -> Result<RequestMessage, String> {
    match item_type(item)? {
        "message" => {
            let role = map_role(required_str(item, "role", "message")?)?;
            let text = message_text(item.get("content"))?;
            Ok(RequestMessage {
                role,
                content: Some(MessageContent::Text(text)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
        }
        "function_call" => {
            let call_id = required_str(item, "call_id", "function_call")?;
            let name = required_str(item, "name", "function_call")?;
            // `arguments` is a JSON **string** on both wires, so it is carried
            // across untouched: re-parsing and re-serialising here would
            // reorder keys and change the bytes the model is shown.
            let arguments = required_str(item, "arguments", "function_call")?;
            Ok(RequestMessage {
                role: "assistant".to_owned(),
                content: None,
                tool_calls: Some(vec![ToolCallDto {
                    id: call_id.to_owned(),
                    kind: "function".to_owned(),
                    function: FunctionCallDto {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            })
        }
        "function_call_output" => {
            let call_id = required_str(item, "call_id", "function_call_output")?;
            Ok(RequestMessage {
                role: "tool".to_owned(),
                content: Some(MessageContent::Text(call_output_text(item)?)),
                tool_calls: None,
                tool_call_id: Some(call_id.to_owned()),
                name: None,
            })
        }
        other => Err(format!(
            "an `input` item of type `{other}` is not supported on this endpoint: only \
             `message`, `function_call` and `function_call_output` items are read, so this \
             one would be absent from the conversation the model was shown. Drop it from \
             `input`, or send its information as a `message`."
        )),
    }
}

/// One SSE frame: the event name and its `data:` payload.
pub type Frame = (&'static str, String);

/// Builds the Responses event sequence for one request.
///
/// ## The sequence, and how it was settled
///
/// A/B'd against `codex-cli 0.147.0` on 2026-09-13 by serving each event set
/// from a mock and running a real tool-using turn, because the alternative —
/// reading a shipped binary for string literals — cannot distinguish "absent"
/// from "packed". What that measured:
///
/// | omitted | result |
/// |---|---|
/// | `response.created` | **turn completed** — it is not required |
/// | `response.output_item.added` | `OutputTextDelta without active item`, then completed |
/// | `response.output_text.delta` | turn completed — deltas are optional |
/// | `response.output_item.done` | **turn produced nothing**: the tool call was never dispatched |
/// | `response.completed` | **`stream closed before response.completed`**, then five retries |
///
/// So the load-bearing pair is `output_item.done` and `completed`. This writer
/// emits the full sequence anyway — `created`, the `added`/`delta`/`done`
/// triplet and `completed` — because the full sequence is what OpenAI's own
/// wire carries and a client that reads the optional events gets a live stream
/// rather than a silent wait.
pub struct ResponseWriter {
    id: String,
    model: String,
    created: u64,
    seq: u64,
    output_index: u64,
    output: Vec<Value>,
}

impl ResponseWriter {
    /// Start a writer for one response.
    #[must_use]
    pub fn new(id: String, model: String, created: u64) -> Self {
        Self {
            id,
            model,
            created,
            seq: 0,
            output_index: 0,
            output: Vec::new(),
        }
    }

    /// This response's id, as the `response.*` events carry it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Stamp one event with the next sequence number and its type.
    fn frame(&mut self, name: &'static str, mut data: Value) -> Frame {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("type".to_owned(), json!(name));
            obj.insert("sequence_number".to_owned(), json!(self.seq));
        }
        self.seq += 1;
        (name, data.to_string())
    }

    /// The terminal event name and response `status` one finish reason produces.
    /// See [`Self::finish`] for why a truncated generation gets its own pair.
    const fn terminal(finish: FinishReason) -> (&'static str, &'static str) {
        match finish {
            FinishReason::Stop => ("response.completed", "completed"),
            FinishReason::Length => ("response.incomplete", "incomplete"),
        }
    }

    /// The `response` object every lifecycle event embeds.
    fn envelope(&self, status: &str, usage: Option<&crate::types::Usage>) -> Value {
        let mut response = json!({
            "id": self.id,
            "object": "response",
            "created_at": self.created,
            "status": status,
            "model": self.model,
            "output": self.output,
            // Echoed `false` because nothing is retained; see `RESPONSES_PARAMS`.
            "store": false,
        });
        if let (Some(usage), Some(obj)) = (usage, response.as_object_mut()) {
            obj.insert(
                "usage".to_owned(),
                json!({
                    "input_tokens": usage.prompt_tokens,
                    "output_tokens": usage.completion_tokens,
                    "total_tokens": usage.total_tokens,
                }),
            );
        }
        response
    }

    /// `response.created` — the stream's opening event.
    pub fn created(&mut self) -> Frame {
        let response = self.envelope("in_progress", None);
        self.frame("response.created", json!({ "response": response }))
    }

    /// Open a `message` item: `output_item.added` then `content_part.added`.
    pub fn message_start(&mut self) -> Vec<Frame> {
        let item_id = format!("msg_{}_{}", self.id, self.output_index);
        let output_index = self.output_index;
        let added = self.frame(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        );
        let part = self.frame(
            "response.content_part.added",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []},
            }),
        );
        vec![added, part]
    }

    /// One piece of assistant text.
    pub fn text_delta(&mut self, delta: &str) -> Frame {
        let item_id = format!("msg_{}_{}", self.id, self.output_index);
        let output_index = self.output_index;
        self.frame(
            "response.output_text.delta",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": delta,
            }),
        )
    }

    /// Close a `message` item with the text it accumulated, and record it in
    /// the response's `output` so the terminal event carries it.
    ///
    /// The item's own `status` follows `finish`: a message cut at the token cap
    /// is `incomplete`, matching the response status [`Self::finish`] sets.
    pub fn message_done(&mut self, text: &str, finish: FinishReason) -> Vec<Frame> {
        let item_id = format!("msg_{}_{}", self.id, self.output_index);
        let output_index = self.output_index;
        let part = json!({"type": "output_text", "text": text, "annotations": []});
        let item = json!({
            "id": item_id,
            "type": "message",
            "status": Self::terminal(finish).1,
            "role": "assistant",
            "content": [part],
        });
        let text_done = self.frame(
            "response.output_text.done",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "text": text,
            }),
        );
        let part_done = self.frame(
            "response.content_part.done",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": part,
            }),
        );
        let item_done = self.frame(
            "response.output_item.done",
            json!({"output_index": output_index, "item": item}),
        );
        self.output.push(item);
        self.output_index += 1;
        vec![text_done, part_done, item_done]
    }

    /// A whole `function_call` item, opened and closed.
    ///
    /// Not token-incremental for the same reason the chat wire's `tool_calls`
    /// chunk is not: the tool loop resolves before anything may be published.
    /// The `arguments` delta is emitted in one piece, which is what the chat
    /// surface already declares (`docs/SERVING.md`).
    pub fn function_call(&mut self, call: &ToolCallDto) -> Vec<Frame> {
        let item_id = format!("fc_{}_{}", self.id, self.output_index);
        let output_index = self.output_index;
        let item = json!({
            "id": item_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call.id,
            "name": call.function.name,
            "arguments": call.function.arguments,
        });
        let added = self.frame(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": {
                    "id": item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call.id,
                    "name": call.function.name,
                    "arguments": "",
                },
            }),
        );
        let args_delta = self.frame(
            "response.function_call_arguments.delta",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "delta": call.function.arguments,
            }),
        );
        let args_done = self.frame(
            "response.function_call_arguments.done",
            json!({
                "item_id": item_id,
                "output_index": output_index,
                "arguments": call.function.arguments,
            }),
        );
        let item_done = self.frame(
            "response.output_item.done",
            json!({"output_index": output_index, "item": item}),
        );
        self.output.push(item);
        self.output_index += 1;
        vec![added, args_delta, args_done, item_done]
    }

    /// The terminal event — without one, a client waits forever.
    ///
    /// `response.completed` for a generation that stopped on its own, and
    /// `response.incomplete`, carrying an `incomplete_details.reason`, for one
    /// the token budget cut short.
    ///
    /// **A generation cut at the token cap is not a completed one**, and saying
    /// so is the whole of the distinction. The chat wire here already spells it
    /// `finish_reason: "length"`; reporting `response.completed` on this wire
    /// would hand a client a truncated answer with nothing on the wire to say it
    /// was truncated, which is the silent-contradiction class the rest of this
    /// module refuses by name.
    ///
    /// Measured rather than assumed, the same way the rest of the sequence was:
    /// served from a mock to `codex-cli` 0.147.0, `response.incomplete` is
    /// understood and surfaces as `Incomplete response returned, reason:
    /// max_output_tokens` — a named error, not a wedged stream. That mattered,
    /// because `response.completed` is the one event that client cannot do
    /// without, so replacing it needed checking rather than assuming.
    pub fn finish(&mut self, usage: &crate::types::Usage, finish: FinishReason) -> Frame {
        let (event, status) = Self::terminal(finish);
        let mut response = self.envelope(status, Some(usage));
        if finish == FinishReason::Length
            && let Some(obj) = response.as_object_mut()
        {
            obj.insert(
                "incomplete_details".to_owned(),
                json!({"reason": "max_output_tokens"}),
            );
        }
        self.frame(event, json!({ "response": response }))
    }

    /// `response.failed` — generation broke part-way.
    ///
    /// A lifecycle event rather than a bare `error` blob, because a client
    /// waiting for `response.completed` needs a terminal event of the shape it
    /// is waiting for; anything else leaves it retrying a stream that is over.
    pub fn failed(&mut self, message: &str) -> Frame {
        let mut response = self.envelope("failed", None);
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "error".to_owned(),
                json!({"code": "inference_error", "message": message}),
            );
        }
        self.frame("response.failed", json!({ "response": response }))
    }
}

#[cfg(test)]
mod tests {
    use super::{RESPONSES_PARAMS, ResponsesRequest, published_table};
    use crate::types::Limits;
    use serde_json::json;

    fn chat_of(body: serde_json::Value) -> Result<crate::types::ChatCompletionRequest, String> {
        serde_json::from_value::<ResponsesRequest>(body)
            .map_err(|e| e.to_string())?
            .into_chat(Limits::default())
    }

    /// The rendered prompt turns, `(role, content)`, after `normalise`.
    fn turns(body: serde_json::Value) -> Vec<(String, String)> {
        serde_json::from_value::<ResponsesRequest>(body)
            .expect("a well-formed body")
            .normalise(Limits::default())
            .expect("a normalisable body")
            .request
            .messages
            .into_iter()
            .map(|m| (m.role, m.content))
            .collect()
    }

    fn base(input: &serde_json::Value) -> serde_json::Value {
        json!({"model": "echo", "stream": true, "input": input})
    }

    #[test]
    fn a_bare_string_input_is_one_user_turn() {
        assert_eq!(
            turns(base(&json!("hello"))),
            vec![("user".to_owned(), "hello".to_owned())]
        );
    }

    #[test]
    fn instructions_become_a_leading_system_turn() {
        let body = json!({
            "model": "echo", "stream": true, "instructions": "be terse",
            "input": [{"type": "message", "role": "user",
                       "content": [{"type": "input_text", "text": "hi"}]}],
        });
        assert_eq!(
            turns(body),
            vec![
                ("system".to_owned(), "be terse".to_owned()),
                ("user".to_owned(), "hi".to_owned()),
            ]
        );
    }

    #[test]
    fn a_developer_role_becomes_system() {
        let body = base(&json!([{"type": "message", "role": "developer", "content": "rules"}]));
        assert_eq!(turns(body)[0].0, "system");
    }

    /// The whole reason this is an adapter: a Responses `function_call` item and
    /// a replayed chat `tool_calls` entry must reach the model as the **same
    /// bytes**, or a multi-turn tool conversation degrades at turn three with
    /// nothing on the wire to say so.
    #[test]
    fn a_function_call_round_trips_byte_for_byte_with_the_chat_wire() {
        let arguments = "{\"query\":\"debt\",\"limit\":3}";
        let via_responses = turns(base(&json!([
            {"type": "message", "role": "user", "content": "find debt"},
            {"type": "function_call", "call_id": "call_1", "name": "search",
             "arguments": arguments},
            {"type": "function_call_output", "call_id": "call_1", "output": "3 results"},
        ])));

        let via_chat: Vec<(String, String)> =
            serde_json::from_value::<crate::types::ChatCompletionRequest>(json!({
                "model": "echo",
                "messages": [
                    {"role": "user", "content": "find debt"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "call_1", "type": "function",
                         "function": {"name": "search", "arguments": arguments}}]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "3 results"},
                ],
            }))
            .expect("a well-formed chat body")
            .normalise(Limits::default())
            .expect("a normalisable chat body")
            .request
            .messages
            .into_iter()
            .map(|m| (m.role, m.content))
            .collect();

        assert_eq!(via_responses, via_chat);
        // And the rendering really is the in-band protocol, not two empty
        // strings agreeing with each other.
        //
        // Note the argument keys come back **sorted**, not in the order they
        // were sent: `render_tool_call` re-parses the `arguments` string into a
        // `serde_json::Value`, whose object is a sorted map. That is a property
        // of the chat wire this adapter inherits rather than one it introduces
        // — which is the whole point of inheriting it, because a second
        // implementation would have had to rediscover it.
        assert_eq!(
            via_responses[1].1,
            "<tool_call>{\"name\":\"search\",\"arguments\":{\"limit\":3,\"query\":\"debt\"}}</tool_call>"
        );
        assert_eq!(via_responses[2].0, "user");
        assert_eq!(
            via_responses[2].1,
            "<tool_response>3 results</tool_response>"
        );
    }

    #[test]
    fn a_function_call_output_may_be_an_array_of_text_parts() {
        let body = base(&json!([
            {"type": "message", "role": "user", "content": "go"},
            {"type": "function_call", "call_id": "c", "name": "t", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c",
             "output": [{"type": "output_text", "text": "done"}]},
        ]));
        assert_eq!(turns(body)[2].1, "<tool_response>done</tool_response>");
    }

    #[test]
    fn a_responses_tool_is_flat_not_nested() {
        let body = json!({
            "model": "echo", "stream": true, "input": "hi",
            "tools": [{"type": "function", "name": "search",
                       "description": "find", "parameters": {"type": "object"}}],
        });
        let normalised = serde_json::from_value::<ResponsesRequest>(body)
            .expect("a well-formed body")
            .normalise(Limits::default())
            .expect("a normalisable body");
        assert_eq!(normalised.client_tools.len(), 1);
        assert_eq!(normalised.client_tools[0].name, "search");
        assert_eq!(normalised.client_tools[0].description, "find");
    }

    /// Codex advertises `{"type":"web_search"}` on every request. Refusing it —
    /// the chat wire's rule for a non-`function` tool — would fail every turn
    /// over a tool the model was never going to be offered.
    #[test]
    fn a_hosted_tool_is_dropped_rather_than_refused() {
        let body = json!({
            "model": "echo", "stream": true, "input": "hi",
            "tools": [
                {"type": "web_search", "external_web_access": true},
                {"type": "function", "name": "shell", "parameters": {"type": "object"}},
            ],
        });
        let normalised = serde_json::from_value::<ResponsesRequest>(body)
            .expect("a well-formed body")
            .normalise(Limits::default())
            .expect("a normalisable body");
        assert_eq!(normalised.client_tools.len(), 1);
        assert_eq!(normalised.client_tools[0].name, "shell");
    }

    #[test]
    fn max_output_tokens_is_the_generation_budget() {
        let body = json!({"model": "echo", "stream": true, "input": "hi",
                          "max_output_tokens": 64});
        let normalised = serde_json::from_value::<ResponsesRequest>(body)
            .expect("a well-formed body")
            .normalise(Limits::default())
            .expect("a normalisable body");
        assert_eq!(normalised.request.max_tokens, 64);
    }

    #[test]
    fn a_non_streaming_request_is_refused_by_name() {
        let msg = chat_of(json!({"model": "echo", "input": "hi"})).expect_err("a refusal");
        assert!(msg.starts_with("`stream` must be `true`"), "{msg}");
        assert!(msg.contains("response.completed"), "{msg}");
    }

    #[test]
    fn an_unsupported_item_type_is_refused_by_name() {
        let msg = chat_of(base(&json!([
            {"type": "message", "role": "user", "content": "hi"},
            {"type": "reasoning", "id": "rs_1", "summary": []},
        ])))
        .expect_err("a refusal");
        assert!(msg.contains("`reasoning`"), "names the item type: {msg}");
        assert!(msg.contains("not supported"), "{msg}");
    }

    #[test]
    fn an_unsupported_content_part_is_refused_by_name() {
        let msg = chat_of(base(&json!([{
            "type": "message", "role": "user",
            "content": [{"type": "input_image", "image_url": "data:image/png;base64,AA"}],
        }])))
        .expect_err("a refusal");
        assert!(msg.contains("`input_image`"), "{msg}");
    }

    #[test]
    fn an_unknown_role_is_refused_rather_than_rendered_literally() {
        let msg =
            chat_of(base(&json!([{"role": "narrator", "content": "hi"}]))).expect_err("a refusal");
        assert!(msg.contains("`narrator`"), "{msg}");
    }

    #[test]
    fn a_function_call_without_a_call_id_is_refused() {
        let msg = chat_of(base(&json!([
            {"type": "function_call", "name": "t", "arguments": "{}"},
        ])))
        .expect_err("a refusal");
        assert!(msg.contains("`call_id`"), "{msg}");
    }

    #[test]
    fn empty_input_is_refused() {
        let msg =
            chat_of(json!({"model": "echo", "stream": true, "input": []})).expect_err("a refusal");
        assert!(msg.contains("`input` must not be empty"), "{msg}");
    }

    #[test]
    fn previous_response_id_is_refused_because_nothing_is_stored() {
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "previous_response_id": "resp_123"}))
        .expect_err("a refusal");
        assert!(
            msg.starts_with("`previous_response_id` is not supported"),
            "{msg}"
        );
        assert!(msg.contains("`input`"), "names the way forward: {msg}");
    }

    /// The bookkeeping half of the rule, against the real thing.
    ///
    /// This body is the top level of an actual `codex-cli` 0.147.0 request,
    /// captured on the wire on 2026-09-13: five declared parameters
    /// (`store`, `reasoning`, `include`, `prompt_cache_key`, `tool_choice`)
    /// plus `parallel_tool_calls`, plus `client_metadata`, which is **not** an
    /// OpenAI parameter at all and passes through on the unknown-key rule.
    /// None of them changes what comes back, so none of them is refused —
    /// otherwise every Codex turn would fail on a field nobody typed.
    ///
    /// Note what is *absent*: Codex does not send `truncation`, which is why
    /// refusing it (it asks for a trimming this endpoint does not do) costs
    /// this client nothing.
    #[test]
    fn the_parameters_codex_actually_sends_are_dropped_not_refused() {
        let body = json!({
            "model": "echo", "stream": true, "input": "hi",
            "store": false, "reasoning": {"effort": "none", "summary": "auto"},
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "01a098c0-c98e-7f73-9a35-6e3d097437a1",
            "tool_choice": "auto", "parallel_tool_calls": false,
            "client_metadata": {"turn_id": "t1"},
        });
        chat_of(body).expect("no refusal for bookkeeping parameters");
    }

    /// `truncation: "auto"` is a decision, not bookkeeping: it asks for turns to
    /// be dropped so an over-long input still answers, and this endpoint refuses
    /// such an input instead. `disabled` — the default, and what this endpoint
    /// actually does — is inert.
    #[test]
    fn truncation_auto_is_refused_and_disabled_is_not() {
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "truncation": "auto"}))
        .expect_err("a refusal");
        assert!(msg.starts_with("`truncation` is not supported"), "{msg}");
        chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                       "truncation": "disabled"}))
        .expect("`disabled` is what this endpoint already does");
    }

    /// A supported part whose `text` is missing is refused, not read as `""`.
    /// Reading it as empty would delete that much of the caller's prompt while
    /// still answering.
    #[test]
    fn a_text_part_without_text_is_refused_rather_than_read_as_empty() {
        let msg = chat_of(base(&json!([{
            "type": "message", "role": "user",
            "content": [{"type": "input_text"}],
        }])))
        .expect_err("a refusal");
        assert!(msg.contains("string `text`"), "{msg}");
    }

    /// `text` is not a Responses content-part type; the refusal message and
    /// `docs/SERVING.md` both name only `input_text` and `output_text`, so
    /// accepting a third spelling would make all three disagree.
    #[test]
    fn a_part_type_of_text_is_refused_like_any_other_unknown_one() {
        let msg = chat_of(base(&json!([{
            "type": "message", "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        }])))
        .expect_err("a refusal");
        assert!(msg.contains("`text`"), "{msg}");
    }

    /// `null` is never a decision — the same rule the chat table keys on.
    #[test]
    fn a_null_refused_parameter_is_not_a_decision() {
        chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                       "top_p": null, "previous_response_id": null}))
        .expect("nulls express no preference");
    }

    /// Responses' *other* way of naming state that lives on the server. Missing
    /// from the first version of this table, which is what
    /// `RESPONSES_PARAMS`' provenance note now records: an unlisted stateful
    /// parameter is a silently wrong answer, not a harmless unknown.
    #[test]
    fn conversation_is_refused_because_nothing_is_stored() {
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "conversation": "conv_123"}))
        .expect_err("a refusal");
        assert!(msg.starts_with("`conversation` is not supported"), "{msg}");
        assert!(msg.contains("`input`"), "names the way forward: {msg}");
    }

    /// The tool bound counts what the **caller sent**, not what survives the
    /// hosted-tool filter. Filtering first would leave the count of dropped
    /// entries unbounded, so a request could make the server deserialise and
    /// walk an arbitrarily long array and never meet the cap.
    #[test]
    fn a_flood_of_hosted_tools_is_refused_before_they_are_filtered() {
        let hosted: Vec<serde_json::Value> = (0..=crate::types::MAX_CLIENT_TOOLS)
            .map(|_| json!({"type": "web_search", "external_web_access": true}))
            .collect();
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "tools": hosted}))
        .expect_err("a refusal");
        assert!(msg.starts_with("too many tools"), "{msg}");
        // And the bound is not merely "some hosted tools are refused": the same
        // count of `function` tools is refused too, and one fewer is served.
        let at_limit: Vec<serde_json::Value> = (0..crate::types::MAX_CLIENT_TOOLS)
            .map(|_| json!({"type": "web_search", "external_web_access": true}))
            .collect();
        chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                       "tools": at_limit}))
        .expect("the limit itself is served");
    }

    /// `type` absent means `message` — some clients omit it — but a `type` that
    /// is present and not a string is malformed, and coercing it would admit an
    /// item while every unsupported *type* is refused.
    #[test]
    fn a_non_string_item_type_is_refused_rather_than_defaulted() {
        let msg = chat_of(base(
            &json!([{"type": 123, "role": "user", "content": "hi"}]),
        ))
        .expect_err("a refusal");
        assert!(msg.contains("must be a string"), "{msg}");
    }

    /// The same fallback on the tool side would advertise
    /// `{"type": 123, "name": "x"}` to the model as a function tool, past both
    /// the hosted/function split and the chat wire's refusal of an unknown kind.
    #[test]
    fn a_non_string_tool_type_is_refused_rather_than_defaulted() {
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "tools": [{"type": 123, "name": "x"}]}))
        .expect_err("a refusal");
        assert!(msg.contains("must be a string"), "{msg}");
    }

    /// A `message` item with no `content` was read as an empty turn, which
    /// deletes it from the conversation the model answers from while still
    /// returning a `200`. An *explicitly* empty string is a different thing and
    /// is served.
    #[test]
    fn a_message_without_content_is_refused_but_an_empty_string_is_served() {
        for absent in [
            json!({"role": "user"}),
            json!({"role": "user", "content": null}),
        ] {
            let msg = chat_of(base(&json!([absent]))).expect_err("a refusal");
            assert!(msg.contains("must carry `content`"), "{msg}");
        }
        assert_eq!(
            turns(base(&json!([{"role": "user", "content": ""}]))),
            vec![("user".to_owned(), String::new())]
        );
    }

    /// The byte half of the tool bound, on the array **as sent**. Filtering
    /// hosted entries out first left them unmeasured, so 128 of them carrying
    /// multi-megabyte descriptions met no limit at all.
    #[test]
    fn an_oversized_tools_array_is_refused_even_when_every_entry_is_hosted() {
        let fat: Vec<serde_json::Value> = (0..8)
            .map(|_| json!({"type": "web_search", "note": "z".repeat(8 * 1024)}))
            .collect();
        let req: ResponsesRequest = serde_json::from_value(json!({
            "model": "echo", "stream": true, "input": "hi", "tools": fat,
        }))
        .expect("a well-formed body");
        let msg = req.into_chat(Limits::default()).expect_err("a refusal");
        assert!(msg.contains("over the 32768 byte limit"), "{msg}");
    }

    /// A refusal that names a field must say why *that* field is needed. One
    /// sentence for all of them read "it is what correlates the call with its
    /// result", which is true of `call_id` and false of the other three.
    #[test]
    fn a_missing_field_refusal_explains_that_field_and_not_another() {
        let name = chat_of(base(&json!([
            {"type": "function_call", "call_id": "c", "arguments": "{}"},
        ])))
        .expect_err("a refusal");
        assert!(name.contains("`name`"), "{name}");
        assert!(
            !name.contains("correlates"),
            "`name` does not correlate anything: {name}"
        );

        let call_id = chat_of(base(&json!([
            {"type": "function_call_output", "output": "x"},
        ])))
        .expect_err("a refusal");
        assert!(
            call_id.contains("correlates the call with its result"),
            "{call_id}"
        );
    }

    /// Retention is stateful, not bookkeeping: `store: true` asks for a response
    /// to be kept and addressable, and nothing here keeps anything, so the id
    /// the caller was handed would address nothing. `false` — the default, and
    /// what this endpoint does — is inert, which is what the one measured client
    /// sends.
    #[test]
    fn store_true_is_refused_and_store_false_is_not() {
        let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                 "store": true}))
        .expect_err("a refusal");
        assert!(msg.starts_with("`store` is not supported"), "{msg}");
        chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                       "store": false}))
        .expect("`false` is what this endpoint already does");
    }

    /// A malformed tool field is refused rather than dropped: dropping it would
    /// advertise a tool the model knows less about than the caller described,
    /// and the call would come back looking like an answer to the tool that was
    /// sent. The chat wire gets this from its typed deserialisation.
    #[test]
    fn a_malformed_tool_description_or_schema_is_refused_not_dropped() {
        for bad in [
            json!({"type": "function", "name": "t", "description": 123}),
            json!({"type": "function", "name": "t", "parameters": "an object, honest"}),
        ] {
            let msg = chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                                     "tools": [bad]}))
            .expect_err("a refusal");
            assert!(msg.contains("tool `t`"), "names the tool: {msg}");
        }
        // Omitting either is still fine — a tool may take no arguments.
        chat_of(json!({"model": "echo", "stream": true, "input": "hi",
                       "tools": [{"type": "function", "name": "t"}]}))
        .expect("a tool may carry neither");
    }

    #[test]
    fn the_table_is_sorted_and_every_refusal_says_what_why_and_what_next() {
        let names: Vec<&str> = RESPONSES_PARAMS.iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "RESPONSES_PARAMS must be sorted by name");
        for p in RESPONSES_PARAMS {
            match p.refusal() {
                Some(msg) => {
                    assert!(msg.contains(p.name), "a refusal names its parameter: {msg}");
                    assert!(msg.contains("is not supported"), "{msg}");
                    assert!(p.note.is_empty(), "`{}` carries two explanations", p.name);
                }
                None => assert!(!p.note.is_empty(), "`{}` explains nothing", p.name),
            }
        }
    }

    /// `docs/SERVING.md` publishes this table, so the document and the code are
    /// compared rather than trusted — the arrangement #488 put in place for the
    /// chat table, for the same reason and by the same method.
    ///
    /// **The generated block, not a substring.** `contains` would pass on a
    /// document carrying an *extra* row, and an extra row is a published
    /// capability that `check_declared` does not enforce — a client author
    /// reading roteiro.dev/serving to find out what to send is the worst
    /// possible audience for one. So the block is extracted whole and compared,
    /// exactly as `openai_params::tests::the_published_table_is_this_table`
    /// does.
    #[test]
    fn the_published_responses_table_is_this_table() {
        let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/SERVING.md");
        let text = std::fs::read_to_string(&doc).expect("docs/SERVING.md");
        let expected = published_table();
        // The Responses table is the *second* table with this header; the chat
        // one comes first. Anchored on the heading above it so the two cannot
        // be confused as rows are added to either.
        let section = text
            .find("{#responses-api}")
            .expect("docs/SERVING.md must publish the Responses section");
        let header = expected.lines().next().expect("a header row");
        let start = section
            + text[section..]
                .find(header)
                .expect("the Responses section must publish the parameter table");
        let published: String = text[start..]
            .lines()
            .take_while(|l| l.starts_with('|'))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
        assert_eq!(
            published, expected,
            "the Responses parameter table in docs/SERVING.md has drifted from \
             `responses::RESPONSES_PARAMS`; regenerate it with `cargo run -p \
             rto-serve --example print_declared_table -- responses`"
        );
    }
}
