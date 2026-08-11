// roteiro:ignore-file — the `debt` tool's own description and tests name the
// intent-debt vocabulary (todo/fixme/stub/deferred); not real debt here.
//! Model Context Protocol server exposing the query surface to agents, behind
//! the `mcp` feature, built on the official [`rmcp`] SDK.
//!
//! Two transports are offered (see [`serve_stdio`] / [`serve_http`]): stdio for
//! a local agent-spawned subprocess, and streamable-HTTP for networked,
//! multi-client serving (terminate TLS at a reverse proxy). Both expose the
//! same tools — `search`, `explain`, `list_kind`, `path`, `debt`, and
//! `list_projects` — as thin wrappers over the matching [`rto_graph`] query
//! primitives, so agents and the CLI see the same graph. Each tool takes an
//! optional `project` selector for a multi-repo workspace (ADR-0008). See
//! ADR-0002 for the decision to adopt `rmcp`.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    tool, tool_handler, tool_router,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use rto_graph::{NodeKind, Store, StoreError, Workspace, debt, explain, list_kind, path, search};
use schemars::JsonSchema;
use serde::Deserialize;

/// Errors from running the MCP server.
type McpError = Box<dyn std::error::Error + Send + Sync>;

/// The workspace shared across sessions. A [`Workspace`] is `Send + Sync` (it
/// serialises its own store access internally), so it is shared directly; each
/// stdio session or HTTP connection queries the same registry.
type SharedWorkspace = Arc<Workspace>;

