//! Graph-tool auto-registration (ADR-0006): hand the served model Roteiro's
//! graph tools so it can query *this codebase* while answering.
//!
//! Decoupled from the graph via the [`ToolRegistry`] trait — rto-serve owns the
//! protocol (advertise tools in a system prompt, parse the model's `<tool_call>`
//! output, run a bounded execute-and-feed-back loop), while the caller (roteiro)
//! backs the tools with the actual graph store. llama.cpp/`llama-cpp-2` provides
//! no native tool support, so the protocol is hand-rolled and model-agnostic:
//! any instruction-following model that emits the documented `<tool_call>` form
//! works, without a model-specific template. A model that emits its *own* form
//! instead works too, provided that form is a [`Dialect`] — the envelope is a
//! property of the dialect (#592), so `Dialect::ALL` is the whole list of what
//! can be read and nothing sits above it deciding what is worth reading.
//!
//! # Two kinds of tool, and the boundary between them
//!
//! A request may also carry the **client's** own `tools` (OpenAI's `tools` field,
//! #485). The two sets are not interchangeable, and conflating them is the trap:
//!
//! > **Roteiro never executes a client's tool.** It returns the call and stops,
//! > with `finish_reason: "tool_calls"`, and the client runs it.
//!
//! That is the security-relevant property of this design, and it is why client
//! tools need no consent gate or authorisation story — unlike almost every other
//! capability in this project. It is a stronger guarantee than a gate: there is
//! no code path that reaches a client's tool, so there is nothing to gate.
//! **Anyone changing [`chat_with_client_tools`] is changing that guarantee.**
//!
//! # A tool call is never the user's answer (#489)
//!
//! The other property, in the other direction, and the one the loop kept losing:
//!
//! > **Where Roteiro advertised tools, content carrying tool-call markup is
//! > never returned as the user's answer.**
//!
//! The qualifier is load-bearing, and every statement of this property carries
//! it. `<tool_call>` means "I am calling a tool" only because
//! [`tool_system_prompt`] said so, and that prompt is sent only when something
//! was advertised; a run that advertised nothing is a plain [`Engine::chat`]
//! whose output passes through untouched ([`Ending::Untooled`]). Every other run
//! is covered without qualification — every Ask, and every request carrying
//! `tools`.
//!
//! Every way out of [`chat_with_client_tools`] goes through [`finish`], which is
//! the only place a [`ToolLoopOutcome`] is built and the only place a generation
//! is declared to be prose the user may read. So a new exit cannot leak markup by
//! forgetting a check — it can only fail to compile, because leaving means naming
//! which [`Ending`] it is.
//!
//! This replaces three separate return sites that each decided for themselves,
//! and that is what let the same defect land three ways: an unreadable dialect
//! (the issue as filed), a `<tool_call>` the model never closed, and a call in
//! the generation *after* the round budget ran out.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::{ChatRequest, Completion, Engine, EngineError, FinishReason, Message};
use crate::thinking::{self, Unterminated};

/// Max **bytes** of a tool result fed back into the conversation, so a large
/// query result cannot blow the context window. (`truncate` caps by UTF-8 bytes,
/// backing up to a char boundary.)
///
/// One of the three constants [`crate::budget`] prices: this many bytes,
/// `server::MAX_TOOL_ROUNDS` times, is the tool-result half of the prompt a
/// request grows, and raising it past the budget there is a compile error
/// (#556).
pub(crate) const MAX_TOOL_RESULT: usize = 4000;

/// A callable tool advertised to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The tool's function name (what the model emits to call it).
    pub name: String,
    /// One-line description of what it does and when to use it.
    pub description: String,
    /// JSON Schema (an `object`) describing the tool's arguments.
    ///
    /// `"additionalProperties": false` here is **enforced, not decorative**: the
    /// tool loop refuses a call carrying a key `properties` does not declare
    /// rather than dropping it. See [`unknown_argument`] for why a dropped key is
    /// worse than a refused one.
    pub parameters: serde_json::Value,
}

/// A set of tools the served model may call. Implemented by the caller over its
/// own data source (roteiro backs it with the graph store).
pub trait ToolRegistry: Send + Sync {
    /// The tools to advertise to the model.
    fn tools(&self) -> Vec<ToolDef>;

    /// Execute `name` with the given JSON `arguments`, returning a text result.
    ///
    /// # Errors
    /// Returns human-readable error text (fed back to the model, not fatal) when
    /// the tool is unknown, the arguments are invalid, or the query fails.
    fn call(&self, name: &str, arguments: &serde_json::Value) -> Result<String, String>;

    /// The named projects this registry can serve, for `GET /v1/projects` — the
    /// hosted repos of a workspace (ADR-0008), so a client can discover them
    /// without a model round-trip. Defaults to none (a single, unnamed source).
    fn projects(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A tool call parsed out of a model's generation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCall {
    name: String,
    arguments: serde_json::Value,
}

/// A call against one of the **client's** tools, returned to the client instead
/// of being executed. See this module's documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientToolCall {
    /// Correlation id the client echoes back as `tool_call_id`.
    pub id: String,
    /// The client tool's name.
    pub name: String,
    /// The arguments the model produced.
    pub arguments: serde_json::Value,
}

/// The result of a tool-loop run: the model's completion, plus any calls against
/// the client's tools that ended it.
///
/// Built in exactly one place — [`finish`] — which is what holds #489's property:
/// when `client_tool_calls` is empty, `completion.content` is prose the user may
/// read as the answer, and it carries no tool-call markup.
///
/// **One exception, and it is the only one:** a run that advertised no tools at
/// all — neither a registry's nor the client's — was never sent the tool system
/// prompt, so `<tool_call>` in its output is text the model wrote rather than a
/// call Roteiro asked for, and it passes through deliberately
/// ([`Ending::Untooled`] carries the reasoning). Read the guarantee as
/// conditional on the *call*, not on the outcome: advertise a tool and it holds
/// without qualification, which covers every Ask and every request carrying
/// `tools`.
#[derive(Debug, Clone)]
pub struct ToolLoopOutcome {
    /// The last generation. On a client-tool turn its `content` is the raw text
    /// the model emitted (the `<tool_call>` markup); the wire layer replaces it
    /// with `null` and the structured calls.
    pub completion: Completion,
    /// Non-empty exactly when the loop stopped because the model called a client
    /// tool. The wire layer renders `finish_reason: "tool_calls"`.
    pub client_tool_calls: Vec<ClientToolCall>,
}

/// What to do with the calls in one generated turn.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// Every call is Roteiro's to run: execute them and feed the results back.
    Execute,
    /// At least one call names a client tool: return them **all**, run **none**.
    ReturnAll,
}

/// Decide a turn's disposition.
///
/// > **If any call in a turn names a client tool, return all of them and execute
/// > none.**
///
/// Not "execute the graph half and return the client half". The client would
/// receive a `tool_calls` response for an assistant turn it cannot reconstruct:
/// it never saw the graph call or its result, so its follow-up would be missing
/// turns and its `tool_call_id` correlation would be against a transcript that
/// never existed on its side. Stop-and-return-all is the only shape that keeps
/// the client's transcript reconstructible.
///
/// Under suppression ([`chat_with_client_tools`]) a mixed turn is rare, because
/// the graph tools are not advertised when client tools are present. The rule is
/// written down anyway: rare is where silent corruption lives.
fn disposition(calls: &[ToolCall], client_names: &HashSet<&str>) -> Disposition {
    if calls.iter().any(|c| client_names.contains(c.name.as_str())) {
        Disposition::ReturnAll
    } else {
        Disposition::Execute
    }
}

/// The refusal a call carrying an argument key the tool does not declare gets,
/// or `None` for one whose arguments the schema admits.
///
/// > **An argument key a tool does not declare is refused, never dropped.**
///
/// The defect this closes is not a mistyped key; it is what a dropped key leaves
/// behind. Roteiro's two tool surfaces spell one of `debt`'s arguments
/// differently — `kind` on MCP, `categories` here — and neither surface used to
/// reject the other's name, so a model that reached `debt` through one surface
/// with the other's spelling was handed **every marker in the repository,
/// presented as the filtered set it asked for**. No error, and nothing in the
/// answer to tell it apart from a real one. Every mistyped, hallucinated or
/// cross-surface key has that shape: the model asked a narrower question than the
/// one it got an answer to, and only the argument object knows.
///
/// # The declaration is what enforces it
///
/// This reads `additionalProperties: false` off the **composed** schema the
/// registry advertises — the one the model was shown, `project` spliced in and
/// all — rather than off a list kept beside it. So a tool that closes its
/// argument object is enforced by the same words that told the model what it
/// takes, and the two cannot drift apart; a registry that declares nothing is
/// untouched, which is what keeps this a property a tool opts into rather than a
/// rule imposed on every implementor of [`ToolRegistry`].
///
/// The message names the keys the tool *does* take, for the reason serde's own
/// does ("unknown field `categories`, expected one of `kind`, `project`"): a
/// model that is told only that it was wrong will guess again, and the surface it
/// guessed from is the one that misled it.
fn unknown_argument(def: &ToolDef, arguments: &serde_json::Value) -> Option<String> {
    if def.parameters.get("additionalProperties") != Some(&serde_json::Value::Bool(false)) {
        return None;
    }
    let sent = arguments.as_object()?;
    let declared = def.parameters.get("properties").and_then(|p| p.as_object());
    let is_declared = |key: &String| declared.is_some_and(|d| d.contains_key(key));
    let unknown: Vec<&str> = sent
        .keys()
        .filter(|k| !is_declared(k))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return None;
    }
    let named = |keys: &[&str]| {
        keys.iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let takes: Vec<&str> = declared.map_or_else(Vec::new, |d| {
        d.keys().map(String::as_str).collect::<Vec<_>>()
    });
    let expected = if takes.is_empty() {
        "no arguments at all".to_owned()
    } else {
        format!("only {}", named(&takes))
    };
    Some(format!(
        "unknown argument {} — `{}` takes {expected}. Nothing was run: an argument this \
         tool does not declare is refused rather than ignored, because ignoring it would \
         answer a narrower question than the one you asked and give you no way to tell.",
        named(&unknown),
        def.name,
    ))
}

/// Why a run of the tool loop ended — the input to [`finish`].
///
/// The variants are exhaustive over the ways a generation leaves this module,
/// and each names a *judgement* rather than a shape: this is the answer, the
/// client runs these, the model was calling a tool and the call did not arrive,
/// the budget ran out. Adding a fifth exit to [`chat_with_client_tools`] means
/// naming which of these it is, which is the point: there is no way to leave
/// without saying what the generation was.
enum Ending {
    /// The generation carried no tool call: it is the model's answer.
    ///
    /// A *claim*, which [`finish`] checks — see its documentation.
    Answer,
    /// The turn named a client tool. Every call is handed back, none is run.
    ClientCalls(Vec<ToolCall>),
    /// The model was calling a tool and the call did not arrive intact.
    Unfinished(Unfinished),
    /// The generation opened a `<think>` block and never closed it, so it never
    /// started an answer at all (#583).
    ///
    /// A sibling of [`Ending::Unfinished`] and named to rhyme with it: both are
    /// a marker that was opened and never closed, and both mean the generation
    /// stopped before it became a reply. Established by [`read_reasoning`] the
    /// moment the generation arrives, which is why no arm of [`finish`] has to
    /// look for it.
    Unanswered(Unterminated),
    /// The round budget ran out with the model still calling Roteiro's tools,
    /// so it never reached an answer.
    Exhausted,
    /// Nothing was advertised, so [`tool_system_prompt`] was never injected and
    /// this is a plain [`Engine::chat`] passed straight through.
    ///
    /// The one ending whose content is *not* read for markup, named here rather
    /// than left as an omission at the return site. `<tool_call>` means "I am
    /// calling a tool" only because the tool prompt said so; a request that
    /// advertised no tools never sent that prompt, so the marker is ordinary
    /// text — a general-backend client asking a model what a tool call looks
    /// like must get its answer, not a refusal. Nothing is executable on this
    /// path either, so there is no call to lose.
    Untooled,
}

/// Tool-call markup that never became a runnable call, and why — the three want
/// different sentences from the refusal, because they have different ways
/// forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unfinished {
    /// The call's own dialect could not prove it whole ([`Envelope`]), and
    /// generation stopped at the token cap.
    ///
    /// This is **evidence, not inference**: [`FinishReason::Length`] is set by
    /// the decode loop only when `max_tokens` was reached before an end-of-
    /// generation token, so the engine is reporting the truncation rather than
    /// the parser guessing at it.
    CutAtTokenCap,
    /// The same unproven call, but the model chose to stop: it emitted
    /// end-of-generation part-way through writing one.
    CutShort,
    /// The call arrived whole and no [`Dialect`] understood its body — the shape
    /// #489 opened on, still reachable by a dialect Roteiro has not learned.
    Unreadable,
}

/// The two budgets a refusal has to be able to name, carried to [`finish`] so
/// the message can quote the actual number rather than gesture at a limit.
#[derive(Clone, Copy)]
struct Limits {
    /// The request's `max_tokens` — the cap a mid-call truncation hit.
    max_tokens: u32,
    /// The tool-round budget for this request (`server::MAX_TOOL_ROUNDS`).
    rounds: usize,
}

/// Roteiro's own voice in the assistant's slot.
///
/// A refusal is rendered by the Ask panel exactly where the model's answer would
/// go, so it says who is speaking: a reader who takes this for the model's words
/// learns something false about their model.
const REFUSAL: &str = "Roteiro: ";

/// Build the run's [`ToolLoopOutcome`] — **the only place one is built**, and so
/// the only place a generation becomes the user's answer.
///
/// The property this exists to hold, and #489's whole subject:
///
/// > **Where Roteiro advertised tools, content carrying tool-call markup is
/// > never returned as the user's answer.**
///
/// Under [`tool_system_prompt`] the marker `<tool_call>` means the model was
/// *calling a tool*: the prompt told it to "reply with ONLY a tool call ... in
/// exactly this form". A generation carrying that marker is therefore not an
/// answer, whatever prose it also contains — prose written on the way into a
/// call the model never completed is an answer it had not finished forming. So
/// this **refuses**, naming the budget that ran out, rather than stripping the
/// markup and passing the remainder off as a reply. Stripping would be the
/// silent downgrade `docs/REVIEW_CHECKLIST.md` §Refusals forbids: an incomplete
/// thing presented as the whole one, with the evidence removed.
///
/// [`Ending::Answer`] is a claim by the caller and this function is what checks
/// it: a caller that mis-reads a call as an answer gets the refusal, not the
/// markup. That is why the check lives here rather than at each return site — a
/// check every site must remember to call is how #489's next leak arrives. The
/// single exception is [`Ending::Untooled`], named rather than omitted — it is
/// the "where Roteiro advertised tools" qualifier above, and the only thing that
/// makes that statement a qualified one.
///
/// Only `content` is replaced. The token counts stay as generated (the model
/// really did spend them) and so does `finish_reason`, which for a truncation is
/// already `length` and tells a machine client what happened.
fn finish(completion: Completion, ending: Ending, limits: Limits) -> ToolLoopOutcome {
    let refusal = match ending {
        Ending::ClientCalls(calls) => {
            // The wire layer renders `content: null` alongside these, so the raw
            // markup in `completion.content` is not published. That holds by
            // composition rather than by a rule each renderer remembers: content
            // is only ever rendered when `client_tool_calls` is empty, and an
            // outcome with no client calls came through one of the arms below.
            return ToolLoopOutcome {
                completion,
                client_tool_calls: into_client_calls(calls),
            };
        }
        Ending::Untooled => None,
        Ending::Unfinished(why) => Some(unfinished_refusal(why, limits)),
        Ending::Unanswered(why) => Some(unanswered_refusal(why, limits.max_tokens)),
        Ending::Exhausted => Some(still_calling_refusal(limits)),
        // The caller says this generation is the answer. It is one only if it
        // carries no call — the same read the loop made, made again where it
        // cannot be skipped.
        Ending::Answer => match read_markup(&completion) {
            Markup::None => None,
            Markup::Unfinished(why) => Some(unfinished_refusal(why, limits)),
            // Unreachable from today's callers, which route a parsed call to
            // `Execute`, `ClientCalls` or `Exhausted`. Kept because "a complete
            // call reached the answer exit" is precisely what a future fifth
            // exit would do wrong, and refusing is right whichever side is: the
            // markup is not an answer.
            Markup::Call(_) => Some(still_calling_refusal(limits)),
        },
    };

    ToolLoopOutcome {
        completion: match refusal {
            None => completion,
            Some(content) => Completion {
                content,
                ..completion
            },
        },
        client_tool_calls: Vec::new(),
    }
}

/// Replace a generation's `content` with the answer inside it, dropping any
/// `<think>…</think>` block — or hand it back with the verdict that it never
/// reached an answer.
///
/// **Called the moment a generation arrives, and that placement is the point.**
/// A reasoning block is not an answer, not a tool call, and not something to
/// feed back into the next round, so nothing downstream should ever see one:
/// past this function `content` is the model's reply and only its reply.
/// Three things follow, and each was a live defect before it:
///
/// 1. **The `/v1` response carries the answer, not the deliberation** (#582).
///    Roteiro strips this block for every CLI consumer and did not strip it
///    here, so the same model producing the same block was cleaned for one
///    consumer and passed through raw for the other. Measured live: ~95 of 105
///    completion tokens were reasoning for a one-word answer.
/// 2. **The loop stops re-sending it.** The assistant turn appended below is fed
///    back into the next round's prompt, so a block left in was re-prefilled
///    once per round against the budget [`crate::budget`] accounts for — and
///    there is no prefix cache today (#578), so it is paid in full each time.
///    The same compounding happens on the client's side of the wire, because a
///    multi-turn caller echoes assistant turns back as history.
/// 3. **[`read_markup`] judges the reply rather than the deliberation.** A model
///    that writes "I could call `<tool_call>`…" *while thinking* and then answers
///    plainly used to be refused for markup it never emitted as a call. The
///    marker means "I am calling a tool" because [`tool_system_prompt`] said so,
///    and the prompt is talking about the reply.
///
/// Only `content` changes. The token counts stay as generated — the model really
/// did spend them on reasoning, and a client billing or budgeting against them
/// would be told a comfortable lie by any other choice — and so does
/// `finish_reason`, which is what [`Unterminated`] was read from.
fn read_reasoning(completion: Completion) -> Result<Completion, (Completion, Unterminated)> {
    match thinking::answer(&completion.content, completion.finish_reason) {
        Ok(answer) => {
            let content = answer.to_owned();
            Ok(Completion {
                content,
                ..completion
            })
        }
        // Handed back whole. The refusal replaces the content anyway, and the
        // counts and stop reason travel with it.
        Err(why) => Err((completion, why)),
    }
}

