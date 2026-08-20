//! The declared boundary of `POST /v1/chat/completions` (issue #488).
//!
//! `/v1/chat/completions` is OpenAI's path, and a path sets an expectation.
//! Before this module the request type named eight fields and **everything else
//! a client sent was discarded silently at deserialisation** — so a caller that
//! set `seed` believing its output was reproducible, or `response_format`
//! believing the body would parse as JSON, got a `200` and a wrong answer with
//! nothing on the wire to say so.
//!
//! [`OPENAI_CHAT_PARAMS`] is the artefact this module exists for: **every**
//! parameter of OpenAI's chat-completions request, each carrying what Roteiro
//! does with it. It is the single declaration — `docs/SERVING.md` publishes it,
//! [`check_declared`] enforces it, and the tests at the bottom of this file hold
//! all three together in both directions so none can drift from the others.
//!
//! ## Where the list came from
//!
//! Read on 2026-08-20 from OpenAI's published `OpenAPI` document
//! (<https://github.com/openai/openai-openapi>, `components.schemas.
//! CreateChatCompletionRequest` with its `allOf` composition resolved) and
//! cross-checked against the rendered reference at
//! <https://developers.openai.com/api/reference/resources/chat>. Both sources
//! yield the same 37 names. This is recorded because the issue was itself filed
//! with a stale list — it names five request fields where the code already had
//! eight — and a stale list is the defect being fixed.
//!
//! ## The rule that decides `dropped` from `400`
//!
//! Not "is it implemented" — almost none of these are. The line is **what the
//! client can conclude from a `200`**:
//!
//! - A parameter that only drives OpenAI's own bookkeeping — storage, billing,
//!   cache bucketing, abuse attribution — has no analogue in a loopback server
//!   and changes nothing observable about the response. Ignoring it leaves the
//!   client with no false belief, so it is **dropped**.
//! - A parameter the client set to change *what comes back or how it was
//!   produced* is different in kind. Ignoring it hands back a response that
//!   silently contradicts the request. Those are **refused with a 400**.
//!
//! ## `present` is not `meaningfully set`
//!
//! A refusal that fires on a field the caller never decided is a worse defect
//! than the silence it replaces. Client libraries serialise defaults: `n: 1`,
//! `top_p: 1`, `frequency_penalty: 0`, and a great many `null`s go out from
//! callers who expressed no preference at all — and for every one of those,
//! Roteiro's behaviour is *already* what was asked for.
//!
//! So the check keys on the **value**, never on the key's presence:
//!
//! - `null` is never a decision, for any parameter.
//! - A value equal to OpenAI's own documented default ([`Param::inert`]) is not
//!   a decision either.
//! - Anything else is a decision the caller made and did not get.
//!
//! `n: 1` and `n: 3` are therefore answered differently and correctly by the
//! same rule: the first asks for what Roteiro does, the second asks for
//! something it cannot do.
//!
//! ## One budget, two spellings
//!
//! `max_completion_tokens` and `max_tokens` are OpenAI's current and deprecated
//! names for the *same* number, and both are read — see
//! [`generation_budget`], and the comment on the `max_completion_tokens` row for
//! why refusing the current spelling was the wrong boundary. It is the only row
//! where a wire name and a request-type field are spelled differently, so it is
//! also the only row carrying [`Param::served_by`].
//!
//! ## An unknown key is still dropped, deliberately
//!
//! `#[serde(deny_unknown_fields)]` was the obvious instrument and is the wrong
//! one: it would reject `user`, `store`, `metadata`, `service_tier` and
//! `prompt_cache_key` — keys clients send by habit and whose absence harms
//! nobody — so it would break working callers to fix a correctness problem they
//! do not have. A key in no row of this table is either a typo or a parameter
//! newer than the table, and refusing the second would break callers on
//! OpenAI's release schedule rather than on Roteiro's. Unknown keys are ignored;
//! this paragraph is that decision being declared rather than assumed.

use std::collections::BTreeMap;

use serde_json::Value;

/// What Roteiro does with one OpenAI chat-completions parameter.
///
/// The four words are the vocabulary `docs/SERVING.md`'s divergence table
/// already established for tool-calling surfaces; the parameter rows join that
/// table rather than inventing a parallel scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Read and acted on. Must be a field of
    /// [`crate::types::ChatCompletionRequest`] — asserted in both directions by
    /// [`tests::the_struct_and_the_table_declare_the_same_fields`].
    Supported,
    /// Parsed and carried to a named place, then deliberately not acted on.
    /// Distinct from `Dropped` in that the type system can see it: the value
    /// reaches [`crate::types::NormalisedChat`] so the "accepted, not forced"
    /// claim is checkable at a boundary rather than asserted in prose.
    AcceptedNotEnforced,
    /// Ignored, and ignoring it leaves the client with no false belief about the
    /// response.
    Dropped,
    /// Refused with a `400`, because ignoring it would return a response that
    /// silently contradicts the request.
    Rejected {
        /// What is actually true, in place of what the client asked for. Reads
        /// after "is not supported: ".
        because: &'static str,
        /// What the caller should do instead.
        forward: Forward,
    },
}

/// The way forward named by a refusal.
///
/// `docs/REVIEW_CHECKLIST.md` requires a refusal to name the way forward and not
/// only the obstacle. For several of these parameters there genuinely is none,
/// and inventing one would be the "wrong answer that reads like a right one"
/// the same checklist warns about — so the two cases are distinguished in the
/// type, both carry prose, and
/// [`tests::every_refusal_says_what_why_and_what_next`] asserts neither is
/// empty. Saying "there is no way to do this here" plainly *is* the way forward
/// when it is the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forward {
    /// An action the caller can take that works. Any parameter this names in
    /// backticks must exist in [`OPENAI_CHAT_PARAMS`] — asserted by
    /// [`tests::a_way_forward_never_points_at_a_parameter_that_does_not_exist`],
    /// so a redirection cannot rot into pointing at a name nobody serves.
    Do(&'static str),
    /// No action exists. The prose says why, rather than leaving the reader to
    /// go looking for a flag that is not there.
    Nothing(&'static str),
}

impl Forward {
    /// The sentence, whichever kind it is.
    #[must_use]
    pub const fn prose(&self) -> &'static str {
        match self {
            Self::Do(s) | Self::Nothing(s) => s,
        }
    }
}