/// Arguments for the `explain` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct ExplainArgs {
    /// Node key, e.g. `sym:rust:<path>#<Name>`, `file:<path>`, or `adr:<id>`.
    key: String,
    /// Which hosted project to query, when this server hosts several (ADR-0008);
    /// omit for a single-project server. See `list_projects`.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Free-text query; matches node names, keys, paths, and captured content.
    query: String,
    /// Max hits to return (default 10, capped at 25).
    #[serde(default)]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `list_kind` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct ListKindArgs {
    /// Node kind token, e.g. `fn`, `struct`, `adr`, `file`.
    kind: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `path` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct PathArgs {
    /// Start node key.
    from: String,
    /// Goal node key.
    to: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `debt` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct DebtArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    kind: Vec<String>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// The MCP server handler over a [`Workspace`] of one or more project graphs
/// (ADR-0008). Every tool takes an optional `project` selector and a
/// `list_projects` tool enumerates the hosted projects; a single-project
/// workspace resolves that sole project for a bare call, so it serves as before.
#[derive(Clone)]
struct GraphServer {
    workspace: SharedWorkspace,
    // Populated by the `#[tool_router]` macro and consumed by the
    // `#[tool_handler]`-generated routing; not read by hand.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl GraphServer {
    fn new(workspace: SharedWorkspace) -> Self {
        Self {
            workspace,
            tool_router: Self::tool_router(),
        }
    }

    /// Run `f` against the selected project's store. Returns the inner query
    /// result, or a project-resolution error (unknown/ambiguous `project`) as a
    /// message string for the caller to surface as a tool error.
    fn with_project<R>(
        &self,
        project: Option<&str>,
        f: impl FnOnce(&Store) -> R,
    ) -> Result<R, String> {
        self.workspace
            .with_store(project, f)
            .map_err(|e| e.to_string())
    }
}

/// Collapse the `Result<Result<T, StoreError>, String>` a `with_project` query
/// produces into a tool result: the JSON value, a query error, or a
/// project-resolution error.
fn query_result<T: serde::Serialize>(r: Result<Result<T, StoreError>, String>) -> CallToolResult {
    match r {
        Ok(Ok(value)) => json_result(&value),
        Ok(Err(e)) => tool_error(&format!("query error: {e}")),
        Err(e) => tool_error(&e),
    }
}

#[tool_router]
impl GraphServer {
    /// Explain a node: its record and provenance-labelled incoming/outgoing edges.
    #[tool(description = "Explain a graph node: its record and its \
                          provenance-labelled incoming/outgoing edges. \
                          Keys: sym:<lang>:<path>#<Name>, file:<path>, adr:<id>.")]
    async fn explain(&self, Parameters(args): Parameters<ExplainArgs>) -> CallToolResult {
        let result = self.with_project(args.project.as_deref(), |store| explain(store, &args.key));
        match result {
            Ok(Ok(Some(ex))) => json_result(&ex),
            Ok(Ok(None)) => CallToolResult::success(vec![ContentBlock::text(format!(
                "no node with key `{}`",
                args.key
            ))]),
            Ok(Err(e)) => tool_error(&format!("query error: {e}")),
            Err(e) => tool_error(&e),
        }
    }

    /// Search the graph by text, ranked — the entry point for "what/why" questions.
    #[tool(
        description = "Search graph nodes by text — names, keys, paths, and captured \
                          content (doc comments, README/ADR/blueprint prose). Returns \
                          ranked hits with keys; curated ADRs/blueprints and READMEs rank \
                          first, so it's the entry point for \"what is X / why\" questions. \
                          Then `explain` a returned key. Args: query, optional limit."
    )]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> CallToolResult {
        let limit = usize::try_from(args.limit.unwrap_or(10).clamp(1, 25)).unwrap_or(10);
        query_result(self.with_project(args.project.as_deref(), |store| {
            search(store, &args.query, limit)
        }))
    }

    /// List all nodes of a given kind.
    #[tool(description = "List all nodes of a given kind (fn, struct, enum, \
                          trait, module, file, adr, …).")]
    async fn list_kind(&self, Parameters(args): Parameters<ListKindArgs>) -> CallToolResult {
        query_result(self.with_project(args.project.as_deref(), |store| {
            list_kind(store, &NodeKind::from_token(&args.kind))
        }))
    }

    /// Find a shortest path between two nodes.
    #[tool(
        description = "Find a shortest path between two graph nodes, following \
                          edges in either direction. Each hop records the edge kind, \
                          provenance, and traversal direction (outgoing/incoming). \
                          Args: from, to (node keys)."
    )]
    async fn path(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        query_result(self.with_project(args.project.as_deref(), |store| {
            path(store, &args.from, &args.to)
        }))
    }

    /// List intent-debt markers (TODOs, stubs, deferred work).
    #[tool(
        description = "List intent-debt markers found in the codebase — TODO/FIXME/HACK \
                          comments, todo!()/unimplemented!() stubs, and deferred-work notes — \
                          grouped by category (todo, fixme, hack, stub, deferred). Optional \
                          `kind` restricts to given categories. Each marker links to its \
                          enclosing symbol or file via a `contains` edge."
    )]
    async fn debt(&self, Parameters(args): Parameters<DebtArgs>) -> CallToolResult {
        query_result(self.with_project(args.project.as_deref(), |store| {
            debt(store, &args.kind, &[])
        }))
    }

    /// List the projects this server hosts (ADR-0008).
    #[tool(
        description = "List the projects this server hosts. Pass one as `project` to the \
                          other tools to query it. A single-project server needs no `project`."
    )]
    async fn list_projects(&self) -> CallToolResult {
        json_result(&serde_json::json!({ "projects": self.workspace.names() }))
    }
}