/// The refusal for a generation that never left its reasoning block.
///
/// Written to the same rule as [`unfinished_refusal`] below, because it is the
/// same situation one marker over: an opened block that never closed is not a
/// short answer, and presenting one as an answer would be the silent downgrade
/// `docs/REVIEW_CHECKLIST.md` §Refusals forbids. It names the budget for the
/// cap case because that is the number a caller can change — and `max_tokens`
/// really is the binding constraint here, unlike in the tool-call case #489
/// re-measured: `types::DEFAULT_MAX_TOKENS` records `qwen3.8-27b` spending an
/// entire 1,200-token budget inside `<think>` and emitting no answer at all.
pub(crate) fn unanswered_refusal(why: Unterminated, max_tokens: u32) -> String {
    match why {
        Unterminated::CutAtTokenCap => format!(
            "{REFUSAL}the model spent its whole token budget (`max_tokens` = {max_tokens}) \
             inside its reasoning block and never started an answer, so there is nothing here \
             to read as one. Retry with a larger `max_tokens`, or ask a narrower question."
        ),
        Unterminated::CutShort => format!(
            "{REFUSAL}the model stopped part-way through its reasoning and never started an \
             answer, so there is nothing here to read as one. Retry the question; if it keeps \
             happening this model is not finishing its reasoning, and `roteiro model list` \
             shows the others installed."
        ),
    }
}

/// The refusal for a call that did not arrive intact. Each arm names the *right
/// kind* of way forward: a token cap wants a bigger cap, a model that stops
/// mid-call wants a different model, an unread dialect wants a bug report.
fn unfinished_refusal(why: Unfinished, limits: Limits) -> String {
    match why {
        Unfinished::CutAtTokenCap => format!(
            "{REFUSAL}the model hit its token limit (`max_tokens` = {}) part-way through a \
             tool call, so this reply is an unfinished call rather than an answer. Nothing \
             was executed. Retry with a larger `max_tokens`, or ask a narrower question.",
            limits.max_tokens
        ),
        // The marker itself is deliberately *not* quoted in any of these
        // sentences. A refusal is assistant-slot text: it can be shown to the
        // user and it can be carried back into a following turn, and a Roteiro
        // message that contains the marker would be read as a call by the very
        // function that wrote it. So the property holds over this module's own
        // prose too — see `every_ending_that_returns_prose_is_markup_free`.
        //
        // **One sentence serves every [`Envelope`], so it may not describe a
        // delimiter only some of them have.** It used to say the model "opened a
        // tool-call block and never closed it", which was exact while every
        // dialect was wrapped in `<tool_call>` and became false the moment one
        // was not: [`Envelope::SelfClosing`] has no closing marker to be missing,
        // so a Mistral call truncated inside its JSON was refused with a sentence
        // describing a tag it was never going to write. "Started one and never
        // finished it" is true of both, which is the property a shared sentence
        // has to have.
        Unfinished::CutShort => format!(
            "{REFUSAL}the model stopped part-way through a tool call — it started one and \
             never finished it — so this reply is an unfinished call rather than an answer. \
             Nothing was executed. Retry the question; if it keeps happening this model is \
             not following the call protocol, and `roteiro model list` shows the others \
             installed."
        ),
        // Two situations share this sentence because they share a way forward:
        // a body in a dialect Roteiro has not learned, and a body in a dialect it
        // has, written wrongly. Observed live on `qwen3-coder-30b-a3b`, the
        // common one is the second — valid-looking JSON with a brace too many —
        // so the message does not claim to know which.
        //
        // The dialects are deliberately *not* enumerated here. The sentence used
        // to say "neither the JSON nor the XML dialect", which was a list that
        // went stale the moment `Dialect::ALL` grew a third entry (#592) — and
        // went stale silently, because prose is not driven from the array.
        Unfinished::Unreadable => format!(
            "{REFUSAL}the model wrote a tool call Roteiro could not read: it parses in none \
             of the call forms Roteiro knows, so it is either malformed or a form Roteiro \
             has not learned. Either way this reply is a call rather than an answer. \
             Nothing was executed. Retry the question; if it keeps happening, report the \
             model and the call it wrote at \
             https://github.com/OffeneDatenmodellierung/Roteiro/issues."
        ),
    }
}

/// The refusal for a model that is still calling tools when it has run out of
/// rounds to call them in.
fn still_calling_refusal(limits: Limits) -> String {
    format!(
        "{REFUSAL}the model was still calling Roteiro's graph tools after {} tool round{} — \
         the budget for one request — so it never reached an answer. Nothing further was \
         executed. Ask a narrower question, or raise `MAX_TOOL_ROUNDS` in \
         `crates/rto-serve/src/server.rs` and rebuild.",
        limits.rounds,
        if limits.rounds == 1 { "" } else { "s" }
    )
}

/// A registry that advertises nothing and **can execute nothing**.
///
/// This is the structural half of suppression. Emptying the advertised tool list
/// stops the graph schemas reaching the prompt, but it does not stop a *call*:
/// the model is primed toward `search` and `explain` by name in
/// [`tool_system_prompt`]'s own instruction prose, and a call to one of those
/// would otherwise take the execute branch and run a graph tool the request
/// deliberately did not advertise.
///
/// So under suppression the loop does not hold an executable registry at all —
/// the caller's is swapped for this one, and both the advertised list and the
/// execute branch read from the *same* binding. They cannot diverge, because
/// there is only one of them. That is the same shape as the guarantee in the
/// other direction ("Roteiro never executes a client's tool"): unreachable by
/// construction rather than guarded by a membership test a later edit can drop.
struct SuppressedTools;

impl ToolRegistry for SuppressedTools {
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call(&self, name: &str, _arguments: &serde_json::Value) -> Result<String, String> {
        // Fed back to the model, which then has a round to correct itself. The
        // message says *why* rather than "unknown tool": the tool exists, it is
        // out of scope for this request, and a model told the truth about that
        // stops trying.
        Err(format!(
            "tool `{name}` is not available: this request supplied its own tools, \
             so Roteiro's graph tools are not in scope"
        ))
    }
}

