//! Graph-tool auto-registration (ADR-0006): hand the served model Roteiro's
//! graph tools so it can query *this codebase* while answering.
//!
//! Decoupled from the graph via the [`ToolRegistry`] trait — rto-serve owns the
//! protocol (advertise tools in a system prompt, parse the model's `<tool_call>`
//! output, run a bounded execute-and-feed-back loop), while the caller (roteiro)
//! backs the tools with the actual graph store. llama.cpp/`llama-cpp-2` provides
//! no native tool support, so the protocol is hand-rolled and model-agnostic:
//! any instruction-following model that emits the documented `<tool_call>` form
//! works, without a model-specific template.
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

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::{ChatRequest, Completion, Engine, EngineError, FinishReason, Message};

/// Max **bytes** of a tool result fed back into the conversation, so a large
/// query result cannot blow the context window. (`truncate` caps by UTF-8 bytes,
/// backing up to a char boundary.)
const MAX_TOOL_RESULT: usize = 4000;

/// A callable tool advertised to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The tool's function name (what the model emits to call it).
    pub name: String,
    /// One-line description of what it does and when to use it.
    pub description: String,
    /// JSON Schema (an `object`) describing the tool's arguments.
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
    /// `</tool_call>` never arrived and generation stopped at the token cap.
    ///
    /// This is **evidence, not inference**: [`FinishReason::Length`] is set by
    /// the decode loop only when `max_tokens` was reached before an end-of-
    /// generation token, so the engine is reporting the truncation rather than
    /// the parser guessing at it.
    CutAtTokenCap,
    /// `</tool_call>` never arrived, but the model chose to stop: it emitted
    /// end-of-generation part-way through writing a call.
    CutShort,
    /// The call was closed and no [`Dialect`] understood its body — the shape
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
        Unfinished::CutShort => format!(
            "{REFUSAL}the model stopped part-way through a tool call — it opened a \
             tool-call block and never closed it — so this reply is an unfinished call \
             rather than an answer. Nothing was executed. Retry the question; if it keeps \
             happening this model is not following the call protocol, and \
             `roteiro model list` shows the others installed."
        ),
        // Two situations share this sentence because they share a way forward:
        // a body in a dialect Roteiro has not learned, and a body in a dialect it
        // has, written wrongly. Observed live on `qwen3-coder-30b-a3b`, the
        // common one is the second — valid-looking JSON with a brace too many —
        // so the message does not claim to know which.
        Unfinished::Unreadable => format!(
            "{REFUSAL}the model wrote a tool call Roteiro could not read: it parses as \
             neither the JSON nor the XML dialect, so it is either malformed or a form \
             Roteiro has not learned. Either way this reply is a call rather than an \
             answer. Nothing was executed. Retry the question; if it keeps happening, \
             report the model and the call it wrote at \
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
        return Ok(finish(engine.chat(req)?, Ending::Untooled, limits));
    }

    // Prepend a system turn advertising the tools and the call protocol.
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    messages.push(Message {
        role: "system".to_owned(),
        content: tool_system_prompt(&advertised),
    });
    messages.extend(req.messages.iter().cloned());

    let generate = |messages: &[Message]| {
        engine.chat(&ChatRequest {
            model: req.model.clone(),
            messages: messages.to_vec(),
            images: req.images.clone(),
            audio: req.audio.clone(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        })
    };

    for _ in 0..limits.rounds {
        let completion = generate(&messages)?;

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
            let result = registry
                .call(&call.name, &call.arguments)
                .unwrap_or_else(|e| format!("tool `{}` error: {e}", call.name));
            messages.push(Message {
                role: "user".to_owned(),
                content: format!("<tool_response>{}</tool_response>", truncate(&result)),
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
    let completion = generate(&messages)?;
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

/// Render the system prompt that advertises `tools` and the `<tool_call>`
/// protocol. Kept model-agnostic: any instruction-following model can comply.
fn tool_system_prompt(tools: &[&ToolDef]) -> String {
    let mut out = String::from(
        "You answer questions about this codebase using ONLY its Roteiro knowledge \
         graph, reached through the tools below. When a tool would help, reply with \
         ONLY a tool call on its own, in exactly this form:\n\
         <tool_call>{\"name\": \"<tool>\", \"arguments\": { ... }}</tool_call>\n\
         After you receive a <tool_response>, use it to answer. Ground every claim \
         in what the tools return: use `search` to find relevant nodes, then read \
         each hit's `snippet` or call `explain` on its key to read the node's \
         actual content BEFORE describing it — never guess from a node's name \
         alone. Cite the node keys you used (e.g. `file:README.md`, `fn:foo`). If \
         the tools do not contain the answer, say you could not find it rather than \
         making one up. If no tool is needed, just answer directly. Available \
         tools:\n",
    );
    for t in tools {
        let params = serde_json::to_string(&t.parameters).unwrap_or_else(|_| "{}".to_owned());
        let _ = writeln!(
            out,
            "- {}: {} arguments schema: {params}",
            t.name, t.description
        );
    }
    out
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
    /// No `<tool_call>` at all: whatever the model wrote is its own text.
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
    /// `<tool_call>` markup that is not a runnable call.
    Unfinished(Unfinished),
}

/// The `<tool_call>` wrapper in `text` — read once here for every [`Dialect`],
/// because both forms nest inside it and a change to the wrapper must not reach
/// one dialect and miss the other.
enum Wrapper<'a> {
    /// No `<tool_call>` in the text.
    Absent,
    /// `<tool_call>…</tool_call>`, with this body between them.
    Closed(&'a str),
    /// `<tool_call>` was opened and never closed.
    Open,
}

/// The wrapper tags, named because each is matched in two places.
const TOOL_CALL_OPEN: &str = "<tool_call>";
const TOOL_CALL_CLOSE: &str = "</tool_call>";

/// Find the first `<tool_call>` wrapper in `text` and report what state it is in.
fn wrapper(text: &str) -> Wrapper<'_> {
    let Some(open) = text.find(TOOL_CALL_OPEN) else {
        return Wrapper::Absent;
    };
    let rest = &text[open + TOOL_CALL_OPEN.len()..];
    match rest.find(TOOL_CALL_CLOSE) {
        Some(end) => Wrapper::Closed(rest[..end].trim()),
        None => Wrapper::Open,
    }
}

/// Read a generation as tool-call markup, using the engine's own account of why
/// generation stopped to say what an unclosed wrapper means.
///
/// # An unclosed `<tool_call>` is not parsed leniently
///
/// The tempting reading is "the body is probably complete and only the tag is
/// missing, so parse the remainder". It is not taken, and the reason is that it
/// is only checkable in one of the two dialects. A JSON body proves its own
/// completeness — `serde_json` rejects an unclosed object, so a body that parses
/// is a body that arrived whole. An **XML** body proves nothing:
/// [`parse_xml_body`] deliberately tolerates a missing `</function>` and a
/// missing `</parameter>`, so `…<parameter=query>\nwin` parses happily into
/// `search(query="win")` — a *different question, silently answered*, which is
/// the exact failure class #489 is about. A rule that holds for one dialect and
/// not the other is the drift [`Dialect::ALL`] exists to prevent, so the rule is
/// the uniform one: **without `</tool_call>` the call did not arrive.**
///
/// The cost of that choice is a refusal where a complete-but-untagged JSON call
/// could have been run — one retry. The cost of the other choice is a truncated
/// query answered as if it were the real one, and nothing downstream can tell.
///
/// `finish_reason` then distinguishes *why* it did not arrive, which is what the
/// refusal needs in order to name a way forward. It is engine evidence rather
/// than parser inference: `rto_llama`'s decode loop starts at
/// [`FinishReason::Length`] and reaches [`FinishReason::Stop`] only on an
/// end-of-generation token, so `Length` means precisely "`max_tokens` was
/// reached first".
fn read_markup(completion: &Completion) -> Markup {
    match wrapper(&completion.content) {
        Wrapper::Absent => Markup::None,
        Wrapper::Open => Markup::Unfinished(match completion.finish_reason {
            FinishReason::Length => Unfinished::CutAtTokenCap,
            FinishReason::Stop => Unfinished::CutShort,
        }),
        Wrapper::Closed(body) => Dialect::ALL
            .into_iter()
            .find_map(|d| d.parse(body))
            .map_or(Markup::Unfinished(Unfinished::Unreadable), Markup::Call),
    }
}

/// A call syntax a model may emit inside `<tool_call>…</tool_call>`.
///
/// [`tool_system_prompt`] instructs the JSON form and most models comply, but a
/// model's *training* can beat an instruction: the chat templates shipped in
/// `qwen3-coder-30b-a3b` and `qwen3.8-27b` tell them that "an inner
/// `<function=...></function>` block must be nested within `<tool_call>` XML
/// tags", and they sometimes revert to it. A body the parser did not understand
/// used to make the whole call invisible — [`chat_with_client_tools`] then read
/// the generation as the model's final answer and handed the raw markup to the
/// user as prose (#489).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// `{"name": "search", "arguments": {"query": "x"}}` — the instructed form,
    /// and what three of the five registry models emit.
    Json,
    /// `<function=search><parameter=query>x</parameter></function>` — the form
    /// the two Qwen XML templates train.
    Xml,
}

impl Dialect {
    /// Every dialect, in the order a body is tried against them.
    ///
    /// **This list is the only thing that makes a dialect reachable.** Nothing
    /// else dispatches on [`Dialect`]: [`parse_tool_call`] drives from `ALL`, and
    /// so does `every_dialect_parses_to_the_same_call`, which renders one call in
    /// each dialect and asserts they arrive identical. So a third dialect either
    /// lands here — parsed *and* held to the same output shape as the other two —
    /// or it is inert. There is no arrangement in which one of them sees it and
    /// the other does not.
    const ALL: [Self; 2] = [Self::Json, Self::Xml];

    /// Parse `body` as this dialect, or `None` when it is not this dialect (or is
    /// malformed in it). Each dialect judges only its own syntax and neither
    /// guesses: a body that is neither JSON nor XML stays `None` for both, which
    /// is what keeps "understand a second dialect" from becoming "accept garbage".
    fn parse(self, body: &str) -> Option<ToolCall> {
        match self {
            Self::Json => parse_json_body(body),
            Self::Xml => parse_xml_body(body),
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
        Dialect, Disposition, Ending, Limits, Markup, SuppressedTools, ToolCall, Unfinished,
        disposition, finish, read_markup, tool_system_prompt,
    };
    use crate::engine::{
        ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message,
        ModelInfo,
    };
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
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>());
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("search: find nodes"));
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
        let prompt = tool_system_prompt(&tools.iter().collect::<Vec<_>>());
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

    fn client_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_owned(),
            description: "a tool the client runs".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn user_request() -> ChatRequest {
        ChatRequest {
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
            engine.first_system_prompt().contains("echo: echo the key"),
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
            let markup = match dialect {
                Dialect::Json => call_markup("search", "{\"query\":\"window\",\"limit\":5}"),
                Dialect::Xml => xml_markup("search", &[("query", "window"), ("limit", "5")]),
            };
            assert_eq!(
                call_in(&markup),
                Some(expected.clone()),
                "{dialect:?} produced a different call"
            );
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
        assert!(out.content.contains("never closed it"), "{}", out.content);
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