/// One row of the declared boundary.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    /// The parameter name exactly as OpenAI spells it on the wire.
    pub name: &'static str,
    /// The [`crate::types::ChatCompletionRequest`] field that carries this wire
    /// name, when it is **not** a field of the same name.
    ///
    /// `None` — every row but one — means the obvious thing: a served parameter
    /// has a field spelled as it is spelled on the wire, and a parameter that is
    /// not served has no field at all.
    ///
    /// `Some(other)` is the one case where a wire name and a field name come
    /// apart: OpenAI spells the *same* generation budget two ways, so
    /// `max_completion_tokens` is served by the `max_tokens` field. The
    /// relationship is declared here rather than left as an exception inside
    /// [`tests::the_struct_and_the_table_declare_the_same_fields`], because a
    /// guard with an exception carved into it for one row is how the next row
    /// gets one too. Checked by
    /// [`tests::a_served_by_pointer_names_a_parameter_that_serves_itself`].
    pub served_by: Option<&'static str>,
    /// What Roteiro does with it.
    pub support: Support,
    /// The published table's "why" cell for a parameter that is **not**
    /// refused.
    ///
    /// Empty for a [`Support::Rejected`] row, whose published cell is its
    /// [`Param::refusal`] verbatim — one source of truth per row, asserted by
    /// [`tests::a_refused_row_carries_no_second_explanation`]. The whole of
    /// `docs/SERVING.md`'s parameter table is generated from this struct and
    /// compared back against it, so the document cannot drift from the code the
    /// way six earlier tables here did.
    pub note: &'static str,
    /// Values that mean the caller expressed no preference, as JSON literals.
    ///
    /// OpenAI's own documented defaults, so a client library emitting them has
    /// asked for precisely what Roteiro already does. `null` is inert for every
    /// parameter and is not repeated here. Only consulted for
    /// [`Support::Rejected`] rows; see this module's header.
    pub inert: &'static [&'static str],
}

impl Param {
    /// The request-type field that carries this wire name.
    ///
    /// The same as [`Param::name`] for every row but `max_completion_tokens`;
    /// see [`Param::served_by`].
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self.served_by {
            Some(field) => field,
            None => self.name,
        }
    }

    /// The `400` message, or `None` for a parameter that is not refused.
    ///
    /// Three parts, in the order `docs/REVIEW_CHECKLIST.md` asks for them: what
    /// was refused, why, and what to do about it.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self.support {
            Support::Rejected { because, forward } => Some(format!(
                "`{}` is not supported: {because}. {}",
                self.name,
                forward.prose()
            )),
            _ => None,
        }
    }
}