/// A process-unique id for a returned tool call, so a client can correlate its
/// `tool_call_id` results. Uniqueness for the process lifetime is all a client
/// needs — it matches ids against the turn it just received — and a counter gets
/// that without a random source, matching [`crate::server`]'s completion ids.
fn next_call_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("call_{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Run a chat completion with `registry`'s tools available to the model: inject
/// a tool system prompt, generate, and while the model emits a `<tool_call>`,
/// execute it and feed the result back — up to `max_rounds` before returning the
/// last generation. With no tools, this is a plain [`Engine::chat`].
///
/// This is [`chat_with_client_tools`] with an empty client tool set; that
/// function is the full surface.
///
/// # Errors
/// Propagates [`EngineError`] from the underlying generation.
pub fn chat_with_tools(
    engine: &dyn Engine,
    registry: &dyn ToolRegistry,
    req: &ChatRequest,
    max_rounds: usize,
) -> Result<Completion, EngineError> {
    Ok(chat_with_client_tools(engine, registry, &[], req, max_rounds)?.completion)
}

/// As [`chat_with_tools`], but also honouring the tools the **client** supplied
/// on the request (#485).
///
/// Two rules govern the boundary, and both are load-bearing:
///
/// 1. **Suppression.** When `client_tools` is non-empty, `registry`'s graph tools
///    are *not* injected. A client sending its own tools is using this as a
///    general backend; adding ~3,100 tokens of graph schemas it never asked for
///    is the surprise that produced #485, and it would leave no context budget
///    for the client's own array. Name collisions therefore cannot arise in
///    practice — and where they could, the client wins its own namespace,
///    because its tools are resolved first.
/// 2. **Roteiro never executes a client's tool.** A call naming one ends the loop
///    and is returned in [`ToolLoopOutcome::client_tool_calls`] for the client to
///    run. See [`disposition`] for what happens to a turn that names both kinds.
///
/// # Errors
/// Propagates [`EngineError`] from the underlying generation.
pub fn chat_with_client_tools(
    engine: &dyn Engine,
    registry: &dyn ToolRegistry,
    client_tools: &[ToolDef],
    req: &ChatRequest,
    max_rounds: usize,
) -> Result<ToolLoopOutcome, EngineError> {
    // Rule 1: client tools suppress the graph tools entirely — not merged, not
    // appended. Suppression is applied by *replacing the registry*, not by
    // emptying a list: past this line the caller's registry is unreachable from
    // the loop, so a graph tool cannot be executed however the model names it.
    // See [`SuppressedTools`].
    let registry: &dyn ToolRegistry = if client_tools.is_empty() {
        registry
    } else {
        &SuppressedTools
    };
    // Derived from the same binding the execute branch calls, so what is
    // advertised and what is executable cannot drift apart.
    let graph_tools = registry.tools();
    let client_names: HashSet<&str> = client_tools.iter().map(|t| t.name.as_str()).collect();
    // The client's tools come first, so a graph tool sharing a name is shadowed
    // rather than executed in its place.
    let advertised: Vec<&ToolDef> = client_tools.iter().chain(graph_tools.iter()).collect();
    let limits = Limits {
        max_tokens: req.max_tokens,
        rounds: max_rounds.max(1),
    };
    if advertised.is_empty() {
        // Untooled passes *tool* markup through, never reasoning: `<tool_call>`
        // is ordinary text here because nothing injected the prompt that gives it
        // meaning, whereas `<think>` was assigned its meaning by the model and
        // means the same thing on every path. So this reads reasoning exactly as
        // the tooled path below does.
        return Ok(match read_reasoning(engine.chat(req)?) {
            Ok(completion) => finish(completion, Ending::Untooled, limits),
            Err((completion, why)) => finish(completion, Ending::Unanswered(why), limits),
        });
    }

    // Each tool is stated to the model exactly **once**, and this chooses where.
    //
    // Preferably through the request, so the model's own chat template renders
    // the tools in the shape it was trained for (#492): each template does this
    // differently — all three in the registry wrap tools in `<tools>`, but
    // `qwen3-coder-30b-a3b` declares each one as XML elements inside it while the
    // others use a JSON object — and only the template knows which. This path was
    // impossible before, because `apply_chat_template` took no tools argument at
    // all.
    //
    // Otherwise here, in the system turn. `carries_tools` is what decides, and it
    // has to be asked rather than assumed: a request routed to a remote tier is
    // sent as an ADR-0019 allow-listed payload with no `tools` field at all, so
    // for that model the conversation is the only way tools can travel.
    //
    // The rest of the system turn goes out either way, because it is not the tool
    // list: it states the `<tool_call>` protocol this server parses, and the
    // grounding rules (`search` first, cite node keys, never guess from a name).
    // A template renders tool *definitions* and says nothing about how this
    // server wants a call framed.
    // Whether the graph is on the table at all. `SuppressedTools` empties it the
    // moment a client supplies tools of its own, and the prompt must say so.
    let graph_reachable = !graph_tools.is_empty();
    let tools_for_template = openai_tool_shape(&advertised);
    // Both halves, not just `carries_tools`: dropping the listing is safe only
    // if the tools were actually put on the request. The early return above
    // makes the first half true today, and it is what keeps the two facts tied
    // together if that ever stops being so.
    let engine_advertises = tools_for_template.is_some() && engine.carries_tools(&req.model);

    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    messages.push(Message {
        role: "system".to_owned(),
        content: if engine_advertises {
            grounding_rules(graph_reachable)
        } else {
            tool_system_prompt(&advertised, graph_reachable)
        },
    });
    messages.extend(req.messages.iter().cloned());

    let generate = |messages: &[Message]| {
        engine.chat(&ChatRequest {
            tools: tools_for_template.clone(),
            model: req.model.clone(),
            messages: messages.to_vec(),
            images: req.images.clone(),
            audio: req.audio.clone(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        })
    };

    for _ in 0..limits.rounds {
        // Before anything reads this generation as a call, an answer, or a turn
        // to feed back — see [`read_reasoning`] for why all three want it gone.
        let completion = match read_reasoning(generate(&messages)?) {
            Ok(completion) => completion,
            Err((completion, why)) => {
                return Ok(finish(completion, Ending::Unanswered(why), limits));
            }
        };

        let calls = match read_markup(&completion) {
            // No tool call: this is the model's final answer.
            Markup::None => return Ok(finish(completion, Ending::Answer, limits)),
            // The model was calling a tool and the call did not arrive intact.
            // That is not an absence of a call, and reading it as one is what
            // handed the raw markup to the user as prose (#489). Nothing here is
            // executable — the call is not there to run — so the loop ends and
            // `finish` says why.
            Markup::Unfinished(why) => {
                return Ok(finish(completion, Ending::Unfinished(why), limits));
            }
            // One call per turn: the divergence declared on [`Markup::Call`].
            Markup::Call(call) => vec![call],
        };

        if disposition(&calls, &client_names) == Disposition::ReturnAll {
            // Rule 2. Return every call in the turn and execute none.
            return Ok(finish(completion, Ending::ClientCalls(calls), limits));
        }

        // Execute the tools and feed the (bounded) results back. Each response is
        // a `user` turn (not a `tool` role): `Message` is only guaranteed to
        // render system/user/assistant, and a `tool` role would emit a token the
        // models were never trained on — while a `<tool_response>` user turn is
        // what every Qwen template emits natively. The marker carries the
        // semantics, as the system prompt told the model to expect.
        messages.push(Message {
            role: "assistant".to_owned(),
            content: completion.content.clone(),
        });
        for call in &calls {
            // The argument object is checked against the schema the model was
            // shown, **before** the registry sees it — see [`unknown_argument`].
            // Here rather than inside each registry because this is the one
            // execution funnel, so a tool cannot be reached with an argument
            // nobody read; and before the call rather than after, so a wrapper
            // that pre-binds an argument of its own (`server::ScopedTools` fills
            // in `project`) is filling in a server-side value rather than
            // submitting one to be judged.
            let refused = graph_tools
                .iter()
                .find(|t| t.name == call.name)
                .and_then(|def| unknown_argument(def, &call.arguments));
            let result = match refused {
                Some(why) => format!("tool `{}` error: {why}", call.name),
                None => registry
                    .call(&call.name, &call.arguments)
                    .unwrap_or_else(|e| format!("tool `{}` error: {e}", call.name)),
            };
            messages.push(Message {
                role: "user".to_owned(),
                content: tool_response_turn(&result),
            });
        }
    }

    // Round budget exhausted while still calling tools: run one final generation
    // over the accumulated context (including the last tool response) but do not
    // execute any further tools, so the last result actually informs the answer.
    //
    // This generation is read exactly as one inside the loop is, and by the same
    // function. A client-tool call is returned structurally; a *graph*-tool call
    // is the model saying it still needs to look something up after the budget
    // is gone, which is not an answer either — publishing it as prose was the
    // second half of #489, and the half the Ask path actually took, because Ask
    // supplies no client tools so `client_names` is always empty there.
    // Read exactly as one inside the loop is, reasoning included: this is the
    // generation the Ask path most often publishes, and the round budget running
    // out is not a reason to hand back a block the loop would have dropped.
    let completion = match read_reasoning(generate(&messages)?) {
        Ok(completion) => completion,
        Err((completion, why)) => {
            return Ok(finish(completion, Ending::Unanswered(why), limits));
        }
    };
    let ending = match read_markup(&completion) {
        Markup::None => Ending::Answer,
        Markup::Unfinished(why) => Ending::Unfinished(why),
        Markup::Call(call) => {
            let calls = vec![call];
            if disposition(&calls, &client_names) == Disposition::ReturnAll {
                Ending::ClientCalls(calls)
            } else {
                Ending::Exhausted
            }
        }
    };
    Ok(finish(completion, ending, limits))
}

/// Stamp a correlation id onto each parsed call on its way back to the client.
fn into_client_calls(calls: Vec<ToolCall>) -> Vec<ClientToolCall> {
    calls
        .into_iter()
        .map(|c| ClientToolCall {
            id: next_call_id(),
            name: c.name,
            arguments: c.arguments,
        })
        .collect()
}

/// The turn a tool result is fed back as: the capped result inside the
/// `<tool_response>` markers the system prompt told the model to expect.
///
/// A named function rather than a `format!` inline in the loop so that
/// [`crate::budget`]'s allowance for everything the cap does *not* cover — those
/// markers, and `truncate`'s own suffix — can be checked against the string this
/// actually builds instead of against a comment counting characters.
pub(crate) fn tool_response_turn(result: &str) -> String {
    format!("<tool_response>{}</tool_response>", truncate(result))
}

/// Cap a tool result to [`MAX_TOOL_RESULT`] bytes (backing up to a char boundary).
fn truncate(s: &str) -> String {
    if s.len() <= MAX_TOOL_RESULT {
        return s.to_owned();
    }
    let end = (0..=MAX_TOOL_RESULT)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}… (truncated)", &s[..end])
}

/// The system turn `tools` are advertised with **when the engine carries none of
/// them itself** — the largest surface a model can be asked to spend prefill on.
///
/// Public so the **other** side can measure and pin its own surface: the graph
/// tool registry lives in the `roteiro` binary, and issue #590 found that nothing
/// pinned this format at all, so the mass of what is sent on every turn was
/// discoverable only by hand. Measured on this repository's own graph tools, that
/// mass is 78% tool descriptions and 21% argument schemas — which is what decides
/// where shrinking is worth doing.
///
/// At the 3.13 ms/token prefill measured in issue #578 the length of this string
/// is seconds per turn, on every turn, with no cache.
///
/// # It is a bound, not the bytes every model sees
///
/// Since #492 a model whose engine reports [`Engine::carries_tools`] gets the
/// tools from its own chat template instead, and this listing is not sent to it
/// at all — it receives the grounding rules alone. The full listing still goes to
/// engines that cannot carry tools, the remote tier among them, so this remains
/// the number to shrink: it is what the worst-served model pays, and no model
/// pays more.
#[must_use]
pub fn advertised_system_prompt(tools: &[&ToolDef]) -> String {
    tool_system_prompt(tools, true)
}

/// The advertised tools in OpenAI's `[{type, function:{…}}]` shape — the shape
/// every chat template in the registry is written against.
///
/// `None` for an empty set, because a template distinguishes "no tools" from
/// "an empty list of tools" only by accident, and the request should say what
/// the caller meant.
fn openai_tool_shape(advertised: &[&ToolDef]) -> Option<serde_json::Value> {
    if advertised.is_empty() {
        return None;
    }
    Some(serde_json::Value::Array(
        advertised
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect(),
    ))
}

/// The grounding rules alone, for a model whose engine advertises the tools
/// itself.
///
/// Everything here is Roteiro's and appears in no chat template: a template
/// renders tool *definitions* and has nothing to say about answering only from
/// the graph or citing node keys. Sent on every tooled turn, so the rules do not
/// depend on which engine is serving.
fn grounding_rules(graph: bool) -> String {
    system_prompt(&[], false, graph)
}

/// Render the system prompt that advertises `tools` and the `<tool_call>`
/// protocol. Kept model-agnostic: any instruction-following model can comply.
fn tool_system_prompt(tools: &[&ToolDef], graph: bool) -> String {
    system_prompt(tools, true, graph)
}

/// The system turn, with the tool listing when `list` and without it otherwise.
///
/// One function for both so the rules cannot drift apart between the engine that
/// carries tools and the engine that does not — two wordings of one instruction
/// would be the same defect as two listings.
///
/// The protocol sentence differs because the situations do. When the listing is
/// here, nothing else has told the model how to frame a call, so this states the
/// form outright. When the engine advertises, the model's own template has
/// already framed it in the shape it was trained on, and naming a competing form
/// is how #489's dialect problem starts.
fn system_prompt(tools: &[&ToolDef], list: bool, graph: bool) -> String {
    // The `<tool_call>` envelope is stated either way, because it is *this
    // server's* and no chat template supplies it: a model left to its own would
    // use whatever envelope it was trained on — which for a model Roteiro has
    // never seen is not this one. #592 narrowed what this sentence has to carry
    // and did not remove it, so it is still the last line of defence: a model
    // whose training beats the instruction is now read anyway **if the envelope
    // it used is a [`Dialect`]** — the call runs, or the refusal says why it
    // could not. An envelope in no `Dialect` is recognised by nothing: every
    // dialect answers [`Reading::Absent`], the fold answers [`Markup::None`],
    // and the generation is published as the model's prose. That is exactly the
    // outcome this instruction exists to prevent, and the reason it is stated to
    // a template-carrying engine too.
    //
    // What differs is the body. With no listing here the model has been shown the
    // tools in its own trained shape, so pinning a competing inner form is how
    // #489's dialect problem starts; `Dialect::ALL` reads both anyway. With the
    // listing, nothing else has defined a body shape, so this defines one.
    let protocol = if list {
        "reply with ONLY a tool call on its own, as \
         `<tool_call>{\"name\": \"<tool>\", \"arguments\": { … }}</tool_call>`"
    } else {
        "reply with ONLY a tool call on its own, wrapped in \
         `<tool_call>`…`</tool_call>`, with the call itself in the form your \
         instructions specify"
    };
    // Only when the graph is actually reachable. A client that supplies its own
    // tools suppresses the graph ones entirely (see `SuppressedTools`), and
    // telling that model to answer "using ONLY the Roteiro knowledge graph", via
    // a `search` and an `explain` it has not been given, is an instruction it
    // cannot follow — it can only refuse, or keep calling tools that are not
    // there. What survives is the part that is true either way: ground the answer
    // in what the tools returned, and do not invent when they do not answer.
    let opening = if graph {
        "You answer questions about this codebase using ONLY its Roteiro knowledge \
         graph, reached through the tools available to you."
    } else {
        "You answer questions using the tools available to you."
    };
    let grounding = if graph {
        "Ground every claim in what the tools return: use `search` to find \
         relevant nodes, then read each hit's `snippet` or call `explain` on its \
         key to read the node's actual content BEFORE describing it — never guess \
         from a node's name alone. Cite the node keys you used (e.g. \
         `file:README.md`, `fn:foo`)."
    } else {
        "Ground every claim in what the tools return, and never describe \
         something a tool has not shown you."
    };
    let mut out = format!(
        "{opening} When a tool would help, {protocol}.\n\
         After you receive a <tool_response>, use it to answer. {grounding} If \
         the tools do not contain the answer, say you could not find it rather than \
         making one up. If no tool is needed, just answer directly.\n"
    );
    if !list {
        return out;
    }
    out.push_str("Available tools:\n");
    let advertised: Vec<Advertisement> = tools.iter().map(|t| advertise(t)).collect();
    // Argument descriptions repeated verbatim across tools, stated once. On the
    // graph surface that is the `project` selector, carried identically by
    // thirteen tools; on a client surface it is whatever that client repeats.
    // The rule is mechanical rather than a special case for a known argument.
    let shared = shared_notes(&advertised);
    if advertised.iter().any(|a| a.call.is_some()) {
        out.push_str(
            "Each entry is `name(arguments) — what it does`. `?` marks an optional \
             argument, `a|b|c` the permitted values, `1..25` an inclusive range.\n",
        );
    }
    if !shared.is_empty() {
        out.push_str("Arguments shared by several tools:\n");
        for (arg, note) in &shared {
            let _ = writeln!(out, "  {arg}: {note}");
        }
    }
    for (t, ad) in tools.iter().zip(&advertised) {
        // A schema this renderer cannot state without losing something —
        // nesting, a `$ref`, a combinator — goes out verbatim, exactly as every
        // schema used to. A client's arguments are the client's contract and
        // Roteiro does not get to summarise them lossily.
        let Some(call) = &ad.call else {
            let params = serde_json::to_string(&t.parameters).unwrap_or_else(|_| "{}".to_owned());
            let _ = writeln!(
                out,
                "- {}: {} arguments schema: {params}",
                t.name, t.description
            );
            continue;
        };
        let _ = writeln!(out, "- {call} — {}", t.description);
        for (arg, note) in &ad.notes {
            if shared.iter().any(|(a, n)| a == arg && n == note) {
                continue;
            }
            let _ = writeln!(out, "  {arg}: {note}");
        }
    }
    out
}

/// One tool as [`tool_system_prompt`] states it.
struct Advertisement {
    /// The call form — `search(query: str, limit?: int 1..25)` — or `None` when
    /// the schema is not one [`advertise`] can state without losing something.
    call: Option<String>,
    /// `(argument, description)` pairs the schema carried, in schema order.
    notes: Vec<(String, String)>,
}

/// Render one tool's arguments as a signature instead of as raw JSON Schema.
///
/// # Why this is a rendering change and nothing more
///
/// The wire value is untouched: [`crate::types`] measures a client's `tools`
/// array — name, description and compact-JSON `parameters` — *before* anything
/// here runs, so `MAX_CLIENT_TOOL_BYTES` is exactly where it was and this makes
/// it more conservative rather than less, producing fewer prompt tokens from the
/// same 32 KiB budget (issue #590).
///
/// # Why it declines rather than approximates
///
/// A nested object, a `$ref`, a `oneOf`, an unrecognised keyword — anything this
/// cannot state — yields `None`, and the caller emits the schema verbatim. A
/// client's argument shape is the contract Roteiro hands back for the client to
/// execute; summarising it lossily would leave the model calling a tool whose
/// arguments no longer match what will run, which is the failure
/// `MAX_CLIENT_TOOL_BYTES` refuses to truncate for.
fn advertise(t: &ToolDef) -> Advertisement {
    let none = || Advertisement {
        call: None,
        notes: Vec::new(),
    };
    let Some(schema) = t.parameters.as_object() else {
        return none();
    };
    // The flat-object shape, plus the two keywords a real generated schema
    // carries that say nothing about how to *call* the tool. `$schema` names the
    // dialect; `additionalProperties: false` is what an exhaustive signature
    // already states, and any other value of it means extra arguments are legal,
    // which a signature would not convey — so that one falls back.
    for (key, value) in schema {
        match key.as_str() {
            "type" | "properties" | "required" | "$schema" => {}
            "additionalProperties" if value == &serde_json::Value::Bool(false) => {}
            _ => return none(),
        }
    }
    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return none();
    }
    // Absent is not malformed: schemars renders a struct whose fields are all
    // `Option<T>` with no `required` key at all, which legitimately means "no
    // required arguments" and is what Roteiro's own MCP schemas do. Any *other*
    // shape — a bare string, a number, an array holding a non-string — is a
    // `required` this renderer does not understand, so it declines like every
    // other keyword it cannot state. Reading it as empty instead would advertise
    // each required argument as optional, and a model that then omits one gets a
    // call that fails at dispatch: the lossy summary the doc above rules out,
    // pointing the one direction that breaks the call.
    let required: Vec<&str> = match schema.get("required") {
        None => Vec::new(),
        Some(r) => {
            let Some(names) = r
                .as_array()
                .and_then(|a| a.iter().map(serde_json::Value::as_str).collect())
            else {
                return none();
            };
            names
        }
    };
    let mut args = Vec::new();
    let mut notes = Vec::new();
    if let Some(props) = schema.get("properties") {
        let Some(props) = props.as_object() else {
            return none();
        };
        // `serde_json::Map` is a `BTreeMap` here, so this already iterated in
        // sorted order — but only because nothing in the tree enables
        // `serde_json/preserve_order`. That is feature resolution, not a
        // decision: any dependency turning it on would silently switch this to
        // insertion order and change every advertised signature, with no diff to
        // show for it. Holding the advertised surface still is what this renderer
        // is for — issue #578 measured 3.13 ms per prompt token — so sort here
        // and mean it. No test can tell this sort from the `BTreeMap` it is
        // shadowing, so what watches the assumption is
        // `nothing_has_turned_on_preserve_order_behind_the_advertised_surface`.
        let mut props: Vec<_> = props.iter().collect();
        props.sort_by_key(|(name, _)| *name);
        for (name, spec) in props {
            let Some(spec) = spec.as_object() else {
                return none();
            };
            let Some(rendered) = argument_type(spec) else {
                return none();
            };
            // Optional two ways: absent from `required`, or nullable — schemars
            // renders an `Option<T>` field as `"type": ["T", "null"]` and lists
            // no `required` at all, which is what Roteiro's own MCP schemas do.
            let optional = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            args.push(format!("{name}{optional}: {rendered}"));
            if let Some(note) = spec.get("description").and_then(serde_json::Value::as_str) {
                notes.push((name.clone(), note.to_owned()));
            }
        }
    }
    Advertisement {
        call: Some(format!("{}({})", t.name, args.join(", "))),
        notes,
    }
}

/// One property's type as a signature fragment, or `None` if it carries anything
/// this cannot state — which makes the whole tool fall back to raw schema.
///
/// Nothing is dropped silently. A `format` says little on an integer whose range
/// is already given, but it is rendered anyway rather than judged: deciding which
/// keywords are "noise" is how a lossy renderer starts.
fn argument_type(spec: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    for key in spec.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "enum"
                | "items"
                | "minimum"
                | "maximum"
                | "description"
                | "format"
                | "default"
        ) {
            return None;
        }
    }
    if let Some(values) = spec.get("enum") {
        let values = values.as_array()?;
        let names: Option<Vec<&str>> = values.iter().map(serde_json::Value::as_str).collect();
        return Some(with_default(names?.join("|"), spec));
    }
    let name = match base_type(spec.get("type")?)? {
        "string" => "str".to_owned(),
        "integer" => "int".to_owned(),
        "number" => "num".to_owned(),
        "boolean" => "bool".to_owned(),
        "array" => {
            let items = spec.get("items")?.as_object()?;
            format!("[{}]", argument_type(items)?)
        }
        _ => return None,
    };
    let name = match spec.get("format") {
        None => name,
        Some(format) => format!("{name}({})", format.as_str()?),
    };
    // A bound is allow-listed above *because* this renderer claims to state it,
    // so one it cannot state — the fractional `minimum` JSON Schema permits on a
    // `number` — declines the tool rather than falling through as if the keyword
    // were absent, which would drop the bound from the signature silently and
    // leave the model free to send a value the tool will reject.
    for key in ["minimum", "maximum"] {
        if spec.get(key).is_some_and(|v| v.as_i64().is_none()) {
            return None;
        }
    }
    // The advertised bound, which every ranking tool here carries and also states
    // in its description. Rendering it in the signature is what makes the prose
    // restatement redundant rather than the only place it appears.
    let name = match (
        spec.get("minimum").and_then(serde_json::Value::as_i64),
        spec.get("maximum").and_then(serde_json::Value::as_i64),
    ) {
        (Some(lo), Some(hi)) => format!("{name} {lo}..{hi}"),
        (Some(lo), None) => format!("{name} >={lo}"),
        (None, Some(hi)) => format!("{name} <={hi}"),
        (None, None) => name,
    };
    Some(with_default(name, spec))
}

/// A property's `type`, reading the nullable-union form an `Option<T>` field
/// generates (`["string", "null"]`) as plain `T` — the field is already marked
/// optional by its absence from `required`, so `| null` would be a second
/// spelling of the `?` in the signature.
fn base_type(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(t) => Some(t.as_str()),
        serde_json::Value::Array(types) => {
            let mut named = types
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|t| *t != "null");
            let only = named.next()?;
            named.next().is_none().then_some(only)
        }
        _ => None,
    }
}

/// Append ` = <value>` for a declared default. A `null` default is how a
/// generated schema spells "unset", which the `?` already says.
fn with_default(rendered: String, spec: &serde_json::Map<String, serde_json::Value>) -> String {
    match spec.get("default") {
        None | Some(serde_json::Value::Null) => rendered,
        Some(value) => format!("{rendered} = {value}"),
    }
}

/// The `(argument, description)` pairs at least two tools carry **verbatim**,
/// sorted, so the prompt can state each once above the list instead of once per
/// tool.
fn shared_notes(advertised: &[Advertisement]) -> Vec<(String, String)> {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for ad in advertised {
        if ad.call.is_none() {
            continue;
        }
        for (arg, note) in &ad.notes {
            *counts.entry((arg.as_str(), note.as_str())).or_default() += 1;
        }
    }
    let mut shared: Vec<(String, String)> = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|((arg, note), _)| (arg.to_owned(), note.to_owned()))
        .collect();
    shared.sort();
    shared
}

/// What the tool-call markup in a generation turned out to be — the reading
/// [`chat_with_client_tools`] acts on, and the reading [`finish`] re-checks.
///
/// The variant that did not exist before #489 is [`Markup::Unfinished`]. The
/// parser used to answer `Option<ToolCall>`, which collapses "the model was not
/// calling a tool" and "the model was calling a tool and it did not arrive" into
/// one `None` — and the loop read that `None` as *"this is the final answer"*.
/// Three of them are three different situations and only one of them is an
/// answer, so the type says three things.
enum Markup {
    /// No [`Dialect`]'s opening marker at all: whatever the model wrote is its
    /// own text.
    None,
    /// A complete call, in one of the [`Dialect`]s.
    ///
    /// **Divergence, declared rather than half-implemented:** a turn carries at
    /// most one call, so `parallel_tool_calls` is accepted-and-carried rather
    /// than enforced. N-call parsing lands with the grammar work (#485 PR 2);
    /// the callers already wrap this in the `Vec` [`disposition`] takes, so the
    /// mixed-turn rule is expressible and testable before the parser can produce
    /// a mixed turn.
    Call(ToolCall),
    /// Tool-call markup, in some dialect, that is not a runnable call.
    Unfinished(Unfinished),
}

