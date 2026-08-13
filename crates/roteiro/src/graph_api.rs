//! Read-only JSON HTTP API over the workspace graph (`/v1/graph/*`).
//!
//! The interactive workspace explorer's **data foundation**: a small,
//! side-effect-free axum router that surfaces the graphs a server already holds
//! in memory. Every route is a `GET` returning JSON — no mutation, no model, no
//! llama.cpp. It runs two ways, over the *same* handlers:
//!
//! - merged onto `/v1` inside a full `roteiro serve --models` process (gated on
//!   `serve`, see `serve_v1_tail`), sharing its port and workspace; or
//! - stood up alone by the llama-free `roteiro explorer` command (gated on
//!   `explorer`), which binds axum directly with no C/C++ toolchain.
//!
//! **Multi-workspace (ADR-0008).** The router is built over a
//! [`WorkspaceSet`] — an install's many named workspaces (linked multi-repo
//! groups and standalone singletons) — rather than a single [`Workspace`]. The
//! set is listed at `GET /v1/graph/workspaces`, and every graph view is served
//! twice:
//!
//! - **nested** under `/v1/graph/workspaces/{ws}/…` — the canonical form. Because
//!   project names may collide across workspaces, the workspace is an explicit
//!   path segment, so a per-project route always resolves within one specific
//!   workspace by construction; and
//! - **flat** under `/v1/graph/…` — a convenience bound to the server's *default*
//!   workspace (the sole one, or the one containing the current repo, or an
//!   explicit `--workspace-name`), so a single-workspace / cwd-default config
//!   keeps working with the original paths unchanged.
//!
//! Both forms dispatch to one set of handlers; the only difference is how the
//! target [`Workspace`] is selected (the `{ws}` path segment vs. the default).
//!
//! Two routes reuse binary-local code — the override matrix reuses
//! [`crate::overview::build`], and the cross-repo views reconstruct the persisted
//! external-ref edges the workspace resolver walks — which is why the API lives in
//! the `roteiro` binary rather than `rto-render`.
//!
//! Cross-repo semantics are read straight from the stores (so the API is fully
//! testable over in-memory [`Workspace`]s, with no config-file scan): an inferred
//! **external-ref** edge that still resolves to its hub node is a *match*; one
//! whose hub node is gone is *drift* — the same "orphan" the `/resolve` route
//! reports as `{ "drift": true, "target": null }`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Query, RawPathParams, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rto_graph::{
    EXTERNAL_REF_KIND, Edge, Node, NodeKind, Provenance, Store, StoreError, Workspace,
    WorkspaceError, WorkspaceSet, debt, explain, external_ref_target, parse_qualified,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::overview;

/// The router's shared state: the workspace set every handler resolves against,
/// plus the default workspace for the flat (`/v1/graph/…`) routes.
#[derive(Clone)]
struct AppState {
    /// The install's named workspaces (ADR-0008).
    set: Arc<WorkspaceSet>,
    /// The workspace the flat routes operate on when no `{ws}` segment is given:
    /// an explicit `--workspace-name`, else the workspace containing the current
    /// repo. `None` falls back to [`WorkspaceSet::select`]'s default (the sole
    /// workspace, else an "ambiguous" error steering the caller to name one).
    default: Option<String>,
}

/// A handler result: a JSON [`Response`], or an [`ApiError`] rendered as one.
type ApiResult = Result<Response, ApiError>;

/// A collected subgraph: its nodes (root first) and the edges among them.
type Subgraph = (Vec<Node>, Vec<Edge>);

/// Default page size for `/nodes` when no `limit` is given.
const DEFAULT_NODE_LIMIT: usize = 100;
/// Default number of `/hotspots` returned when no `limit` is given.
const DEFAULT_HOTSPOTS: usize = 20;
/// Upper bound on `/neighbourhood` traversal depth, so a request can't walk an
/// unbounded subgraph.
const MAX_DEPTH: usize = 5;

/// Build the read-only `/v1/graph/*` router over a [`WorkspaceSet`].
///
/// `default` names the workspace the flat routes bind to (see [`AppState`]).
/// Merge this into a larger app the same way the MCP router is merged, or serve
/// it directly (the llama-free `roteiro explorer` server).
pub fn router(set: Arc<WorkspaceSet>, default: Option<String>) -> Router {
    let state = AppState { set, default };
    Router::new()
        // The set itself: every workspace, its linkage, and its projects.
        .route("/v1/graph/workspaces", get(workspaces))
        // Flat views over the default workspace (single-workspace / cwd default).
        .merge(graph_routes("/v1/graph"))
        // Nested, collision-safe views: the workspace is an explicit path segment.
        .merge(graph_routes("/v1/graph/workspaces/{ws}"))
        .with_state(state)
}

/// The per-workspace graph views, registered under `prefix`. Used twice — once
/// flat (`/v1/graph`) and once nested (`/v1/graph/workspaces/{ws}`) — so both
/// forms share one set of handlers. A handler tells them apart by whether a `ws`
/// path parameter is present (see [`select_ws`]).
fn graph_routes(prefix: &str) -> Router<AppState> {
    Router::new()
        .route(&format!("{prefix}/projects"), get(projects))
        .route(&format!("{prefix}/topology"), get(topology))
        .route(&format!("{prefix}/matrix"), get(matrix))
        .route(&format!("{prefix}/resolve"), get(resolve))
        .route(&format!("{prefix}/{{project}}"), get(project_graph))
        .route(&format!("{prefix}/{{project}}/nodes"), get(project_nodes))
        .route(&format!("{prefix}/{{project}}/links"), get(project_links))
        .route(
            &format!("{prefix}/{{project}}/node/{{*key}}"),
            get(node_detail),
        )
        .route(
            &format!("{prefix}/{{project}}/neighbourhood/{{*key}}"),
            get(neighbourhood),
        )
        .route(&format!("{prefix}/{{project}}/debt"), get(project_debt))
        .route(&format!("{prefix}/{{project}}/hotspots"), get(hotspots))
}

// ---------------------------------------------------------------------------
// Workspace / path-parameter resolution
// ---------------------------------------------------------------------------

/// Look up a named path parameter (percent-decoded by axum), or `None`.
fn param<'a>(params: &'a RawPathParams, name: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// Select the [`Workspace`] a request targets: the `{ws}` path segment for a
/// nested route, else the flat routes' default workspace (which itself falls
/// back to the set's sole/ambiguous default). Borrows from `st`, so it is called
/// synchronously inside a handler before any query runs.
fn select_ws<'a>(st: &'a AppState, params: &RawPathParams) -> Result<&'a Workspace, ApiError> {
    match param(params, "ws") {
        Some(name) => Ok(st.set.select(Some(name))?),
        None => Ok(st.set.select(st.default.as_deref())?),
    }
}

/// The `{project}` path segment, which every per-project route declares (so its
/// absence is an internal invariant break, not a client error).
fn require_project(params: &RawPathParams) -> Result<&str, ApiError> {
    param(params, "project")
        .ok_or_else(|| ApiError::Internal("missing `project` path parameter".to_owned()))
}

