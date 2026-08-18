// roteiro:ignore-file — the `debt` tool's own description and tests name the
// intent-debt vocabulary (todo/fixme/stub/deferred); not real debt here.
//! Model Context Protocol server exposing the query surface to agents, behind
//! the `mcp` feature, built on the official [`rmcp`] SDK.
//!
//! Two transports are offered (see [`serve_stdio`] / [`serve_http`]): stdio for
//! a local agent-spawned subprocess, and streamable-HTTP for networked,
//! multi-client serving (terminate TLS at a reverse proxy). Both expose the
//! same tools — `search`, `explain`, `list_kind`, `path`, `debt`,
//! `debt_density`, `coupling`, `config_secrets`, and `list_projects` — as thin
//! wrappers over the
//! matching [`rto_graph`] query primitives, so agents and the CLI see the same
//! graph. Each tool takes an optional `project` selector for a multi-repo
//! workspace (ADR-0008). See ADR-0002 for the decision to adopt `rmcp`.

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
    /// Max hits to return (default 10, clamped to 1..=25 — this surface has no
    /// "unlimited": see `model_limit`).
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

/// Arguments for the `debt_density` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct DensityArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    kind: Vec<String>,
    /// Rank by `density` (default), `markers` (the raw count) or `lines`.
    #[serde(default)]
    order: Option<String>,
    /// Max files to return (default 20, clamped to 1..=100 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    limit: Option<u32>,
    /// Shortest file that may be ranked (default 50; `0` ranks every file).
    #[serde(default)]
    min_lines: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `config_secrets` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct ConfigSecretArgs {
    /// Max keys to return (default 50, clamped to 1..=200 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `coupling` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct CouplingArgs {
    /// Rank by `total` (default), `fan_in` (most depended-on) or `fan_out`
    /// (reaches furthest).
    #[serde(default)]
    order: Option<String>,
    /// Max nodes to return (default 20, clamped to 1..=100 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    limit: Option<u32>,
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

/// The page size for one model-facing tool call: the model's `limit` if it gave
/// one, else `default`, clamped into `1..=max`.
///
/// # Why the floor is `1` here when the library reads `0` as unlimited
///
/// [`rto_graph::window`] — and, since issue #393, the search channels too — read
/// `limit == 0` as *unlimited*. **These tools deliberately do not offer that
/// reading**, and the floor is where they decline it.
///
/// The reason is the same one the ceiling exists for: a tool result is spent
/// against a model's context window, so every tool here advertises a maximum
/// (`25`, `100`, `200`). If `0` meant unlimited it would be the single value
/// that escaped that maximum — the ceiling would hold for `1_000_000` and not
/// for `0`, which is the worst possible place for an exception. Each tool's
/// JSON schema says `"minimum": 1`, so `0` is not part of the advertised
/// contract; the clamp is what happens to a client that sends it anyway, and it
/// yields the *smallest expressible page*, never an empty result. That matters:
/// the defect #393 is about is a caller asking for something and being handed
/// silence, and no value a model can send produces silence here.
///
/// So the library and the tools do not disagree about what `0` means. The
/// library defines it; this surface does not accept it, and says so in its
/// schema. The matching note lives on [`rto_graph::window`] and on the
/// served-chat tool registry in the `roteiro` binary — if this rule changes, all
/// three change together.
fn model_limit(given: Option<u32>, default: u32, max: u32) -> usize {
    // The clamp bounds the value by `max` — 200 at the widest — before the
    // conversion, so `try_from` cannot fail on any target this builds for. The
    // fallback is the floor again rather than a third reading of `limit`.
    usize::try_from(given.unwrap_or(default).clamp(1, max)).unwrap_or(1)
}

/// Resolve a tool key against a `project`: a project-qualified key
/// (`<project>::<key>`) follows a cross-repo link into that project (ADR-0009),
/// overriding `project`; a bare key uses `project`. Owned parts so the query
/// closure can capture them.
fn qualified_or(key: &str, project: Option<&str>) -> (Option<String>, String) {
    rto_graph::parse_qualified(key).map_or_else(
        || (project.map(str::to_owned), key.to_owned()),
        |(p, bare)| (Some(p.to_owned()), bare.to_owned()),
    )
}

#[tool_router]
impl GraphServer {
    /// Explain a node: its record and provenance-labelled incoming/outgoing edges.
    #[tool(description = "Explain a graph node: its record and its \
                          provenance-labelled incoming/outgoing edges. \
                          Keys: sym:<lang>:<path>#<Name>, file:<path>, adr:<id>. \
                          A key may be project-qualified (<project>::<key>) to follow a \
                          cross-repo link into another hosted project (see list_projects).")]
    async fn explain(&self, Parameters(args): Parameters<ExplainArgs>) -> CallToolResult {
        // A project-qualified key follows a cross-repo link into that project.
        let (proj, bare) = qualified_or(&args.key, args.project.as_deref());
        let result = self.with_project(proj.as_deref(), |store| explain(store, &bare));
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
        let limit = model_limit(args.limit, 10, 25);
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
                          Args: from, to (node keys). A path lives within one project: \
                          a project-qualified `from` (<project>::<key>) selects that \
                          project (see list_projects)."
    )]
    async fn path(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        // A path lives within one graph: a qualified `from` selects the project,
        // and a qualifier on either endpoint is stripped to a bare, in-store key.
        let (proj, from_bare) = qualified_or(&args.from, args.project.as_deref());
        let to_bare = rto_graph::parse_qualified(&args.to)
            .map_or_else(|| args.to.clone(), |(_, b)| b.to_owned());
        query_result(self.with_project(proj.as_deref(), |store| path(store, &from_bare, &to_bare)))
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
        // `ignore` is empty by necessity, not by oversight: this crate has no
        // access to the target project's `roteiro.toml`, so there is no list to
        // apply. Every surface that *can* reach the config does — the
        // enumeration is on `debt_density` below.
        query_result(self.with_project(args.project.as_deref(), |store| {
            debt(store, &args.kind, &[])
        }))
    }

    /// Rank files by intent-debt density (markers per 1,000 lines).
    #[tool(
        description = "Rank FILES by intent-debt DENSITY — markers per 1,000 lines — rather \
                          than by raw marker count, which ranks the biggest file first by \
                          construction. Each row carries `markers`, `lines`, `per_kloc` and a \
                          per-category split; `overall_per_kloc` is the repository baseline to \
                          read a file's figure against. Args: kind, order (density|markers|\
                          lines), limit, min_lines. \
                          Two limits worth passing on to the user rather than reporting a \
                          number as a finding. The denominator is FILE LENGTH — every line, \
                          blanks and comments included — not source lines of code, so figures \
                          run lower than an SLOC tool's and flatter verbose or generated \
                          files. And the markers beneath it include prose matches (`for now`, \
                          `placeholder`, `tbd`), so a design document can rank as dense debt. \
                          This is a measurement, not a gate."
    )]
    async fn debt_density(&self, Parameters(args): Parameters<DensityArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 20, 100);
        let min_lines = args.min_lines.unwrap_or(rto_graph::DEFAULT_MIN_LINES);
        // An unrecognised `order` is an error, not a silent fall back to
        // `density`: a model told it ranked by `markers` when it did not will
        // report that as fact.
        let order = match args.order.as_deref() {
            None => rto_graph::DensityOrder::default(),
            Some(token) => match rto_graph::DensityOrder::from_token(token) {
                Some(order) => order,
                None => {
                    return tool_error(&format!(
                        "unknown order `{token}` (expected {})",
                        rto_graph::DensityOrder::tokens().join("|")
                    ));
                }
            },
        };
        // `ignore` is empty here for the same reason `debt` above passes none:
        // this crate has no access to the target project's `roteiro.toml`. The
        // CLI (`debt`, `debt-density`, `check`), the graph API, the served-chat
        // tool registry and the Obsidian `_Home` overview all apply the project's
        // own `[debt] ignore`. That is the complete list, written out rather than
        // summarised because `_Home` was missed when the same defect was fixed on
        // the surfaces that happened to be reported (issues #321, #372). An MCP
        // client sees the unfiltered inventory.
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::debt_density(store, &args.kind, &[], order, limit, min_lines)
        }))
    }

    /// Inventory secret-named config keys and their redaction state.
    #[tool(
        description = "Inventory the SECRET-NAMED config keys in the graph: their file \
                          paths, their key names, and whether each value was redacted \
                          before being stored (`state` = redacted | declared | present). \
                          Answers \"which of this repo's config surfaces deal in \
                          credentials\" and \"did anything unredacted get into this \
                          graph\". Args: limit. \
                          THIS IS NOT A SECRET SCANNER — state the limits when you \
                          report it, and never imply a security guarantee. It CANNOT \
                          find a hardcoded credential in source code: it reads config-key \
                          nodes, so a token in a Rust or Python string literal produces \
                          nothing here and is invisible. It CANNOT judge whether a value \
                          is valid, because it never sees one — values are redacted \
                          before they reach the store. It CANNOT tell a real secret from \
                          a placeholder: `API_TOKEN=changeme` in a committed \
                          `.env.example` and a live token are the same row. And an EMPTY \
                          RESULT DOES NOT MEAN THERE ARE NO SECRETS — it means no config \
                          key is secret-NAMED; a credential under an innocuous key like \
                          `dsn` or `endpoint` never appears. If asked to scan for \
                          secrets, say plainly that this tool cannot do it."
    )]
    async fn config_secrets(
        &self,
        Parameters(args): Parameters<ConfigSecretArgs>,
    ) -> CallToolResult {
        let limit = model_limit(args.limit, 50, 200);
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::config_secrets(store, limit)
        }))
    }

    /// Rank nodes by directed call coupling (fan-in / fan-out).
    #[tool(
        description = "Rank symbols by DIRECTED call coupling over `calls` edges: `fan_in` \
                          (how many distinct symbols call this one), `fan_out` (how many it \
                          calls), and `instability` = fan_out/(fan_in+fan_out). Use \
                          `order`=fan_in to find what the codebase most depends on, \
                          `order`=fan_out for the symbols that reach furthest, `total` \
                          (default) for overall coupling. Args: order, limit. \
                          Caveat worth passing on to the user: call edges are resolved by \
                          simple name, so a short generically-named function can absorb \
                          every call to that name and show an inflated `fan_in`. Treat a \
                          high figure on such a symbol as a question, not a finding."
    )]
    async fn coupling(&self, Parameters(args): Parameters<CouplingArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 20, 100);
        // An unrecognised `order` is an error, not a silent fall back to `total`:
        // a model told it ranked by `fan_in` when it did not will report that.
        let order = match args.order.as_deref() {
            None => rto_graph::CouplingOrder::default(),
            Some(token) => match rto_graph::CouplingOrder::from_token(token) {
                Some(order) => order,
                None => {
                    return tool_error(&format!(
                        "unknown order `{token}` (expected {})",
                        rto_graph::CouplingOrder::tokens().join("|")
                    ));
                }
            },
        };
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::coupling(store, order, limit)
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
             enumerates a kind, `path` finds how two nodes connect, `debt` lists \
             intent-debt markers, `debt_density` ranks files by markers per 1,000 \
             lines, `coupling` ranks symbols by directed call fan-in/fan-out, and \
             `config_secrets` inventories secret-named config keys (an inventory, \
             not a secret scan — see its description)."
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