/// Where a [`Dialect`]'s calls begin and end in a generation, and — the half
/// that carries the weight — **what proves one arrived whole**.
///
/// This is what makes a dialect *reachable* (#592). The `<tool_call>` wrapper
/// used to be searched for one layer *above* [`Dialect`], and a dialect was
/// consulted only inside a wrapper that had already been found. So
/// [`Dialect::ALL`] guaranteed that every dialect was handled *consistently* —
/// parser and parity test drive from the same array — and guaranteed nothing
/// about any of them being reachable: a model whose call form does not nest
/// inside `<tool_call>` never reached a dialect at all. That is not
/// hypothetical. `voxtral-mini-3b`'s own chat template renders a call as
/// `[TOOL_CALLS]name[ARGS]{…}` and contains no `<tool_call>` anywhere, so once
/// `rto_llama` renders that template rather than assuming the `ChatML` shape
/// (#492), the model emits a well-formed call the parser cannot see, and the raw
/// markup goes to the user as prose — #489's failure mode by a new route.
///
/// Widening the old search to a longer list of literals was the fix not taken:
/// it rebuilds the same layering one literal taller, and the next form Roteiro
/// has not met is unreachable again. The envelope is a property of the dialect
/// instead, so `ALL` now means reachable as well as consistent.
///
/// # The completeness rule, restated so that it covers both shapes
///
/// [`read_markup`] used to own this rule, and owned it in the only vocabulary a
/// wrapper has: *without `</tool_call>` the call did not arrive*. That sentence
/// cannot even be spoken about a form with no closing marker, so the rule is
/// stated one level up, where it says the same thing about both:
///
/// > **A call arrived only when its own dialect can prove it whole.**
///
/// The two variants are the two proofs, and the difference between them is not
/// a matter of taste:
///
/// * [`Self::Delimited`] proves arrival with the **closing marker, never with
///   the body**. A JSON body does prove itself — `serde_json` rejects an
///   unclosed object — but an XML one does not: [`parse_xml_body`] deliberately
///   tolerates a missing `</function>` and a missing `</parameter>`, so
///   `…<parameter=query>\nwin` parses happily into `search(query="win")`, a
///   *different question, silently answered*. Trusting the body here would hold
///   for one dialect and not the other, which is precisely the drift
///   [`Dialect::ALL`] exists to prevent.
/// * [`Self::SelfClosing`] has no closing marker to wait for — Mistral's form is
///   terminated by its own JSON — so the proof must come from the body's
///   grammar. That is legitimate **only** for a grammar that rejects a truncated
///   body, which is the condition the XML dialect fails and JSON meets.
///
/// The condition is not left to a reader of this comment:
/// `no_dialect_reads_a_truncated_call_as_a_whole_one` drives every dialect in
/// `ALL`, feeds each one every proper prefix of a call it renders itself, and
/// requires that none of them yields a call. Declaring a lenient grammar
/// self-closing fails it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Envelope {
    /// A call runs from `open` to `close`, and the closing marker is the whole
    /// proof that it arrived: `<tool_call>`…`</tool_call>`.
    Delimited {
        /// The marker that says a call started.
        open: &'static str,
        /// The marker that proves it finished.
        close: &'static str,
    },
    /// A call starts at `open` and ends where its own body ends, so the body's
    /// grammar is the proof: `[TOOL_CALLS]name[ARGS]{…}` ends with its JSON.
    SelfClosing {
        /// The marker that says a call started.
        open: &'static str,
    },
}

/// The wrapper tags, named because each is matched in two places.
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

/// The Mistral envelope's markers, as `voxtral-mini-3b`'s template writes them:
/// `"[TOOL_CALLS]" + name + "[ARGS]" + arguments`.
const TOOL_CALLS_OPEN: &str = "[TOOL_CALLS]";
const ARGS_OPEN: &str = "[ARGS]";

impl Envelope {
    /// The marker that says a call *started* — the only thing every envelope
    /// has, and so the only thing a generic caller may key on.
    ///
    /// Used by `no_refusal_contains_any_dialects_opening_marker`: Roteiro's own
    /// refusals are assistant-slot text that can be carried into a following
    /// turn, so a refusal containing one of these would be read back as a call
    /// by the very function that wrote it.
    const fn opening(self) -> &'static str {
        match self {
            Self::Delimited { open, .. } | Self::SelfClosing { open } => open,
        }
    }

    /// Frame the first call in `text` under this envelope.
    fn frame(self, text: &str) -> Framing<'_> {
        let open = self.opening();
        let Some(at) = text.find(open) else {
            return Framing::Absent;
        };
        let rest = &text[at + open.len()..];
        match self {
            Self::Delimited { close, .. } => match rest.find(close) {
                Some(end) => Framing::Body(rest[..end].trim()),
                None => Framing::Opened,
            },
            // Nothing here can say whether the call finished; the dialect's body
            // grammar is what decides, and is required to be able to.
            Self::SelfClosing { .. } => Framing::Body(rest.trim()),
        }
    }
}

/// What an [`Envelope`] found in a generation.
enum Framing<'a> {
    /// The envelope's opening marker is not in the text: this dialect has
    /// nothing to say about this generation.
    Absent,
    /// A call started and the envelope's own proof of arrival never came. Only
    /// [`Envelope::Delimited`] can report this — [`Envelope::SelfClosing`] has
    /// no such proof to wait for and defers the whole question to the body.
    Opened,
    /// The call's text. Under [`Envelope::Delimited`] arrival is already proven
    /// and the body only has to be *readable*; under [`Envelope::SelfClosing`]
    /// the body still has to prove arrival as well.
    Body(&'a str),
}

/// One dialect's reading of a whole generation — "is a call here, and did it
/// arrive?", which is the question a dialect owns now that it owns its
/// envelope.
///
/// [`read_markup`] folds these across [`Dialect::ALL`]; the fold is where the
/// per-dialect readings become the one [`Markup`] the loop acts on.
#[derive(Debug, PartialEq, Eq)]
enum Reading {
    /// Not this dialect's form at all.
    Absent,
    /// This dialect's form, and nothing proves the call arrived whole.
    Unarrived,
    /// A call that arrived whole and that this dialect could not read.
    Unreadable,
    /// A call, whole and read.
    Call(ToolCall),
}

/// Read a generation as tool-call markup: ask every [`Dialect`] what it sees,
/// and use the engine's own account of why generation stopped to say what a call
/// that did not arrive means.
///
/// # Nothing is parsed leniently, in any dialect
///
/// The tempting reading is "the body is probably complete and only the marker is
/// missing, so parse the remainder". It is not taken, and [`Envelope`] carries
/// the reason in full: leniency is *unverifiable* in the XML dialect, because
/// [`parse_xml_body`] tolerates a missing `</function>` and a missing
/// `</parameter>` and so reads a truncated call as a whole one — a different
/// question, silently answered, which is the failure class #489 is about. The
/// rule that follows is uniform over dialects because a rule that held for one
/// and not the other is the drift [`Dialect::ALL`] exists to prevent:
///
/// > **A call arrived only when its own dialect can prove it whole.**
///
/// The cost of that choice is a refusal where a complete-but-unterminated JSON
/// call could have been run — one retry. The cost of the other choice is a
/// truncated query answered as if it were the real one, and nothing downstream
/// can tell.
///
/// # Folding the readings
///
/// Each dialect answers about the whole generation, so several may answer at
/// once, and the fold ranks them by how much each one establishes:
///
/// 1. **A call that arrived beats one that did not**, so a [`Reading::Call`]
///    returns immediately — in `ALL` order, which is the order a body was tried
///    in before.
/// 2. **[`Reading::Unarrived`] beats [`Reading::Unreadable`]**, because it is
///    the reading backed by evidence: `finish_reason` then distinguishes *why*
///    the call did not arrive, and that is engine evidence rather than parser
///    inference — `rto_llama`'s decode loop starts at [`FinishReason::Length`]
///    and reaches [`FinishReason::Stop`] only on an end-of-generation token, so
///    `Length` means precisely "`max_tokens` was reached first". The
///    `Unreadable` refusal asks for a bug report, and a truncated generation is
///    not a bug worth reporting.
///
/// [`Dialect::Json`] and [`Dialect::Xml`] share one envelope, so between them the
/// ranking never comes up: they always agree on whether a call arrived and
/// differ only in whether they can read it — which is exactly what the old
/// two-level read did, and why those two do not move. It comes up only across
/// envelopes, and a model writes the one form it was trained on, so a mixed
/// generation is not something to expect. It is expressible, though, and every
/// ranking here refuses either way — what is at stake is which sentence the user
/// is given — so the order is pinned by
/// `a_call_that_did_not_arrive_outranks_one_that_could_not_be_read` rather than
/// left to the shape of a loop. Rare is where silent corruption lives.
fn read_markup(completion: &Completion) -> Markup {
    let mut unarrived = false;
    let mut unreadable = false;
    for dialect in Dialect::ALL {
        match dialect.read(&completion.content) {
            Reading::Call(call) => return Markup::Call(call),
            Reading::Unarrived => unarrived = true,
            Reading::Unreadable => unreadable = true,
            Reading::Absent => {}
        }
    }
    if unarrived {
        return Markup::Unfinished(match completion.finish_reason {
            FinishReason::Length => Unfinished::CutAtTokenCap,
            FinishReason::Stop => Unfinished::CutShort,
        });
    }
    if unreadable {
        return Markup::Unfinished(Unfinished::Unreadable);
    }
    Markup::None
}

/// A call form a model may emit — its [`Envelope`] and the body grammar inside
/// it, together, because a form is both.
///
/// [`tool_system_prompt`] instructs the JSON form inside `<tool_call>` and most
/// models comply — but it is sent only to a model whose engine cannot carry
/// tools, and in any case a model's *training* can beat an instruction. That is
/// this type's whole subject, and it has now been measured twice:
///
/// * The chat templates shipped in `qwen3-coder-30b-a3b` and `qwen3.8-27b` tell
///   them that "an inner `<function=...></function>` block must be nested within
///   `<tool_call>` XML tags", and they sometimes revert to it. A body the parser
///   did not understand used to make the whole call invisible —
///   [`chat_with_client_tools`] then read the generation as the model's final
///   answer and handed the raw markup to the user as prose (#489).
/// * `voxtral-mini-3b`'s template does not mention `<tool_call>` at all and
///   renders a call as `[TOOL_CALLS]name[ARGS]{…}` (#592). Its *envelope*
///   differs, not just its body, which is why the envelope had to become a
///   property of the dialect rather than a gate above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// `<tool_call>{"name": "search", "arguments": {"query": "x"}}</tool_call>`
    /// — the instructed form, and what three of the five registry models emit.
    Json,
    /// `<tool_call><function=search><parameter=query>x</parameter></function></tool_call>`
    /// — the form the two Qwen XML templates train.
    Xml,
    /// `[TOOL_CALLS]search[ARGS]{"query": "x"}` — the form Mistral-family
    /// templates train, `voxtral-mini-3b`'s among them. The dialect that proves
    /// [`Envelope`] is not `<tool_call>` with extra steps: it has a second
    /// envelope shape, and no closing marker at all.
    Mistral,
}

impl Dialect {
    /// Every dialect, in the order a generation is read against them.
    ///
    /// **This list is the only thing that makes a dialect reachable, and since
    /// #592 that is true rather than merely intended.** Nothing else dispatches
    /// on [`Dialect`]: [`read_markup`] drives from `ALL`, and so do
    /// `every_dialect_parses_to_the_same_call`, which renders one call in each
    /// dialect and asserts they arrive identical, and
    /// `no_dialect_reads_a_truncated_call_as_a_whole_one`, which holds each of
    /// them to the completeness rule. So a fourth dialect either lands here —
    /// reached, held to the same output shape, and held to the same rule about
    /// truncation — or it is inert. There is no arrangement in which one of them
    /// sees it and the others do not.
    const ALL: [Self; 3] = [Self::Json, Self::Xml, Self::Mistral];

    /// How this dialect's calls are delimited, and what proves one arrived.
    const fn envelope(self) -> Envelope {
        match self {
            // One envelope, named once, shared by both bodies that nest in it: a
            // change to the wrapper must not reach one dialect and miss the
            // other.
            Self::Json | Self::Xml => Envelope::Delimited {
                open: TOOL_CALL_OPEN,
                close: TOOL_CALL_CLOSE,
            },
            Self::Mistral => Envelope::SelfClosing {
                open: TOOL_CALLS_OPEN,
            },
        }
    }

    /// Read a whole generation as this dialect.
    fn read(self, text: &str) -> Reading {
        match self.envelope().frame(text) {
            Framing::Absent => Reading::Absent,
            Framing::Opened => Reading::Unarrived,
            Framing::Body(body) => self.parse(body),
        }
    }

    /// Read this dialect's `body`. Each dialect judges only its own syntax and
    /// none guesses: a body in no dialect stays unread in all of them, which is
    /// what keeps "understand another dialect" from becoming "accept garbage".
    ///
    /// [`Reading::Absent`] is not reachable from here — the envelope already
    /// said this is the dialect's form — and [`Reading::Unarrived`] is reachable
    /// only under [`Envelope::SelfClosing`], where arrival is the body grammar's
    /// to establish. An [`Envelope::Delimited`] dialect has had arrival proven
    /// by its closing marker before this is called.
    fn parse(self, body: &str) -> Reading {
        match self {
            Self::Json => parse_json_body(body).map_or(Reading::Unreadable, Reading::Call),
            Self::Xml => parse_xml_body(body).map_or(Reading::Unreadable, Reading::Call),
            Self::Mistral => parse_mistral_body(body),
        }
    }
}

/// The XML dialect's tags. Named rather than inlined because the opening ones are
/// matched by prefix and the closing ones by search, and a literal typed twice is
/// a literal that can be corrected once.
const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAMETER_OPEN: &str = "<parameter=";
const PARAMETER_CLOSE: &str = "</parameter>";

/// [`Dialect::Json`]: a JSON object carrying `name` and optional `arguments`.
///
/// Lifted out of [`parse_tool_call`] unchanged when the XML dialect landed. Three
/// of the five registry models emit this form, so it is the one that must not
/// move: same input, same `Option<ToolCall>`, including for the malformed bodies
/// it already rejected.
fn parse_json_body(body: &str) -> Option<ToolCall> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let name = value.get("name")?.as_str()?.to_owned();
    if name.is_empty() {
        return None;
    }
    // `arguments` may be an object or absent; default to an empty object.
    let arguments = value
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(ToolCall { name, arguments })
}

/// [`Dialect::Mistral`]: `name[ARGS]{…}`, the text after `[TOOL_CALLS]`.
///
/// # This is the dialect that proves its own completeness
///
/// It has no closing marker, so under [`Envelope::SelfClosing`] the grammar here
/// is the whole proof that a call arrived — see [`Envelope`] for why that is
/// allowed of this body and not of [`parse_xml_body`]. Two things follow, and
/// both are the reason this returns a [`Reading`] rather than an `Option`:
///
/// * **`[ARGS]` missing is truncation, not garbage.** The template writes it
///   immediately after the name, so a body without it is a call cut off while
///   still writing that name. [`Reading::Unarrived`] then gets the refusal that
///   names `max_tokens`, rather than the one that asks for a bug report.
/// * **A JSON error that is end-of-input is truncation too.** `serde_json`
///   classifies it, so `{"query":"win` is `Unarrived` while `{"query":,}` is
///   [`Reading::Unreadable`]. Neither is ever a call: a truncated object does
///   not parse, which is exactly the property the XML body lacks.
///
/// The **first** JSON value is taken, and trailing text after it is ignored
/// rather than fatal. The template loops over `tool_calls`, so a two-call turn
/// arrives as two `[TOOL_CALLS]…[ARGS]…` runs with no separator; one call per
/// turn is the divergence declared on [`Markup::Call`], and the second run is
/// left where a grammar-based parser (#485 PR 2) will find it.
fn parse_mistral_body(body: &str) -> Reading {
    let Some(split) = body.find(ARGS_OPEN) else {
        return Reading::Unarrived;
    };
    let name = body[..split].trim();
    if name.is_empty() {
        return Reading::Unreadable;
    }
    let arguments = &body[split + ARGS_OPEN.len()..];
    let mut values = serde_json::Deserializer::from_str(arguments).into_iter::<serde_json::Value>();
    match values.next() {
        // Nothing after `[ARGS]` yet: the call stopped on the marker itself.
        None => Reading::Unarrived,
        Some(Err(e)) if e.classify() == serde_json::error::Category::Eof => Reading::Unarrived,
        // Anything else is a body that arrived and is not a call — a syntax
        // error, or a value that is not an arguments object.
        //
        // **Stricter than [`parse_json_body`], and deliberately so**, which is
        // the one place the dialects differ on what they accept rather than on
        // how they are spelled. There, a non-object `arguments` is carried
        // through: the body is an object with a `name`, and *that* is what says
        // a call is here, so an odd field inside a structure already recognised
        // as a call reaches the registry, whose error is fed back and gives the
        // model a round to correct itself. Here the JSON is not a field of
        // anything — it **is** the arguments, and it is also the only proof the
        // call arrived — so a value that is not an arguments object is a body in
        // a shape this dialect does not recognise, and guessing at one is what
        // "each dialect judges only its own syntax" rules out. Pinned from both
        // sides by `the_two_envelopes_differ_on_a_non_object_arguments`.
        Some(parsed) => parsed.ok().filter(serde_json::Value::is_object).map_or(
            Reading::Unreadable,
            |arguments| {
                Reading::Call(ToolCall {
                    name: name.to_owned(),
                    arguments,
                })
            },
        ),
    }
}

/// [`Dialect::Xml`]: `<function=name>` wrapping `<parameter=key>value</parameter>`.
///
/// # What an argument value may contain
///
/// Everything after `<parameter=key>` is model text: it may hold `<`, `>`,
/// newlines, or something that reads as another tag, and the wire form offers no
/// escaping. So parameters are found by splitting on the *opening* tag, and a
/// value ends at the **last** `</parameter>` before the next one — not the first,
/// which would cut a value that quotes the closing tag, and not the first `<`,
/// which would truncate any value mentioning a type or a generic.
///
/// Two limits this accepts, both unreachable without the model emitting the
/// literal tag text inside a value: a value containing `<parameter=` splits into
/// two arguments, and a value containing `</function>` ends the call early.
/// Nothing on the wire distinguishes those from the real tags.
///
/// A missing `</function>` is not fatal — a generation cut short mid-call is
/// still a call, and reading it as one is the whole point of #489 — but a missing
/// or empty `<function=name>` is: without a name there is nothing to call.
fn parse_xml_body(body: &str) -> Option<ToolCall> {
    let after_open = body.find(FUNCTION_OPEN)? + FUNCTION_OPEN.len();
    let rest = &body[after_open..];
    let name_end = rest.find('>')?;
    let name = rest[..name_end].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let inner = &rest[name_end + 1..];
    let inner = inner.find(FUNCTION_CLOSE).map_or(inner, |i| &inner[..i]);

    let mut arguments = serde_json::Map::new();
    // `skip(1)`: the text before the first `<parameter=` is the template's own
    // newline, never an argument.
    for segment in inner.split(PARAMETER_OPEN).skip(1) {
        let Some(key_end) = segment.find('>') else {
            continue;
        };
        let key = segment[..key_end].trim();
        if key.is_empty() {
            continue;
        }
        let raw = &segment[key_end + 1..];
        let raw = raw.rfind(PARAMETER_CLOSE).map_or(raw, |i| &raw[..i]);
        // The wire form writes a newline either side of the value, so the value's
        // own leading/trailing whitespace is not recoverable and is not preserved.
        arguments.insert(key.to_owned(), xml_argument(raw.trim()));
    }
    Some(ToolCall {
        name,
        arguments: serde_json::Value::Object(arguments),
    })
}

