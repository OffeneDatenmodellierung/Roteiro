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

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engine::{ChatRequest, Completion, Engine, EngineError, Message};

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
    // appended. `registry` is left unconsulted rather than advertised-but-inert,
    // so its schemas never reach the prompt and never cost context.
    let graph_tools = if client_tools.is_empty() {
        registry.tools()
    } else {
        Vec::new()
    };
    let client_names: HashSet<&str> = client_tools.iter().map(|t| t.name.as_str()).collect();
    // The client's tools come first, so a graph tool sharing a name is shadowed
    // rather than executed in its place.
    let advertised: Vec<&ToolDef> = client_tools.iter().chain(graph_tools.iter()).collect();
    if advertised.is_empty() {
        return Ok(ToolLoopOutcome {
            completion: engine.chat(req)?,
            client_tool_calls: Vec::new(),
        });
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

    for _ in 0..max_rounds.max(1) {
        let completion = generate(&messages)?;

        let calls = parse_tool_calls(&completion.content);
        if calls.is_empty() {
            // No tool call: this is the model's final answer.
            return Ok(ToolLoopOutcome {
                completion,
                client_tool_calls: Vec::new(),
            });
        }

        if disposition(&calls, &client_names) == Disposition::ReturnAll {
            // Rule 2. Return every call in the turn and execute none.
            return Ok(ToolLoopOutcome {
                completion,
                client_tool_calls: into_client_calls(calls),
            });
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
    // A client-tool call in *this* generation is still returned structurally —
    // leaking it to the user as prose is the silent-wrong-answer shape of #489.
    let completion = generate(&messages)?;
    let calls = parse_tool_calls(&completion.content);
    let client_tool_calls = if disposition(&calls, &client_names) == Disposition::ReturnAll {
        into_client_calls(calls)
    } else {
        Vec::new()
    };
    Ok(ToolLoopOutcome {
        completion,
        client_tool_calls,
    })
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

/// Every tool call in `text`.
///
/// **Divergence, declared rather than half-implemented:** today this is
/// [`parse_tool_call`]'s single *first* call wrapped in a vector, so a turn never
/// carries more than one call and `parallel_tool_calls` is accepted-and-carried
/// rather than enforced. N-call parsing lands with the grammar work (#485 PR 2).
///
/// The slice shape exists now because [`disposition`] needs it: the mixed-turn
/// rule has to be expressible — and testable — before the parser can produce a
/// mixed turn.
fn parse_tool_calls(text: &str) -> Vec<ToolCall> {
    parse_tool_call(text).into_iter().collect()
}

/// Extract the first `<tool_call>…</tool_call>` from `text` and parse its JSON
/// body into a [`ToolCall`]. Returns `None` if there is no well-formed call.
fn parse_tool_call(text: &str) -> Option<ToolCall> {
    let start = text.find("<tool_call>")? + "<tool_call>".len();
    let rest = &text[start..];
    let end = rest.find("</tool_call>")?;
    let body = rest[..end].trim();

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

#[cfg(test)]
mod tests {
    use super::{Disposition, ToolCall, disposition, parse_tool_call, tool_system_prompt};
    use crate::engine::{
        ChatRequest, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
    };
    use crate::tools::{ToolDef, ToolRegistry, chat_with_client_tools, chat_with_tools};
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn parses_a_well_formed_tool_call() {
        let text = "sure\n<tool_call>{\"name\": \"explain\", \"arguments\": {\"key\": \"fn:foo\"}}</tool_call>";
        let call = parse_tool_call(text).expect("parsed");
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
        assert!(parse_tool_call("just a normal answer").is_none());
        assert!(parse_tool_call("<tool_call>not json</tool_call>").is_none());
        assert!(parse_tool_call("<tool_call>{\"arguments\":{}}</tool_call>").is_none());
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
    }

    impl Engine for ScriptedEngine {
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
            let next = self.turns.lock().unwrap().remove(0);
            on_token(&next);
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
        let engine = ScriptedEngine {
            turns: Mutex::new(vec![
                "<tool_call>{\"name\":\"echo\",\"arguments\":{\"key\":\"abc\"}}</tool_call>"
                    .to_owned(),
                "the node abc is a function".to_owned(),
            ]),
        };
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
        let engine = ScriptedEngine {
            turns: Mutex::new(vec!["direct answer".to_owned()]),
        };
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
            panic!("executed `{name}` — Roteiro must never execute a client's tool");
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
        let engine = ScriptedEngine {
            turns: Mutex::new(vec![call_markup("get_weather", r#"{"city":"Berlin"}"#)]),
        };
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
        let engine = ScriptedEngine {
            turns: Mutex::new(vec![call_markup("echo", r#"{"key":"abc"}"#)]),
        };
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
        let engine = ScriptedEngine {
            turns: Mutex::new(vec![
                call_markup("nope", "{}"),
                call_markup("get_weather", r#"{"city":"Berlin"}"#),
            ]),
        };
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
}