#[tool_handler]
impl ServerHandler for GraphServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`; build from default then set fields.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("roteiro", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Roteiro codebase knowledge graph. Start with `search` to find nodes by \
             text (it searches captured content too — README/ADR/blueprint prose — \
             and ranks curated docs first, so it answers \"what is X / why\"); then \
             `explain` a key for its provenance-labelled neighbourhood. `list_kind` \
             enumerates a kind, `path` finds how two nodes connect, and `debt` lists \
             intent-debt markers."
                .into(),
        );
        info
    }
}

/// An error `tools/call` result carrying `message`.
fn tool_error(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.to_owned())])
}

/// A successful `tools/call` result carrying `value` as pretty JSON, or a tool
/// error if serialization fails. Shared by every tool handler.
fn json_result<T: serde::Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) => tool_error(&format!("serialize error: {e}")),
    }
}

/// Build a current-thread-safe multi-thread tokio runtime.
fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Serve the graph over stdio (for a local, agent-spawned server), blocking
/// until stdin closes. Takes ownership of `workspace` (one project or many,
/// ADR-0008).
///
/// # Errors
/// Returns an error if the runtime cannot start or the transport fails.
pub fn serve_stdio(workspace: Arc<Workspace>) -> Result<(), McpError> {
    let shared: SharedWorkspace = workspace;
    runtime()?.block_on(async move {
        let service = GraphServer::new(shared).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// Serve the graph over the streamable-HTTP transport at `addr`, on the `/mcp`
/// path (for networked, multi-client access; terminate TLS at a reverse proxy).
/// Takes ownership of `workspace`.
///
/// # Errors
/// Returns an error if the runtime cannot start, the address cannot be bound, or
/// the server fails.
pub fn serve_http(workspace: Arc<Workspace>, addr: SocketAddr) -> Result<(), McpError> {
    let shared: SharedWorkspace = workspace;
    runtime()?.block_on(async move {
        let service = StreamableHttpService::new(
            move || Ok(GraphServer::new(shared.clone())),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{DebtArgs, ExplainArgs, GraphServer, ListKindArgs, PathArgs, SearchArgs};
    use rmcp::ServerHandler;
    use rmcp::handler::server::wrapper::Parameters;
    use std::sync::Arc;

    use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Store, Workspace};

    fn seeded() -> GraphServer {
        let mut store = Store::open_in_memory().expect("store");
        let mut marker = Node::new("marker:a.rs#7", NodeKind::Marker, "TODO wire this up");
        marker.meta =
            serde_json::json!({ "category": "todo", "text": "TODO wire this up", "line": 7 });
        marker.path = Some("a.rs".into());
        let facts = FactSet::new()
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_node(marker)
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "marker:a.rs#7",
                EdgeKind::Contains,
            ));
        store.apply_factset(&facts).expect("apply");
        GraphServer::new(Arc::new(Workspace::single("test", store)))
    }

    fn text_of(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    #[tokio::test]
    async fn explain_tool_returns_graph_json() {
        let server = seeded();
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:a.rs#main".into(),
                project: None,
            }))
            .await;
        let text = text_of(&out);
        let json: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#main");
        assert_eq!(json["outgoing"][0]["node"], "sym:rust:a.rs#helper");
        assert_eq!(json["outgoing"][0]["provenance"], "derived");
    }

    #[tokio::test]
    async fn list_kind_tool_lists_nodes() {
        let server = seeded();
        let out = server
            .list_kind(Parameters(ListKindArgs {
                kind: "fn".into(),
                project: None,
            }))
            .await;
        let text = text_of(&out);
        assert!(text.contains("sym:rust:a.rs#helper"));
        assert!(text.contains("sym:rust:a.rs#main"));
    }

    #[tokio::test]
    async fn search_tool_finds_nodes_by_text() {
        let server = seeded();
        let out = server
            .search(Parameters(SearchArgs {
                query: "helper".into(),
                limit: None,
                project: None,
            }))
            .await;
        let text = text_of(&out);
        assert!(text.contains("sym:rust:a.rs#helper"), "{text}");
    }

    #[tokio::test]
    async fn explain_missing_node_is_not_an_error() {
        let server = seeded();
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:a.rs#ghost".into(),
                project: None,
            }))
            .await;
        assert!(text_of(&out).contains("no node with key"));
    }

    #[tokio::test]
    async fn path_tool_returns_connecting_path() {
        let server = seeded();
        let out = server
            .path(Parameters(PathArgs {
                from: "sym:rust:a.rs#main".into(),
                to: "sym:rust:a.rs#helper".into(),
                project: None,
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["found"], true);
        assert_eq!(json["length"], 1);
        assert_eq!(json["hops"][0]["node"], "sym:rust:a.rs#helper");
        assert_eq!(json["hops"][0]["provenance"], "derived");
    }

    #[tokio::test]
    async fn debt_tool_lists_and_filters_markers() {
        let server = seeded();
        // No filter: the seeded marker is reported and counted.
        let all = text_of(&server.debt(Parameters(DebtArgs::default())).await);
        let json: serde_json::Value = serde_json::from_str(&all).expect("json");
        assert_eq!(json["total"], 1);
        assert_eq!(json["by_category"]["todo"], 1);
        assert_eq!(json["items"][0]["key"], "marker:a.rs#7");
        assert_eq!(json["items"][0]["line"], 7);

        // A non-matching category filter yields nothing.
        let none = text_of(
            &server
                .debt(Parameters(DebtArgs {
                    kind: vec!["stub".into()],
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&none).expect("json");
        assert_eq!(json["total"], 0);
    }

    #[test]
    fn get_info_advertises_tools() {
        let server = seeded();
        let info = server.get_info();
        assert_eq!(info.server_info.name, "roteiro");
        assert!(info.capabilities.tools.is_some());
    }
}