/// Give an XML-dialect argument value the type it had before the template
/// flattened it, so `arguments` is shaped the same whichever dialect produced it.
///
/// The wire form is untyped — every value arrives as text — but
/// [`ToolCall::arguments`] is a `serde_json::Value` that the registry reads with
/// `as_u64`, `as_array` and `as_str`. Both XML templates write a string bare and
/// run a non-string through `tojson` (`qwen3.8-27b`) or Python's `str`
/// (`qwen3-coder-30b-a3b`), so a value that parses as JSON *and is not a string*
/// is one that had a type: `10` is the integer `limit` the `search` schema
/// declares, `["todo"]` is `debt`'s `categories` array. Both reach their tool
/// correctly through this. A JSON *string* body is not unwrapped — the wire form
/// writes strings unquoted, so quotes in the text are part of the value.
///
/// The limit, in the other direction: a value whose schema type is `string` but
/// whose text is itself valid JSON — `search(query="42")` — is typed as a number,
/// and `search` then answers "needs a string `query`". That is fed back to the
/// model, which has a round to correct itself; it is a visible, self-correcting
/// error rather than the silent wrong default an untyped `"10"` would give
/// `limit`. `qwen3-coder`'s Python rendering of a bool or `None` (`True`, `None`)
/// is not JSON and stays a string, which its schema-less `debt` filter tolerates.
fn xml_argument(raw: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::String(_)) | Err(_) => serde_json::Value::String(raw.to_owned()),
        Ok(typed) => typed,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Dialect, Disposition, Ending, Limits, Markup, REFUSAL, SuppressedTools, TOOL_CALL_OPEN,
        ToolCall, Unfinished, disposition, finish, read_markup, system_prompt, tool_system_prompt,
        unknown_argument,
    };
    use crate::engine::{
        ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message,
        ModelInfo,
    };
    use crate::thinking::Unterminated;
    use crate::tools::{ToolDef, ToolRegistry, chat_with_client_tools, chat_with_tools};
    use std::collections::HashSet;
    use std::fmt::Write as _;
    use std::sync::Mutex;

    /// A generation the engine says ended on an end-of-generation token — the
    /// ordinary case, and the one the dialect tests are about.
    fn stopped(content: &str) -> Completion {
        Completion {
            content: content.to_owned(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: FinishReason::Stop,
        }
    }

    /// A generation the engine says was cut off at `max_tokens`.
    fn truncated(content: &str) -> Completion {
        Completion {
            finish_reason: FinishReason::Length,
            ..stopped(content)
        }
    }

    /// The budgets a refusal names, with values a test can recognise in a message.
    fn limits() -> Limits {
        Limits {
            max_tokens: 64,
            rounds: 4,
        }
    }

    /// The call in `text`, read as a cleanly-ended generation.
    ///
    /// The dialect tests drive [`read_markup`] — the function the loop itself
    /// reads generations with — rather than a parser entry point sitting beside
    /// it. A dialect that parses in isolation but never reaches the loop is
    /// exactly the gap #489 opened in, so there is no isolated entry point left
    /// to pass in.
    fn call_in(text: &str) -> Option<ToolCall> {
        match read_markup(&stopped(text)) {
            Markup::Call(call) => Some(call),
            Markup::None | Markup::Unfinished(_) => None,
        }
    }

    #[test]
    fn parses_a_well_formed_tool_call() {
        let text = "sure\n<tool_call>{\"name\": \"explain\", \"arguments\": {\"key\": \"fn:foo\"}}</tool_call>";
        let call = call_in(text).expect("parsed");
        assert_eq!(
            call,
            ToolCall {
                name: "explain".to_owned(),
                arguments: serde_json::json!({"key": "fn:foo"}),
            }
        );
    }

    #[test]
    fn ignores_text_without_a_tool_call_or_malformed_json() {
        assert!(call_in("just a normal answer").is_none());
        assert!(call_in("<tool_call>not json</tool_call>").is_none());
        assert!(call_in("<tool_call>{\"arguments\":{}}</tool_call>").is_none());
    }

    #[test]
    fn system_prompt_lists_each_tool() {
        let tools = [ToolDef {
            name: "search".to_owned(),
            description: "find nodes".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("search() — find nodes"));
    }

    /// The advertised surface is rendered as signatures rather than as raw JSON
    /// Schema, and this pins the format — which issue #590 found nothing did, so
    /// the mass a model prefills on every turn was discoverable only by hand.
    ///
    /// Everything the schema declares about *calling* the tool has to survive:
    /// which arguments exist, which are required, their types, their permitted
    /// values and their bounds.
    #[test]
    fn a_tool_is_advertised_as_a_signature_that_keeps_what_the_schema_declared() {
        let tools = [ToolDef {
            name: "search".to_owned(),
            description: "find nodes".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 25 },
                    "order": { "type": "string", "enum": ["rank", "recent"] },
                    "kinds": { "type": "array", "items": { "type": "string" } },
                },
                "required": ["query"],
            }),
        }];
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
        // One exact line, because the rendering is the thing being pinned:
        // every argument present, `?` on each one `required` omits, each type
        // and its bound, and the description after an em dash.
        //
        // The order is sorted rather than declared, but this cannot claim to
        // hold that: a `serde_json::Map` is a `BTreeMap` here, so deleting the
        // sort in `advertise` leaves this assertion green. What holds the
        // ordering still is the sort; what notices if the ground under it moves
        // is `nothing_has_turned_on_preserve_order_behind_the_advertised_surface`
        // below.
        assert!(
            prompt.contains(
                "- search(kinds?: [str], limit?: int 1..25, order?: rank|recent, query: str) \
                 — find nodes"
            ),
            "{prompt}",
        );
        // The bound is now IN the prompt rather than only in a schema a model may
        // not validate against, which is the reason `model_limit` gives for each
        // tool restating it in prose.
        assert!(prompt.contains("1..25"), "{prompt}");
        // And the raw form is gone, or the rendering saved nothing.
        assert!(!prompt.contains("arguments schema:"), "{prompt}");
    }

    /// One thing in this module renders a `serde_json::Map` in the map's own
    /// iteration order: the verbatim fallback in `tool_system_prompt`, which
    /// hands the whole schema to `serde_json::to_string` — that has no sort and
    /// cannot have one, because being verbatim is the point of it. `advertise`
    /// beside it collects the properties into a `Vec` and sorts that, so an
    /// order flip leaves every signature it renders exactly where it was: the
    /// fallback is the surface that stays exposed to one.
    ///
    /// What holds the fallback still today is one thing not visible in this
    /// file: nothing in the *normal* dependency graph enables
    /// `serde_json/preserve_order`, so a `Map` is a `BTreeMap`. `tree-sitter`
    /// does enable it, but as a build-dependency, and `resolver = "3"` resolves
    /// those features separately.
    ///
    /// That is feature resolution rather than a decision anyone here made, so
    /// any dependency can flip it with no diff in this crate to show for it, and
    /// `Cargo.lock` records packages rather than features — grepping the lock
    /// would not see it. This is what sees it.
    ///
    /// A flip is worth being told about for both halves, not just the exposed
    /// one. The fallback starts emitting whatever key order the client sent; and
    /// the sort in `advertise` stops being belt-and-braces over a `BTreeMap` and
    /// becomes the only thing holding the advertised signatures still, which is
    /// a change in what that line is load-bearing for and in what deleting it
    /// would cost.
    ///
    /// It is deliberately not a test of the sort. The signature test above
    /// cannot distinguish a sorted `advertise` from a sorted `Map`, and nothing
    /// can while the `Map` is ordered; this asserts the assumption instead.
    #[test]
    fn nothing_has_turned_on_preserve_order_behind_the_advertised_surface() {
        let mut map = serde_json::Map::new();
        map.insert("b".to_owned(), serde_json::Value::Null);
        map.insert("a".to_owned(), serde_json::Value::Null);
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["a", "b"],
            "`serde_json/preserve_order` is enabled: a `serde_json::Map` now \
             iterates in insertion order. The sort in `advertise` has gone from \
             defensive to load-bearing and is now the only thing holding the \
             signatures still, and the verbatim fallback beside it has no sort \
             at all — it will emit whatever key order the client sent. Re-check \
             the advertised surface and the prompt-byte figures these tests \
             quote before accepting the change.",
        );
    }

    /// A generated schema — `$schema`, a nullable union type, a `default`, a
    /// `format` — is the shape a **real client** sends, and is what the rendering
    /// is worth most on. Measured on Roteiro's own MCP surface driven back in as
    /// a client payload: 21,180 → 17,454 prompt bytes.
    ///
    /// `Option<T>` becomes `"type": ["T", "null"]` with no `required` entry, so a
    /// renderer that only understood a bare string type would fall back on every
    /// tool and save nothing at all.
    #[test]
    fn a_generated_schema_renders_rather_than_falling_back() {
        let tools = [ToolDef {
            name: "debt".to_owned(),
            description: "list markers".to_owned(),
            parameters: serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "limit": {
                        "default": null,
                        "format": "uint32",
                        "maximum": 200,
                        "minimum": 1,
                        "type": ["integer", "null"],
                    },
                    "project": { "default": null, "type": ["string", "null"] },
                },
            }),
        }];
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
        assert!(
            prompt.contains("- debt(limit?: int(uint32) 1..200, project?: str) — list markers"),
            "{prompt}",
        );
    }

    /// **The renderer declines rather than approximating.** A client's argument
    /// shape is the contract Roteiro hands back for the client to execute, so a
    /// schema this cannot state — a nested object here — goes out verbatim.
    ///
    /// Summarising it lossily would leave the model calling a tool whose
    /// arguments no longer match what will run, which is the same failure
    /// `MAX_CLIENT_TOOL_BYTES` refuses to truncate for.
    #[test]
    fn a_schema_the_renderer_cannot_state_is_sent_verbatim() {
        for parameters in [
            // A nested object: the signature form has no way to say its shape.
            serde_json::json!({
                "type": "object",
                "properties": { "filter": { "type": "object", "properties": {} } },
            }),
            // A combinator, likewise.
            serde_json::json!({
                "type": "object",
                "properties": { "id": { "oneOf": [{ "type": "string" }] } },
            }),
            // `additionalProperties: true` means arguments beyond the ones named
            // are legal, which an exhaustive-looking signature would deny.
            serde_json::json!({
                "type": "object",
                "properties": { "a": { "type": "string" } },
                "additionalProperties": true,
            }),
            // `required` as a bare string, and as an array holding a non-string.
            // Neither is a `required` this understands, and the failure of
            // reading them as "nothing is required" points the one direction
            // that breaks the call: `name` would advertise as `name?: str`, the
            // model would omit it, and dispatch would reject the call. Absent
            // stays legitimate — the generated-schema test above renders a
            // schema carrying no `required` key at all.
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": "name",
            }),
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name", 1],
            }),
            // A fractional bound is legal JSON Schema on a `number` and the
            // signature has no form for `>=0.5`; rendering a bare `num` would
            // drop a bound the model is meant to respect.
            serde_json::json!({
                "type": "object",
                "properties": { "ratio": { "type": "number", "minimum": 0.5 } },
            }),
            // Likewise a `format` that is not a string.
            serde_json::json!({
                "type": "object",
                "properties": { "when": { "type": "string", "format": 7 } },
            }),
        ] {
            let tools = [ToolDef {
                name: "t".to_owned(),
                description: "d".to_owned(),
                parameters: parameters.clone(),
            }];
            let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
            assert!(
                prompt.contains("arguments schema:"),
                "must fall back for {parameters}: {prompt}",
            );
        }
    }

    /// An argument description repeated verbatim across tools is stated once.
    ///
    /// On Roteiro's own graph surface that is the `project` selector, carried
    /// identically by thirteen of fourteen tools — 1,690 wire bytes of one
    /// sentence. The rule is mechanical rather than a special case for a known
    /// argument, so a client that repeats its own gets the same treatment.
    #[test]
    fn an_argument_description_several_tools_repeat_is_stated_once() {
        let with_project = |name: &str, own: &str| ToolDef {
            name: name.to_owned(),
            description: "d".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "which hosted project" },
                    "only_here": { "type": "string", "description": own },
                },
            }),
        };
        let tools = [
            with_project("a", "peculiar to a"),
            with_project("b", "peculiar to b"),
        ];
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
        assert_eq!(
            prompt.matches("which hosted project").count(),
            1,
            "the repeated note belongs above the list, once: {prompt}",
        );
        assert!(
            prompt.contains("Arguments shared by several tools:"),
            "{prompt}"
        );
        // A note only one tool carries stays with that tool.
        assert_eq!(prompt.matches("peculiar to a").count(), 1, "{prompt}");
        assert_eq!(prompt.matches("peculiar to b").count(), 1, "{prompt}");
        // And every tool still advertises the argument in its own signature, so
        // hoisting the prose does not hide that the argument exists.
        assert_eq!(prompt.matches("project?: str").count(), 2, "{prompt}");
    }

    #[test]
    fn system_prompt_forces_grounding() {
        // Pin the grounding levers so a weak model cannot drift back to answering
        // from a node's name alone (the hallucination this prompt guards against):
        // answer only from tool output, read content (snippet/explain) before
        // describing, cite the keys used, and refuse rather than fabricate.
        let tools = [ToolDef {
            name: "search".to_owned(),
            description: "find nodes".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>(), true);
        let lower = prompt.to_lowercase();
        // Pin the *grounding* "only" (answer from the graph), not the "reply with
        // ONLY a tool call" formatting rule that also contains the bare word — so
        // this fails if the grounding instruction itself regresses.
        assert!(
            prompt.contains("using ONLY its Roteiro knowledge graph"),
            "answer only from tool output"
        );
        assert!(
            prompt.contains("snippet") && prompt.contains("explain"),
            "read the returned content before describing a node"
        );
        assert!(
            lower.contains("before describing"),
            "read content BEFORE describing the node"
        );
        assert!(lower.contains("cite"), "cite the node keys used");
        assert!(
            lower.contains("could not find"),
            "refuse rather than fabricate when the answer is absent"
        );
    }

    /// An engine scripted to emit a tool call first, then a final answer, so the
    /// full loop (inject → call → execute → feed back → answer) is exercised.
    struct ScriptedEngine {
        turns: Mutex<Vec<String>>,
        seen: Mutex<Vec<Vec<Message>>>,
        /// What the engine reports about how each generation ended. `Length` is
        /// the engine's own account of hitting `max_tokens`, which is what
        /// separates "cut off mid-call" from "stopped mid-call" — see
        /// [`read_markup`].
        finish_reason: FinishReason,
    }

    impl ScriptedEngine {
        fn new<S: AsRef<str>>(turns: &[S]) -> Self {
            Self {
                turns: Mutex::new(turns.iter().map(|t| t.as_ref().to_owned()).collect()),
                seen: Mutex::new(Vec::new()),
                finish_reason: FinishReason::Stop,
            }
        }
        /// As [`Self::new`], but every generation is reported as having hit the
        /// token cap — a model cut off part-way through what it was writing.
        fn truncating<S: AsRef<str>>(turns: &[S]) -> Self {
            Self {
                finish_reason: FinishReason::Length,
                ..Self::new(turns)
            }
        }
        /// The messages handed to generation number `round` (0-based).
        fn round(&self, round: usize) -> Vec<Message> {
            self.seen.lock().unwrap()[round].clone()
        }
    }

    impl Engine for ScriptedEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "scripted".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.seen.lock().unwrap().push(req.messages.clone());
            let next = self.turns.lock().unwrap().remove(0);
            on_token(&next);
            Ok(CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: self.finish_reason,
            })
        }
    }

    /// An engine that calls a tool on **every** generation and never answers —
    /// including the one run after the round budget is spent. The model that has
    /// decided it needs more lookups than it is allowed.
    struct AlwaysCalling {
        markup: String,
        generations: Mutex<usize>,
    }

    impl AlwaysCalling {
        fn new(markup: String) -> Self {
            Self {
                markup,
                generations: Mutex::new(0),
            }
        }
    }

    impl Engine for AlwaysCalling {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "scripted".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            _req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            *self.generations.lock().unwrap() += 1;
            on_token(&self.markup);
            Ok(CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: FinishReason::Stop,
            })
        }
    }

    struct EchoRegistry;
    impl ToolRegistry for EchoRegistry {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "echo".to_owned(),
                description: "echo the key".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }
        fn call(&self, name: &str, arguments: &serde_json::Value) -> Result<String, String> {
            assert_eq!(name, "echo");
            Ok(format!(
                "echoed:{}",
                arguments["key"].as_str().unwrap_or("")
            ))
        }
    }

    #[test]
    fn loop_executes_a_tool_then_returns_the_final_answer() {
        let engine = ScriptedEngine::new(&[
            "<tool_call>{\"name\":\"echo\",\"arguments\":{\"key\":\"abc\"}}</tool_call>",
            "the node abc is a function",
        ]);
        let req = ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "explain abc".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        };
        let out = chat_with_tools(&engine, &EchoRegistry, &req, 4).expect("completion");
        assert_eq!(out.content, "the node abc is a function");
        // Both scripted turns were consumed (tool round + answer round).
        assert!(engine.turns.lock().unwrap().is_empty());
    }

    #[test]
    fn no_tools_is_a_plain_completion() {
        struct NoTools;
        impl ToolRegistry for NoTools {
            fn tools(&self) -> Vec<ToolDef> {
                vec![]
            }
            fn call(&self, _: &str, _: &serde_json::Value) -> Result<String, String> {
                Err("unused".to_owned())
            }
        }
        let engine = ScriptedEngine::new(&["direct answer"]);
        let req = ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "hi".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        };
        let out = chat_with_tools(&engine, &NoTools, &req, 4).expect("completion");
        assert_eq!(out.content, "direct answer");
    }

    // ------------------------------------------ unknown tool arguments ---
    // **An argument key a tool does not declare is refused, never dropped.**
    // The measured defect: the two surfaces spell `debt`'s filter differently
    // (`kind` on MCP, `categories` here), and a dropped key returned every
    // marker in the repository as the filtered set the model asked for.

    /// The served `debt` schema, closed the way `roteiro`'s registry declares it
    /// — `categories` plus the spliced-in `project` — over a body that **panics
    /// if executed**.
    ///
    /// The refusal is asserted by construction: a call that reaches the tool
    /// fails the test outright, which is the half that matters. A test that only
    /// inspected the returned text would still pass if the tool ran and its
    /// unfiltered answer were thrown away.
    struct ClosedDebt;

    impl ToolRegistry for ClosedDebt {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "debt".to_owned(),
                description: "list intent-debt markers".to_owned(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "categories": { "type": "array", "items": { "type": "string" } },
                        "project": { "type": "string" },
                    },
                    "additionalProperties": false,
                }),
            }]
        }
        fn call(&self, name: &str, arguments: &serde_json::Value) -> Result<String, String> {
            assert!(
                arguments.get("kind").is_none(),
                "`{name}` was reached with an argument its schema does not declare — \
                 the refusal is what stops an unfiltered answer being returned as a \
                 filtered one",
            );
            Ok(format!("ran `{name}` with {arguments}"))
        }
    }

    /// One `<tool_call>` turn followed by an answer, run against [`ClosedDebt`],
    /// returning the whole `<tool_response>` the model was handed for it.
    fn debt_call_response(arguments: &str) -> String {
        let engine = ScriptedEngine::new(&[
            format!("<tool_call>{{\"name\":\"debt\",\"arguments\":{arguments}}}</tool_call>"),
            "here is what I found".to_owned(),
        ]);
        let req = ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "what todo markers are there?".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        };
        let out = chat_with_tools(&engine, &ClosedDebt, &req, 4).expect("completion");
        assert_eq!(out.content, "here is what I found");
        // The second generation's last turn is the tool response fed back.
        engine
            .round(1)
            .last()
            .expect("a tool response was fed back")
            .content
            .clone()
    }

    /// The defect, end to end: the *other* surface's spelling for the same
    /// argument, over the surface that does not know it.
    #[test]
    fn an_argument_the_tool_does_not_declare_is_refused_not_dropped() {
        let response = debt_call_response(r#"{"kind":["todo"]}"#);
        // What the caller actually receives — the model, which is who a tool
        // error is addressed to on this surface.
        assert!(
            response.contains("tool `debt` error: unknown argument `kind`"),
            "the refusal must name the offending key: {response}",
        );
        assert!(
            response.contains("`debt` takes only `categories`, `project`"),
            "and the keys that would have worked, as serde's own message does: \
             {response}",
        );
        assert!(
            response.contains("Nothing was run"),
            "and must say no answer was produced, so the model does not read the \
             refusal as an empty result: {response}",
        );
    }

    /// The other direction, and the one that stops the rule being satisfied by
    /// refusing everything: the declared keys still run — including `project`,
    /// which reaches the schema only through the registry's `with_project`
    /// wrapper, so this fails if the flag lands on an inner fragment the wrapper
    /// then extends.
    #[test]
    fn the_declared_arguments_still_run() {
        let response = debt_call_response(r#"{"categories":["todo"],"project":"roteiro"}"#);
        assert!(
            response.contains("ran `debt`"),
            "a call using only declared keys must execute: {response}",
        );
        assert!(
            !response.contains("unknown argument"),
            "and must not be refused: {response}",
        );
    }

    /// A schema that has not closed its argument object is left exactly as it
    /// was. The rule is one a tool declares, not one imposed on every
    /// implementor of [`ToolRegistry`] — see [`unknown_argument`].
    #[test]
    fn an_open_schema_still_tolerates_extra_keys() {
        let def = ToolDef {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
            }),
        };
        assert_eq!(
            unknown_argument(&def, &serde_json::json!({ "nonsense": 1 })),
            None,
        );
    }

    /// A tool that declares no arguments at all says so, rather than listing an
    /// empty set of alternatives.
    #[test]
    fn a_tool_taking_no_arguments_says_so() {
        let def = ToolDef {
            name: "list_projects".to_owned(),
            description: "the hosted projects".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        };
        let why =
            unknown_argument(&def, &serde_json::json!({ "project": "roteiro" })).expect("refused");
        assert!(why.contains("takes no arguments at all"), "{why}");
    }

    // ---------------------------------------------------------------- #485 ---
    // Client-supplied tools. The property under test throughout is the one in
    // this module's documentation: **Roteiro never executes a client's tool.**

    /// A graph registry that advertises `name` and **panics if executed**. Every
    /// test below that must not run a tool asserts it by construction rather than
    /// by inspecting a flag afterwards: an execution fails the test outright.
    ///
    /// It guards both directions — Roteiro never runs a *client's* tool, and
    /// under suppression it never runs a *graph* tool either.
    struct NeverCalled(&'static str);

    impl ToolRegistry for NeverCalled {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: self.0.to_owned(),
                description: "a graph tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }
        fn call(&self, name: &str, _arguments: &serde_json::Value) -> Result<String, String> {
            panic!(
                "executed `{name}` — this registry is unreachable by construction: \
                 Roteiro never runs a client's tool, and under suppression never \
                 runs a graph tool either"
            );
        }
    }

    /// A server with no graph tools at all: the shape a general-backend client
    /// meets when it supplies its own.
    struct NoGraphTools;

    impl ToolRegistry for NoGraphTools {
        fn tools(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        fn call(&self, name: &str, _arguments: &serde_json::Value) -> Result<String, String> {
            Err(format!("unknown tool `{name}`"))
        }
    }

    /// An engine that records every message list it is handed, so a test can
    /// assert what the model was actually told.
    struct RecordingEngine {
        reply: String,
        seen: Mutex<Vec<Vec<Message>>>,
    }

    impl RecordingEngine {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_owned(),
                seen: Mutex::new(Vec::new()),
            }
        }
        /// The system prompt of the first generation.
        fn first_system_prompt(&self) -> String {
            self.seen.lock().unwrap()[0][0].content.clone()
        }
    }

    impl Engine for RecordingEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "scripted".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.seen.lock().unwrap().push(req.messages.clone());
            on_token(&self.reply);
            Ok(CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: FinishReason::Stop,
            })
        }
    }

    /// A [`RecordingEngine`] that reports carrying tools itself, as
    /// `LlamaEngine` does.
    struct TemplateEngine(RecordingEngine);

    impl Engine for TemplateEngine {
        fn models(&self) -> Vec<ModelInfo> {
            self.0.models()
        }
        fn carries_tools(&self, _model: &str) -> bool {
            true
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.0.chat_stream(req, on_token)
        }
    }

    fn client_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_owned(),
            description: "a tool the client runs".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn user_request() -> ChatRequest {
        ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "weather in Berlin?".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        }
    }

    fn call_markup(name: &str, arguments: &str) -> String {
        format!("<tool_call>{{\"name\":\"{name}\",\"arguments\":{arguments}}}</tool_call>")
    }

    /// An engine that carries the tools is not sent them a second time.
    ///
    /// The duplication this pair exists to prevent: the model's own chat
    /// template renders every tool, and a system turn that lists them again
    /// states one set of tools twice in two different shapes. What must survive
    /// is the grounding prose, which is Roteiro's and is in no template.
    #[test]
    fn an_engine_that_carries_tools_gets_no_second_listing() {
        let engine = TemplateEngine(RecordingEngine::new("done"));
        let _ = chat_with_tools(&engine, &EchoRegistry, &user_request(), 4).expect("completion");

        let system = engine.0.first_system_prompt();
        let system = Message {
            role: "system".to_owned(),
            content: system,
        };
        assert!(
            !system.content.contains("Available tools:"),
            "the tool listing was sent as well as the template's: {}",
            system.content
        );
        assert!(
            !system.content.contains("echo("),
            "a tool was named in the system turn: {}",
            system.content
        );
        assert!(
            system.content.contains("Cite the node keys you used"),
            "the grounding rules went missing with the listing: {}",
            system.content
        );
    }

    /// A client's own tools suppress the graph, and the prompt stops claiming it.
    ///
    /// `SuppressedTools` empties the graph registry the moment a client supplies
    /// tools, so telling that model to answer "using ONLY the Roteiro knowledge
    /// graph" through a `search` and an `explain` it was never given is an
    /// instruction it cannot follow. It can only refuse, or keep calling tools
    /// that are not there.
    #[test]
    fn a_client_tool_turn_does_not_claim_a_graph_that_was_suppressed() {
        let engine = RecordingEngine::new("done");
        let _ = chat_with_client_tools(
            &engine,
            &EchoRegistry,
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");

        let prompt = engine.first_system_prompt();
        assert!(
            !prompt.contains("Roteiro knowledge graph"),
            "claimed the graph while it was suppressed: {prompt}"
        );
        assert!(
            !prompt.contains("`search`"),
            "named a graph tool the model was not given: {prompt}"
        );
        // What must survive: the envelope, and grounding in what tools returned.
        assert!(prompt.contains("<tool_call>"), "{prompt}");
        assert!(prompt.contains("Ground every claim"), "{prompt}");
    }

    /// Both variants name the `<tool_call>` envelope.
    ///
    /// It is the one thing that cannot be delegated to a chat template: a
    /// template is free to have trained the model on a different envelope, and
    /// `Dialect::ALL` can only read the envelopes Roteiro has met. Dropping the
    /// tool *list* for a template-carrying engine must not drop the envelope with
    /// it, or the model's call comes back in an envelope this server does not
    /// know and is handed to the user as prose — which is #489 exactly.
    #[test]
    fn every_variant_states_the_envelope_this_server_parses() {
        let tools = [client_tool("echo")];
        let refs: Vec<&ToolDef> = tools.iter().collect();
        for prompt in [
            system_prompt(&refs, true, true),
            system_prompt(&[], false, true),
        ] {
            assert!(
                prompt.contains("<tool_call>"),
                "the envelope went unstated: {prompt}"
            );
        }
    }

    /// An engine that does *not* carry them still gets the full listing.
    ///
    /// The remote tier is this case: its ADR-0019 payload has no `tools` field,
    /// so the conversation is the only way a tool can reach the model. Dropping
    /// the listing for everyone would have left that path advertising nothing.
    #[test]
    fn an_engine_that_carries_nothing_still_gets_the_listing() {
        let engine = RecordingEngine::new("done");
        let _ = chat_with_tools(&engine, &EchoRegistry, &user_request(), 4).expect("completion");

        let system = Message {
            role: "system".to_owned(),
            content: engine.first_system_prompt(),
        };
        assert!(
            system.content.contains("Available tools:"),
            "an engine that carries no tools was sent none: {}",
            system.content
        );
        assert!(system.content.contains("echo("), "{}", system.content);
    }

    #[test]
    fn a_client_tool_call_ends_the_loop_and_is_never_executed() {
        let engine = ScriptedEngine::new(&[call_markup("get_weather", r#"{"city":"Berlin"}"#)]);
        let out = chat_with_client_tools(
            &engine,
            &NeverCalled("graph_only_tool"),
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");

        assert_eq!(out.client_tool_calls.len(), 1);
        assert_eq!(out.client_tool_calls[0].name, "get_weather");
        assert_eq!(
            out.client_tool_calls[0].arguments,
            serde_json::json!({"city": "Berlin"})
        );
        assert!(
            out.client_tool_calls[0].id.starts_with("call_"),
            "a correlation id the client can echo as `tool_call_id`: {}",
            out.client_tool_calls[0].id
        );
        // Exactly one generation: the loop stopped rather than executing the tool
        // and feeding a result back for a second round.
        assert!(
            engine.turns.lock().unwrap().is_empty(),
            "one generation only"
        );
    }

    #[test]
    fn client_tools_suppress_the_graph_tools() {
        // The graph tool is deliberately named something the boilerplate prose
        // cannot contain, so the negative assertion means what it says.
        let engine = RecordingEngine::new("no tool needed");
        let out = chat_with_client_tools(
            &engine,
            &NeverCalled("graph_only_tool"),
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");
        assert!(out.client_tool_calls.is_empty());

        let prompt = engine.first_system_prompt();
        assert!(
            prompt.contains("get_weather"),
            "the client's tool is advertised: {prompt}"
        );
        assert!(
            !prompt.contains("graph_only_tool"),
            "the graph tools are suppressed, not merged: {prompt}"
        );
    }

    #[test]
    fn graph_tools_are_advertised_when_the_client_sends_none() {
        // The other half of suppression: with no client tools, nothing changes.
        let engine = RecordingEngine::new("no tool needed");
        let out = chat_with_client_tools(&engine, &EchoRegistry, &[], &user_request(), 4)
            .expect("outcome");
        assert!(out.client_tool_calls.is_empty());
        assert!(
            engine.first_system_prompt().contains("echo("),
            "the graph tools are still advertised"
        );
    }

    #[test]
    fn any_client_tool_in_a_turn_returns_all_of_them_and_executes_none() {
        // The rule that prevents silent transcript corruption. `parse_tool_calls`
        // cannot yet produce a multi-call turn, so the rule is asserted here
        // directly — it has to be correct before it can be reached.
        let client: HashSet<&str> = ["client_tool"].into_iter().collect();
        let graph = ToolCall {
            name: "graph_tool".to_owned(),
            arguments: serde_json::json!({}),
        };
        let theirs = ToolCall {
            name: "client_tool".to_owned(),
            arguments: serde_json::json!({}),
        };

        assert_eq!(disposition(&[], &client), Disposition::Execute);
        assert_eq!(
            disposition(&[graph.clone(), graph.clone()], &client),
            Disposition::Execute,
            "an all-graph turn runs as before"
        );
        assert_eq!(
            disposition(std::slice::from_ref(&theirs), &client),
            Disposition::ReturnAll
        );
        assert_eq!(
            disposition(&[graph, theirs], &client),
            Disposition::ReturnAll,
            "a mixed turn returns everything and executes nothing — not the graph \
             half executed and the client half returned"
        );
    }

    #[test]
    fn a_client_tool_shadows_a_graph_tool_of_the_same_name() {
        // The client wins its own namespace. `NeverCalled` panics if executed, so
        // resolving `echo` to the graph registry instead would fail the test.
        let engine = ScriptedEngine::new(&[call_markup("echo", r#"{"key":"abc"}"#)]);
        let out = chat_with_client_tools(
            &engine,
            &NeverCalled("echo"),
            &[client_tool("echo")],
            &user_request(),
            4,
        )
        .expect("outcome");
        assert_eq!(out.client_tool_calls.len(), 1);
        assert_eq!(out.client_tool_calls[0].name, "echo");
    }

    #[test]
    fn a_client_call_in_the_final_generation_is_returned_not_leaked_as_prose() {
        // The round budget is exhausted, so the last generation is not looped on.
        // A client-tool call there must still be returned structurally — handing
        // the raw `<tool_call>` markup back as the assistant's answer is the
        // silent-wrong-answer shape #489 describes.
        let engine = ScriptedEngine::new(&[
            call_markup("nope", "{}"),
            call_markup("get_weather", r#"{"city":"Berlin"}"#),
        ]);
        let out = chat_with_client_tools(
            &engine,
            &NoGraphTools,
            &[client_tool("get_weather")],
            &user_request(),
            1,
        )
        .expect("outcome");
        assert_eq!(out.client_tool_calls.len(), 1);
        assert_eq!(out.client_tool_calls[0].name, "get_weather");
    }

    #[test]
    fn a_graph_tool_call_under_suppression_is_refused_not_executed() {
        // The mirror of "Roteiro never executes a client's tool", and the one
        // that suppression-by-empty-list did NOT provide: the graph tools are
        // unadvertised, but `tool_system_prompt`'s own prose names `search` and
        // `explain`, so a model is primed to call exactly those. Under
        // suppression such a call must be refused, not executed.
        //
        // `NeverCalled` panics if executed, so this asserts the guarantee by
        // construction — reverting the registry swap in `chat_with_client_tools`
        // makes this test abort inside the registry rather than fail an
        // assertion.
        let engine = ScriptedEngine::new(&[
            call_markup("search", r#"{"query":"secrets"}"#),
            "I could not use that tool.".to_owned(),
        ]);
        let out = chat_with_client_tools(
            &engine,
            &NeverCalled("search"),
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");

        // Not returned to the client either: `search` is not the client's tool.
        assert!(out.client_tool_calls.is_empty());
        assert_eq!(out.completion.content, "I could not use that tool.");

        // The refusal was fed back, and it says why rather than "unknown tool" —
        // the tool exists, it is out of scope for this request.
        let second = engine.round(1);
        // Matched on the `user` turn specifically: the system prompt also mentions
        // `<tool_response>`, in the prose telling the model to expect one.
        let refusal = second
            .iter()
            .find(|m| m.role == "user" && m.content.starts_with("<tool_response>"))
            .expect("a tool response was fed back");
        assert!(refusal.content.contains("not available"), "{refusal:?}");
        assert!(
            refusal.content.contains("supplied its own tools"),
            "{refusal:?}"
        );
    }

    #[test]
    fn the_suppressed_registry_advertises_and_executes_nothing() {
        // The two halves of suppression read from one binding, so they cannot
        // drift apart. Pin both on the type directly.
        assert!(SuppressedTools.tools().is_empty());
        let refused = SuppressedTools
            .call("search", &serde_json::json!({}))
            .expect_err("must never succeed");
        assert!(refused.contains("search"), "{refused}");
        assert!(refused.contains("not available"), "{refused}");
    }

    #[test]
    fn client_tools_alone_still_drive_the_loop_on_a_server_with_no_graph_tools() {
        // A general-backend client against a `roteiro serve` with no graph store:
        // its tools must still reach the model rather than falling through to a
        // plain completion.
        let engine = RecordingEngine::new("no tool needed");
        chat_with_client_tools(
            &engine,
            &NoGraphTools,
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");
        assert!(engine.first_system_prompt().contains("get_weather"));
    }

    // ---------------------------------------------------------------- #489 ---
    // The XML call dialect. `qwen3-coder-30b-a3b` and `qwen3.8-27b` ship chat
    // templates that train a `<function=…>` block nested inside `<tool_call>`,
    // and a body the parser did not understand did not *fail* — it made the call
    // invisible, so the loop read the generation as the model's final answer and
    // handed the raw markup to the user as prose.

    /// The wire form both Qwen XML templates render: `<function=name>` on its own
    /// line, then `<parameter=key>` / value / `</parameter>` per argument, each on
    /// its own line, then `</function>`. Built from the templates' own string
    /// concatenations so the tests below are exercising the real syntax.
    fn xml_markup(name: &str, args: &[(&str, &str)]) -> String {
        let mut out = format!("<tool_call>\n<function={name}>\n");
        for (key, value) in args {
            let _ = writeln!(out, "<parameter={key}>\n{value}\n</parameter>");
        }
        out.push_str("</function>\n</tool_call>");
        out
    }

    #[test]
    fn parses_the_xml_dialect_a_qwen_template_trains() {
        let call = call_in(&xml_markup("explain", &[("key", "fn:foo")]))
            .expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(
            call,
            ToolCall {
                name: "explain".to_owned(),
                arguments: serde_json::json!({"key": "fn:foo"}),
            },
            "the same `ToolCall` the JSON dialect yields for the same call"
        );
    }

    #[test]
    fn xml_reasoning_before_the_call_is_not_part_of_it() {
        // The templates explicitly permit "optional reasoning for your function
        // call in natural language BEFORE the function call", so a leading
        // sentence is normal output, not a malformed call.
        let text = format!(
            "I should look that up.\n{}",
            xml_markup("search", &[("query", "window")])
        );
        let call =
            call_in(&text).expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments, serde_json::json!({"query": "window"}));
    }

    #[test]
    fn an_xml_argument_keeps_a_value_full_of_markup() {
        // Argument values are arbitrary model text: angle brackets, newlines, and
        // even the closing tag itself. Ending the value at the first `<`, or at
        // the first `</parameter>`, would truncate this query silently — the
        // model would get an answer to a question it did not ask.
        let value = "impl<T> where T: Iterator<Item = u8>\nand a literal </parameter> in the text";
        let call = call_in(&xml_markup("search", &[("query", value)]))
            .expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(call.arguments["query"], serde_json::json!(value));
    }

    #[test]
    fn xml_arguments_arrive_typed_like_the_json_dialect() {
        // The wire form is untyped, but the graph registry reads `limit` with
        // `as_u64` and `categories` with `as_array`: handing those on as strings
        // would not error, it would fall back to the default limit and to "all
        // categories" — the model's request quietly ignored.
        let call = call_in(&xml_markup(
            "search",
            &[
                ("query", "window"),
                ("limit", "25"),
                ("categories", "[\"todo\", \"fixme\"]"),
            ],
        ))
        .expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(call.arguments["query"], serde_json::json!("window"));
        assert_eq!(call.arguments["limit"], serde_json::json!(25));
        assert_eq!(
            call.arguments["categories"],
            serde_json::json!(["todo", "fixme"])
        );
    }

    #[test]
    fn a_string_argument_whose_text_is_json_is_typed_as_json() {
        // The declared limit of typing an untyped wire, pinned so it stays a
        // decision rather than a surprise. `search(query="42")` reaches the tool
        // as the number 42 and `search` answers "needs a string `query`", which
        // is fed back and gives the model a round to correct itself — visible and
        // self-correcting, unlike the silent default an untyped `limit` would get.
        let call = call_in(&xml_markup("search", &[("query", "42")]))
            .expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(call.arguments["query"], serde_json::json!(42));
        // Quotes in the value are the value's own: the wire form writes strings
        // bare, so a quoted body is text that happened to contain quotes.
        let call = call_in(&xml_markup("search", &[("query", "\"42\"")]))
            .expect("an XML-dialect body is a tool call, not the model's answer");
        assert_eq!(call.arguments["query"], serde_json::json!("\"42\""));
    }

    #[test]
    fn a_body_in_neither_dialect_is_still_no_call() {
        // Understanding a second dialect must not slide into accepting anything.
        // This one passes before the fix as well as after, by construction: it
        // exists to fail if a later widening makes the parser guess.
        for body in [
            "not json, and not xml either",
            "<function>search</function>",
            "<function=>\n<parameter=key>fn:foo</parameter>\n</function>",
            "<parameter=key>fn:foo</parameter>",
            "{\"arguments\": {\"key\": \"fn:foo\"}}",
        ] {
            assert!(
                call_in(&format!("<tool_call>{body}</tool_call>")).is_none(),
                "accepted a body in neither dialect: {body}"
            );
        }
    }

    #[test]
    fn every_dialect_parses_to_the_same_call() {
        // Driven from `Dialect::ALL`, which is the only thing that makes a
        // dialect reachable: a third one has to render this call and arrive at
        // the same `ToolCall`, arguments and their types included. Two parsers
        // that agree today and drift tomorrow is the shape being avoided.
        let expected = ToolCall {
            name: "search".to_owned(),
            arguments: serde_json::json!({"query": "window", "limit": 5}),
        };
        for dialect in Dialect::ALL {
            assert_eq!(
                call_in(&rendered_call(dialect)),
                Some(expected.clone()),
                "{dialect:?} produced a different call"
            );
        }
    }

    /// The one call `search(query="window", limit=5)`, written in `dialect`'s own
    /// wire form — envelope included, because since #592 the envelope is part of
    /// the form.
    ///
    /// The single place a dialect's wire form is spelled out for the tests driven
    /// from `Dialect::ALL`, so a fourth dialect supplies one arm and is then held
    /// to all of them at once: it must arrive as the same `ToolCall` as the
    /// others (`every_dialect_parses_to_the_same_call`), and no prefix of it may
    /// be read as a whole call (`no_dialect_reads_a_truncated_call_as_a_whole_one`).
    fn rendered_call(dialect: Dialect) -> String {
        match dialect {
            Dialect::Json => call_markup("search", "{\"query\":\"window\",\"limit\":5}"),
            Dialect::Xml => xml_markup("search", &[("query", "window"), ("limit", "5")]),
            Dialect::Mistral => mistral_markup("search", "{\"query\":\"window\",\"limit\":5}"),
        }
    }

    #[test]
    fn the_loop_runs_an_xml_dialect_call_instead_of_answering_with_it() {
        // The defect is not in the parser alone. An unparsed call is not an error
        // — it is *absence* — so the loop reads the generation as the model's
        // final answer and returns the raw markup to the user. The parser unit
        // tests above cannot see that; this one can.
        let engine = ScriptedEngine::new(&[
            xml_markup("echo", &[("key", "abc")]),
            "the node abc is a function".to_owned(),
        ]);
        let req = ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "explain abc".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        };
        let out = chat_with_tools(&engine, &EchoRegistry, &req, 4).expect("completion");
        assert_eq!(
            out.content, "the node abc is a function",
            "the answer, not the tool call rendered as prose"
        );
        assert!(
            engine.turns.lock().unwrap().is_empty(),
            "both rounds ran: the call was executed and fed back"
        );
        let fed_back = engine
            .round(1)
            .into_iter()
            .find(|m| m.role == "user" && m.content.starts_with("<tool_response>"))
            .expect("a tool result was fed back");
        assert!(
            fed_back.content.contains("echoed:abc"),
            "the XML argument reached the tool: {fed_back:?}"
        );
    }

    #[test]
    fn an_xml_dialect_client_tool_call_is_returned_structurally() {
        // The same silent-wrong-answer shape on the #485 path: a client whose
        // model wrote XML would receive `finish_reason: "stop"` and the markup as
        // the assistant's message, instead of a tool call it could run.
        let engine = ScriptedEngine::new(&[xml_markup("get_weather", &[("city", "Berlin")])]);
        let out = chat_with_client_tools(
            &engine,
            &NeverCalled("graph_only_tool"),
            &[client_tool("get_weather")],
            &user_request(),
            4,
        )
        .expect("outcome");
        assert_eq!(out.client_tool_calls.len(), 1);
        assert_eq!(out.client_tool_calls[0].name, "get_weather");
        assert_eq!(
            out.client_tool_calls[0].arguments,
            serde_json::json!({"city": "Berlin"})
        );
    }

    // ---------------------------------------------------------------- #592 ---
    // Reachability. `Dialect::ALL` used to guarantee that every dialect was
    // handled *consistently* and nothing about any of them being reachable: the
    // `<tool_call>` wrapper was searched for one layer above `Dialect`, so a
    // model whose call form does not nest inside it never reached a dialect at
    // all. `voxtral-mini-3b`'s template is that model — measured on the real
    // embedded template, it renders `[TOOL_CALLS]name[ARGS]{…}` and contains no
    // `<tool_call>` anywhere — so once its template is rendered (#492) it emits
    // a well-formed call the parser could not see, and the loop hands the raw
    // markup to the user as prose. That is #489's failure mode by a new route.

    /// The wire form `voxtral-mini-3b`'s template renders, built from the
    /// template's own concatenation: `"[TOOL_CALLS]" + name + "[ARGS]" +
    /// arguments`. No wrapper, and no closing marker of any kind — the call ends
    /// where its JSON ends.
    fn mistral_markup(name: &str, arguments: &str) -> String {
        format!("[TOOL_CALLS]{name}[ARGS]{arguments}")
    }

    #[test]
    fn a_call_in_no_wrapper_at_all_still_reaches_a_dialect() {
        // The reachability miss itself, at the smallest scale that shows it.
        // Before the fix this markup returned `Wrapper::Absent`, which the loop
        // read as "no call, this is the final answer".
        let markup = mistral_markup("explain", "{\"key\":\"fn:foo\"}");
        assert!(
            !markup.contains(TOOL_CALL_OPEN),
            "the point of this case is that there is no wrapper: {markup}"
        );
        assert_eq!(
            call_in(&markup),
            Some(ToolCall {
                name: "explain".to_owned(),
                arguments: serde_json::json!({"key": "fn:foo"}),
            }),
            "the same `ToolCall` the wrapped dialects yield for the same call"
        );
    }

    #[test]
    fn no_dialect_reads_a_truncated_call_as_a_whole_one() {
        // The completeness rule, held over every dialect at once — which is what
        // makes it *uniform* rather than a promise in a comment. A dialect owns
        // its envelope now, so it also owns "did this call arrive?", and a
        // dialect that answered that question leniently would run a truncated
        // call as if it were whole: a different question, silently answered,
        // which is the failure class #489 is about.
        //
        // Every proper prefix, because truncation happens wherever the token cap
        // falls. `Envelope::Delimited` survives this because its closing marker
        // is missing from every prefix; `Envelope::SelfClosing` survives it
        // because the only body grammar allowed to be self-closing is one that
        // rejects a truncated body. Declare `Dialect::Xml` self-closing —
        // `parse_xml_body` tolerates a missing `</function>` and a missing
        // `</parameter>` — and this fails on the prefix that drops them.
        for dialect in Dialect::ALL {
            let whole = rendered_call(dialect);
            assert!(
                matches!(read_markup(&stopped(&whole)), Markup::Call(_)),
                "{dialect:?} cannot read its own rendering, so the prefixes prove nothing"
            );
            for cut in 0..whole.len() {
                if !whole.is_char_boundary(cut) {
                    continue;
                }
                let prefix = &whole[..cut];
                assert!(
                    !matches!(read_markup(&stopped(prefix)), Markup::Call(_)),
                    "{dialect:?} read a truncated call as a whole one: {prefix:?}"
                );
            }
        }
    }

    #[test]
    fn the_loop_runs_a_wrapperless_dialect_call_instead_of_answering_with_it() {
        // The parser tests above cannot see the defect that matters: an unparsed
        // call is not an error, it is *absence*, so the loop reads the generation
        // as the model's final answer and returns the raw markup to the user.
        let engine = ScriptedEngine::new(&[
            mistral_markup("echo", "{\"key\":\"abc\"}"),
            "the node abc is a function".to_owned(),
        ]);
        let out = chat_with_tools(&engine, &EchoRegistry, &user_request(), 4).expect("completion");
        assert_eq!(
            out.content, "the node abc is a function",
            "the answer, not the tool call rendered as prose"
        );
        let fed_back = engine
            .round(1)
            .into_iter()
            .find(|m| m.role == "user" && m.content.starts_with("<tool_response>"))
            .expect("a tool result was fed back");
        assert!(
            fed_back.content.contains("echoed:abc"),
            "the wrapperless call's argument reached the tool: {fed_back:?}"
        );
    }

    #[test]
    fn a_wrapperless_call_cut_off_in_its_arguments_is_not_an_answer() {
        // The half of #592 that is easy to lose: making a form reachable also
        // makes its *truncations* reachable, and a form with no closing marker
        // has no marker to notice missing. The JSON body is the proof instead —
        // `serde_json` reports end-of-input, which is truncation and not
        // malformation — so this is refused rather than published as prose, and
        // refused with the sentence for a model that stopped mid-call.
        let engine = ScriptedEngine::new(&["[TOOL_CALLS]echo[ARGS]{\"key\":\"ab"]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");
        assert_refusal(&out.content);
        // The phrase that separates this refusal from the token-cap one, and the
        // reason it is not "never closed it": the sentence serves every
        // `Envelope`, and this generation had no closing marker to be missing.
        assert!(out.content.contains("never finished it"), "{}", out.content);
    }

    #[test]
    fn a_wrapperless_call_cut_off_before_its_arguments_is_not_an_answer() {
        // Cut earlier: the template writes `[ARGS]` immediately after the name,
        // so a body without it is a call still writing its name rather than a
        // body Roteiro cannot read. Naming it a truncation is what puts
        // `max_tokens` in the refusal instead of a request to file a bug.
        let engine = ScriptedEngine::truncating(&["[TOOL_CALLS]ech"]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");
        assert_refusal(&out.content);
        assert!(out.content.contains("`max_tokens` = 64"), "{}", out.content);
    }

    #[test]
    fn a_wrapperless_body_in_no_dialect_is_still_no_call() {
        // Reaching a second envelope must not slide into accepting anything. A
        // body that arrived and is not a call is `Unreadable` — refused, and
        // never run — which is the same answer the wrapped dialects give.
        for body in [
            "[TOOL_CALLS][ARGS]{\"key\":\"fn:foo\"}",
            "[TOOL_CALLS]echo[ARGS]\"just a string\"",
            "[TOOL_CALLS]echo[ARGS][{\"key\":\"fn:foo\"}]",
            "[TOOL_CALLS]echo[ARGS]{\"key\":,}",
        ] {
            assert!(
                call_in(body).is_none(),
                "accepted a body in no dialect: {body}"
            );
            assert!(
                matches!(
                    read_markup(&stopped(body)),
                    Markup::Unfinished(Unfinished::Unreadable)
                ),
                "a body that arrived and is not a call is unreadable, not absent: {body}"
            );
        }
    }

    #[test]
    fn an_envelope_in_no_dialect_is_still_published_as_prose() {
        // **The gap #592 does not close**, pinned so the fix is not read as
        // wider than it is. `Dialect::ALL` is now the whole list of what can be
        // read — which makes it, equally, the whole list of what can be
        // *noticed*. A model emitting a fourth envelope is `Absent` to every
        // dialect, `None` to the fold, and its generation becomes the answer.
        //
        // `<|python_tag|>` is Llama 3.1's, chosen because it is real rather than
        // invented: this is the shape of the next report, not a hypothetical.
        //
        // Which is why `system_prompt` states the `<tool_call>` envelope even to
        // an engine whose template already carried the tools. That instruction
        // is the only thing standing between an unlearned envelope and #489, and
        // this test is what says so rather than leaving it to a comment. A fourth
        // dialect landing in `ALL` should make this fail — that is the signal to
        // pick a different unlearned envelope, not to delete the test.
        let unlearned = "<|python_tag|>{\"name\": \"echo\", \"arguments\": {\"key\": \"abc\"}}";
        assert!(
            matches!(read_markup(&stopped(unlearned)), Markup::None),
            "an envelope in no dialect is not recognised, by construction"
        );
        let engine = ScriptedEngine::new(&[unlearned]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");
        assert_eq!(
            out.content, unlearned,
            "and so it reaches the user unchanged — the residual risk the tool \
             system prompt exists to reduce"
        );
    }

    #[test]
    fn the_two_envelopes_differ_on_a_non_object_arguments() {
        // The one place the dialects differ on what they *accept*, recorded from
        // both sides so it stays a decision rather than becoming a surprise.
        //
        // The wrapped body is an object carrying a `name`, and that is what says
        // a call is here; an odd `arguments` inside a structure already
        // recognised as a call is carried to the registry, whose error is fed
        // back and gives the model a round to correct itself. That behaviour is
        // older than #592 and is asserted here **unchanged** — three of the five
        // registry models emit this dialect.
        let call = call_in("<tool_call>{\"name\":\"echo\",\"arguments\":\"a string\"}</tool_call>")
            .expect("the JSON dialect carries the call whatever `arguments` holds");
        assert_eq!(call.arguments, serde_json::json!("a string"));

        // The wrapperless body is not a field of anything — it *is* the
        // arguments, and also the only proof the call arrived — so there is no
        // surrounding structure left to recognise, and a scalar is refused
        // rather than guessed at.
        assert!(matches!(
            read_markup(&stopped(&mistral_markup("echo", "\"a string\""))),
            Markup::Unfinished(Unfinished::Unreadable)
        ));
    }

    #[test]
    fn a_call_that_did_not_arrive_outranks_one_that_could_not_be_read() {
        // Several dialects can answer about one generation now, so the fold in
        // `read_markup` has an order — and an order nothing exercises is an order
        // that drifts. This generation is `Unarrived` to the wrapped dialects (no
        // `</tool_call>`) and `Unreadable` to the wrapperless one (a call with no
        // name). Both refuse, so the safety property is not at stake; what is at
        // stake is which sentence the user gets, and the truncation reading is the
        // one backed by `finish_reason` rather than the one asking for a bug
        // report about a generation that was merely cut off.
        let mixed = "<tool_call>{\"name\":\"echo\"[TOOL_CALLS][ARGS]{}";
        assert!(matches!(
            read_markup(&stopped(mixed)),
            Markup::Unfinished(Unfinished::CutShort)
        ));
        assert!(matches!(
            read_markup(&truncated(mixed)),
            Markup::Unfinished(Unfinished::CutAtTokenCap)
        ));
    }

    #[test]
    fn no_refusal_contains_any_dialects_opening_marker() {
        // Roteiro's refusals are assistant-slot text: they are shown to the user
        // *and* can be carried back into a following turn, so a refusal
        // containing a dialect's opening marker would be read as a call by the
        // very function that wrote it. That rule used to be a comment naming one
        // literal; it is now driven from `Dialect::ALL`, so a dialect added later
        // is checked against every refusal without anyone remembering to.
        let markup = call_markup("echo", "{}");
        let refusals = [
            finish(stopped(&markup), Ending::Answer, limits()),
            finish(
                stopped(&markup),
                Ending::Unfinished(Unfinished::CutAtTokenCap),
                limits(),
            ),
            finish(
                stopped(&markup),
                Ending::Unfinished(Unfinished::CutShort),
                limits(),
            ),
            finish(
                stopped(&markup),
                Ending::Unfinished(Unfinished::Unreadable),
                limits(),
            ),
            finish(stopped(&markup), Ending::Exhausted, limits()),
            finish(
                stopped(&markup),
                Ending::Unanswered(Unterminated::CutAtTokenCap),
                limits(),
            ),
            finish(
                stopped(&markup),
                Ending::Unanswered(Unterminated::CutShort),
                limits(),
            ),
        ];
        for out in refusals {
            let content = out.completion.content;
            assert!(
                content.starts_with(REFUSAL),
                "not a refusal, so this proves nothing: {content}"
            );
            for dialect in Dialect::ALL {
                let marker = dialect.envelope().opening();
                assert!(
                    !content.contains(marker),
                    "a refusal carries {dialect:?}'s opening marker `{marker}`, so the next \
                     turn reads Roteiro's own words as a call: {content}"
                );
            }
        }
    }

    // ------------------------------------------------------- #489, the rest ---
    // The XML dialect above was one cause of three. The other two do not involve
    // a dialect at all — the model writes a call the parser *would* understand,
    // and the loop still hands it to the user as prose:
    //
    //   1. the wrapper is never closed, so there is no body to parse;
    //   2. the round budget runs out with the model still calling.
    //
    // Both are asserted through the loop, because both are properties of how the
    // loop reads a generation rather than of the parser.

    /// The Ask request shape: graph tools, no client tools. `client_names` is
    /// empty here, which is why the budget-exhausted branch could not be saved by
    /// the client-tool half of the old code.
    fn ask_request() -> ChatRequest {
        ChatRequest {
            tools: None,
            model: "scripted".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "what does `window` do?".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        }
    }

    /// Assert `content` is Roteiro speaking, not the model's markup passed off as
    /// prose. The negative half is the defect; the positive half is the refusals
    /// rule in `docs/REVIEW_CHECKLIST.md` — say who is speaking, and say what to
    /// do next.
    fn assert_refusal(content: &str) {
        assert!(
            !content.contains("<tool_call>"),
            "tool-call markup reached the user as the answer: {content}"
        );
        assert!(
            content.starts_with("Roteiro: "),
            "a refusal says who is speaking: {content}"
        );
    }

    #[test]
    fn an_unclosed_tool_call_is_a_call_that_did_not_arrive_not_an_answer() {
        // Cause 1, measured at 4/12 rounds on `qwen3-coder-30b-a3b`. The model
        // opens `<tool_call>` and stops before closing it, so the body never
        // parses, so the old parser answered `None` — and `None` meant "no call,
        // this is the final answer". The user got `<tool_call>{"name":"search"`
        // as the assistant's reply.
        let engine = ScriptedEngine::new(&["<tool_call>{\"name\":\"search\""]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");

        assert_refusal(&out.content);
        // The model stopped of its own accord rather than hitting the cap, so the
        // way forward is the model, not a bigger budget.
        //
        // The phrase moved from "never closed it" to "never finished it" in #592,
        // and the assertion moved with it rather than being loosened. What it
        // pins is unchanged — this is the stopped-mid-call sentence, not the
        // token-cap one — but the old wording described a closing marker, and one
        // sentence now serves envelopes that have none.
        assert!(out.content.contains("never finished it"), "{}", out.content);
        assert!(
            out.content.contains("roteiro model list"),
            "{}",
            out.content
        );
    }

    #[test]
    fn a_call_cut_off_at_the_token_cap_names_the_cap() {
        // The same unclosed wrapper, but the engine reports `length` — it hit
        // `max_tokens` mid-call. That is evidence rather than inference
        // (`rto_llama` only reports `stop` on an end-of-generation token), and it
        // changes the way forward: the budget is the obstacle, so the refusal
        // names the budget and its value.
        let engine = ScriptedEngine::truncating(&["<tool_call>{\"name\":\"search\",\"argum"]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");

        assert_refusal(&out.content);
        assert!(out.content.contains("`max_tokens` = 64"), "{}", out.content);
        // The truncation is still reported on the wire, so a machine client can
        // see what happened without reading English.
        assert_eq!(out.finish_reason, FinishReason::Length);
    }

    #[test]
    fn a_complete_json_call_missing_only_its_closing_tag_is_still_refused() {
        // The decision, pinned so it stays one. This body *is* complete — lenient
        // parsing would recover a runnable `search(query="window")` and save a
        // round. It is refused anyway, because completeness is only checkable in
        // the JSON dialect: `parse_xml_body` tolerates a missing `</function>`
        // and a missing `</parameter>`, so the same leniency would run a
        // *truncated* XML call as if it were whole — a different question,
        // silently answered. One rule for both dialects, and it errs toward the
        // retry rather than toward the wrong answer.
        let engine = ScriptedEngine::new(&[
            "<tool_call>{\"name\":\"echo\",\"arguments\":{\"key\":\"abc\"}}",
        ]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");
        assert_refusal(&out.content);
    }

    #[test]
    fn a_graph_call_after_the_round_budget_is_not_the_models_answer() {
        // Cause 2, measured at 2/12 rounds on `qwen3.8-27b`, and the branch the
        // Ask path actually takes: no client tools, so `client_names` is empty,
        // so `disposition` was always `Execute`, so the post-loop generation was
        // returned verbatim — raw markup and all. The comment above that code
        // claimed #489 was handled there. It was, for client tools only.
        let engine = AlwaysCalling::new(call_markup("echo", r#"{"key":"abc"}"#));
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");

        assert_refusal(&out.content);
        assert!(
            out.content.contains("4 tool rounds"),
            "the refusal names the budget that ran out: {}",
            out.content
        );
        assert!(out.content.contains("MAX_TOOL_ROUNDS"), "{}", out.content);
        // Four looped generations plus the final one, and no more: the budget was
        // spent, not silently extended.
        assert_eq!(*engine.generations.lock().unwrap(), 5);
    }

    #[test]
    fn an_unspent_round_is_never_a_generation() {
        // The property that makes a generous `MAX_TOOL_ROUNDS` affordable, and
        // the whole reason raising it was a defensible trade rather than a
        // latency tax: the loop returns the moment a generation carries no tool
        // call, so a budget a request does not spend costs that request nothing.
        //
        // Measured live before it was pinned — a question answering in two
        // rounds produced byte-identical output at 4, 6, 8 and 10, same prompt
        // *and* completion token counts. This is that observation as a test, so
        // a later change that pays per round regardless (pre-allocating the
        // messages, draining the budget, retrying a round) has to redden here
        // rather than quietly making the constant expensive.
        //
        // It asserts a **relationship between two budgets**, never either
        // number: pinning a constant's value is the guard #329 removed after it
        // broke `main`.
        let script = || {
            ScriptedEngine::new(&[
                call_markup("echo", r#"{"key":"abc"}"#),
                "the node abc is a function".to_owned(),
            ])
        };
        let frugal = script();
        let generous = script();

        let lean = chat_with_tools(&frugal, &EchoRegistry, &ask_request(), 2).expect("completion");
        let fat =
            chat_with_tools(&generous, &EchoRegistry, &ask_request(), 100).expect("completion");

        assert_eq!(
            lean.content, fat.content,
            "an answer must not depend on budget the request never spent"
        );
        // One generation per scripted turn under both budgets — so the 98 extra
        // rounds bought no extra inference, which is the claim.
        let spent = |e: &ScriptedEngine| e.seen.lock().unwrap().len();
        assert_eq!(spent(&frugal), 2);
        assert_eq!(
            spent(&frugal),
            spent(&generous),
            "a 50x budget ran the same number of generations"
        );
    }

    #[test]
    fn a_budget_that_covers_the_lookups_answers_where_a_smaller_one_refuses() {
        // What raising the constant actually bought, stated as a relationship
        // rather than a number: one question, one unchanged model behaviour,
        // refused under a budget below the lookups it needs and answered under
        // one that covers them. Live, two of six Ask questions crossed exactly
        // this line — at 6 and at 8 rounds on `qwen3-coder-30b-a3b` — which is
        // why 4 was too small and why the number chosen clears the harder of
        // them with headroom.
        const LOOKUPS: usize = 5;

        let script = || {
            let mut turns: Vec<String> = (0..LOOKUPS)
                .map(|_| call_markup("echo", r#"{"key":"abc"}"#))
                .collect();
            turns.push("the node abc is a function".to_owned());
            ScriptedEngine::new(&turns)
        };

        // One short: the post-budget generation is still a call, so it refuses.
        let starved = script();
        let out = chat_with_tools(&starved, &EchoRegistry, &ask_request(), LOOKUPS - 1)
            .expect("completion");
        assert_refusal(&out.content);

        // Exactly enough: the same model reaches its answer.
        let funded = script();
        let out =
            chat_with_tools(&funded, &EchoRegistry, &ask_request(), LOOKUPS).expect("completion");
        assert_eq!(out.content, "the node abc is a function");
    }

    #[test]
    fn prose_beside_an_abandoned_call_is_not_promoted_to_the_answer() {
        // The rejected alternative, pinned. Stripping the markup would leave
        // "Let me look that up." — fluent, plausible, and an answer the model had
        // not finished forming. That is the silent downgrade
        // `docs/REVIEW_CHECKLIST.md` §Refusals forbids: the incomplete thing
        // presented as the whole one, with the evidence removed.
        let engine = AlwaysCalling::new(format!(
            "Let me look that up.\n{}",
            call_markup("echo", r#"{"key":"abc"}"#)
        ));
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 2).expect("completion");

        assert_refusal(&out.content);
        assert!(
            !out.content.contains("Let me look that up"),
            "the model's preamble is not the answer either: {}",
            out.content
        );
    }

    #[test]
    fn a_closed_call_in_no_dialect_is_not_the_models_answer() {
        // #489 as filed, and the reason it stays fixed after the XML dialect
        // landed: a *third* dialect is still a body neither parser reads. It is
        // no longer an absence that reads as an answer — it is a call Roteiro
        // could not understand, and the refusal says so and where to report it.
        let engine = ScriptedEngine::new(&["<tool_call>search(query='window')</tool_call>"]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");

        assert_refusal(&out.content);
        assert!(out.content.contains("could not read"), "{}", out.content);
        assert!(
            out.content
                .contains("github.com/OffeneDatenmodellierung/Roteiro/issues"),
            "{}",
            out.content
        );
    }

    #[test]
    fn a_call_in_the_right_dialect_written_wrongly_is_not_the_models_answer() {
        // Not a third dialect — the *instructed* one, with a brace too many. This
        // exact string came off `qwen3-coder-30b-a3b` against this repo's graph,
        // and it is what #489 looks like most often in practice: the XML dialect
        // landed for models that revert to their training, but a model that
        // complies with the instruction and then miscounts its own JSON fails the
        // same way, because `serde_json` rejects it and a rejection was an
        // absence. Neither dialect is missing here; the call is just wrong.
        let observed = "<tool_call>{\"name\": \"search\", \"arguments\": {\"query\": \"window function\"}}}</tool_call>";
        let engine = ScriptedEngine::new(&[observed]);
        let out = chat_with_tools(&engine, &EchoRegistry, &ask_request(), 4).expect("completion");

        assert_refusal(&out.content);
        assert!(out.content.contains("malformed"), "{}", out.content);
    }

    // -- the chokepoint itself -------------------------------------------------

    #[test]
    fn the_answer_ending_cannot_publish_a_tool_call() {
        // `finish` is the only place an outcome is built, and `Ending::Answer` is
        // a *claim* it checks rather than trusts. This is the guard against the
        // fourth cause: a future exit that reads a generation some other way — or
        // does not read it at all — and reports it as the answer. There is no
        // caller that can do this today; the point is that when one appears it
        // refuses instead of leaking.
        for content in [
            "<tool_call>{\"name\":\"echo\",\"arguments\":{}}</tool_call>",
            "<tool_call>{\"name\":\"echo\"",
            "<tool_call>neither dialect</tool_call>",
        ] {
            let out = finish(stopped(content), Ending::Answer, limits());
            assert_refusal(&out.completion.content);
            assert!(out.client_tool_calls.is_empty());
        }
    }

    #[test]
    fn an_answer_with_no_markup_passes_through_untouched() {
        // The other half: the check must not eat ordinary answers. Accounting is
        // preserved either way — the model really did spend those tokens.
        let out = finish(
            stopped("`window` caps a slice by bytes."),
            Ending::Answer,
            limits(),
        );
        assert_eq!(out.completion.content, "`window` caps a slice by bytes.");
        assert_eq!(out.completion.completion_tokens, 1);
        assert!(out.client_tool_calls.is_empty());
    }

    #[test]
    fn an_untooled_generation_is_the_one_named_exception() {
        // Advertising nothing means `tool_system_prompt` was never injected, so
        // `<tool_call>` carries no meaning Roteiro assigned and censoring it would
        // break a general-backend client that asked its model what a tool call
        // looks like. The exception is a named `Ending`, not a missing check, so
        // it is visible at every call site and pinned here.
        let example = "It looks like <tool_call>{\"name\":\"search\"}</tool_call>.";
        let out = finish(stopped(example), Ending::Untooled, limits());
        assert_eq!(out.completion.content, example);
    }

    #[test]
    fn a_refusal_keeps_the_generations_accounting() {
        // Only `content` is replaced. The tokens were really spent, and
        // `finish_reason` is the engine's account of how generation ended — for a
        // truncation it already says `length`, which is the honest wire answer.
        let out = finish(
            truncated("<tool_call>{\"name\":\"echo\""),
            Ending::Unfinished(Unfinished::CutAtTokenCap),
            limits(),
        );
        assert_refusal(&out.completion.content);
        assert_eq!(out.completion.prompt_tokens, 1);
        assert_eq!(out.completion.completion_tokens, 1);
        assert_eq!(out.completion.finish_reason, FinishReason::Length);
    }

    #[test]
    fn read_markup_separates_the_three_things_a_none_used_to_mean() {
        // The type change that makes the rest possible. `Option<ToolCall>`
        // collapsed "not calling a tool", "calling one that did not arrive" and
        // "calling one in a form I cannot read" into a single `None`, and the loop
        // read that `None` as "final answer". Only the first is an answer.
        assert!(matches!(
            read_markup(&stopped("just an answer")),
            Markup::None
        ));
        assert!(matches!(
            read_markup(&stopped(&call_markup("echo", "{}"))),
            Markup::Call(_)
        ));
        assert!(matches!(
            read_markup(&stopped("<tool_call>{\"name\":\"echo\"")),
            Markup::Unfinished(Unfinished::CutShort)
        ));
        assert!(matches!(
            read_markup(&truncated("<tool_call>{\"name\":\"echo\"")),
            Markup::Unfinished(Unfinished::CutAtTokenCap)
        ));
        assert!(matches!(
            read_markup(&stopped("<tool_call>nonsense</tool_call>")),
            Markup::Unfinished(Unfinished::Unreadable)
        ));
    }

    #[test]
    fn every_ending_that_returns_prose_is_markup_free() {
        // Driven over the endings rather than over three hand-picked cases, so an
        // `Ending` added later has to decide what it does with markup — the same
        // shape as `Dialect::ALL`.
        //
        // Two of the five are deliberately absent, and between this test and
        // theirs all five are accounted for — an `Ending` nobody asserts is how
        // the guarantee gets a hole:
        //
        // * `ClientCalls` is the one ending whose outcome carries calls, and the
        //   wire layer renders `content: null` beside them.
        // * `Untooled` is the exception itself: it passes markup through on
        //   purpose, which is asserted the other way round in
        //   `an_untooled_generation_is_the_one_named_exception`.
        //
        // So this loop is "every ending that returns prose Roteiro asked a tool
        // for", which is exactly the qualified property the module documents —
        // not "every ending".
        let markup = call_markup("echo", "{}");
        for ending in [
            Ending::Answer,
            Ending::Unfinished(Unfinished::CutAtTokenCap),
            Ending::Unfinished(Unfinished::CutShort),
            Ending::Unfinished(Unfinished::Unreadable),
            Ending::Exhausted,
        ] {
            let out = finish(stopped(&markup), ending, limits());
            assert!(
                out.client_tool_calls.is_empty(),
                "no client calls on a prose ending"
            );
            assert_refusal(&out.completion.content);
        }
    }
}
