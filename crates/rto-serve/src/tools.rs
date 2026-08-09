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

use std::fmt::Write as _;

use crate::engine::{ChatRequest, Completion, Engine, EngineError, Message};

/// Max **bytes** of a tool result fed back into the conversation, so a large
/// query result cannot blow the context window. (`truncate` caps by UTF-8 bytes,
/// backing up to a char boundary.)
const MAX_TOOL_RESULT: usize = 4000;

/// A callable tool advertised to the model.
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
}

/// A tool call parsed out of a model's generation.
#[derive(Debug, PartialEq, Eq)]
struct ToolCall {
    name: String,
    arguments: serde_json::Value,
}

/// Run a chat completion with `registry`'s tools available to the model: inject
/// a tool system prompt, generate, and while the model emits a `<tool_call>`,
/// execute it and feed the result back — up to `max_rounds` before returning the
/// last generation. With no tools, this is a plain [`Engine::chat`].
///
/// # Errors
/// Propagates [`EngineError`] from the underlying generation.
pub fn chat_with_tools(
    engine: &dyn Engine,
    registry: &dyn ToolRegistry,
    req: &ChatRequest,
    max_rounds: usize,
) -> Result<Completion, EngineError> {
    let tools = registry.tools();
    if tools.is_empty() {
        return engine.chat(req);
    }

    // Prepend a system turn advertising the tools and the call protocol.
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    messages.push(Message {
        role: "system".to_owned(),
        content: tool_system_prompt(&tools),
    });
    messages.extend(req.messages.iter().cloned());

    let generate = |messages: &[Message]| {
        engine.chat(&ChatRequest {
            model: req.model.clone(),
            messages: messages.to_vec(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        })
    };

    for _ in 0..max_rounds.max(1) {
        let completion = generate(&messages)?;

        let Some(call) = parse_tool_call(&completion.content) else {
            // No tool call: this is the model's final answer.
            return Ok(completion);
        };

        // Execute the tool and feed the (bounded) result back. The response is a
        // `user` turn (not a `tool` role): `Message` is only guaranteed to render
        // system/user/assistant, so this stays portable across chat templates —
        // the `<tool_response>` marker carries the semantics, as the system prompt
        // told the model to expect.
        let result = registry
            .call(&call.name, &call.arguments)
            .unwrap_or_else(|e| format!("tool `{}` error: {e}", call.name));
        messages.push(Message {
            role: "assistant".to_owned(),
            content: completion.content.clone(),
        });
        messages.push(Message {
            role: "user".to_owned(),
            content: format!("<tool_response>{}</tool_response>", truncate(&result)),
        });
    }

    // Round budget exhausted while still calling tools: run one final generation
    // over the accumulated context (including the last tool response) but do not
    // execute any further tools, so the last result actually informs the answer.
    generate(&messages)
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
fn tool_system_prompt(tools: &[ToolDef]) -> String {
    let mut out = String::from(
        "You can call tools to query this codebase's knowledge graph. When a tool \
         would help, reply with ONLY a tool call on its own, in exactly this form:\n\
         <tool_call>{\"name\": \"<tool>\", \"arguments\": { ... }}</tool_call>\n\
         After you receive a <tool_response>, use it to answer. If no tool is \
         needed, just answer directly. Available tools:\n",
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
    use super::{ToolCall, parse_tool_call, tool_system_prompt};
    use crate::engine::{
        ChatRequest, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
    };
    use crate::tools::{ToolDef, ToolRegistry, chat_with_tools};
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
        let tools = vec![ToolDef {
            name: "search".to_owned(),
            description: "find nodes".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let prompt = tool_system_prompt(&tools);
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("search: find nodes"));
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
            temperature: 0.0,
            max_tokens: 64,
        };
        let out = chat_with_tools(&engine, &NoTools, &req, 4).expect("completion");
        assert_eq!(out.content, "direct answer");
    }
}
