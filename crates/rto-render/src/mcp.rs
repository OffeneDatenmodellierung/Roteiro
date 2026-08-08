//! A minimal Model Context Protocol server exposing the query surface to agents
//! over stdio, behind the `mcp` feature.
//!
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0, so this is a small
//! synchronous loop over stdin/stdout — no async runtime, no `rmcp` (keeping the
//! default build lean and offline). It adds *no query logic*: the `explain` and
//! `list_kind` tools are thin wrappers over [`rto_graph::explain`] /
//! [`rto_graph::list_kind`], so agents and the CLI see the same graph.

use std::io::{self, BufRead, Write};

use rto_graph::{NodeKind, Store, explain, list_kind};
use serde_json::{Value, json};

/// The MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    /// JSON-RPC error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
}

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
    fn invalid_params(msg: &str) -> Self {
        Self {
            code: -32602,
            message: msg.to_owned(),
        }
    }
    fn internal(msg: &str) -> Self {
        Self {
            code: -32603,
            message: msg.to_owned(),
        }
    }
}

/// Run the MCP server loop against `store`, reading requests from stdin and
/// writing responses to stdout until EOF.
///
/// # Errors
/// Returns [`io::Error`] if reading stdin or writing stdout fails.
pub fn serve(store: &Store) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications carry no `id` and get no response.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let response = match dispatch(store, method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": e.code, "message": e.message}
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

/// Handle one JSON-RPC method, returning its `result` value or an error.
///
/// # Errors
/// Returns [`RpcError`] for an unknown method, invalid parameters, or an
/// underlying store failure.
pub fn dispatch(store: &Store, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "roteiro", "version": env!("CARGO_PKG_VERSION")},
        })),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => tools_call(store, params),
        "ping" => Ok(json!({})),
        other => Err(RpcError::method_not_found(other)),
    }
}

/// The tool definitions advertised to clients.
fn tool_defs() -> Value {
    json!([
        {
            "name": "explain",
            "description": "Explain a graph node: its record and its \
                            provenance-labelled incoming/outgoing edges.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Node key, e.g. sym:rust:<path>#<Name>, file:<path>, adr:<id>."
                    }
                },
                "required": ["key"],
            },
        },
        {
            "name": "list_kind",
            "description": "List all nodes of a given kind (fn, struct, adr, file, …).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "description": "Node kind token."}
                },
                "required": ["kind"],
            },
        },
    ])
}

/// Dispatch a `tools/call` request to the named tool.
fn tools_call(store: &Store, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    match name {
        "explain" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::invalid_params("`explain` requires a string `key`"))?;
            let text = match explain(store, key).map_err(|e| RpcError::internal(&e.to_string()))? {
                Some(ex) => serde_json::to_string_pretty(&ex)
                    .map_err(|e| RpcError::internal(&e.to_string()))?,
                None => format!("no node with key `{key}`"),
            };
            Ok(text_content(&text))
        }
        "list_kind" => {
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::invalid_params("`list_kind` requires a string `kind`"))?;
            let listing = list_kind(store, &NodeKind::from_token(kind))
                .map_err(|e| RpcError::internal(&e.to_string()))?;
            let text = serde_json::to_string_pretty(&listing)
                .map_err(|e| RpcError::internal(&e.to_string()))?;
            Ok(text_content(&text))
        }
        other => Err(RpcError::invalid_params(&format!("unknown tool: {other}"))),
    }
}

/// Wrap `text` in an MCP `tools/call` text-content result.
fn text_content(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

#[cfg(test)]
mod tests {
    use super::dispatch;
    use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};
    use serde_json::json;

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ));
        store.apply_factset(&facts).expect("apply");
        store
    }

    #[test]
    fn initialize_advertises_tools_capability() {
        let store = seeded();
        let r = dispatch(&store, "initialize", &json!(null)).expect("init");
        assert_eq!(r["protocolVersion"], super::PROTOCOL_VERSION);
        assert!(r["capabilities"]["tools"].is_object());
        assert_eq!(r["serverInfo"]["name"], "roteiro");
    }

    #[test]
    fn tools_list_includes_explain_and_list_kind() {
        let store = seeded();
        let r = dispatch(&store, "tools/list", &json!(null)).expect("list");
        let names: Vec<_> = r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"explain"));
        assert!(names.contains(&"list_kind"));
    }

    #[test]
    fn tools_call_explain_returns_graph_json() {
        let store = seeded();
        let params = json!({"name": "explain", "arguments": {"key": "sym:rust:a.rs#main"}});
        let r = dispatch(&store, "tools/call", &params).expect("call");
        assert_eq!(r["isError"], false);
        let text = r["content"][0]["text"].as_str().unwrap();
        // The tool returns the query surface's JSON verbatim.
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["node"]["key"], "sym:rust:a.rs#main");
        assert_eq!(inner["outgoing"][0]["node"], "sym:rust:a.rs#helper");
        assert_eq!(inner["outgoing"][0]["provenance"], "derived");
    }

    #[test]
    fn tools_call_list_kind_lists_nodes() {
        let store = seeded();
        let params = json!({"name": "list_kind", "arguments": {"kind": "fn"}});
        let r = dispatch(&store, "tools/call", &params).expect("call");
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("sym:rust:a.rs#helper"));
        assert!(text.contains("sym:rust:a.rs#main"));
    }

    #[test]
    fn unknown_method_and_tool_error() {
        let store = seeded();
        assert_eq!(
            dispatch(&store, "bogus", &json!(null)).unwrap_err().code,
            -32601
        );
        let params = json!({"name": "nope", "arguments": {}});
        assert_eq!(
            dispatch(&store, "tools/call", &params).unwrap_err().code,
            -32602
        );
    }

    #[test]
    fn explain_missing_node_is_not_an_error() {
        let store = seeded();
        let params = json!({"name": "explain", "arguments": {"key": "sym:rust:a.rs#ghost"}});
        let r = dispatch(&store, "tools/call", &params).expect("call");
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("no node with key")
        );
    }
}