/// Every parameter of OpenAI's `POST /v1/chat/completions` request body, and
/// what Roteiro does with each.
///
/// Sorted by name so this list and the published table can be compared row for
/// row. See the module header for provenance and for the rule that assigns the
/// statuses.
pub const OPENAI_CHAT_PARAMS: &[Param] = &[
    Param {
        name: "audio",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no audio is generated, so a request asking for it would come \
                      back as text with nothing to say the voice was ignored",
            forward: Forward::Nothing(
                "This endpoint has no audio output path at all; there is no setting that \
                 enables one.",
            ),
        },
        inert: &[],
    },
    Param {
        name: "frequency_penalty",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "repetition penalties are not wired to the sampler, so the \
                      sampling you configured is not the sampling that ran",
            forward: Forward::Do(
                "`temperature` is the one sampling control this endpoint honours.",
            ),
        },
        inert: &["0"],
    },
    Param {
        name: "function_call",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the deprecated `function_call` field is not read, so a forced \
                      call would simply not be forced",
            forward: Forward::Do(
                "Send `tool_choice` instead — though note that it too is accepted and \
                 not enforced here, so neither field will force a named function today.",
            ),
        },
        inert: &["\"none\""],
    },
    Param {
        name: "functions",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the deprecated `functions` array is not read, so the model \
                      would be advertised no tools whatsoever and could never call one",
            forward: Forward::Do("Send the same functions as `tools`, which is supported."),
        },
        inert: &["[]"],
    },
    Param {
        name: "logit_bias",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no per-token bias reaches the sampler, so tokens you banned \
                      can still be generated",
            forward: Forward::Do(
                "Steer with a system message instead; there is no per-token control here.",
            ),
        },
        inert: &["{}"],
    },
    Param {
        name: "logprobs",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no log probabilities are computed, so the response carries none \
                      and a client reading them finds `null`",
            forward: Forward::Nothing(
                "This endpoint returns no log probabilities, and there is no flag that \
                 turns them on.",
            ),
        },
        inert: &["false"],
    },
    // OpenAI spells one generation budget two ways, and this is the newer
    // spelling: `max_completion_tokens` supersedes `max_tokens`, which its own
    // schema marks deprecated. Until 2026-08-20 this row was a `400` whose way
    // forward was "Send `max_tokens` instead" — so a client on OpenAI's current
    // name was refused and told to fall back to the name OpenAI is retiring.
    // That is the one row of the thirty-seven where the wire says a thing,
    // Roteiro can do exactly that thing, and Roteiro declines: honouring it is
    // not a new capability but the same budget answered under its current name,
    // and `served_by` below is that sameness written down rather than implied.
    // Resolved by `generation_budget`, which refuses only the genuinely
    // ambiguous request — both names, two different numbers.
    Param {
        name: "max_completion_tokens",
        served_by: Some("max_tokens"),
        note: "OpenAI's current name for the generation budget; the same budget as `max_tokens`, read from the same field — send either, but sending both with different values is a `400`",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "max_tokens",
        served_by: None,
        note: "the generation budget, and the input that sizes the context window for the request; OpenAI deprecated this spelling in favour of `max_completion_tokens`, and both are read",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "messages",
        served_by: None,
        note: "the conversation, including replayed `tool_calls` and `role: \"tool\"` results",
        support: Support::Supported,
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
        name: "modalities",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "only text is generated, so asking for another modality returns \
                      text with no indication the request was not met",
            forward: Forward::Nothing(
                "Text is the only output modality this endpoint has; there is no setting \
                 that adds another.",
            ),
        },
        inert: &["[\"text\"]"],
    },
    Param {
        name: "model",
        served_by: None,
        note: "the model id to run; must be one of `/v1/models`",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "moderation",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no moderation pass runs over the input or the output, so a \
                      response you believe was screened was not",
            forward: Forward::Nothing(
                "A loopback server has no moderation backend to call; screen on your side \
                 if you need it.",
            ),
        },
        inert: &[],
    },
    Param {
        name: "n",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "exactly one choice is generated, so `choices` would come back \
                      shorter than you asked for",
            forward: Forward::Do(
                "Send the request once per choice you need and collect the responses \
                 yourself; each one is a full generation and costs like one.",
            ),
        },
        inert: &["1"],
    },
    Param {
        name: "parallel_tool_calls",
        served_by: None,
        note: "at most one call is parsed per turn today, so a turn never carries more than one regardless",
        support: Support::AcceptedNotEnforced,
        inert: &[],
    },
    Param {
        name: "prediction",
        served_by: None,
        note: "a speculative-decoding latency hint — the output is byte-identical with or without it, so ignoring it costs only the speed-up",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "presence_penalty",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "repetition penalties are not wired to the sampler, so the \
                      sampling you configured is not the sampling that ran",
            forward: Forward::Do(
                "`temperature` is the one sampling control this endpoint honours.",
            ),
        },
        inert: &["0"],
    },
    Param {
        name: "prompt_cache_key",
        served_by: None,
        note: "a cache-bucketing hint for OpenAI's prompt cache; nothing about the response depends on it",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "prompt_cache_options",
        served_by: None,
        note: "as `prompt_cache_key`",
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
        name: "reasoning_effort",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the served model's reasoning is not budgeted by this field, so \
                      asking for more or less thinking changes nothing",
            forward: Forward::Do(
                "Raise `max_tokens`: a reasoning model spends that budget inside its \
                 `<think>` block before it writes a token of answer, so the budget is \
                 what actually governs how much it may think.",
            ),
        },
        inert: &["\"medium\""],
    },
    Param {
        name: "response_format",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the body is prose whatever format you ask for — there is no \
                      grammar-constrained sampling on this endpoint, so `json_object` \
                      and `json_schema` would both return text that need not parse",
            forward: Forward::Do("Ask for JSON in the prompt and parse defensively."),
        },
        inert: &["{\"type\":\"text\"}"],
    },
    Param {
        name: "safety_identifier",
        served_by: None,
        note: "as `user`, which it replaces",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "seed",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "sampling is not seeded, so repeated requests with the same seed \
                      need not agree and the output you believe is reproducible is not",
            forward: Forward::Do(
                "`temperature: 0` selects greedy decoding, which is the nearest thing to \
                 reproducible output here — but it is not a seed, and no determinism is \
                 guaranteed.",
            ),
        },
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
        name: "stop",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no stop sequence is applied, so generation runs to the \
                      `max_tokens` budget and your marker appears in the output rather \
                      than ending it",
            forward: Forward::Do(
                "Truncate at your marker on the client side, and set `max_tokens` as the \
                 ceiling.",
            ),
        },
        inert: &["[]", "\"\""],
    },
    Param {
        name: "store",
        served_by: None,
        note: "asks OpenAI to retain the completion for its evals products; Roteiro stores nothing and sends nothing anywhere",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "stream",
        served_by: None,
        note: "SSE chunks terminated by `data: [DONE]`",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "stream_options",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "the extra `usage` chunk `include_usage` promises is never \
                      streamed, so a client waiting for one before `[DONE]` waits for \
                      something that will not arrive",
            forward: Forward::Do(
                "Read `usage` from the non-streaming response, which does carry it.",
            ),
        },
        inert: &["{}", "{\"include_usage\":false}"],
    },
    Param {
        name: "temperature",
        served_by: None,
        note: "the one sampling control this endpoint honours; `0` (or omitted) is greedy",
        support: Support::Supported,
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
        note: "advertised to the model; calls are returned, never run — bounded at 128 entries / 32 KiB",
        support: Support::Supported,
        inert: &[],
    },
    Param {
        name: "top_logprobs",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "no log probabilities are computed, so no alternatives come back \
                      at any position",
            forward: Forward::Nothing(
                "This endpoint returns no log probabilities, and there is no flag that \
                 turns them on.",
            ),
        },
        inert: &["0"],
    },
    Param {
        name: "top_p",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "nucleus sampling is not wired to the sampler, so the sampling \
                      you configured is not the sampling that ran",
            forward: Forward::Do(
                "`temperature` is the one sampling control this endpoint honours.",
            ),
        },
        inert: &["1"],
    },
    Param {
        name: "user",
        served_by: None,
        note: "an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way",
        support: Support::Dropped,
        inert: &[],
    },
    Param {
        name: "verbosity",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "response length is not constrained by this field, so a `low` \
                      request can return the same wall of text as a `high` one",
            forward: Forward::Do(
                "Ask for the length you want in the prompt, and set `max_tokens` as the \
                 hard ceiling.",
            ),
        },
        inert: &["\"medium\""],
    },
    Param {
        name: "web_search_options",
        served_by: None,
        note: "",
        support: Support::Rejected {
            because: "there is no web-search tool and the server never leaves the \
                      machine, so a response you believe was informed by a search was not",
            forward: Forward::Do(
                "Omit `tools` to get Ask mode, whose tools search your own graph — that is \
                 the local search this endpoint does have.",
            ),
        },
        inert: &[],
    },
];