/// The `{*key}` catch-all segment (a node key), 404 if somehow absent.
fn require_key(params: &RawPathParams) -> Result<&str, ApiError> {
    param(params, "key").ok_or_else(|| ApiError::NotFound("missing node key".to_owned()))
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

/// A handler failure, carrying the HTTP status it maps to. Rendered as
/// `{ "error": "<message>" }`.
enum ApiError {
    /// 400 — a malformed or missing query parameter.
    BadRequest(String),
    /// 404 — an unknown workspace, project, node, or key.
    NotFound(String),
    /// 500 — an internal store or workspace failure.
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<WorkspaceError> for ApiError {
    fn from(e: WorkspaceError) -> Self {
        match e {
            // A named workspace/project isn't hosted, or a project has no graph
            // yet → 404 (the addressed resource does not exist).
            WorkspaceError::UnknownWorkspace { .. }
            | WorkspaceError::UnknownProject { .. }
            | WorkspaceError::NoGraph { .. } => ApiError::NotFound(e.to_string()),
            // The caller's input was malformed or under-specified → 400. An
            // ambiguous workspace (several configured, none selected) tells the
            // caller to address a nested `/v1/graph/workspaces/{ws}/…` route.
            WorkspaceError::Unqualified { .. }
            | WorkspaceError::AmbiguousProject { .. }
            | WorkspaceError::AmbiguousWorkspace { .. }
            | WorkspaceError::Empty => ApiError::BadRequest(e.to_string()),
            // Poisoned lock, git, store, discovery, prepare-hook failures → 500.
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Query-parameter shapes
// ---------------------------------------------------------------------------

/// Filters and paging for `/v1/graph/{project}/nodes`.
#[derive(Deserialize)]
struct NodesQuery {
    /// Comma-separated node-kind tokens (e.g. `fn,struct`) to keep.
    kinds: Option<String>,
    /// Provenance to keep: `derived`, `authored`, or `inferred`.
    provenance: Option<String>,
    /// Case-insensitive substring to match against a node's name or key.
    q: Option<String>,
    /// Page size (default [`DEFAULT_NODE_LIMIT`]).
    limit: Option<usize>,
    /// Number of matching nodes to skip (default 0).
    offset: Option<usize>,
}

/// Traversal depth for `/v1/graph/{project}/neighbourhood/{key}`.
#[derive(Deserialize)]
struct DepthQuery {
    /// Hops from the root node (default 1, capped at [`MAX_DEPTH`]).
    depth: Option<usize>,
}

/// A `limit` for `/v1/graph/{project}/hotspots`.
#[derive(Deserialize)]
struct LimitQuery {
    /// Number of top-degree nodes to return (default [`DEFAULT_HOTSPOTS`]).
    limit: Option<usize>,
}

/// The `qualified` key for `/v1/graph/resolve`.
#[derive(Deserialize)]
struct ResolveQuery {
    /// A project-qualified key, `<project>::<key>`.
    qualified: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /v1/graph/workspaces` → every configured workspace as
/// `{ name, linked, projects }`. Standalone repos appear as their own
/// `linked: false` singletons (one project each); linked groups list all their
/// member projects. The entry point a client uses to discover the set before
/// addressing a nested `/v1/graph/workspaces/{ws}/…` route.
async fn workspaces(State(st): State<AppState>) -> ApiResult {
    let mut out: Vec<Value> = Vec::new();
    for name in st.set.names() {
        let linked = st.set.linked(&name).unwrap_or(false);
        let projects = st.set.select(Some(&name))?.names();
        out.push(json!({ "name": name, "linked": linked, "projects": projects }));
    }
    Ok(Json(out).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/projects` → the selected workspace's hosted
/// project names and whether more than one is hosted (so a client knows to offer
/// project selection).
async fn projects(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    Ok(Json(json!({
        "projects": ws.names(),
        "isMulti": ws.is_multi(),
    }))
    .into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}` → the whole graph as
/// `{ nodes, edges, counts }`.
async fn project_graph(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let facts = ws.with_store(Some(project), Store::export_factset)??;
    let (nodes, edges) = (facts.nodes.len(), facts.edges.len());
    Ok(Json(json!({
        "nodes": facts.nodes,
        "edges": facts.edges,
        "counts": { "nodes": nodes, "edges": edges },
    }))
    .into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/links` → this project's cross-repo
/// links, each annotated with everything the project-graph UI needs to draw a
/// spoke's dashed config→app-key edges and its drift markers:
///
/// ```json
/// { "project": "<project>",
///   "links": [ {
///     "from": "cfgkey:<file>#<dotted>",     // the spoke config_key node it starts at
///     "fromName": "<dotted>",               // that key's short label (chip text)
///     "to": "extref:<proj>::<key>",         // the external-ref placeholder node key
///     "toQualified": "<proj>::<key>",        // the project-qualified hub target
///     "toName": "<hub key>" | null,          // the resolved hub node's name, null on drift
///     "provenance": "authored" | "inferred", // the edge's real provenance (gold/slate)
///     "confidence": <f64> | null,            // inferred score; null for an authored link
///     "drift": <bool>                        // true when the target resolves to no hub node
///   } ] }
/// ```
///
/// Drift is computed exactly like `/resolve` and the matrix — via
/// [`Workspace::follow_external_ref`] — so a link whose qualified target does not
/// resolve to a hub node is `drift: true` (and `toName: null`). A **non-spoke**
/// project (no external-ref nodes) simply returns `links: []`, so the UI keeps its
/// plain project-graph rendering for the hub and for standalone code repos.
async fn project_links(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let refs = ws.with_store(Some(project), external_refs)??;
    // node key → (dotted key, value), so a link's source node resolves back to the
    // spoke's own short key for the chip label.
    let spoke_cfg = ws.with_store(Some(project), config_by_node_key)??;

    let mut links: Vec<Value> = Vec::new();
    for ExternalRef {
        src,
        node,
        provenance,
        confidence,
    } in &refs
    {
        let qualified = external_ref_target(node).unwrap_or_default();
        let from_name = spoke_cfg
            .get(src)
            .map_or_else(|| src.clone(), |(key, _)| key.clone());
        // A link whose hub node is gone — or whose project isn't hosted / has no
        // graph — is drift for that link, not a fatal error for the endpoint
        // (mirroring `/resolve` and `/matrix`).
        let resolved = resolve_link(ws, node)?;
        let drift = resolved.is_none();
        let to_name = resolved.map(|n| n.name);
        links.push(json!({
            "from": src,
            "fromName": from_name,
            "to": node.key,
            "toQualified": qualified,
            "toName": to_name,
            "provenance": provenance.as_str(),
            "confidence": confidence,
            "drift": drift,
        }));
    }

    Ok(Json(json!({ "project": project, "links": links })).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/nodes?kinds=&provenance=&q=&limit=&offset=`
/// → filtered, paged nodes plus the pre-paging `total`.
async fn project_nodes(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(p): Query<NodesQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let provenance = match p.provenance.as_deref() {
        Some(s) => Some(parse_provenance(s)?),
        None => None,
    };
    let kinds: Option<Vec<String>> = p.kinds.as_ref().map(|s| {
        s.split(',')
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect()
    });
    let needle = p.q.map(|s| s.to_lowercase());
    let offset = p.offset.unwrap_or(0);
    let limit = p.limit.unwrap_or(DEFAULT_NODE_LIMIT);

    let mut nodes = ws.with_store(Some(project), Store::all_nodes)??;
    nodes.retain(|n| {
        kinds
            .as_ref()
            .is_none_or(|ks| ks.iter().any(|k| k == n.kind.as_str()))
            && provenance.is_none_or(|pv| n.provenance == pv)
            && needle.as_ref().is_none_or(|q| {
                n.name.to_lowercase().contains(q) || n.key.to_lowercase().contains(q)
            })
    });
    let total = nodes.len();
    let page: Vec<Node> = nodes.into_iter().skip(offset).take(limit).collect();
    Ok(Json(json!({
        "nodes": page,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
    .into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/node/{key}` → the node plus its
/// in/out edges (`query::explain`). 404 when the key is unknown.
async fn node_detail(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let key = require_key(&params)?;
    let explanation = ws.with_store(Some(project), |s| explain(s, key))??;
    match explanation {
        Some(e) => Ok(Json(e).into_response()),
        None => Err(ApiError::NotFound(format!(
            "no node `{key}` in project `{project}`"
        ))),
    }
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/neighbourhood/{key}?depth=1` → the
/// subgraph within `depth` hops of `key`. 404 when the root key is unknown.
async fn neighbourhood(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(dq): Query<DepthQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let key = require_key(&params)?;
    let depth = dq.depth.unwrap_or(1).min(MAX_DEPTH);
    let sub = ws.with_store(Some(project), |s| neighbourhood_subgraph(s, key, depth))??;
    match sub {
        Some((nodes, edges)) => Ok(Json(json!({
            "root": key,
            "depth": depth,
            "nodes": nodes,
            "edges": edges,
            "counts": { "nodes": nodes.len(), "edges": edges.len() },
        }))
        .into_response()),
        None => Err(ApiError::NotFound(format!(
            "no node `{key}` in project `{project}`"
        ))),
    }
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/debt` → the intent-debt report
/// (`query::debt`).
async fn project_debt(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let report = ws.with_store(Some(project), |s| debt(s, &[], &[]))??;
    Ok(Json(report).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/hotspots?limit=` → the top-`limit`
/// nodes by total degree (in + out edges).
async fn hotspots(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(lq): Query<LimitQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let limit = lq.limit.unwrap_or(DEFAULT_HOTSPOTS);
    let ranked = ws.with_store(Some(project), |s| compute_hotspots(s, limit))??;
    Ok(Json(json!({ "hotspots": ranked, "limit": limit })).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/resolve?qualified=<project>::<key>` → the
/// target node and a `drift` flag: `{ target: null, drift: true }` when the key
/// is well-formed but its node is gone (a removed or renamed cross-repo target).
/// Resolution is scoped to the selected workspace, so a `<project>` naming a
/// project in another workspace does not leak across.
async fn resolve(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(rq): Query<ResolveQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let qualified = rq
        .qualified
        .ok_or_else(|| ApiError::BadRequest("missing `qualified` query parameter".to_owned()))?;
    let target = ws.resolve_qualified(&qualified)?;
    let drift = target.is_none();
    Ok(Json(json!({ "target": target, "drift": drift })).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/topology` → the cross-repo hub-and-spoke
/// shape of the selected workspace: the hub project, a summary per spoke
/// (`keyCount`, `driftCount`), and the inferred cross-repo links (`from`/`to`
/// node keys, provenance, confidence).
async fn topology(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let names = ws.names();
    let hub = determine_hub(ws, &names)?;

    let mut links: Vec<Value> = Vec::new();
    let mut spokes: Vec<Value> = Vec::new();
    for name in &names {
        if Some(name) == hub.as_ref() {
            continue;
        }
        let refs = ws.with_store(Some(name), external_refs)??;
        if refs.is_empty() {
            continue; // Only projects that reference the hub are spokes.
        }
        let key_count = ws.with_store(Some(name), |s| s.config_keys().map(|c| c.len()))??;
        let mut drift_count = 0usize;
        for ExternalRef {
            src,
            node,
            provenance,
            confidence,
        } in &refs
        {
            if let Some(target) = external_ref_target(node) {
                links.push(json!({
                    "from": format!("{name}::{src}"),
                    "to": target,
                    "provenance": provenance.as_str(),
                    "confidence": confidence,
                }));
            }
            // A link whose target is gone — or whose project isn't hosted / has no
            // graph — is drift for that link, not a fatal error for the endpoint.
            if resolve_link(ws, node)?.is_none() {
                drift_count += 1;
            }
        }
        spokes.push(json!({
            "name": name,
            "label": name,
            "keyCount": key_count,
            "driftCount": drift_count,
        }));
    }

    Ok(Json(json!({ "hub": hub, "spokes": spokes, "links": links })).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/matrix` → the cross-repo config override
/// matrix + drift ([`overview::OverrideMatrix`]) for the selected workspace,
/// reconstructed from the persisted external-ref edges: a resolving edge is an
/// override cell, a dangling one is drift.
async fn matrix(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let names = ws.names();
    let Some(hub) = determine_hub(ws, &names)? else {
        // Nothing references anything — no hub, so an empty (but well-shaped) matrix.
        return Ok(Json(json!({
            "hub": Value::Null, "spokes": [], "rows": [], "drift": []
        }))
        .into_response());
    };

    let hub_values = ws.with_store(Some(&hub), config_values)??;

    let mut spokes: Vec<overview::SpokeInput> = Vec::new();
    for name in &names {
        if name == &hub {
            continue;
        }
        let refs = ws.with_store(Some(name), external_refs)??;
        if refs.is_empty() {
            continue;
        }
        // node key → (dotted key, value) for this spoke's own config keys.
        let spoke_cfg = ws.with_store(Some(name), config_by_node_key)??;

        let mut matches: Vec<overview::MatchInput> = Vec::new();
        let mut orphans: Vec<(String, String)> = Vec::new();
        for ExternalRef {
            src,
            node,
            provenance,
            confidence,
        } in &refs
        {
            let (spoke_key, spoke_value) = spoke_cfg
                .get(src)
                .cloned()
                .unwrap_or_else(|| (src.clone(), String::new()));
            match resolve_link(ws, node)? {
                // Resolves to its hub node → a real override cell, tagged with the
                // link's real provenance (authored vs inferred).
                Some(hub_node) => matches.push(overview::MatchInput {
                    hub_key: hub_node.name,
                    spoke_key,
                    spoke_value,
                    confidence: confidence.unwrap_or(0.0),
                    provenance: *provenance,
                }),
                // Hub node gone, or its project isn't hosted / has no graph → drift
                // (an orphan spoke key), not a fatal error for the endpoint.
                None => orphans.push((spoke_key, spoke_value)),
            }
        }
        spokes.push(overview::SpokeInput {
            name: name.clone(),
            matches,
            orphans,
        });
    }

    let assembled = overview::build(&hub, &hub_values, spokes);
    Ok(Json(assembled).into_response())
}

// ---------------------------------------------------------------------------
// Store helpers (pure reads; run inside `Workspace::with_store`)
// ---------------------------------------------------------------------------

/// One persisted inferred cross-repo link, read from a spoke store: the local
/// config-key node it starts at, the external-ref placeholder it points to, and
/// its confidence.
struct ExternalRef {
    /// The spoke's own config-key node key (`cfgkey:<file>#<dotted>`).
    src: String,
    /// The external-ref placeholder node (carries the qualified hub target).
    node: Node,
    /// How the cross-repo edge was produced — [`Provenance::Authored`] (a declared
    /// `[[links]]`) or [`Provenance::Inferred`] (a confidence-scored match).
    provenance: Provenance,
    /// The edge's confidence; `Some` only for an inferred edge (an authored link
    /// carries no score — see the [`Edge`] invariant).
    confidence: Option<f64>,
}

/// The cross-repo external-ref links persisted in `store` (ADR-0009): every
/// external-ref placeholder node and the edge that points at it. Both
/// [`Provenance::Authored`] (declared `[[links]]`) and [`Provenance::Inferred`]
/// (matched) edges are collected, each carrying its real provenance — a
/// *derived* edge never targets an external-ref placeholder, so it is excluded.
fn external_refs(store: &Store) -> Result<Vec<ExternalRef>, StoreError> {
    let placeholders = store.nodes_by_kind(&NodeKind::Other(EXTERNAL_REF_KIND.to_owned()))?;
    let mut out = Vec::new();
    for node in placeholders {
        for edge in store.edges_to(&node.key)? {
            if matches!(edge.provenance, Provenance::Inferred | Provenance::Authored) {
                out.push(ExternalRef {
                    src: edge.src,
                    node: node.clone(),
                    provenance: edge.provenance,
                    confidence: edge.confidence,
                });
            }
        }
    }
    Ok(out)
}

/// This store's config keys as a `dotted key → value` map (mirrors the CLI's
/// `links --matrix` hub-values assembly).
fn config_values(store: &Store) -> Result<BTreeMap<String, String>, StoreError> {
    Ok(store
        .config_keys()?
        .into_iter()
        .map(|c| (c.key, c.value))
        .collect())
}

/// This store's config keys as a `node key → (dotted key, value)` map, so a link's
/// source node key can be looked back up to the spoke's own key and value.
fn config_by_node_key(store: &Store) -> Result<BTreeMap<String, (String, String)>, StoreError> {
    Ok(store
        .config_keys()?
        .into_iter()
        // A `config_key` node's key is `cfgkey:<file>#<dotted>` (see `rto_graph`
        // extraction), so rebuild it from the file + dotted key.
        .map(|c| (format!("cfgkey:{}#{}", c.file, c.key), (c.key, c.value)))
        .collect())
}

/// Follow an external-ref to its hub node for the cross-repo views, mapping a
/// target that no longer resolves — the hub node is gone, or its project isn't
/// hosted / has no graph — to **drift** (`Ok(None)`) rather than an error. A
/// single dangling link must not fail the whole endpoint, mirroring how
/// `/resolve` reports `{drift:true}`. Genuine internal failures (poisoned lock,
/// store/git errors) still propagate.
fn resolve_link(ws: &Workspace, node: &Node) -> Result<Option<Node>, ApiError> {
    match ws.follow_external_ref(node) {
        Ok(target) => Ok(target),
        Err(
            WorkspaceError::UnknownProject { .. }
            | WorkspaceError::NoGraph { .. }
            | WorkspaceError::Unqualified { .. }
            | WorkspaceError::AmbiguousProject { .. }
            | WorkspaceError::Empty,
        ) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The hub project: the **hosted** project most external-ref edges point into.
/// Targets naming a project not in `names` (an unhosted repo) are ignored, so the
/// hub is always a project the workspace can actually read — never a phantom.
/// `None` when nothing references a hosted project (a single-repo or unlinked
/// workspace).
fn determine_hub(ws: &Workspace, names: &[String]) -> Result<Option<String>, ApiError> {
    let hosted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        for ExternalRef { node, .. } in ws.with_store(Some(name), external_refs)?? {
            if let Some(qualified) = external_ref_target(&node)
                && let Some((project, _)) = parse_qualified(&qualified)
                && hosted.contains(project)
            {
                *targets.entry(project.to_owned()).or_default() += 1;
            }
        }
    }
    Ok(targets
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(p, _)| p))
}

/// The subgraph within `depth` hops of `root`, as `(nodes, edges)` with the root
/// first. `None` when `root` is not a node in the store. Nodes and edges are
/// de-duplicated; edges are keyed by `(src, dst, kind)`.
fn neighbourhood_subgraph(
    store: &Store,
    root: &str,
    depth: usize,
) -> Result<Option<Subgraph>, StoreError> {
    let Some(root_node) = store.get_node(root)? else {
        return Ok(None);
    };
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut edges: BTreeMap<(String, String, String), Edge> = BTreeMap::new();
    nodes.insert(root_node.key.clone(), root_node);

    let mut frontier = vec![root.to_owned()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for key in &frontier {
            let incident = store
                .edges_from(key)?
                .into_iter()
                .chain(store.edges_to(key)?);
            for edge in incident {
                let other = if edge.src == *key {
                    edge.dst.clone()
                } else {
                    edge.src.clone()
                };
                edges
                    .entry((
                        edge.src.clone(),
                        edge.dst.clone(),
                        edge.kind.as_str().to_owned(),
                    ))
                    .or_insert(edge);
                if !nodes.contains_key(&other) {
                    if let Some(n) = store.get_node(&other)? {
                        nodes.insert(other.clone(), n);
                    }
                    next.push(other);
                }
            }
        }
        frontier = next;
    }
    Ok(Some((
        nodes.into_values().collect(),
        edges.into_values().collect(),
    )))
}

/// The top-`limit` nodes by total degree (in + out edges), each as
/// `{ key, name, kind, degree }`. Ties break by key, so the order is stable.
fn compute_hotspots(store: &Store, limit: usize) -> Result<Vec<Value>, StoreError> {
    let mut degree: BTreeMap<String, u32> = BTreeMap::new();
    for edge in store.all_edges()? {
        *degree.entry(edge.src).or_default() += 1;
        *degree.entry(edge.dst).or_default() += 1;
    }
    let mut ranked: Vec<(u32, Node)> = store
        .all_nodes()?
        .into_iter()
        .map(|n| (degree.get(&n.key).copied().unwrap_or(0), n))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));
    Ok(ranked
        .into_iter()
        .take(limit)
        .map(|(deg, n)| json!({ "key": n.key, "name": n.name, "kind": n.kind.as_str(), "degree": deg }))
        .collect())
}

/// Parse a provenance filter token, or a 400 for anything else.
fn parse_provenance(s: &str) -> Result<Provenance, ApiError> {
    match s {
        "derived" => Ok(Provenance::Derived),
        "authored" => Ok(Provenance::Authored),
        "inferred" => Ok(Provenance::Inferred),
        other => Err(ApiError::BadRequest(format!(
            "unknown provenance `{other}` (expected derived|authored|inferred)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use rto_graph::{Edge, EdgeKind, FactSet, external_ref_key, external_ref_node};
    use tower::ServiceExt as _; // for `oneshot`

    // -- synthetic in-memory workspaces -----------------------------------

    /// The hub project name used across the tests.
    const HUB: &str = "hub";
    /// The spoke project name used across the tests.
    const SPOKE: &str = "spoke";

    /// A config-key node, mirroring `rto_graph`'s extraction: key
    /// `cfgkey:<file>#<dotted>`, name the dotted key, `meta { key, value }`.
    fn cfg_node(file: &str, dotted: &str, value: &str) -> Node {
        let mut node = Node::new(
            format!("cfgkey:{file}#{dotted}"),
            NodeKind::Other("config_key".to_owned()),
            dotted.to_owned(),
        );
        node.path = Some(file.to_owned());
        node.meta = json!({ "key": dotted, "value": value });
        node
    }

    /// Build the hub store: two plain symbols (one calls the other) and two
    /// config keys the spoke can override.
    fn hub_store() -> Store {
        let store = Store::open_in_memory().expect("hub store");
        let facts = FactSet::new()
            .with_node(Node::new("sym:main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:helper", NodeKind::Fn, "helper"))
            .with_node(cfg_node("config.toml", "serve.addr", "127.0.0.1:8017"))
            .with_node(cfg_node("config.toml", "serve.tools", "true"))
            .with_edge(Edge::derived("sym:main", "sym:helper", EdgeKind::Calls));
        apply(store, &facts)
    }

    /// Build the spoke store: one config key linked to a live hub key (a match),
    /// and one linked to a **missing** hub key (drift/orphan).
    fn spoke_store() -> Store {
        let store = Store::open_in_memory().expect("spoke store");

        // A matching override: spoke `SERVE_ADDR` → hub `serve.addr` (still present).
        let live_target = format!("{HUB}::cfgkey:config.toml#serve.addr");
        // A drifted link: spoke `LEGACY_ADDR` → hub key that no longer exists.
        let dead_target = format!("{HUB}::cfgkey:config.toml#serve.legacy");

        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "LEGACY_ADDR", "10.0.0.1:9000"))
            .with_node(external_ref_node(&live_target))
            .with_node(external_ref_node(&dead_target))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#SERVE_ADDR",
                external_ref_key(&live_target),
                EdgeKind::References,
                0.9,
            ))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#LEGACY_ADDR",
                external_ref_key(&dead_target),
                EdgeKind::References,
                0.8,
            ));
        apply(store, &facts)
    }

    /// Build a spoke store with two live overrides of distinct provenance: an
    /// **inferred** match on `serve.addr` (confidence-scored) and an **authored**
    /// `[[links]]`-style edge on `serve.tools` (no confidence). Both resolve to a
    /// present hub key, so both become override cells — proving the matrix carries
    /// each cell's real provenance rather than guessing from confidence.
    fn spoke_mixed_provenance() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let inferred = format!("{HUB}::cfgkey:config.toml#serve.addr");
        let authored = format!("{HUB}::cfgkey:config.toml#serve.tools");

        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "SERVE_TOOLS", "false"))
            .with_node(external_ref_node(&inferred))
            .with_node(external_ref_node(&authored))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#SERVE_ADDR",
                external_ref_key(&inferred),
                EdgeKind::References,
                0.9,
            ))
            .with_edge(Edge::authored(
                "cfgkey:deploy.env#SERVE_TOOLS",
                external_ref_key(&authored),
                EdgeKind::References,
            ));
        apply(store, &facts)
    }

    /// Build a spoke store with all three link flavours the project-graph view
    /// must render distinctly: one **inferred** live link (`SERVE_ADDR` →
    /// `serve.addr`, slate), one **authored** live link (`SERVE_TOOLS` →
    /// `serve.tools`, gold), and one **drift** link (`LEGACY_ADDR` → a hub key that
    /// no longer exists, red `?`). Reuses the hub keys `hub_store` defines, so the
    /// two live links resolve and the drift one does not.
    fn spoke_authored_inferred_drift() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let inferred = format!("{HUB}::cfgkey:config.toml#serve.addr");
        let authored = format!("{HUB}::cfgkey:config.toml#serve.tools");
        let drift = format!("{HUB}::cfgkey:config.toml#serve.legacy");

        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "SERVE_TOOLS", "false"))
            .with_node(cfg_node("deploy.env", "LEGACY_ADDR", "10.0.0.1:9000"))
            .with_node(external_ref_node(&inferred))
            .with_node(external_ref_node(&authored))
            .with_node(external_ref_node(&drift))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#SERVE_ADDR",
                external_ref_key(&inferred),
                EdgeKind::References,
                0.9,
            ))
            .with_edge(Edge::authored(
                "cfgkey:deploy.env#SERVE_TOOLS",
                external_ref_key(&authored),
                EdgeKind::References,
            ))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#LEGACY_ADDR",
                external_ref_key(&drift),
                EdgeKind::References,
                0.8,
            ));
        apply(store, &facts)
    }

    /// Build a spoke store whose links point at project `ghost`, which no
    /// workspace below hosts: `live` links per `to_ghost`, plus (optionally) one
    /// link to the hosted hub's `serve.addr`.
    fn spoke_linking_unhosted(to_ghost: usize, link_hub: bool) -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let mut facts = FactSet::new();

        if link_hub {
            let live = format!("{HUB}::cfgkey:config.toml#serve.addr");
            facts = facts
                .with_node(cfg_node("deploy.env", "HUB_ADDR", "0.0.0.0:8443"))
                .with_node(external_ref_node(&live))
                .with_edge(Edge::inferred(
                    "cfgkey:deploy.env#HUB_ADDR",
                    external_ref_key(&live),
                    EdgeKind::References,
                    0.9,
                ));
        }
        for i in 0..to_ghost {
            let spoke_key = format!("GHOST_{i}");
            let ghost = format!("ghost::cfgkey:g.env#K{i}");
            facts = facts
                .with_node(cfg_node("deploy.env", &spoke_key, "x"))
                .with_node(external_ref_node(&ghost))
                .with_edge(Edge::inferred(
                    format!("cfgkey:deploy.env#{spoke_key}"),
                    external_ref_key(&ghost),
                    EdgeKind::References,
                    0.8,
                ));
        }
        apply(store, &facts)
    }

    /// A distinct single-symbol store for the standalone singleton — its sole
    /// project is *also* named `hub`, so it collides with the linked workspace's
    /// `hub` and must resolve independently.
    fn solo_store() -> Store {
        let store = Store::open_in_memory().expect("solo store");
        let facts = FactSet::new().with_node(Node::new("sym:only", NodeKind::Fn, "only"));
        apply(store, &facts)
    }

    fn apply(mut store: Store, facts: &FactSet) -> Store {
        store.apply_factset(facts).expect("apply factset");
        store
    }

    /// The linked hub+spoke workspace (the cross-repo scenario).
    fn linked_workspace() -> Workspace {
        Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_store()),
        ])
    }

    /// Wrap a single [`Workspace`] as a one-entry (linked) set — the
    /// single-workspace default the flat routes serve.
    fn single_set(ws: Workspace) -> WorkspaceSet {
        WorkspaceSet::from_workspaces([("linked".to_owned(), ws, true)])
    }

    /// A two-workspace set: the linked hub+spoke group, and a standalone
    /// singleton `solo` (`linked:false`) whose sole project is also named `hub`
    /// — so the two `hub` projects collide across workspaces.
    fn multi_set() -> WorkspaceSet {
        WorkspaceSet::from_workspaces([
            ("linked".to_owned(), linked_workspace(), true),
            (
                "solo".to_owned(),
                Workspace::single(HUB, solo_store()),
                false,
            ),
        ])
    }

    /// Drive `uri` against a router built over `set` with default workspace
    /// `default`, returning the status and parsed JSON body.
    async fn get(set: WorkspaceSet, default: Option<&str>, uri: &str) -> (StatusCode, Value) {
        let resp = router(Arc::new(set), default.map(str::to_owned))
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    // -- multi-workspace: the set listing ---------------------------------

    #[tokio::test]
    async fn workspaces_lists_all_incl_standalone_singleton() {
        let (status, json) = get(multi_set(), None, "/v1/graph/workspaces").await;
        assert_eq!(status, StatusCode::OK);
        let arr = json.as_array().expect("workspaces array");
        assert_eq!(arr.len(), 2, "both workspaces are listed");

        // Stable (name) order: `linked` then `solo`.
        assert_eq!(arr[0]["name"], "linked");
        assert_eq!(arr[0]["linked"], true, "the hub+spoke group is linked");
        let linked_projects: Vec<String> =
            serde_json::from_value(arr[0]["projects"].clone()).expect("projects");
        assert!(
            linked_projects.contains(&HUB.to_owned())
                && linked_projects.contains(&SPOKE.to_owned())
        );

        // A standalone repo appears as its own `linked:false` singleton.
        assert_eq!(arr[1]["name"], "solo");
        assert_eq!(arr[1]["linked"], false);
        assert_eq!(arr[1]["projects"], json!([HUB]));
    }

    // -- multi-workspace: scoping + cross-workspace collisions ------------

    #[tokio::test]
    async fn nested_per_project_resolves_within_its_workspace() {
        // Both workspaces host a project named `hub`, but with different graphs:
        // the linked one has 4 nodes, the standalone singleton just 1. The nested
        // route must resolve `hub` within the *named* workspace, never leak across.
        let (status, linked_hub) = get(multi_set(), None, "/v1/graph/workspaces/linked/hub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(linked_hub["counts"]["nodes"], 4);

        let (_, solo_hub) = get(multi_set(), None, "/v1/graph/workspaces/solo/hub").await;
        assert_eq!(
            solo_hub["counts"]["nodes"], 1,
            "the standalone `hub` is distinct"
        );
    }

    #[tokio::test]
    async fn nested_topology_is_scoped_to_the_workspace() {
        // The linked workspace has the hub-and-spoke shape…
        let (status, linked) = get(multi_set(), None, "/v1/graph/workspaces/linked/topology").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(linked["hub"], HUB);
        assert_eq!(linked["spokes"].as_array().unwrap().len(), 1);

        // …while the standalone singleton has no cross-repo links → no hub.
        let (_, solo) = get(multi_set(), None, "/v1/graph/workspaces/solo/topology").await;
        assert_eq!(solo["hub"], Value::Null);
    }

    #[tokio::test]
    async fn nested_nodes_and_resolve_are_scoped() {
        // Nodes: the linked `hub` has two `fn` nodes.
        let (_, nodes) = get(
            multi_set(),
            None,
            "/v1/graph/workspaces/linked/hub/nodes?kinds=fn",
        )
        .await;
        assert_eq!(nodes["total"], 2);

        // Resolve within `linked`: the live hub key resolves (no drift).
        let (_, live) = get(
            multi_set(),
            None,
            "/v1/graph/workspaces/linked/resolve?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(live["drift"], false);
        assert_eq!(live["target"]["name"], "serve.addr");

        // The SAME qualified key resolved within `solo` drifts: solo's `hub` has
        // no such node. Proof that resolution is workspace-scoped.
        let (_, drift) = get(
            multi_set(),
            None,
            "/v1/graph/workspaces/solo/resolve?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(drift["drift"], true);
        assert_eq!(drift["target"], Value::Null);
    }

    #[tokio::test]
    async fn unknown_workspace_is_404() {
        let (status, _) = get(multi_set(), None, "/v1/graph/workspaces/ghost/topology").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // -- flat routes: single-workspace / default "just works" -------------

    #[tokio::test]
    async fn flat_routes_serve_the_sole_workspace_by_default() {
        // A one-workspace set needs no `--workspace-name`: the flat routes resolve
        // to the sole workspace, so today's single-workspace config is unchanged.
        let (status, json) = get(single_set(linked_workspace()), None, "/v1/graph/hub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["counts"]["nodes"], 4);

        let (_, top) = get(single_set(linked_workspace()), None, "/v1/graph/topology").await;
        assert_eq!(top["hub"], HUB);
    }

    #[tokio::test]
    async fn flat_route_is_ambiguous_without_a_default() {
        // Several workspaces and no default selected: a flat route can't pick one,
        // so it 400s (steering the caller to a nested `/workspaces/{ws}/…` route).
        let (status, _) = get(multi_set(), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn flat_route_honours_the_named_default() {
        // With `solo` as the default, a flat `/v1/graph/hub` hits solo's `hub`.
        let (status, json) = get(multi_set(), Some("solo"), "/v1/graph/hub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["counts"]["nodes"], 1);
    }

    // -- per-route behaviour (flat, over the sole/default workspace) ------

    #[tokio::test]
    async fn projects_reports_names_and_multiplicity() {
        let (status, json) = get(single_set(linked_workspace()), None, "/v1/graph/projects").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["isMulti"], true);
        let names: Vec<String> =
            serde_json::from_value(json["projects"].clone()).expect("projects array");
        assert!(names.contains(&HUB.to_owned()) && names.contains(&SPOKE.to_owned()));

        let single = single_set(Workspace::single(HUB, hub_store()));
        let (_, one) = get(single, None, "/v1/graph/projects").await;
        assert_eq!(one["isMulti"], false);
    }

    #[tokio::test]
    async fn project_graph_returns_nodes_edges_and_counts() {
        let set = single_set(Workspace::single(HUB, hub_store()));
        let (status, json) = get(set, None, "/v1/graph/hub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["counts"]["nodes"], 4);
        assert_eq!(json["counts"]["edges"], 1);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn unknown_project_is_404() {
        let set = single_set(Workspace::single(HUB, hub_store()));
        let (status, _) = get(set, None, "/v1/graph/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn nodes_filter_by_kind_and_page() {
        // Two `fn` nodes in the hub; filter to them, then page one at a time.
        let (status, all) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/nodes?kinds=fn",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all["total"], 2);
        assert_eq!(all["nodes"].as_array().unwrap().len(), 2);

        let (_, page) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/nodes?kinds=fn&limit=1&offset=1",
        )
        .await;
        assert_eq!(page["total"], 2, "total is pre-paging");
        assert_eq!(page["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(page["limit"], 1);
        assert_eq!(page["offset"], 1);
    }

    #[tokio::test]
    async fn nodes_query_matches_name_substring() {
        let (_, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/nodes?q=help",
        )
        .await;
        assert_eq!(json["total"], 1);
        assert_eq!(json["nodes"][0]["name"], "helper");
    }

    #[tokio::test]
    async fn nodes_bad_provenance_is_400() {
        let (status, _) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/nodes?provenance=bogus",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn node_detail_explains_and_404s_unknown_key() {
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/node/sym:main",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["node"]["key"], "sym:main");
        // `main` calls `helper` → one outgoing edge.
        assert_eq!(json["outgoing"].as_array().unwrap().len(), 1);

        let (missing, _) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/node/sym:ghost",
        )
        .await;
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn neighbourhood_returns_root_and_neighbour() {
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/neighbourhood/sym:main",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["root"], "sym:main");
        let keys: Vec<String> = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["key"].as_str().unwrap().to_owned())
            .collect();
        assert!(keys.contains(&"sym:main".to_owned()) && keys.contains(&"sym:helper".to_owned()));
        assert_eq!(json["counts"]["edges"], 1);
    }

    #[tokio::test]
    async fn debt_report_has_expected_shape() {
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/debt",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["schema"].is_string());
        assert!(json["total"].is_number());
        assert!(json["items"].is_array());
    }

    #[tokio::test]
    async fn hotspots_ranks_by_degree() {
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/hotspots?limit=1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["limit"], 1);
        let top = &json["hotspots"][0];
        // Both `sym:main` and `sym:helper` have degree 1; ties break by key, so
        // `sym:helper` sorts first.
        assert_eq!(top["degree"], 1);
        assert_eq!(top["key"], "sym:helper");
    }

    #[tokio::test]
    async fn resolve_returns_hub_node_for_a_live_key() {
        let (status, json) = get(
            single_set(linked_workspace()),
            None,
            "/v1/graph/resolve?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["drift"], false);
        assert_eq!(json["target"]["name"], "serve.addr");
    }

    #[tokio::test]
    async fn resolve_reports_drift_for_an_orphan() {
        let (status, json) = get(
            single_set(linked_workspace()),
            None,
            "/v1/graph/resolve?qualified=hub::cfgkey:config.toml%23serve.legacy",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["drift"], true);
        assert_eq!(json["target"], Value::Null);
    }

    #[tokio::test]
    async fn resolve_unqualified_key_is_400() {
        let (status, _) = get(
            single_set(linked_workspace()),
            None,
            "/v1/graph/resolve?qualified=notqualified",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn topology_shows_hub_spokes_and_links() {
        let (status, json) = get(single_set(linked_workspace()), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB);
        let spokes = json["spokes"].as_array().unwrap();
        assert_eq!(spokes.len(), 1);
        assert_eq!(spokes[0]["name"], SPOKE);
        // One live link + one drifted link → driftCount 1, two links total.
        assert_eq!(spokes[0]["driftCount"], 1);
        assert_eq!(json["links"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn matrix_pivots_overrides_and_lists_drift() {
        let (status, json) = get(single_set(linked_workspace()), None, "/v1/graph/matrix").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB);
        // The live link overrides `serve.addr` → one row, flagged as differing.
        let rows = json["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["hub_key"], "serve.addr");
        assert!(rows[0]["cells"][SPOKE]["differs"].as_bool().unwrap());
        // The drifted link becomes an orphan drift entry.
        let drift = json["drift"].as_array().unwrap();
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0]["key"], "LEGACY_ADDR");
    }

    #[tokio::test]
    async fn matrix_carries_real_per_cell_provenance() {
        // A spoke with one inferred override and one authored override, both of
        // live hub keys. The matrix payload must label each cell with its real
        // provenance — `inferred` for the matched cell, `authored` for the
        // declared one — not a confidence≥1.0 guess.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_mixed_provenance()),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/matrix").await;
        assert_eq!(status, StatusCode::OK);

        let rows = json["rows"].as_array().unwrap();
        let addr = rows.iter().find(|r| r["hub_key"] == "serve.addr").unwrap();
        assert_eq!(
            addr["cells"][SPOKE]["provenance"], "inferred",
            "the confidence-scored match is inferred"
        );
        let tools = rows.iter().find(|r| r["hub_key"] == "serve.tools").unwrap();
        assert_eq!(
            tools["cells"][SPOKE]["provenance"], "authored",
            "the declared link is authored, regardless of confidence"
        );
        // The authored cell carries no confidence score (the `Edge` invariant),
        // so it surfaces as the `unwrap_or(0.0)` default.
        assert_eq!(tools["cells"][SPOKE]["confidence"], json!(0.0));
    }

    #[tokio::test]
    async fn topology_links_report_real_provenance() {
        // The same mixed spoke: the topology links must expose each edge's real
        // provenance too (an authored link reads gold, an inferred one slate).
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_mixed_provenance()),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::OK);
        let provs: Vec<&str> = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["provenance"].as_str())
            .collect();
        assert!(provs.contains(&"authored"), "got provenances: {provs:?}");
        assert!(provs.contains(&"inferred"), "got provenances: {provs:?}");
    }

    #[tokio::test]
    async fn topology_hub_is_always_a_hosted_project() {
        // The spoke references unhosted `ghost` twice but the hosted hub only
        // once. The hub must be the hosted `hub`, never the more-linked phantom.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_linking_unhosted(2, true)),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB, "hub is the hosted project, not `ghost`");
    }

    #[tokio::test]
    async fn topology_unhosted_link_is_drift_not_404() {
        // The spoke's only links point at unhosted `ghost`. Following them can't
        // resolve, but that is drift for those links — the endpoint still returns
        // 200 with the whole response built.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_linking_unhosted(2, false)),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::OK, "an unhosted target must not 404");
        assert_eq!(json["hub"], Value::Null, "no hosted project is referenced");
        let spokes = json["spokes"].as_array().unwrap();
        assert_eq!(spokes.len(), 1);
        assert_eq!(spokes[0]["driftCount"], 2, "both unhosted links are drift");
        assert_eq!(json["links"].as_array().unwrap().len(), 2);
    }

    // -- per-project cross-repo links (the spoke-graph rendering payload) ----

    #[tokio::test]
    async fn project_links_annotate_provenance_and_drift() {
        // A spoke with one authored live link, one inferred live link, and one
        // drift link. `/links` must expose each edge's real provenance and, via the
        // workspace resolver, whether its qualified target still resolves to a hub
        // node — the exact data the project-graph view draws as gold/slate/red.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_authored_inferred_drift()),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/spoke/links").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["project"], SPOKE);

        let links = json["links"].as_array().unwrap();
        assert_eq!(links.len(), 3, "all three links are reported");
        let by = |name: &str| {
            links
                .iter()
                .find(|l| l["fromName"] == name)
                .unwrap_or_else(|| panic!("no link from {name}"))
        };

        // The inferred live link: slate, resolves (no drift), carries its score and
        // the resolved hub key's name.
        let inferred = by("SERVE_ADDR");
        assert_eq!(inferred["provenance"], "inferred");
        assert_eq!(inferred["drift"], false);
        assert_eq!(
            inferred["toQualified"],
            "hub::cfgkey:config.toml#serve.addr"
        );
        assert_eq!(inferred["toName"], "serve.addr");
        assert_eq!(inferred["confidence"], json!(0.9));

        // The authored live link: gold, resolves, and carries no confidence score
        // (the `Edge` invariant) — proving provenance is read from the edge, not
        // guessed from a score.
        let authored = by("SERVE_TOOLS");
        assert_eq!(authored["provenance"], "authored");
        assert_eq!(authored["drift"], false);
        assert_eq!(authored["toName"], "serve.tools");
        assert_eq!(authored["confidence"], Value::Null);

        // The drift link: its hub target is gone, so `drift:true` and `toName:null`
        // (the app doesn't define this key → a red `?` in the UI).
        let drift = by("LEGACY_ADDR");
        assert_eq!(drift["drift"], true);
        assert_eq!(drift["toName"], Value::Null);
        assert_eq!(drift["toQualified"], "hub::cfgkey:config.toml#serve.legacy");
    }

    #[tokio::test]
    async fn project_links_are_empty_for_a_non_spoke() {
        // The hub itself references nothing cross-repo, so `/links` is empty — the
        // UI keeps its plain project-graph rendering (no dashed edges, no `?`).
        let (status, json) = get(single_set(linked_workspace()), None, "/v1/graph/hub/links").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["project"], HUB);
        assert_eq!(json["links"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn project_links_are_scoped_to_the_named_workspace() {
        // The spoke's inferred link resolves within `linked` (drift:false), but the
        // same spoke graph placed under a workspace whose `hub` lacks the key would
        // drift. Here we prove the nested route reads the spoke within its own
        // workspace and resolves against that workspace's hub.
        let (status, json) =
            get(multi_set(), None, "/v1/graph/workspaces/linked/spoke/links").await;
        assert_eq!(status, StatusCode::OK);
        let links = json["links"].as_array().unwrap();
        // `spoke_store` has one live (serve.addr) + one drift (serve.legacy) link.
        assert_eq!(links.len(), 2);
        let drift_count = links.iter().filter(|l| l["drift"] == true).count();
        assert_eq!(drift_count, 1, "one link drifts, one resolves");
    }

    // -- served web app: the explorer server mounts the UI beside the API ----
    //
    // `roteiro explorer` merges the static web app (`explorer_app::router`) onto
    // this data API over the *same* `WorkspaceSet`. These tests reuse the
    // in-memory sets above to prove the app is served (200 HTML shell) alongside a
    // working data route, for both a multi-workspace and a standalone-only config.

    /// The full explorer router as `run_explorer` builds it: the data API plus the
    /// served web app, over one workspace set.
    fn explorer_router(set: WorkspaceSet, default: Option<&str>) -> Router {
        router(Arc::new(set), default.map(str::to_owned)).merge(crate::explorer_app::router())
    }

    /// Fetch `uri` against a merged router, returning `(status, content-type, body)`.
    async fn get_app(router: Router, uri: &str) -> (StatusCode, String, String) {
        let resp = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, ct, String::from_utf8_lossy(&body).into_owned())
    }

    /// A standalone-only set: a single `linked:false` singleton, the shape
    /// `roteiro explorer` falls back to for a lone repo with no cross-repo config.
    fn standalone_only_set() -> WorkspaceSet {
        WorkspaceSet::from_workspaces([(
            "solo".to_owned(),
            Workspace::single(HUB, solo_store()),
            false,
        )])
    }

    #[tokio::test]
    async fn app_is_served_alongside_the_api_for_a_multi_workspace() {
        // The shell (referencing app.js + cytoscape) is served…
        let (status, ct, body) = get_app(explorer_router(multi_set(), None), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"), "content-type was {ct}");
        assert!(body.contains("<!doctype html>"));
        assert!(body.contains("/app.js") && body.contains("/vendor/cytoscape.min.js"));

        // …and the data route the UI reads still works over the same set.
        let (ws_status, ws_ct, ws_body) =
            get_app(explorer_router(multi_set(), None), "/v1/graph/workspaces").await;
        assert_eq!(ws_status, StatusCode::OK);
        assert!(
            ws_ct.contains("application/json"),
            "content-type was {ws_ct}"
        );
        let arr: Value = serde_json::from_str(&ws_body).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 2, "both workspaces listed");
    }

    #[tokio::test]
    async fn app_and_assets_are_served_for_a_standalone_only_config() {
        // The shell serves for a lone standalone workspace too.
        let (status, _, body) =
            get_app(explorer_router(standalone_only_set(), Some("solo")), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<!doctype html>"));

        // The vendored graph library is a non-empty JS asset.
        let (cy_status, cy_ct, cy_body) = get_app(
            explorer_router(standalone_only_set(), Some("solo")),
            "/vendor/cytoscape.min.js",
        )
        .await;
        assert_eq!(cy_status, StatusCode::OK);
        assert!(cy_ct.contains("javascript"), "content-type was {cy_ct}");
        assert!(cy_body.len() > 100_000, "the UMD bundle is substantial");

        // `/app.js` is served, and the standalone workspace lists as linked:false.
        let (js_status, _, _) = get_app(
            explorer_router(standalone_only_set(), Some("solo")),
            "/app.js",
        )
        .await;
        assert_eq!(js_status, StatusCode::OK);
        let (_, _, ws_body) = get_app(
            explorer_router(standalone_only_set(), Some("solo")),
            "/v1/graph/workspaces",
        )
        .await;
        let arr: Value = serde_json::from_str(&ws_body).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
        assert_eq!(arr[0]["linked"], false);
    }
}