/// Build the axum [`Router`](axum::Router) serving the MCP streamable-HTTP
/// transport at the `/mcp` path, for mounting **standalone or merged into
/// another app** — e.g. alongside the `/v1` model endpoint on one port
/// (ADR-0008), so a single process serves both surfaces over one Workspace.
/// Takes ownership of `workspace`.
pub fn mcp_router(workspace: Arc<Workspace>) -> axum::Router {
    let shared: SharedWorkspace = workspace;
    let service = StreamableHttpService::new(
        move || Ok(GraphServer::new(shared.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}

/// Serve the graph over the streamable-HTTP transport at `addr`, on the `/mcp`
/// path (for networked, multi-client access; terminate TLS at a reverse proxy).
/// Takes ownership of `workspace`.
///
/// # Errors
/// Returns an error if the runtime cannot start, the address cannot be bound, or
/// the server fails.
pub fn serve_http(workspace: Arc<Workspace>, addr: SocketAddr) -> Result<(), McpError> {
    let router = mcp_router(workspace);
    runtime()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigSecretArgs, CouplingArgs, DebtArgs, DensityArgs, ExplainArgs, GraphServer,
        ListKindArgs, PathArgs, SearchArgs, model_limit,
    };
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
        // The `file` node carries the `meta.lines` that `debt_density` divides
        // by, exactly as `extract::file_node` emits it.
        let mut file = Node::new("file:a.rs", NodeKind::File, "a.rs");
        file.path = Some("a.rs".into());
        file.meta = serde_json::json!({ "bytes": 2000, "lines": 100 });
        // A secret-named config key, redacted by extraction, plus one that is not
        // secret-named — the `config_secrets` inventory's two cases.
        let cfg = |dotted: &str, value: &str| {
            let mut n = Node::new(
                format!("cfgkey:.env#{dotted}"),
                NodeKind::Other("config_key".to_owned()),
                dotted,
            );
            n.path = Some(".env".into());
            n.meta = serde_json::json!({ "key": dotted, "value": value });
            n
        };
        let facts = FactSet::new()
            .with_node(file)
            .with_node(cfg("API_TOKEN", "<redacted>"))
            .with_node(cfg("PORT", "8017"))
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

    #[tokio::test]
    async fn debt_density_tool_normalises_by_file_length() {
        let server = seeded();
        let out = text_of(
            &server
                .debt_density(Parameters(DensityArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["order"], "density");
        assert_eq!(json["items"][0]["path"], "a.rs");
        assert_eq!(json["items"][0]["markers"], 1);
        assert_eq!(json["items"][0]["lines"], 100, "from the file node: {json}");
        assert_eq!(
            json["items"][0]["per_kloc"], 10.0,
            "1 marker in 100 lines is 10 per 1,000: {json}"
        );
        assert_eq!(json["items"][0]["by_category"]["todo"], 1); // roteiro:ignore

        // The `min_lines` floor is reachable from the tool, and excluding the
        // only file is reported rather than served as an empty ranking.
        let out = text_of(
            &server
                .debt_density(Parameters(DensityArgs {
                    min_lines: Some(500),
                    ..DensityArgs::default()
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["short_files"], 1);
        assert_eq!(json["files_with_markers"], 1);
        assert_eq!(json["items"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn debt_density_tool_errors_rather_than_silently_reordering() {
        // A model told it got `markers` when it got `density` would report that
        // as fact, so an unknown order must surface as a tool error.
        let out = seeded()
            .debt_density(Parameters(DensityArgs {
                order: Some("count".into()),
                ..DensityArgs::default()
            }))
            .await;
        assert_eq!(out.is_error, Some(true), "{out:?}");
        assert!(text_of(&out).contains("unknown order `count`"), "{out:?}");
    }

    #[tokio::test]
    async fn config_secrets_tool_reports_presence_and_state_never_a_value() {
        let out = text_of(
            &seeded()
                .config_secrets(Parameters(ConfigSecretArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["config_keys"], 2, "the population: {json}");
        assert_eq!(json["secret_named"], 1, "only `API_TOKEN` is: {json}");
        assert_eq!(json["redacted"], 1);
        assert_eq!(json["unredacted"], 0);
        assert_eq!(json["items"][0]["name"], "API_TOKEN");
        assert_eq!(json["items"][0]["path"], ".env");
        assert_eq!(json["items"][0]["state"], "redacted");

        // Nothing a model could mistake for a value reaches the tool result.
        assert!(
            json["items"][0].get("value").is_none() && !out.contains("<redacted>"),
            "the tool reports presence and state, never a value: {out}"
        );
    }

    #[test]
    fn config_secrets_tool_description_refuses_the_scanner_reading() {
        // The rename is load-bearing, and a model only sees the description. Each
        // limitation must be stated where the model will read it, not only in the
        // Rust doc comment.
        let server = seeded();
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "config_secrets")
            .expect("`config_secrets` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "NOT A SECRET SCANNER",
            "CANNOT find a hardcoded credential in source code",
            "never sees one",
            "real secret from a placeholder",
            "EMPTY RESULT DOES NOT MEAN THERE ARE NO SECRETS",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    #[tokio::test]
    async fn coupling_tool_separates_the_two_directions() {
        let server = seeded();
        // The seed is `main` → `helper`: identical degree, opposite direction.
        let by_in = text_of(
            &server
                .coupling(Parameters(CouplingArgs {
                    order: Some("fan_in".into()),
                    limit: Some(1),
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&by_in).expect("json");
        assert_eq!(json["order"], "fan_in");
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#helper");

        let by_out = text_of(
            &server
                .coupling(Parameters(CouplingArgs {
                    order: Some("fan_out".into()),
                    limit: Some(1),
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&by_out).expect("json");
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#main");
    }

    #[tokio::test]
    async fn coupling_tool_errors_rather_than_silently_reordering() {
        // A model told it got `fan_in` when it got `total` would report that as
        // fact, so an unknown order must surface as a tool error.
        let out = seeded()
            .coupling(Parameters(CouplingArgs {
                order: Some("degree".into()),
                limit: None,
                project: None,
            }))
            .await;
        assert_eq!(out.is_error, Some(true), "{out:?}");
        assert!(text_of(&out).contains("unknown order `degree`"), "{out:?}");
    }

    /// The floor decision from issue #393, pinned: `rto_graph` reads `limit == 0`
    /// as unlimited, and **this surface does not offer that reading**. `0` is
    /// clamped to the smallest expressible page, never to an empty result — the
    /// silence #393 is about is not reachable from a tool call.
    #[test]
    fn a_model_limit_of_zero_floors_to_one_page_and_never_to_nothing() {
        // Every (default, max) pair the tools declare, with the two bounds
        // written out as the sizes they must produce.
        for (default, max, largest, page) in
            [(10, 25, 25, 10), (20, 100, 100, 20), (50, 200, 200, 50)]
        {
            assert_eq!(
                model_limit(Some(0), default, max),
                1,
                "0 is the smallest page, not unlimited and not nothing",
            );
            // The ceiling is the reason the floor exists: if `0` meant unlimited
            // it would be the one value that escaped this.
            assert_eq!(model_limit(Some(u32::MAX), default, max), largest);
            assert_eq!(model_limit(None, default, max), page);
            assert_eq!(model_limit(Some(3), default, max), 3);
        }
    }

    #[test]
    fn get_info_advertises_tools() {
        let server = seeded();
        let info = server.get_info();
        assert_eq!(info.server_info.name, "roteiro");
        assert!(info.capabilities.tools.is_some());
    }

    /// Create a git repo at `dir` whose graph holds a single struct node `key`.
    fn repo_with_node(dir: &std::path::Path, key: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let status = std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "-q"])
            .current_dir(dir)
            .status()
            .expect("run git");
        assert!(status.success(), "git init failed in {}", dir.display());
        let store_dir = dir.join(".git").join("roteiro");
        std::fs::create_dir_all(&store_dir).unwrap();
        let mut store = Store::open(&store_dir.join("graph.db")).unwrap();
        store
            .apply_factset(&FactSet::new().with_node(Node::new(key, NodeKind::Struct, key)))
            .unwrap();
    }

    #[tokio::test]
    async fn explain_follows_a_project_qualified_key() {
        // A two-project workspace; each node exists only in its own repo.
        let base = std::env::temp_dir().join(format!("rto-mcp-xrepo-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        repo_with_node(&base.join("app"), "sym:rust:a.rs#OnlyInApp");
        repo_with_node(&base.join("deploy"), "sym:rust:b.rs#OnlyInDeploy");
        let ws = Workspace::from_repo_paths([base.join("app"), base.join("deploy")]).unwrap();
        let server = GraphServer::new(Arc::new(ws));

        // A project-qualified key follows the link into `app` — even though the
        // `project` argument names `deploy`, the qualifier wins.
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "app::sym:rust:a.rs#OnlyInApp".into(),
                project: Some("deploy".into()),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#OnlyInApp");

        // A bare key still honours the `project` argument.
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:b.rs#OnlyInDeploy".into(),
                project: Some("deploy".into()),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:b.rs#OnlyInDeploy");

        std::fs::remove_dir_all(&base).ok();
    }
}