impl Support {
    /// The status word `docs/SERVING.md`'s divergence table publishes.
    ///
    /// The four already existed in that table for the tool-calling surfaces
    /// (`supported`, `accepted, not enforced`, `dropped`, and `400` for the
    /// `tools` bounds). The parameter rows join that vocabulary rather than
    /// introducing a second one that a reader would have to reconcile.
    #[must_use]
    pub const fn published(&self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::AcceptedNotEnforced => "accepted, not enforced",
            Self::Dropped => "dropped",
            Self::Rejected { .. } => "400",
        }
    }
}

impl Param {
    /// This parameter's row of the published table, in the exact Markdown
    /// `docs/SERVING.md` carries.
    ///
    /// Generated rather than transcribed, and compared back against the
    /// document by [`tests::the_published_table_is_this_table`]: the failure
    /// mode being designed out is a table row that says one thing while the
    /// code does another, which this repository has shipped six times.
    #[must_use]
    pub fn published_row(&self) -> String {
        let why = self.refusal().unwrap_or_else(|| self.note.to_owned());
        format!(
            "| `{}` | **{}** | {why} |",
            self.name,
            self.support.published()
        )
    }
}

/// The whole published parameter table, header and all.
///
/// `docs/SERVING.md` carries this verbatim.
#[must_use]
pub fn published_table() -> String {
    let mut out = String::from("| parameter | status | what happens |\n| --- | --- | --- |\n");
    for p in OPENAI_CHAT_PARAMS {
        out.push_str(&p.published_row());
        out.push('\n');
    }
    out
}

/// Look one parameter up by wire name.
#[must_use]
pub fn param(name: &str) -> Option<&'static Param> {
    OPENAI_CHAT_PARAMS.iter().find(|p| p.name == name)
}

/// Whether `value` means "the caller expressed no preference" for `param`.
///
/// `null` is inert everywhere: a client library serialising an unset optional
/// has made no decision, and refusing it would be a refusal fired at nobody.
fn is_inert(param: &Param, value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    param.inert.iter().any(|literal| {
        serde_json::from_str::<Value>(literal).is_ok_and(|default| json_eq(value, &default))
    })
}

/// Equality that treats `1` and `1.0` as the same number.
///
/// `serde_json::Value`'s own `PartialEq` does not: it compares the `Number`
/// representations, so a client sending `top_p: 1.0` — which every language
/// with one numeric type does — would miss a `"1"` default and be refused for
/// having asked for the default. That is precisely the fire-on-nobody refusal
/// this module is trying not to ship, and it is one line to avoid.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Enforce the declaration against the request fields Roteiro's own type does
/// not name.
///
/// `extra` is the `#[serde(flatten)]` catch-all on
/// [`crate::types::ChatCompletionRequest`], so it holds exactly the keys that
/// used to be discarded silently. A [`Support::Rejected`] parameter carrying a
/// value that is not inert is refused; everything else — including any key this
/// table has never heard of — passes through untouched.
///
/// # Errors
/// The refusal message for the first refused parameter, in name order. `extra`
/// is a [`BTreeMap`], so "first" is alphabetical and stable rather than
/// dependent on the order the client happened to serialise its JSON.
pub fn check_declared(extra: &BTreeMap<String, Value>) -> Result<(), String> {
    for (name, value) in extra {
        let Some(p) = param(name) else { continue };
        if is_inert(p, value) {
            continue;
        }
        if let Some(refusal) = p.refusal() {
            return Err(refusal);
        }
    }
    Ok(())
}

/// Resolve the generation budget from the two names OpenAI spells it with.
///
/// `named` is the `max_tokens` field; `extra` is the catch-all, where the
/// current spelling `max_completion_tokens` lands. Both name the same number,
/// so the answer is that number however the caller asked for it.
///
/// **Why this and not `#[serde(alias)]`.** The one-line alias was tried first
/// and rejected on evidence: a body carrying both names dies inside serde with
/// ``duplicate field `max_tokens` ``, which axum returns as a `422` of plain
/// text. Three things are wrong with that as a refusal — it is not the
/// `{"error": …}` envelope every other refusal on this endpoint returns, it
/// names `max_tokens` to a caller who sent `max_completion_tokens`, and it says
/// what broke rather than what to do, which `docs/REVIEW_CHECKLIST.md` does not
/// accept as a refusal. Reading the alias out of the catch-all costs this
/// function and buys a `400` that names both spellings and the way forward.
///
/// **Both names is not automatically a conflict.** The module header's rule
/// applies here too: the check keys on the *value*. `null` is no decision, and
/// two spellings carrying the same number are one decision expressed twice —
/// refusing either would be a refusal fired at a caller who was unambiguous.
/// Only two different numbers are ambiguous, and only those are refused;
/// silently picking one of them is the defect class this module exists to
/// remove.
///
/// # Errors
/// A `400` message when the two names disagree, or when the alias carries
/// something that is not a token count.
pub fn generation_budget(
    named: Option<u32>,
    extra: &BTreeMap<String, Value>,
) -> Result<Option<u32>, String> {
    let Some(value) = extra.get("max_completion_tokens").filter(|v| !v.is_null()) else {
        return Ok(named);
    };
    let alias = value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| {
            format!(
                "`max_completion_tokens` is a generation budget and must be a whole \
                 number of tokens: `{value}` is not one. Send a non-negative integer, \
                 exactly as `max_tokens` takes one."
            )
        })?;
    match named {
        Some(named) if named != alias => Err(format!(
            "`max_completion_tokens` and `max_tokens` are the same generation budget \
             under two names, and this request sets them to {alias} and {named}, so \
             there is no way to tell which budget you meant. Send one of them — \
             `max_completion_tokens` is OpenAI's current spelling and `max_tokens` \
             the one it deprecated, so prefer the first."
        )),
        _ => Ok(Some(alias)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Forward, OPENAI_CHAT_PARAMS, Param, Support, check_declared, param};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    /// Every parameter OpenAI's request body carries, as read on 2026-08-20 from
    /// the two sources named in the module header.
    ///
    /// The count is asserted rather than merely the contents, because the way
    /// this table rots is by **omission** — a new OpenAI parameter that nobody
    /// adds a row for is exactly the silent-drop defect #488 is about, one
    /// release later. A bare set comparison against the table itself would be
    /// vacuous (the table would be compared to itself); this is an independent
    /// transcription, so a row deleted from `OPENAI_CHAT_PARAMS` fails here.
    const OPENAI_WIRE_NAMES: &[&str] = &[
        "audio",
        "frequency_penalty",
        "function_call",
        "functions",
        "logit_bias",
        "logprobs",
        "max_completion_tokens",
        "max_tokens",
        "messages",
        "metadata",
        "modalities",
        "model",
        "moderation",
        "n",
        "parallel_tool_calls",
        "prediction",
        "presence_penalty",
        "prompt_cache_key",
        "prompt_cache_options",
        "prompt_cache_retention",
        "reasoning_effort",
        "response_format",
        "safety_identifier",
        "seed",
        "service_tier",
        "stop",
        "store",
        "stream",
        "stream_options",
        "temperature",
        "tool_choice",
        "tools",
        "top_logprobs",
        "top_p",
        "user",
        "verbosity",
        "web_search_options",
    ];

    #[test]
    fn the_table_covers_openai_s_request_body_exactly() {
        let declared: Vec<&str> = OPENAI_CHAT_PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            declared, OPENAI_WIRE_NAMES,
            "the declared table and OpenAI's request body have diverged — a name in \
             one and not the other is an undeclared parameter, which is #488"
        );
        assert_eq!(OPENAI_WIRE_NAMES.len(), 37, "the read count on 2026-08-20");
    }

    /// Sorted, so the table and the published one compare row for row and a new
    /// row lands where a reader looks for it rather than at the bottom.
    /// The struct and the table must name the same fields, **in both
    /// directions**.
    ///
    /// This is the guard against the way this defect returns. A parameter added
    /// to [`crate::types::ChatCompletionRequest`] without a row here would be
    /// served while undeclared; a row marked `supported` here with no field
    /// behind it would publish a capability that does not exist. Either
    /// direction alone would let one of those through, so both are asserted.
    ///
    /// The struct's fields are read out of the **source**, because
    /// `ChatCompletionRequest` is `Deserialize`-only: there is no value to
    /// reflect over and no `Serialize` to enumerate from, and deriving one
    /// purely to be introspected would change the type to suit its test. The
    /// field block is located by the struct's opening line and read to its
    /// closing brace at the same indentation, rather than by scanning the whole
    /// file for `pub …:` — the first version of this test did the latter and
    /// would have happily matched fields of `ToolSpec` and `FunctionSpec` a few
    /// lines below.
    #[test]
    fn the_struct_and_the_table_declare_the_same_fields() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/types.rs");
        let text = std::fs::read_to_string(&source).expect("the request type's source");
        let body = text
            .split_once("pub struct ChatCompletionRequest {")
            .expect("the request type must still be named this")
            .1
            .split_once("\n}\n")
            .expect("an unterminated struct would not compile")
            .0;

        let mut fields: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub "))
            .filter_map(|l| l.split_once(':'))
            .map(|(name, _)| name)
            // The catch-all is the mechanism, not a parameter: it is precisely
            // the keys that have no field, so declaring it would be circular.
            .filter(|name| *name != "extra")
            .collect();
        fields.sort_unstable();
        assert!(
            fields.len() >= 8,
            "read {} fields out of {} — the struct's shape has moved and this \
             test is now sampling something cheaper than what it claims to check",
            fields.len(),
            source.display()
        );

        // `Param::field`, not `Param::name`: one wire name is served by a
        // field spelled differently (`max_completion_tokens` by `max_tokens`),
        // and the table says so on the row itself. Reading the mapping is what
        // keeps this an assertion about *every* row rather than an assertion
        // with one row excused from it — the excused row is how the next one
        // gets excused too. Two rows may name one field, hence the `dedup`.
        let mut declared: Vec<&str> = OPENAI_CHAT_PARAMS
            .iter()
            .filter(|p| matches!(p.support, Support::Supported | Support::AcceptedNotEnforced))
            .map(Param::field)
            .collect();
        declared.sort_unstable();
        declared.dedup();

        assert_eq!(
            fields, declared,
            "the request type and the declared table disagree. A field with no \
             row is a parameter served but undeclared — which is #488 — and a \
             row with no field claims a capability that is not there"
        );
    }

    /// A `served_by` pointer is the same kind of claim as a way forward, and
    /// gets the same treatment: it must name a row that exists and that is
    /// actually served.
    ///
    /// Without this the mapping the guard above now reads could itself be the
    /// hiding place — a row could declare itself served by a field nobody has,
    /// and `the_struct_and_the_table_declare_the_same_fields` would fail with a
    /// message about the struct rather than about the lie on the row.
    #[test]
    fn a_served_by_pointer_names_a_parameter_that_serves_itself() {
        for p in OPENAI_CHAT_PARAMS {
            let Some(target) = p.served_by else { continue };
            assert!(
                matches!(p.support, Support::Supported | Support::AcceptedNotEnforced),
                "`{}` is {:?} yet names a field that serves it; only a served \
                 parameter has a field at all",
                p.name,
                p.support
            );
            assert_ne!(target, p.name, "`{}` is served by itself", p.name);
            let referenced = param(target).unwrap_or_else(|| {
                panic!(
                    "`{}` is served by `{target}`, which is in no row of this table",
                    p.name
                )
            });
            assert!(
                matches!(
                    referenced.support,
                    Support::Supported | Support::AcceptedNotEnforced
                ),
                "`{}` is served by `{target}`, which is itself {:?} — a field \
                 that is not served cannot serve a second name",
                p.name,
                referenced.support
            );
            assert!(
                referenced.served_by.is_none(),
                "`{}` is served by `{target}`, which is served by something else \
                 again; this is a chain, and only a direct pointer is checked",
                p.name
            );
        }
    }

    /// The alias `generation_budget` honours and the alias the table declares
    /// must be the same one.
    ///
    /// The resolver names `max_completion_tokens` in code, which is the one
    /// place in this module a wire name is written outside the table. This is
    /// the thread between them: declare a second budget alias in the table and
    /// this reddens, because the resolver would not read it.
    #[test]
    fn the_budget_alias_the_resolver_honours_is_the_one_the_table_declares() {
        let aliases: Vec<&str> = OPENAI_CHAT_PARAMS
            .iter()
            .filter(|p| p.served_by == Some("max_tokens"))
            .map(|p| p.name)
            .collect();
        assert_eq!(
            aliases,
            ["max_completion_tokens"],
            "`generation_budget` resolves exactly this one alias of the budget; \
             a row declaring another would be published as served and never read"
        );
    }

    /// `docs/SERVING.md` publishes this table, so the document and the code are
    /// one contract and this is what holds them together.
    ///
    /// The page is a **site page** (`site-page: serving`): it publishes to
    /// roteiro.dev/serving and is read by a client author deciding what to
    /// send — someone who cannot check it, because they are reading it in order
    /// to find out. That is the worst possible place for a row that says
    /// `dropped` where the code says `400`, and this repository has shipped
    /// doc-disagrees-with-code six times.
    ///
    /// **The generated block, not the file.** Compared whole against
    /// [`published_table`] rather than row-by-row lookup: a row-at-a-time check
    /// passes when the document has an *extra* row, and an extra row is a
    /// published capability that does not exist. `cargo run -p rto-serve
    /// --example print_declared_table` regenerates the block when this fails.
    #[test]
    fn the_published_table_is_this_table() {
        let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/SERVING.md")
            .canonicalize()
            .expect("the page that publishes the declaration must exist");
        let text = std::fs::read_to_string(&doc).expect("readable");

        let expected = super::published_table();
        let header = expected
            .lines()
            .next()
            .expect("the generated table has a header row");
        let start = text.find(header).unwrap_or_else(|| {
            panic!(
                "{} no longer publishes the parameter table at all — the page a \
                 client author reads to find the boundary must show it",
                doc.display()
            )
        });
        // The table ends at the first line that is not a row of it.
        let mut published = String::new();
        for line in text[start..].lines().take_while(|l| l.starts_with('|')) {
            published.push_str(line);
            published.push('\n');
        }

        assert_eq!(
            published,
            expected,
            "the parameter table in {} has drifted from the code that enforces \
             it; regenerate it with `cargo run -p rto-serve --example \
             print_declared_table`",
            doc.display()
        );
    }

    /// One source of truth per published row.
    ///
    /// A refused parameter's cell is its refusal message; giving it a `note` as
    /// well would create a second explanation that nothing compares against the
    /// first, which is how a table starts disagreeing with itself.
    #[test]
    fn a_refused_row_carries_no_second_explanation() {
        for p in OPENAI_CHAT_PARAMS {
            if matches!(p.support, Support::Rejected { .. }) {
                assert!(
                    p.note.is_empty(),
                    "`{}` is refused, so its published cell is the refusal — the \
                     `note` beside it would never be read",
                    p.name
                );
            } else {
                assert!(
                    p.note.len() > 20,
                    "`{}` publishes an empty cell; a reader learns nothing from it",
                    p.name
                );
            }
        }
    }

    #[test]
    fn the_table_is_sorted_by_name() {
        let mut sorted: Vec<&str> = OPENAI_CHAT_PARAMS.iter().map(|p| p.name).collect();
        sorted.sort_unstable();
        let actual: Vec<&str> = OPENAI_CHAT_PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(actual, sorted);
    }

    #[test]
    fn no_name_is_declared_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for p in OPENAI_CHAT_PARAMS {
            assert!(seen.insert(p.name), "`{}` is declared twice", p.name);
        }
    }

    /// The load-bearing subset the issue names must all be refused.
    ///
    /// Named one by one rather than counted: this is the list #488 was filed
    /// over, and a change that quietly demoted one of them to `dropped` would
    /// re-open the exact hole without failing anything else here.
    #[test]
    fn every_parameter_the_issue_names_is_refused() {
        for name in [
            "seed",
            "stop",
            "response_format",
            "n",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "logit_bias",
        ] {
            let p = param(name).expect("declared");
            assert!(
                matches!(p.support, Support::Rejected { .. }),
                "#488 names `{name}` as a silently wrong answer; it is {:?}",
                p.support
            );
        }
    }

    /// What was refused, why, and what to do next — the three parts
    /// `docs/REVIEW_CHECKLIST.md` requires, asserted on the rendered string a
    /// caller actually receives rather than on the parts in isolation.
    #[test]
    fn every_refusal_says_what_why_and_what_next() {
        for p in OPENAI_CHAT_PARAMS {
            let Support::Rejected { because, forward } = p.support else {
                assert!(p.refusal().is_none(), "`{}` is not refused", p.name);
                continue;
            };
            let message = p.refusal().expect("a rejected parameter has a refusal");
            assert!(
                message.contains(&format!("`{}`", p.name)),
                "a refusal must name what was refused: {message}"
            );
            assert!(
                because.len() > 30,
                "`{}` refuses without saying what is actually true instead",
                p.name
            );
            assert!(
                forward.prose().len() > 30,
                "`{}` names the obstacle and not the way forward",
                p.name
            );
            assert!(
                message.ends_with('.'),
                "a refusal is prose a person reads: {message}"
            );
        }
    }

    /// A `Forward::Do` that redirects to another parameter must redirect to one
    /// that exists.
    ///
    /// "Send `max_tokens` instead" is only useful while `max_tokens` is a thing
    /// this endpoint serves. Renaming or dropping a supported parameter without
    /// this would leave refusals confidently pointing at nothing — the wrong
    /// answer that reads like a right one.
    #[test]
    fn a_way_forward_never_points_at_a_parameter_that_does_not_exist() {
        for p in OPENAI_CHAT_PARAMS {
            let Support::Rejected {
                forward: Forward::Do(prose),
                ..
            } = p.support
            else {
                continue;
            };
            for quoted in backticked(prose) {
                // Only names that look like wire parameters are checked; prose
                // also quotes values (`temperature: 0`) and markup (`<think>`).
                let Some(referenced) = param(&quoted) else {
                    continue;
                };
                assert_ne!(
                    referenced.name, p.name,
                    "`{}`'s way forward is to send `{}`",
                    p.name, quoted
                );
                assert!(
                    !matches!(referenced.support, Support::Dropped),
                    "`{}` redirects to `{}`, which is itself dropped",
                    p.name,
                    quoted
                );
            }
        }
    }

    /// Backticked spans of `prose`, with any trailing `: value` stripped so
    /// ``temperature: 0`` is recognised as a reference to `temperature`.
    fn backticked(prose: &str) -> Vec<String> {
        prose
            .split('`')
            .skip(1)
            .step_by(2)
            .map(|span| span.split(':').next().unwrap_or(span).trim().to_owned())
            .collect()
    }

    /// Only a refused parameter needs inert values, and every inert value must
    /// parse — a typo in a JSON literal would silently never match, arming a
    /// refusal against a caller who sent the default.
    #[test]
    fn inert_defaults_are_only_on_refused_parameters_and_all_parse() {
        for p in OPENAI_CHAT_PARAMS {
            if !matches!(p.support, Support::Rejected { .. }) {
                assert!(
                    p.inert.is_empty(),
                    "`{}` is not refused, so its inert defaults are never read",
                    p.name
                );
            }
            for literal in p.inert {
                let parsed = serde_json::from_str::<Value>(literal).unwrap_or_else(|e| {
                    panic!("`{}`'s inert `{literal}` is not JSON: {e}", p.name)
                });
                assert!(
                    !parsed.is_null(),
                    "`{}` lists `null` as inert; it already is, everywhere",
                    p.name
                );
            }
        }
    }

    fn extra(name: &str, value: Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(name.to_owned(), value)])
    }

    /// The whole point of keying on the value: a client library that serialises
    /// its defaults must not be refused for a decision it never made.
    #[test]
    fn a_default_valued_parameter_is_not_a_decision_and_is_not_refused() {
        for p in OPENAI_CHAT_PARAMS {
            if !matches!(p.support, Support::Rejected { .. }) {
                continue;
            }
            assert!(
                check_declared(&extra(p.name, Value::Null)).is_ok(),
                "`{}` was refused for being present as null — a client library \
                 serialising an unset optional decided nothing",
                p.name
            );
            for literal in p.inert {
                let value: Value = serde_json::from_str(literal).expect("parses");
                assert!(
                    check_declared(&extra(p.name, value)).is_ok(),
                    "`{}` was refused for carrying OpenAI's own default `{literal}` — \
                     which is what Roteiro already does",
                    p.name
                );
            }
        }
    }

    /// `1` and `1.0` are the same request. A language with one numeric type
    /// sends the second.
    #[test]
    fn a_float_spelling_of_an_integer_default_is_still_the_default() {
        assert!(check_declared(&extra("top_p", json!(1.0))).is_ok());
        assert!(check_declared(&extra("n", json!(1.0))).is_ok());
        assert!(check_declared(&extra("frequency_penalty", json!(0.0))).is_ok());
        // …and the corresponding real decisions still are refused, so the
        // widening above did not simply switch the check off.
        assert!(check_declared(&extra("top_p", json!(0.9))).is_err());
        assert!(check_declared(&extra("n", json!(2.0))).is_err());
    }

    /// Every refused parameter, carrying a value that is a real decision,
    /// produces a refusal that names it.
    ///
    /// Table-driven over the whole declaration rather than a handful of
    /// examples: a row added later is covered the moment it is added, which is
    /// the property that keeps this from being the tenth unchecked table.
    #[test]
    fn a_meaningfully_set_refused_parameter_is_refused_by_name() {
        for p in OPENAI_CHAT_PARAMS {
            if !matches!(p.support, Support::Rejected { .. }) {
                continue;
            }
            let err = check_declared(&extra(p.name, decisive_value(p)))
                .expect_err(&format!("`{}` is declared 400 but was accepted", p.name));
            assert!(
                err.contains(&format!("`{}`", p.name)),
                "the refusal for `{}` does not name it: {err}",
                p.name
            );
        }
    }

    /// A value for `p` that no client sends by accident: never `null`, and
    /// never equal to one of `p`'s inert defaults.
    fn decisive_value(p: &Param) -> Value {
        for candidate in [
            json!(7),
            json!("roteiro"),
            json!(["roteiro"]),
            json!({"type": "roteiro"}),
            json!(true),
        ] {
            if !super::is_inert(p, &candidate) {
                return candidate;
            }
        }
        unreachable!("`{}` treats every candidate as inert", p.name)
    }

    /// The other half: a dropped parameter is *accepted*, however it is set.
    ///
    /// This is the half that would break working callers if
    /// `deny_unknown_fields` had been the answer, so it is asserted rather than
    /// assumed.
    #[test]
    fn a_dropped_parameter_is_accepted_however_it_is_set() {
        for p in OPENAI_CHAT_PARAMS {
            if !matches!(p.support, Support::Dropped) {
                continue;
            }
            for value in [
                json!("anything"),
                json!(true),
                json!({"k": "v"}),
                Value::Null,
            ] {
                assert!(
                    check_declared(&extra(p.name, value.clone())).is_ok(),
                    "`{}` is declared dropped but refused `{value}` — a client sending \
                     it by habit has no correctness problem to fix",
                    p.name
                );
            }
        }
    }

    /// The keys clients send by habit, named individually. These are the callers
    /// `deny_unknown_fields` would have broken, and the reason it was not used.
    #[test]
    fn the_habitual_keys_still_work() {
        let habitual = BTreeMap::from([
            ("user".to_owned(), json!("u-42")),
            ("store".to_owned(), json!(true)),
            ("metadata".to_owned(), json!({"run": "nightly"})),
            ("service_tier".to_owned(), json!("auto")),
            ("prompt_cache_key".to_owned(), json!("cache-1")),
            ("safety_identifier".to_owned(), json!("s-9")),
        ]);
        assert_eq!(check_declared(&habitual), Ok(()));
    }

    /// A key in no row of the table is ignored, not refused — the declared
    /// decision from the module header, asserted so that "we chose to ignore
    /// these" cannot silently become "we forgot".
    #[test]
    fn a_key_this_table_has_never_heard_of_is_ignored() {
        assert_eq!(check_declared(&extra("x_roteiro_vendor", json!(1))), Ok(()));
        assert_eq!(
            check_declared(&extra("some_parameter_openai_ships_next_year", json!("v"))),
            Ok(())
        );
    }

    /// With several refused parameters set, the refusal is the alphabetically
    /// first — the same one every time, not whichever the client's JSON
    /// serialiser happened to emit first.
    #[test]
    fn the_refusal_is_stable_when_several_parameters_are_refused() {
        let many = BTreeMap::from([
            ("seed".to_owned(), json!(42)),
            ("n".to_owned(), json!(3)),
            ("stop".to_owned(), json!(["END"])),
        ]);
        let err = check_declared(&many).expect_err("all three are refused");
        assert!(err.starts_with("`n`"), "{err}");
    }

    /// The messages a caller most often meets, quoted whole.
    ///
    /// The table-driven tests above prove every row is refused and names
    /// itself; they cannot tell whether the sentence is any use to a person.
    /// These two are the ones #488 calls "most likely to be load-bearing in an
    /// automated caller", so their wording is pinned.
    #[test]
    fn the_two_most_load_bearing_refusals_read_as_advice() {
        let seed = param("seed").expect("declared").refusal().expect("refused");
        assert!(
            seed.contains("not reproducible") || seed.contains("need not agree"),
            "{seed}"
        );
        assert!(seed.contains("temperature: 0"), "{seed}");

        let fmt = param("response_format")
            .expect("declared")
            .refusal()
            .expect("refused");
        assert!(fmt.contains("prose"), "{fmt}");
        assert!(fmt.contains("Ask for JSON in the prompt"), "{fmt}");
    }

    // --- the generation budget under its two names -------------------------

    fn budget(named: Option<u32>, alias: Option<Value>) -> Result<Option<u32>, String> {
        let mut extra = BTreeMap::new();
        if let Some(v) = alias {
            extra.insert("max_completion_tokens".to_owned(), v);
        }
        super::generation_budget(named, &extra)
    }

    /// The change this row records: a client on OpenAI's current spelling gets
    /// the budget it asked for, rather than a refusal pointing it at the
    /// spelling OpenAI deprecated.
    #[test]
    fn the_current_spelling_of_the_budget_is_honoured() {
        assert_eq!(budget(None, Some(json!(256))), Ok(Some(256)));
        assert!(
            param("max_completion_tokens")
                .expect("declared")
                .refusal()
                .is_none(),
            "the current name of a budget this endpoint honours must not be a 400"
        );
    }

    /// Neither name set is still no budget: the caller gets the default, and
    /// nothing here invents one.
    #[test]
    fn neither_name_leaves_the_budget_unset() {
        assert_eq!(budget(None, None), Ok(None));
        assert_eq!(budget(Some(64), None), Ok(Some(64)));
    }

    /// `null` is no decision here either, exactly as it is nowhere else in this
    /// module.
    #[test]
    fn a_null_alias_is_not_a_budget() {
        assert_eq!(budget(Some(64), Some(Value::Null)), Ok(Some(64)));
        assert_eq!(budget(None, Some(Value::Null)), Ok(None));
    }

    /// Both names, one number: unambiguous, so it is answered rather than
    /// refused. A refusal here would fire at a caller who decided exactly once.
    #[test]
    fn both_names_agreeing_is_one_decision_expressed_twice() {
        assert_eq!(budget(Some(128), Some(json!(128))), Ok(Some(128)));
    }

    /// Both names, two numbers: nothing can tell which was meant, and picking
    /// one silently is the defect class this module exists to remove.
    #[test]
    fn both_names_disagreeing_is_refused_and_names_both() {
        let err = budget(Some(10), Some(json!(20))).expect_err("two budgets is ambiguous");
        assert!(err.contains("`max_completion_tokens`"), "{err}");
        assert!(err.contains("`max_tokens`"), "{err}");
        assert!(err.contains("10") && err.contains("20"), "{err}");
        assert!(err.contains("Send one of them"), "the way forward: {err}");
        assert!(
            err.ends_with('.'),
            "a refusal is prose a person reads: {err}"
        );
    }

    /// A budget that is not a token count is refused by name rather than
    /// silently becoming the default — which is the same silent-wrong-answer
    /// shape, one layer down.
    #[test]
    fn a_budget_that_is_not_a_token_count_is_refused() {
        for value in [
            json!("512"),
            json!(-1),
            json!(1.5),
            json!([512]),
            json!(u64::from(u32::MAX) + 1),
        ] {
            let err = budget(None, Some(value.clone()))
                .expect_err(&format!("`max_completion_tokens: {value}` is not a budget"));
            assert!(err.contains("`max_completion_tokens`"), "{err}");
            assert!(err.ends_with('.'), "{err}");
        }
    }
}
