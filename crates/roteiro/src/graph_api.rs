//! Read-only JSON HTTP API over the workspace graph (`/v1/graph/*`).
//!
//! The interactive workspace explorer's **data foundation**: a small axum router
//! that surfaces the graphs a server already holds in memory. Nearly every route
//! is a `GET` returning JSON — no model, no llama.cpp. The sole exception is
//! `POST …/links/write`, which materialises the inferred cross-repo links into the
//! spoke stores (the same mutation `roteiro links --infer --write` performs). It
//! runs two ways, over the *same* handlers:
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
use axum::routing::{get, post};
use rto_graph::{
    ConfigKey, EXTERNAL_REF_KIND, Edge, Follow, LINKS_REF, Node, NodeKind, Provenance, Store,
    StoreError, Workspace, WorkspaceError, WorkspaceSet, debt, explain, external_ref_node,
    external_ref_target, parse_qualified,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::overview;

/// The router's shared state: the workspace set every handler resolves against,
/// the default workspace for the flat (`/v1/graph/…`) routes, and the build-gated
/// capabilities the web app reads at startup.
#[derive(Clone)]
struct AppState {
    /// The install's named workspaces (ADR-0008).
    set: Arc<WorkspaceSet>,
    /// The workspace the flat routes operate on when no `{ws}` segment is given:
    /// an explicit `--workspace-name`, else the workspace containing the current
    /// repo. `None` falls back to [`WorkspaceSet::select`]'s default (the sole
    /// workspace, else an "ambiguous" error steering the caller to name one).
    default: Option<String>,
    /// What optional, build-gated surfaces this server exposes (chiefly the Ask
    /// chat endpoint). Reported verbatim at `GET /v1/graph/capabilities`.
    caps: Capabilities,
}

/// The optional, build-gated surfaces a running explorer server exposes, read by
/// the web app **once at startup** (`GET /v1/graph/capabilities`) to enable or
/// disable UI that depends on them — chiefly the **Ask** tab, which needs the
/// `serve` build's graph-grounded chat endpoint (`/v1/chat/completions`).
///
/// The llama-free `roteiro explorer` build has no model engine, so it reports
/// `ask:false` and the UI keeps the Ask tab disabled. A `serve`-backed run
/// (`roteiro serve --models`, built with `--features serve,explorer`) mounts the
/// chat endpoint alongside this API and reports `ask:true` plus the served model
/// ids, so the UI enables Ask. This is the one capability signal the front end
/// keys off — nothing about Ask is hard-coded client-side.
#[derive(Clone, serde::Serialize)]
pub struct Capabilities {
    /// Whether the graph-grounded Ask (chat) endpoint is mounted on this server.
    pub ask: bool,
    /// The served generative model ids available to Ask (empty when `ask` is
    /// false), so the UI can name the model and pick one to send.
    pub models: Vec<String>,
}

impl Capabilities {
    /// The llama-free explorer's capabilities: no chat endpoint is mounted, so
    /// Ask is off and no models are served. This is what `router` reports.
    #[must_use]
    pub fn explorer_only() -> Self {
        Self {
            ask: false,
            models: Vec::new(),
        }
    }
}

/// A handler result: a JSON [`Response`], or an [`ApiError`] rendered as one.
type ApiResult = Result<Response, ApiError>;

/// A collected subgraph: its nodes (root first) and the edges among them.
type Subgraph = (Vec<Node>, Vec<Edge>);

/// Default page size for `/nodes` when no `limit` is given.
const DEFAULT_NODE_LIMIT: usize = 100;
/// Default number of `/hotspots` returned when no `limit` is given.
const DEFAULT_HOTSPOTS: usize = 20;
/// Default number of `/config-secrets` rows when no `limit` is given. Higher than
/// [`DEFAULT_HOTSPOTS`] because this is an inventory rather than a ranking — a
/// truncated top-20 of an unordered list is far less useful than a truncated
/// top-20 of a ranked one.
const DEFAULT_CONFIG_SECRETS: usize = 50;
/// Upper bound on `/neighbourhood` traversal depth, so a request can't walk an
/// unbounded subgraph.
const MAX_DEPTH: usize = 5;

/// Build the read-only `/v1/graph/*` router over a [`WorkspaceSet`], reporting the
/// llama-free capabilities ([`Capabilities::explorer_only`] — Ask off). This is
/// what the standalone `roteiro explorer` server serves.
///
/// `default` names the workspace the flat routes bind to (see [`AppState`]).
/// Merge this into a larger app the same way the MCP router is merged, or serve
/// it directly (the llama-free `roteiro explorer` server).
pub fn router(set: Arc<WorkspaceSet>, default: Option<String>) -> Router {
    build_router(set, default, Capabilities::explorer_only())
}

/// Like [`router`], but reporting explicit `caps` at `/v1/graph/capabilities` —
/// used by a full `serve` build to advertise the mounted Ask (chat) endpoint
/// (`ask:true` + served model ids) so the web app enables the Ask tab. Only the
/// serve path calls it at runtime (the `graph_api` tests also exercise it), so it
/// is dead code in the llama-free `explorer`-only binary.
#[cfg_attr(not(feature = "serve"), allow(dead_code))]
pub fn router_with_capabilities(
    set: Arc<WorkspaceSet>,
    default: Option<String>,
    caps: Capabilities,
) -> Router {
    build_router(set, default, caps)
}

/// Assemble the `/v1/graph/*` router over a fully-built [`AppState`]. Both public
/// constructors funnel here; they differ only in the [`Capabilities`] reported.
fn build_router(set: Arc<WorkspaceSet>, default: Option<String>, caps: Capabilities) -> Router {
    let state = AppState { set, default, caps };
    Router::new()
        // The build-gated feature signal the web app reads at startup.
        .route("/v1/graph/capabilities", get(capabilities))
        // The set itself: every workspace, its linkage, and its projects.
        .route("/v1/graph/workspaces", get(workspaces))
        // Flat views over the default workspace (single-workspace / cwd default).
        .merge(graph_routes("/v1/graph"))
        // Nested, collision-safe views: the workspace is an explicit path segment.
        .merge(graph_routes("/v1/graph/workspaces/{ws}"))
        .with_state(state)
}

/// `GET /v1/graph/capabilities` — the build-gated surfaces this server exposes
/// (see [`Capabilities`]). Static per process; the web app reads it once at
/// startup to enable the Ask tab only when the chat endpoint is present.
async fn capabilities(State(st): State<AppState>) -> Response {
    Json(st.caps).into_response()
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
        // The one deliberately-mutating route: persist the inferred cross-repo links.
        .route(&format!("{prefix}/links/write"), post(write_links))
        .route(&format!("{prefix}/resolve"), get(resolve))
        .route(&format!("{prefix}/follow"), get(follow))
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
        .route(
            &format!("{prefix}/{{project}}/debt/density"),
            get(debt_density),
        )
        .route(
            &format!("{prefix}/{{project}}/config-secrets"),
            get(config_secrets),
        )
        .route(&format!("{prefix}/{{project}}/hotspots"), get(hotspots))
        .route(&format!("{prefix}/{{project}}/coupling"), get(coupling))
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

/// A `limit` + `order` for `/v1/graph/{project}/coupling`.
#[derive(Deserialize)]
struct CouplingQuery {
    /// Number of top-coupled nodes to return (default [`DEFAULT_HOTSPOTS`]);
    /// `0` returns every coupled node.
    limit: Option<usize>,
    /// Ranking: `total` | `fan_in` | `fan_out` (default `total`).
    order: Option<String>,
}

/// The query for `/v1/graph/{project}/debt/density`.
#[derive(Deserialize)]
struct DensityQuery {
    /// Number of top-density files to return (default [`DEFAULT_HOTSPOTS`]);
    /// `0` returns every ranked file.
    limit: Option<usize>,
    /// Ranking: `density` | `markers` | `lines` (default `density`).
    order: Option<String>,
    /// Shortest file length that may enter the ranking (default
    /// [`rto_graph::DEFAULT_MIN_LINES`]); `0` ranks every file.
    min_lines: Option<u32>,
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
/// in/out edges (`query::explain`), and — on a media node — the **generated**
/// media content recorded for its blob. 404 when the key is unknown.
///
/// The generated array is added beside `explain`'s reply rather than inside it:
/// [`rto_graph::Explanation`] is a statement about the *graph*, and generated
/// text is not a graph fact (ADR-0015). Keeping it a sibling key is the same
/// separation `search --include-generated` makes between its two channels, and
/// it means a consumer that does not know about the field cannot accidentally
/// read a transcript as part of a node's explanation.
async fn node_detail(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let key = require_key(&params)?;
    let detail = ws.with_store(Some(project), |s| -> Result<_, StoreError> {
        let Some(explanation) = explain(s, key)? else {
            return Ok(None);
        };
        let generated = match s.get_node(key)?.and_then(|n| n.blob_hash) {
            Some(blob) => generated_for_blob(s, &blob)?,
            None => Vec::new(),
        };
        Ok(Some((explanation, generated)))
    })??;
    match detail {
        Some((explanation, generated)) => {
            let mut body = serde_json::to_value(&explanation)
                .map_err(|e| ApiError::Internal(format!("could not render node `{key}`: {e}")))?;
            if let Some(object) = body.as_object_mut() {
                object.insert("generated".to_owned(), Value::Array(generated));
            }
            Ok(Json(body).into_response())
        }
        None => Err(ApiError::NotFound(format!(
            "no node `{key}` in project `{project}`"
        ))),
    }
}

/// Every generated-media record for one source blob, rendered for the explorer.
///
/// Each entry is **self-describing about its origin**: `generated: true`, the
/// full producer identity, the model that ran, its quantisation and the prompt
/// it was given. There is no shape here that a consumer could mistake for
/// extracted content — the node's own `meta.content` is a different key entirely,
/// and a record's text lives under `text`, never merged into it.
///
/// A record the [pre-generation gate](rto_graph::media::gate) refused carries
/// `text: null` and a `skipped` object with the value measured, so the UI can
/// say *why* nothing was generated rather than showing an empty panel.
///
/// `rebuild` is the exact command that regenerates this one blob. The explorer
/// is a read-only view over graphs a server already holds, and a rebuild means
/// loading a multi-gigabyte model — so the UI hands the operator the command
/// rather than running it inside an HTTP handler.
fn generated_for_blob(store: &Store, blob: &str) -> Result<Vec<Value>, StoreError> {
    let records = store.media_records(&rto_graph::MediaFilter {
        blob_id: Some(blob),
        ..rto_graph::MediaFilter::default()
    })?;
    Ok(records
        .into_iter()
        .map(|record| {
            json!({
                "generated": true,
                "producer": record.producer_id.to_string(),
                "model": record.producer.model,
                "kind": record.producer.kind.as_str(),
                "quantisation": record.producer.quantisation,
                "prompt": record.producer.prompt,
                "blob": record.blob_id,
                "path": record.path,
                "generation": record.generation,
                "producedAt": record.produced_at,
                "toolVersion": record.tool_version,
                "text": record.outcome.text(),
                "skipped": record.outcome.skip().map(|skip| json!({
                    "reason": skip.reason.as_str(),
                    "metric": skip.reason.metric(),
                    "value": skip.value,
                    "threshold": skip.threshold,
                    "explanation": skip.to_string(),
                })),
                "rebuild": format!("roteiro media build --blob {} --force", record.blob_id),
            })
        })
        .collect())
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
/// (`query::debt`), under **that project's own** `[debt] ignore` exclusions.
///
/// The exclusions are not decoration: without them this endpoint — which the
/// explorer UI reads — counted every marker under `docs/**`, `website/**`,
/// `CHANGELOG.md` and `Cargo.toml` that the CLI excludes, so the browser and the
/// terminal reported different debt for the same repository. Two disagreeing
/// numbers are worse than either being wrong, because neither looks wrong.
///
/// The exclusions come from the **target project's own** repository
/// ([`crate::config::debt_ignore_for`]), not the repo the server was started in.
/// An unreadable or malformed `roteiro.toml` there is a 500, deliberately: a
/// fallback to "no exclusions" would serve a silently different number, which is
/// the defect rather than a graceful degradation.
async fn project_debt(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let ignore = crate::config::debt_ignore_for(ws, Some(project))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let report = ws.with_store(Some(project), |s| debt(s, &[], &ignore))??;
    Ok(Json(report).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/config-secrets?limit=` → an
/// inventory of the **secret-named** config keys in the project's graph: paths,
/// key names, and whether each value was redacted before persistence.
///
/// **Not a secret-scanning endpoint, and cannot be made into one.** Values are
/// redacted at extraction, so this serves presence and redaction state only —
/// never a value, not even the placeholder. It cannot see a hardcoded credential
/// in source code (that produces no `config_key` node), cannot judge a value's
/// validity, and cannot distinguish a real secret from a placeholder. An empty
/// `items` means "no secret-*named* config key", which is a statement about
/// naming rather than a clean bill of health. See `rto_graph::config_secrets`.
///
/// No `order` parameter, deliberately: this is an inventory, ordered by
/// `(path, name, key)`, and an ordering knob would imply some keys are more
/// secret than others.
async fn config_secrets(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(lq): Query<LimitQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let limit = lq.limit.unwrap_or(DEFAULT_CONFIG_SECRETS);
    let report = ws.with_store(Some(project), |s| rto_graph::config_secrets(s, limit))??;
    Ok(Json(report).into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/{project}/debt/density?limit=&order=&min_lines=`
/// → the top-`limit` files by intent-debt **density** (markers per 1,000 lines),
/// each with its marker count, its length and its per-category split.
///
/// A sibling of `/debt` rather than a replacement: `/debt` lists markers, and a
/// count of them ranks the largest file first by construction. Both honour the
/// **target project's own** `[debt] ignore`, by the same rule and for the same
/// reason as `/debt` above — a fallback to "no exclusions" would serve a silently
/// different number than the CLI does.
///
/// An unknown `order` is a 400 rather than a silent fall back to `density`: a
/// caller that asked for `markers` and got a density ranking has no way to tell.
async fn debt_density(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(dq): Query<DensityQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let order = match dq.order.as_deref() {
        None => rto_graph::DensityOrder::default(),
        Some(token) => rto_graph::DensityOrder::from_token(token).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown order `{token}` (expected {})",
                rto_graph::DensityOrder::tokens().join("|")
            ))
        })?,
    };
    let ignore = crate::config::debt_ignore_for(ws, Some(project))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let limit = dq.limit.unwrap_or(DEFAULT_HOTSPOTS);
    let min_lines = dq.min_lines.unwrap_or(rto_graph::DEFAULT_MIN_LINES);
    let report = ws.with_store(Some(project), |s| {
        rto_graph::debt_density(s, &[], &ignore, order, limit, min_lines)
    })??;
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

/// `GET /v1/graph[/workspaces/{ws}]/{project}/coupling?limit=&order=` → the
/// top-`limit` nodes by **directed** call coupling, each with `fan_in`,
/// `fan_out` and Martin's `instability`.
///
/// The counterpart to `/hotspots`, which ranks by *undirected* degree over every
/// edge kind and so cannot distinguish "everything calls this" from "this calls
/// everything". `/hotspots` is left as it is: total degree is a different — and
/// still useful — question, and the explorer UI depends on its shape.
///
/// An unknown `order` is a 400 rather than a silent fall back to `total`: a
/// caller that asked for `fan_in` and got a total ranking has no way to tell.
async fn coupling(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(cq): Query<CouplingQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let project = require_project(&params)?;
    let order = match cq.order.as_deref() {
        None => rto_graph::CouplingOrder::default(),
        Some(token) => rto_graph::CouplingOrder::from_token(token).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown order `{token}` (expected {})",
                rto_graph::CouplingOrder::tokens().join("|")
            ))
        })?,
    };
    let limit = cq.limit.unwrap_or(DEFAULT_HOTSPOTS);
    let report = ws.with_store(Some(project), |s| rto_graph::coupling(s, order, limit))??;
    Ok(Json(report).into_response())
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

/// `GET /v1/graph[/workspaces/{ws}]/follow?qualified=<project>::<key>` → the
/// **follow-the-link hop**: where a click on a spoke's app-key target should jump.
///
/// Unlike `/resolve` (which lands on the raw hub node — for a config override, the
/// hub's `config_key` node), this follows the net-new `config_key → struct` bridge
/// so the jump lands on the Rust **struct** that *defines* the setting whenever
/// that mapping is unambiguous (see [`Workspace::follow_definition`]). The reply
/// discriminates what was returned:
///
/// ```json
/// { "target": Node | null,
///   "kind": "struct_field" | "config_key" | null, // what `target` is; null on drift
///   "field": "<struct field>" | null,             // the bridged field, when struct_field
///   "workspace": "<ws name>" | null,               // the workspace resolution ran in
///   "project": "<hub project>",                    // the target project (to navigate to)
///   "drift": bool }                                // true when the target no longer resolves
/// ```
///
/// - `struct_field` — bridged to the defining struct (`field` names the matched
///   field), OR a target the spoke points straight at that is already a definition.
/// - `config_key` — resolved, but the `config_key` could not be bridged to a struct
///   with confidence (the safe fallback: the raw hub key).
/// - `drift: true` (target/kind null) — the qualified target's node is gone.
///
/// Resolution is scoped to the selected workspace, exactly like `/resolve`.
async fn follow(
    State(st): State<AppState>,
    params: RawPathParams,
    Query(rq): Query<ResolveQuery>,
) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let qualified = rq
        .qualified
        .ok_or_else(|| ApiError::BadRequest("missing `qualified` query parameter".to_owned()))?;
    // The hub project the hop navigates into (the workspace scopes resolution; this
    // is the target project the UI drills to).
    let (project, _) = parse_qualified(&qualified).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "`{qualified}` is not a project-qualified `<proj>::<key>`"
        ))
    })?;
    let project = project.to_owned();
    // The workspace resolution actually ran in — for the UI's breadcrumb trail.
    // Resolve the concrete NAME (not `st.default`, which is `None` on a flat route
    // whose sole workspace is auto-selected), so a resolved hop is never `null`.
    let workspace = st.set.select_name(param(&params, "ws"))?.to_owned();

    let (target, kind, field) = match ws.follow_definition(&qualified)? {
        Follow::StructField { node, field } => (Some(node), Some("struct_field"), Some(field)),
        // A resolved config_key we couldn't bridge stays a config_key; any other
        // resolved node is itself a definition target (`struct_field`).
        Follow::Node { node } => {
            let kind = if node.kind.as_str() == "config_key" {
                "config_key"
            } else {
                "struct_field"
            };
            (Some(node), Some(kind), None)
        }
        Follow::Drift => (None, None, None),
    };
    let drift = target.is_none();
    Ok(Json(json!({
        "target": target,
        "kind": kind,
        "field": field,
        "workspace": workspace,
        "project": project,
        "drift": drift,
    }))
    .into_response())
}

/// `GET /v1/graph[/workspaces/{ws}]/topology` → the cross-repo hub-and-spoke
/// shape of the selected workspace: the hub project, a summary per spoke
/// (`keyCount`, `driftCount`), and the cross-repo links (`from`/`to` node keys,
/// provenance, confidence).
///
/// The links are the **merge** of what is persisted and what is inferred **live**:
/// every persisted `external_ref` edge (authored `[[links]]` → gold, previously
/// `--write`-ten → slate) PLUS the correspondences [`spoke_correspondence`] infers
/// on the fly against the hub — the same match `roteiro links --matrix/--infer`
/// computes. So a plain `sync` (which writes each repo's own `config_key` nodes but
/// no cross-repo edges) already populates this view, with no manual
/// `roteiro links --infer --write` step. A spoke key that matches no hub key is
/// live **drift** (counted in `driftCount`), exactly as the CLI's `--matrix`
/// surfaces orphans.
async fn topology(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let names = ws.names();
    let hub = effective_hub(ws, &names)?;
    // The hub's config keys are the live-inference target (empty when there is no
    // hub, so `spoke_correspondence` yields persisted links only).
    let hub_keys = match &hub {
        Some(h) => ws.with_store(Some(h), Store::config_keys)??,
        None => Vec::new(),
    };

    let mut links: Vec<Value> = Vec::new();
    let mut spokes: Vec<Value> = Vec::new();
    for name in &names {
        if Some(name) == hub.as_ref() {
            continue;
        }
        let (refs, live_orphans) = spoke_correspondence(ws, name, hub.as_deref(), &hub_keys)?;
        if refs.is_empty() && live_orphans.is_empty() {
            continue; // Only projects that reference (or infer against) the hub are spokes.
        }
        let key_count = ws.with_store(Some(name), |s| s.config_keys().map(|c| c.len()))??;
        // A spoke key that matches no hub key is drift, as are the resolving-to-nothing
        // persisted links below.
        let mut drift_count = live_orphans.len();
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
/// matrix + drift ([`overview::OverrideMatrix`]) for the selected workspace.
///
/// Like [`topology`], the cells are the **merge** of the persisted external-ref
/// edges and the correspondences inferred **live** against the hub (via
/// [`spoke_correspondence`], the same match `roteiro links --matrix` computes): a
/// resolving link is an override cell tagged with its real provenance, a persisted
/// link whose hub node is gone is drift, and a spoke key that matches no hub key is
/// live drift too. So the matrix populates straight after a plain `sync`, with no
/// manual `roteiro links --infer --write`.
async fn matrix(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let names = ws.names();
    let Some(hub) = effective_hub(ws, &names)? else {
        // Nothing references (or can infer against) anything — no hub, so an empty
        // (but well-shaped) matrix.
        return Ok(Json(json!({
            "hub": Value::Null, "spokes": [], "rows": [], "drift": []
        }))
        .into_response());
    };

    let hub_values = ws.with_store(Some(&hub), config_values)??;
    let hub_keys = ws.with_store(Some(&hub), Store::config_keys)??;

    let mut spokes: Vec<overview::SpokeInput> = Vec::new();
    for name in &names {
        if name == &hub {
            continue;
        }
        let (refs, live_orphans) = spoke_correspondence(ws, name, Some(&hub), &hub_keys)?;
        if refs.is_empty() && live_orphans.is_empty() {
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
                    // The hub key's source file, so the client (the explorer's "hide
                    // tooling config" toggle) and CLI can classify the row.
                    file: cfgkey_file(&hub_node.key),
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
        // A spoke key that matched no hub key at all is drift too (the CLI's orphans).
        orphans.extend(live_orphans);
        spokes.push(overview::SpokeInput {
            name: name.clone(),
            matches,
            orphans,
        });
    }

    let assembled = overview::build(&hub, &hub_values, spokes);
    Ok(Json(assembled).into_response())
}

/// `POST /v1/graph[/workspaces/{ws}]/links/write` → infer the workspace's cross-repo
/// correspondences and **persist** them into each spoke's graph as durable
/// `inferred` external-ref edges — exactly what `roteiro links --infer --write` does,
/// reusing the same [`crate::infer_links::match_against_hub`] +
/// [`crate::infer_links::link_facts`] + [`Store::apply_import_layer`] path. This is
/// the one deliberately-mutating route on the otherwise read-only API; the durable
/// edges are what the follow-the-link hop and `roteiro check` gates rely on (the live
/// inference in [`topology`]/[`matrix`] does not persist).
///
/// Returns a summary — the hub, per-spoke edge counts, and the total. Idempotent:
/// each spoke's [`LINKS_REF`] layer is re-applied authoritatively (its prior inferred
/// edges cleared first), so re-running returns the same counts and leaves no stale
/// edges.
async fn write_links(State(st): State<AppState>, params: RawPathParams) -> ApiResult {
    let ws = select_ws(&st, &params)?;
    let names = ws.names();
    let Some(hub) = effective_hub(ws, &names)? else {
        return Ok(Json(json!({
            "hub": Value::Null,
            "written": 0,
            "spokes": [],
            "note": "no cross-repo hub — nothing to infer",
        }))
        .into_response());
    };
    let hub_keys = ws.with_store(Some(&hub), Store::config_keys)??;

    let mut total = 0usize;
    let mut per_spoke: Vec<Value> = Vec::new();
    for name in &names {
        if name == &hub {
            continue;
        }
        let spoke_keys = ws.with_store(Some(name), Store::config_keys)??;
        let (matches, _orphans) = crate::infer_links::match_against_hub(&spoke_keys, &hub_keys);
        // Reuse the CLI write path: build the import layer and apply it
        // authoritatively (clearing this ref's prior inferred edges). Applied even
        // with zero matches, so a spoke whose matches have since disappeared has its
        // stale inferred edges cleared — mirroring `persist_inferred_links`.
        let facts = crate::infer_links::link_facts(&hub, &matches);
        let applied =
            ws.with_store_mut(Some(name), |s| s.apply_import_layer(LINKS_REF, &facts))??;
        total += applied.edges_applied;
        per_spoke.push(json!({
            "name": name,
            "matches": matches.len(),
            "written": applied.edges_applied,
        }));
    }

    Ok(Json(json!({ "hub": hub, "written": total, "spokes": per_spoke })).into_response())
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

/// The `<file>` component of a `cfgkey:<file>#<dotted>` config-key node key — the
/// file its config setting was read from. Neither the path nor the dotted key
/// contains `#`, so the first `#` splits them. Returns an empty string for a key
/// that isn't a `cfgkey:` id (the caller treats "unknown file" as app config, so an
/// opt-in tooling filter never hides it). Mirrors the explorer's JS `cfgkeyFile`.
fn cfgkey_file(key: &str) -> String {
    key.strip_prefix("cfgkey:")
        .map(|rest| rest.split_once('#').map_or(rest, |(file, _)| file))
        .unwrap_or_default()
        .to_owned()
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

/// The hub the cross-repo views infer against, honouring both what is persisted and
/// the plain-`sync` case the explorer must now handle:
///
/// - if any persisted external-ref points at a **hosted** project, that project is
///   the hub ([`determine_hub`]) — the historical behaviour, so an authored/linked
///   workspace is unchanged;
/// - otherwise, when there are **no persisted external-ref edges at all** (a repo
///   was synced but never `links --write`-ten), fall back to the CLI's rule — the
///   hosted project with the most `config_key` nodes ([`config_key_count_hub`]) — so
///   live inference has a hub to match against;
/// - but if refs *do* exist yet only target unhosted projects, keep `None` (there is
///   nothing hosted to hub on), preserving the drift-not-404 behaviour.
fn effective_hub(ws: &Workspace, names: &[String]) -> Result<Option<String>, ApiError> {
    if let Some(hub) = determine_hub(ws, names)? {
        return Ok(Some(hub));
    }
    if workspace_has_external_refs(ws, names)? {
        return Ok(None);
    }
    config_key_count_hub(ws, names)
}

/// Whether any hosted project in the workspace carries a persisted external-ref edge
/// (an authored or previously-written cross-repo link). Distinguishes "nothing has
/// been linked yet" (fall back to inference) from "links exist but dangle" (keep the
/// historical `None` hub).
fn workspace_has_external_refs(ws: &Workspace, names: &[String]) -> Result<bool, ApiError> {
    for name in names {
        if !ws.with_store(Some(name), external_refs)??.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The hosted project with the most `config_key` nodes — the CLI's default hub
/// (`roteiro links` picks the repo with the most keys). `None` unless at least two
/// projects have config keys (a single config-bearing repo has nothing to hub). Ties
/// break by name, so selection is deterministic.
fn config_key_count_hub(ws: &Workspace, names: &[String]) -> Result<Option<String>, ApiError> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for name in names {
        let n = ws.with_store(Some(name), |s| s.config_keys().map(|c| c.len()))??;
        if n > 0 {
            counts.push((name.clone(), n));
        }
    }
    if counts.len() < 2 {
        return Ok(None);
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(counts.into_iter().next().map(|(name, _)| name))
}

/// A spoke's merged cross-repo links (persisted + live-inferred) paired with its
/// **live drift** — spoke config keys that match no hub key, as `(dotted key,
/// value)`. The return of [`spoke_correspondence`].
type SpokeLinks = (Vec<ExternalRef>, Vec<(String, String)>);

/// One spoke's cross-repo correspondences: its persisted external-ref links MERGED
/// with the ones inferred **live** against the hub, plus the spoke keys that match
/// no hub key (live drift). This is the single place the explorer brings the CLI's
/// `roteiro links --matrix/--infer` behaviour into the read-only views, reusing
/// [`crate::infer_links::match_against_hub`] verbatim.
///
/// **Persisted wins.** A spoke `config_key` that already carries a persisted link
/// (authored `[[links]]` or a previous `--write`) is left exactly as stored — so
/// authored links still render gold and nothing regresses — and is never
/// re-inferred. Only keys with no persisted link get a live-inferred link (a
/// synthesized [`Provenance::Inferred`] external-ref carrying the real hub target,
/// so the existing resolve/drift path works unchanged) or, matching nothing, become
/// live drift. Dedupe is by (source node, target), keeping the persisted entry.
///
/// With `hub` `None` there is nothing to infer against, so this returns the spoke's
/// persisted links unchanged (and no live drift).
///
/// Cost is O(spoke keys × hub keys) per spoke via the hub-indexed matcher; fine for
/// typical workspaces, but a very large workspace (many spokes, thousands of keys)
/// would want the hub index built once rather than per spoke — noted for follow-up.
fn spoke_correspondence(
    ws: &Workspace,
    name: &str,
    hub: Option<&str>,
    hub_keys: &[ConfigKey],
) -> Result<SpokeLinks, ApiError> {
    let mut refs = ws.with_store(Some(name), external_refs)??;
    let Some(hub) = hub else {
        return Ok((refs, Vec::new()));
    };

    // The spoke source-node keys already covered by a persisted link — never
    // re-inferred, so persisted/authored links are authoritative.
    let persisted: std::collections::HashSet<String> = refs.iter().map(|r| r.src.clone()).collect();

    let spoke_keys = ws.with_store(Some(name), Store::config_keys)??;
    let (matches, key_orphans) = crate::infer_links::match_against_hub(&spoke_keys, hub_keys);

    for m in &matches {
        let src = format!("cfgkey:{}#{}", m.spoke_file, m.spoke_key);
        if persisted.contains(&src) {
            continue; // a persisted link already covers this key
        }
        let qualified = format!("{hub}::cfgkey:{}#{}", m.hub_file, m.hub_key);
        refs.push(ExternalRef {
            src,
            node: external_ref_node(&qualified),
            provenance: Provenance::Inferred,
            confidence: Some(m.confidence),
        });
    }

    // Defensive dedupe by (source, target): persisted entries come first, so they
    // win over any live-inferred duplicate (honouring "dedupe by (from, to)").
    let mut seen = std::collections::HashSet::new();
    refs.retain(|r| {
        seen.insert((
            r.src.clone(),
            external_ref_target(&r.node).unwrap_or_default(),
        ))
    });

    // A spoke key that matched no hub key is live drift — but only if it isn't
    // already carried by a persisted link.
    let orphans = key_orphans
        .into_iter()
        .filter(|o| !persisted.contains(&format!("cfgkey:{}#{}", o.file, o.key)))
        .map(|o| (o.key, o.value))
        .collect();

    Ok((refs, orphans))
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
///
/// **This deliberately discards direction** — both ends of every edge are
/// incremented — because "how connected is this node" is a question about
/// degree, over every edge kind. It is *not* a coupling measure: a node called
/// by twenty callers and a node calling twenty callees score identically here.
/// `/coupling` answers that question instead (`rto_graph::coupling`), and the
/// two are kept separate rather than one being bent into the other.
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

    /// A struct node as extraction emits it: key `sym:rust:<file>#<Name>`, kind
    /// `struct`, declared field names in `meta.fields` (the follow bridge's signal).
    fn struct_node(name: &str, fields: &[&str]) -> Node {
        let mut node = Node::new(
            format!("sym:rust:crates/roteiro/src/config.rs#{name}"),
            NodeKind::Struct,
            name.to_owned(),
        );
        node.path = Some("crates/roteiro/src/config.rs".to_owned());
        node.meta = json!({ "fields": fields });
        node
    }

    /// Build a hub with the config struct AND its config keys, so the follow
    /// endpoint's `config_key → struct` bridge can be exercised end to end: the
    /// `ServeConfig` struct declares `addr` (so `serve.addr` bridges) but not
    /// `tools` (so `serve.tools` resolves yet falls back to its config-key node).
    fn bridge_hub_store() -> Store {
        let store = Store::open_in_memory().expect("hub store");
        let facts = FactSet::new()
            .with_node(struct_node("ServeConfig", &["addr"]))
            .with_node(cfg_node("config.toml", "serve.addr", "127.0.0.1:8017"))
            .with_node(cfg_node("config.toml", "serve.tools", "true"));
        apply(store, &facts)
    }

    /// A spoke whose inferred link points at the hub's bridgeable `serve.addr` key
    /// — the follow-the-link scenario end to end.
    fn bridge_spoke_store() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let target = format!("{HUB}::cfgkey:config.toml#serve.addr");
        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(external_ref_node(&target))
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#SERVE_ADDR",
                external_ref_key(&target),
                EdgeKind::References,
                0.9,
            ));
        apply(store, &facts)
    }

    /// The linked hub+spoke workspace used by the follow-endpoint tests.
    fn bridge_workspace() -> Workspace {
        Workspace::from_stores([
            (HUB.to_owned(), bridge_hub_store()),
            (SPOKE.to_owned(), bridge_spoke_store()),
        ])
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

    /// Build a spoke store where **two distinct config keys point at the SAME hub
    /// target** (`serve.addr`) — one inferred, one authored — plus one drift key.
    /// Because both share a single external-ref placeholder node, this is the case
    /// that a naive `to`-keyed index would collapse: the `/links` payload must still
    /// report both links distinctly, each with its own `from`/provenance, so the UI
    /// can style each config→app-key edge independently (per-edge, not per-target).
    fn spoke_shared_target_and_drift() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let shared = format!("{HUB}::cfgkey:config.toml#serve.addr");
        let drift = format!("{HUB}::cfgkey:config.toml#serve.legacy");

        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "PROXY_ADDR", "0.0.0.0:9443"))
            .with_node(cfg_node("deploy.env", "LEGACY_ADDR", "10.0.0.1:9000"))
            .with_node(external_ref_node(&shared))
            .with_node(external_ref_node(&drift))
            // Two edges into the one shared placeholder — distinct sources, distinct
            // provenance (an inferred match and an authored `[[links]]`).
            .with_edge(Edge::inferred(
                "cfgkey:deploy.env#SERVE_ADDR",
                external_ref_key(&shared),
                EdgeKind::References,
                0.9,
            ))
            .with_edge(Edge::authored(
                "cfgkey:deploy.env#PROXY_ADDR",
                external_ref_key(&shared),
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

    /// A hub store with three config keys and NO cross-repo edges — the plain-`sync`
    /// state a spoke infers against live (no `links --write` has run).
    fn infer_hub_store() -> Store {
        let store = Store::open_in_memory().expect("hub store");
        let facts = FactSet::new()
            .with_node(cfg_node("config.toml", "serve.addr", "127.0.0.1:8017"))
            .with_node(cfg_node("config.toml", "serve.tools", "true"))
            .with_node(cfg_node("config.toml", "serve.workers", "4"));
        apply(store, &facts)
    }

    /// A spoke store with two config keys that match the hub (`SERVE_ADDR`,
    /// `SERVE_TOOLS`) and one that matches nothing (`EXTRA_FLAG`, an orphan) — and
    /// NO persisted external-ref edges, so its cross-repo links exist only live.
    fn infer_spoke_store() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "SERVE_TOOLS", "false"))
            .with_node(cfg_node("deploy.env", "EXTRA_FLAG", "on"));
        apply(store, &facts)
    }

    /// A hub+spoke workspace with matching config keys but NO persisted external-ref
    /// edges — the empty-after-`sync` case the live inference must populate.
    fn inferable_workspace() -> Workspace {
        Workspace::from_stores([
            (HUB.to_owned(), infer_hub_store()),
            (SPOKE.to_owned(), infer_spoke_store()),
        ])
    }

    /// A spoke with ONE persisted authored link (`SERVE_TOOLS` → hub `serve.tools`,
    /// gold) whose OTHER keys only correspond LIVE (`SERVE_ADDR` → `serve.addr`),
    /// plus an orphan (`EXTRA_FLAG`). Proves the merged view keeps the authored link
    /// authored while adding the inferred one — persisted and live side by side.
    fn spoke_authored_plus_inferable() -> Store {
        let store = Store::open_in_memory().expect("spoke store");
        let authored = format!("{HUB}::cfgkey:config.toml#serve.tools");
        let facts = FactSet::new()
            .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
            .with_node(cfg_node("deploy.env", "SERVE_TOOLS", "false"))
            .with_node(cfg_node("deploy.env", "EXTRA_FLAG", "on"))
            .with_node(external_ref_node(&authored))
            .with_edge(Edge::authored(
                "cfgkey:deploy.env#SERVE_TOOLS",
                external_ref_key(&authored),
                EdgeKind::References,
            ));
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

    /// Drive `method uri` against a prebuilt `app` (cloned per call), so several
    /// requests hit the SAME in-memory workspace set — needed to observe a write's
    /// effect on a later read.
    async fn send(app: Router, method: &str, uri: &str) -> (StatusCode, Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
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

    // -- capability signal: what the web app enables -----------------------

    #[tokio::test]
    async fn capabilities_report_ask_off_for_the_llama_free_explorer() {
        // The plain `router` (what `roteiro explorer` serves) has no chat
        // endpoint, so it must advertise `ask:false` and no models — the signal
        // that keeps the web app's Ask tab in its disabled state.
        let (status, json) = get(multi_set(), None, "/v1/graph/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["ask"], false, "explorer build cannot Ask");
        assert_eq!(json["models"], json!([]), "no model is served");
    }

    #[tokio::test]
    async fn capabilities_report_ask_on_with_served_models() {
        // A `serve` build merges this API onto its `/v1` app and reports
        // `router_with_capabilities`, advertising the mounted chat endpoint and
        // the served model ids so the web app enables Ask.
        let caps = Capabilities {
            ask: true,
            models: vec!["qwen3-0.6b".to_owned()],
        };
        let resp = router_with_capabilities(Arc::new(multi_set()), None, caps)
            .oneshot(
                Request::builder()
                    .uri("/v1/graph/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["ask"], true, "serve build can Ask");
        assert_eq!(json["models"], json!(["qwen3-0.6b"]));
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

    // -- follow-the-link hop: config_key → struct bridge ------------------

    #[tokio::test]
    async fn follow_bridges_a_config_key_to_its_defining_struct_field() {
        // The core proof: `serve.addr` follows past the config_key to the
        // `ServeConfig` struct, tagged `struct_field` with the matched field.
        let (status, body) = get(
            single_set(bridge_workspace()),
            None,
            "/v1/graph/follow?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], "struct_field");
        assert_eq!(body["drift"], false);
        assert_eq!(body["project"], "hub");
        assert_eq!(body["field"], "addr");
        // Flat route with a sole workspace and no explicit default: the response
        // still reports the concrete auto-selected workspace name, never null.
        assert_eq!(body["workspace"], "linked");
        assert_eq!(
            body["target"]["key"],
            "sym:rust:crates/roteiro/src/config.rs#ServeConfig"
        );
        assert_eq!(body["target"]["kind"], "struct");
    }

    #[tokio::test]
    async fn follow_falls_back_to_the_config_key_when_unbridgeable() {
        // `serve.tools` resolves, but `ServeConfig` declares no `tools` field, so
        // the hop falls back to the config_key node — never a wrong struct.
        let (status, body) = get(
            single_set(bridge_workspace()),
            None,
            "/v1/graph/follow?qualified=hub::cfgkey:config.toml%23serve.tools",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], "config_key");
        assert_eq!(body["drift"], false);
        assert_eq!(body["field"], Value::Null);
        assert_eq!(body["target"]["key"], "cfgkey:config.toml#serve.tools");
        assert_eq!(body["target"]["kind"], "config_key");
    }

    #[tokio::test]
    async fn follow_reports_drift_for_an_orphan_target() {
        // A well-formed target whose node is gone → drift, no navigation target.
        let (status, body) = get(
            single_set(bridge_workspace()),
            None,
            "/v1/graph/follow?qualified=hub::cfgkey:config.toml%23serve.legacy",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["drift"], true);
        assert_eq!(body["target"], Value::Null);
        assert_eq!(body["kind"], Value::Null);
        assert_eq!(body["project"], "hub");
    }

    #[tokio::test]
    async fn follow_requires_a_qualified_key() {
        let (status, _) = get(single_set(bridge_workspace()), None, "/v1/graph/follow").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = get(
            single_set(bridge_workspace()),
            None,
            "/v1/graph/follow?qualified=notqualified",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn follow_is_scoped_to_the_selected_workspace() {
        // The nested route resolves within the named workspace. The same key
        // followed in `solo` (whose `hub` lacks the struct AND the key) drifts.
        let (status, linked) = get(
            multi_set(),
            None,
            "/v1/graph/workspaces/linked/follow?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // `linked`'s hub_store has the config key but no struct → config_key fallback.
        assert_eq!(linked["kind"], "config_key");
        assert_eq!(linked["drift"], false);
        assert_eq!(linked["workspace"], "linked");

        let (_, solo) = get(
            multi_set(),
            None,
            "/v1/graph/workspaces/solo/follow?qualified=hub::cfgkey:config.toml%23serve.addr",
        )
        .await;
        assert_eq!(solo["drift"], true);
        assert_eq!(solo["target"], Value::Null);
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

    /// A media node's generated content reaches the explorer **attributed**, and
    /// a gate refusal reaches it as a refusal rather than as a hole (ADR-0015).
    ///
    /// The two records here are the two outcomes: one producer transcribed the
    /// clip, another was refused by the pre-generation gate. Both must come back
    /// on the node, marked `generated`, and neither may appear anywhere the UI
    /// reads *extracted* content — which is why the assertions also check the
    /// node's own `meta`.
    #[tokio::test]
    async fn node_detail_surfaces_generated_media_attributed_to_its_producer() {
        let store = Store::open_in_memory().expect("store");
        let mut clip = Node::new("file:assets/silence.wav", NodeKind::File, "silence.wav");
        clip.path = Some("assets/silence.wav".to_owned());
        clip.blob_hash = Some("blob-silence".to_owned());
        let mut store = apply(store, &FactSet::new().with_node(clip));

        let voxtral = rto_graph::Producer {
            kind: rto_graph::MediaKind::Audio,
            model: "voxtral-mini-3b".to_owned(),
            model_digest: "4705be8e".to_owned(),
            quantisation: "Q4_K_M".to_owned(),
            mmproj_digest: "4f24c4ef".to_owned(),
            prompt: "Transcribe this audio recording.".to_owned(),
            temperature: 0.0,
            max_tokens: 512,
        };
        let successor = rto_graph::Producer {
            model: "voxtral-small-24b".to_owned(),
            ..voxtral.clone()
        };
        for (producer, outcome) in [
            (
                &voxtral,
                rto_graph::MediaOutcome::Generated(rto_graph::GeneratedContent {
                    text: "Tonight I want to talk about world government.".to_owned(),
                    confidence: None,
                }),
            ),
            (
                &successor,
                rto_graph::MediaOutcome::Skipped(rto_graph::MediaSkip {
                    reason: rto_graph::GateReason::Silence,
                    value: 0.0,
                    threshold: 0.0001,
                }),
            ),
        ] {
            store
                .record_media_content(&rto_graph::MediaWrite {
                    blob_id: "blob-silence",
                    path: "assets/silence.wav",
                    producer,
                    tool_version: "9.9.9",
                    outcome: &outcome,
                    replace: false,
                })
                .expect("record");
        }

        let (status, json) = get(
            single_set(Workspace::single(HUB, store)),
            None,
            "/v1/graph/hub/node/file:assets/silence.wav",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let records = json["generated"].as_array().expect("a generated array");
        assert_eq!(records.len(), 2, "both producers' records must surface");

        // Ordered by producer id, so the assertions can name them.
        let transcript = records
            .iter()
            .find(|r| r["text"].is_string())
            .expect("the generated record");
        assert_eq!(transcript["generated"], true, "an unmissable marker");
        assert_eq!(transcript["model"], "voxtral-mini-3b");
        assert_eq!(transcript["kind"], "audio");
        assert_eq!(transcript["quantisation"], "Q4_K_M");
        assert_eq!(
            transcript["producer"],
            voxtral.id().to_string(),
            "the full producer identity, not just the model name",
        );
        assert!(transcript["skipped"].is_null());
        // The per-blob rebuild the UI hands the operator.
        assert_eq!(
            transcript["rebuild"],
            "roteiro media build --blob blob-silence --force"
        );

        let refusal = records
            .iter()
            .find(|r| !r["skipped"].is_null())
            .expect("the gated record");
        assert!(
            refusal["text"].is_null(),
            "a gated skip carries no text to render",
        );
        assert_eq!(refusal["skipped"]["reason"], "silence");
        assert_eq!(refusal["skipped"]["metric"], "rms");
        assert_eq!(
            refusal["skipped"]["explanation"], "below silence threshold (rms=0, threshold 0.0001)",
            "the operator-facing line names the metric and its measured value",
        );

        // …and none of it leaked into the node itself. `meta` is where the UI
        // reads EXTRACTED content from, so a transcript appearing there is
        // exactly the confusion ADR-0015 exists to prevent.
        assert!(
            !json["meta"].to_string().contains("world government"),
            "generated text must not reach the node's meta: {}",
            json["meta"],
        );
        assert_eq!(json["node"]["key"], "file:assets/silence.wav");
    }

    /// A node with no blob — every symbol, every config key — gets an **empty**
    /// generated array, not a missing key. The UI can then branch on length
    /// alone instead of on presence.
    #[tokio::test]
    async fn a_node_without_media_reports_an_empty_generated_array() {
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/node/sym:main",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["generated"].as_array().map(Vec::len),
            Some(0),
            "the key is always present, so a consumer never has to guess",
        );
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

    // ---- Issue #321: the graph API must apply the *target repo's own* debt
    // exclusions, so the explorer UI and that repo's CLI report one number. ----

    /// One intent-debt marker as extraction emits it, at `path`.
    fn marker_node(path: &str, line: u32) -> Node {
        let mut node = Node::new(
            format!("marker:{path}#{line}"),
            NodeKind::Marker,
            "TODO: finish".to_owned(),
        );
        node.path = Some(path.to_owned());
        node.meta = json!({ "category": "todo", "text": "TODO: finish", "line": line });
        node
    }

    /// A real git repository at `dir` with an optional `roteiro.toml` and a
    /// `graph.db` holding one debt marker per path — the on-disk shape
    /// [`Workspace::from_repo_paths`] discovers, which is what makes the per-repo
    /// config resolution observable at all.
    fn debt_repo(dir: &std::path::Path, toml: Option<&str>, markers: &[&str]) {
        std::fs::create_dir_all(dir).expect("mkdir repo");
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .expect("run git init")
            .success();
        assert!(ok, "git init failed in {}", dir.display());
        if let Some(toml) = toml {
            std::fs::write(dir.join("roteiro.toml"), toml).expect("write roteiro.toml");
        }
        let store_dir = dir.join(".git").join("roteiro");
        std::fs::create_dir_all(&store_dir).expect("mkdir store");
        let mut store = Store::open(&store_dir.join("graph.db")).expect("open store");
        let mut facts = FactSet::new();
        for (i, path) in markers.iter().enumerate() {
            facts = facts.with_node(marker_node(path, u32::try_from(i).unwrap() + 1));
        }
        store.apply_factset(&facts).expect("apply markers");
    }

    fn fresh_repo_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "roteiro-api-debt-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[tokio::test]
    async fn debt_endpoint_applies_the_repos_own_exclusions() {
        // Before the fix this endpoint called `debt(s, &[], &[])`: the ignore
        // lists were passed EMPTY, so the browser counted markers the CLI
        // excluded and the two disagreed about the same repository.
        let root = fresh_repo_root("own");
        let repo = root.join("app");
        debt_repo(
            &repo,
            Some("[debt]\nignore = [\"docs/**\", \"CHANGELOG.md\"]\n"),
            &["src/lib.rs", "docs/guide.md", "CHANGELOG.md"],
        );

        let ws = Workspace::from_repo_paths([&repo]).expect("workspace");
        let (status, json) = get(single_set(ws), None, "/v1/graph/app/debt").await;
        assert_eq!(status, StatusCode::OK);

        // Only `src/lib.rs` survives — `docs/**` and `CHANGELOG.md` are excluded
        // by the repo's own config, exactly as the CLI excludes them.
        assert_eq!(
            json["total"], 1,
            "excluded paths must not be counted: {json}"
        );
        let paths: Vec<&str> = json["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|i| i["path"].as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs"], "kept the right marker");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn each_repo_is_scanned_under_its_own_config_not_the_first_ones() {
        // The multi-repo half (#321b): repo B's own `[debt] ignore` must govern B
        // even though the request arrives at a server that also hosts A. A
        // repository's own config governs how it is scanned, whoever is asking.
        let root = fresh_repo_root("per-repo");
        let (a, b) = (root.join("alpha"), root.join("beta"));
        // Same marker paths in both repos; different exclusions.
        let markers = ["src/lib.rs", "docs/guide.md", "vendor/dep.rs"];
        debt_repo(&a, Some("[debt]\nignore = [\"docs/**\"]\n"), &markers);
        debt_repo(&b, Some("[debt]\nignore = [\"vendor/**\"]\n"), &markers);

        let ws = Workspace::from_repo_paths([&a, &b]).expect("workspace");
        let set = WorkspaceSet::from_workspaces([("ws".to_owned(), ws, true)]);
        let app = router(Arc::new(set), Some("ws".to_owned()));

        let (sa, ja) = send(app.clone(), "GET", "/v1/graph/alpha/debt").await;
        let (sb, jb) = send(app, "GET", "/v1/graph/beta/debt").await;
        assert_eq!((sa, sb), (StatusCode::OK, StatusCode::OK));

        let paths = |j: &Value| -> Vec<String> {
            j["items"]
                .as_array()
                .expect("items")
                .iter()
                .filter_map(|i| i["path"].as_str().map(str::to_owned))
                .collect()
        };
        // alpha hides docs/, beta hides vendor/ — neither borrows the other's list.
        assert_eq!(
            paths(&ja),
            vec!["src/lib.rs".to_owned(), "vendor/dep.rs".to_owned()],
            "alpha uses alpha's config: {ja}"
        );
        assert_eq!(
            paths(&jb),
            vec!["docs/guide.md".to_owned(), "src/lib.rs".to_owned()],
            "beta uses beta's config, not alpha's: {jb}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn debt_endpoint_reports_no_exclusions_for_a_repoless_project() {
        // A pre-opened store has no repository on disk, so there is no config to
        // consult — and substituting the invoking process's would be the very
        // mix-up the per-repo resolution exists to prevent. Everything counts.
        let (status, json) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/debt",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["total"].is_number(), "still answers: {json}");
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
    async fn coupling_endpoint_reports_the_direction_hotspots_discards() {
        // `sym:main` calls `sym:helper`, so the two have identical *degree* — and
        // `/hotspots` therefore ranks them equal. `/coupling` must not.
        let set = || single_set(Workspace::single(HUB, hub_store()));

        let (status, json) = get(set(), None, "/v1/graph/hub/coupling?order=fan_in&limit=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["order"], "fan_in");
        assert_eq!(json["edge_kind"], "calls");
        assert_eq!(json["items"][0]["key"], "sym:helper", "the callee: {json}");
        assert_eq!(json["items"][0]["fan_in"], 1);
        assert_eq!(json["items"][0]["fan_out"], 0);

        let (status, json) = get(set(), None, "/v1/graph/hub/coupling?order=fan_out&limit=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["items"][0]["key"], "sym:main", "the caller: {json}");
        assert_eq!(json["items"][0]["fan_out"], 1);
    }

    #[tokio::test]
    async fn coupling_endpoint_rejects_an_unknown_order() {
        // Falling back to `total` would answer a question the caller did not ask
        // and give them no way to tell.
        let (status, _) = get(
            single_set(Workspace::single(HUB, hub_store())),
            None,
            "/v1/graph/hub/coupling?order=degree",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Two files, the same marker count, lengths twenty-fold apart — the pair
    /// `/debt` cannot separate and `/debt/density` must.
    fn marked_store() -> Store {
        let file = |path: &str, lines: u64| {
            let mut n = Node::new(format!("file:{path}"), NodeKind::File, path);
            n.path = Some(path.to_owned());
            n.meta = json!({ "bytes": lines * 30, "lines": lines });
            n
        };
        let marker = |path: &str, line: u32| {
            let mut n = Node::new(
                format!("marker:{path}#{line}"),
                NodeKind::Marker,
                "TODO x", // roteiro:ignore
            );
            n.path = Some(path.to_owned());
            n.meta = json!({
                "category": "todo", // roteiro:ignore
                "text": "TODO x",   // roteiro:ignore
                "line": line,
            });
            n
        };
        let mut facts = FactSet::new()
            .with_node(file("big.rs", 4000))
            .with_node(file("small.rs", 200));
        for line in 1..=10 {
            facts = facts.with_node(marker("big.rs", line));
            facts = facts.with_node(marker("small.rs", line));
        }
        apply(Store::open_in_memory().expect("store"), &facts)
    }

    #[tokio::test]
    async fn density_endpoint_normalises_the_count_debt_reports_raw() {
        let set = || single_set(Workspace::single(HUB, marked_store()));

        let (status, json) = get(set(), None, "/v1/graph/hub/debt/density").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["order"], "density");
        assert_eq!(json["items"][0]["path"], "small.rs", "{json}");
        assert_eq!(json["items"][0]["per_kloc"], 50.0);
        assert_eq!(json["items"][1]["per_kloc"], 2.5);
        assert_eq!(
            json["items"][0]["markers"], json["items"][1]["markers"],
            "the same raw count `/debt` would report: {json}"
        );
        assert_eq!(json["overall_per_kloc"], 4.76, "the baseline: {json}");

        // `min_lines` is reachable, and excluding a file is reported rather than
        // served as a silently shorter ranking.
        let (status, json) = get(set(), None, "/v1/graph/hub/debt/density?min_lines=1000").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["short_files"], 1);
        assert_eq!(json["files_with_markers"], 2);
        assert_eq!(json["items"][0]["path"], "big.rs", "{json}");
    }

    #[tokio::test]
    async fn density_endpoint_rejects_an_unknown_order() {
        // Falling back to `density` would answer a question the caller did not
        // ask and give them no way to tell.
        let (status, _) = get(
            single_set(Workspace::single(HUB, marked_store())),
            None,
            "/v1/graph/hub/debt/density?order=count",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn config_secrets_endpoint_reports_state_and_never_a_value() {
        // The hub store's config keys are not secret-named, so start from an
        // inventory that has something in it.
        let store = apply(
            Store::open_in_memory().expect("store"),
            &FactSet::new()
                .with_node(cfg_node(".env", "API_TOKEN", "<redacted>"))
                .with_node(cfg_node("config.toml", "serve.addr", "127.0.0.1:8017")),
        );
        let (status, json) = get(
            single_set(Workspace::single(HUB, store)),
            None,
            "/v1/graph/hub/config-secrets",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["secret_named"], 1, "{json}");
        assert_eq!(json["config_keys"], 2, "the population: {json}");
        assert_eq!(json["redacted"], 1);
        assert_eq!(json["unredacted"], 0);
        assert_eq!(json["items"][0]["name"], "API_TOKEN");
        assert_eq!(json["items"][0]["state"], "redacted");

        // No value reaches the wire — not even the redaction placeholder.
        let body = json.to_string();
        assert!(
            json["items"][0].get("value").is_none() && !body.contains("<redacted>"),
            "the endpoint serves presence and state, never a value: {body}"
        );
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

    // -- LIVE cross-repo inference: populate the hub view after a plain `sync` ----
    //
    // The regression the user hit: `sync` writes each repo's own `config_key` nodes
    // but no cross-repo edges, so the explorer's hub view was EMPTY until a separate
    // `roteiro links --infer --write`. topology/matrix now infer the correspondences
    // LIVE (the same `infer_links::match_against_hub` the CLI's `--matrix` uses),
    // merged with any persisted links, so a plain sync + open explorer just works.

    #[tokio::test]
    async fn topology_infers_cross_repo_links_live_without_persisted_edges() {
        // hub + spoke share matching `config_key` nodes but carry NO external-ref
        // edges (the plain-`sync` state). topology must infer the links + drift LIVE.
        let (status, json) = get(
            single_set(inferable_workspace()),
            None,
            "/v1/graph/topology",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["hub"], HUB,
            "the config-key-rich repo is the inferred hub"
        );
        let spokes = json["spokes"].as_array().unwrap();
        assert_eq!(spokes.len(), 1);
        assert_eq!(spokes[0]["name"], SPOKE);
        // Two live-inferred links (SERVE_ADDR, SERVE_TOOLS); the orphan EXTRA_FLAG drifts.
        let links = json["links"].as_array().unwrap();
        assert_eq!(links.len(), 2, "both matching keys infer a link");
        assert!(
            links.iter().all(|l| l["provenance"] == "inferred"),
            "live-inferred links read slate"
        );
        assert_eq!(
            spokes[0]["driftCount"], 1,
            "the unmatched key is live drift"
        );
    }

    #[tokio::test]
    async fn matrix_infers_overrides_and_drift_live_without_persisted_edges() {
        let (status, json) = get(single_set(inferable_workspace()), None, "/v1/graph/matrix").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB);
        // serve.addr + serve.tools are overridden live → two rows.
        let rows = json["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let addr = rows.iter().find(|r| r["hub_key"] == "serve.addr").unwrap();
        assert_eq!(
            addr["cells"][SPOKE]["provenance"], "inferred",
            "a live correspondence is inferred, not authored"
        );
        // The unmatched spoke key surfaces as drift, exactly like the CLI's orphans.
        let drift = json["drift"].as_array().unwrap();
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0]["key"], "EXTRA_FLAG");
    }

    #[tokio::test]
    async fn matrix_rows_carry_the_hub_source_file_for_tooling_classification() {
        // The explorer's "hide tooling config" toggle must be able to classify an
        // override-matrix row as app vs tooling config — but a row is keyed by its
        // dotted hub key alone, with no source file. This proves the payload now
        // carries the hub key's source file per row: a hub with one app-config key
        // (`config.toml`) and one tooling key (`Cargo.toml`), each overridden by a
        // spoke. Both rows are present BY DEFAULT (the server never filters — the
        // toggle is client-side), and each row's `file` classifies correctly.
        let hub = {
            let store = Store::open_in_memory().expect("hub store");
            let facts = FactSet::new()
                .with_node(cfg_node("config.toml", "serve.addr", "127.0.0.1:8017"))
                .with_node(cfg_node("Cargo.toml", "package.name", "roteiro"));
            apply(store, &facts)
        };
        let spoke = {
            let store = Store::open_in_memory().expect("spoke store");
            let app_target = format!("{HUB}::cfgkey:config.toml#serve.addr");
            let tooling_target = format!("{HUB}::cfgkey:Cargo.toml#package.name");
            let facts = FactSet::new()
                .with_node(cfg_node("deploy.env", "SERVE_ADDR", "0.0.0.0:8443"))
                .with_node(cfg_node("deploy.env", "PACKAGE_NAME", "deploy"))
                .with_node(external_ref_node(&app_target))
                .with_node(external_ref_node(&tooling_target))
                .with_edge(Edge::inferred(
                    "cfgkey:deploy.env#SERVE_ADDR",
                    external_ref_key(&app_target),
                    EdgeKind::References,
                    0.9,
                ))
                .with_edge(Edge::inferred(
                    "cfgkey:deploy.env#PACKAGE_NAME",
                    external_ref_key(&tooling_target),
                    EdgeKind::References,
                    0.9,
                ));
            apply(store, &facts)
        };
        let ws = Workspace::from_stores([(HUB.to_owned(), hub), (SPOKE.to_owned(), spoke)]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/matrix").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB);

        let rows = json["rows"].as_array().unwrap();
        // Both the app-config and the tooling-sourced hub key are shown by default.
        let app_row = rows.iter().find(|r| r["hub_key"] == "serve.addr").unwrap();
        let tooling_row = rows
            .iter()
            .find(|r| r["hub_key"] == "package.name")
            .unwrap();

        // Every row now carries its hub key's source file.
        assert_eq!(app_row["file"], "config.toml");
        assert_eq!(tooling_row["file"], "Cargo.toml");

        // And that file classifies exactly as the shared tooling classifier does, so
        // the client (and CLI) can hide the tooling row when the filter is on.
        assert!(!rto_graph::is_tooling_config_path(
            app_row["file"].as_str().unwrap()
        ));
        assert!(rto_graph::is_tooling_config_path(
            tooling_row["file"].as_str().unwrap()
        ));
    }

    #[tokio::test]
    async fn topology_merges_persisted_authored_with_live_inferred() {
        // A spoke with one AUTHORED persisted link and other keys that only match
        // live: the merged topology carries both — authored stays gold, the rest
        // infer slate — so persisted links never regress under live inference.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_authored_plus_inferable()),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/topology").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["hub"], HUB);
        let provs: std::collections::BTreeSet<&str> = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["provenance"].as_str())
            .collect();
        assert_eq!(
            provs,
            ["authored", "inferred"].into_iter().collect(),
            "the authored link and the live-inferred one both render"
        );
        assert_eq!(
            json["spokes"][0]["driftCount"], 1,
            "the two matched keys resolve; the orphan drifts"
        );
    }

    #[tokio::test]
    async fn write_links_persists_inferred_edges_and_is_idempotent() {
        // Reuse ONE in-memory set across requests (a write must be observable by a
        // later read), so build the router once and clone it per call.
        let app = router(Arc::new(single_set(inferable_workspace())), None);

        // Before: the spoke store holds NO external-ref nodes (nothing persisted).
        let (_, before) = send(
            app.clone(),
            "GET",
            "/v1/graph/spoke/nodes?kinds=external_ref",
        )
        .await;
        assert_eq!(before["total"], 0);

        // Persist: infer + write across the workspace (the CLI `--infer --write` path).
        let (status, body) = send(app.clone(), "POST", "/v1/graph/links/write").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["hub"], HUB);
        assert_eq!(
            body["written"], 2,
            "both matching keys persist an inferred edge"
        );

        // After: the external-ref placeholder nodes now exist in the spoke store.
        let (_, after) = send(
            app.clone(),
            "GET",
            "/v1/graph/spoke/nodes?kinds=external_ref",
        )
        .await;
        assert_eq!(
            after["total"], 2,
            "one external-ref node per persisted link"
        );

        // Idempotent: re-running writes the same count and adds no new nodes.
        let (_, again) = send(app.clone(), "POST", "/v1/graph/links/write").await;
        assert_eq!(again["written"], body["written"]);
        let (_, after2) = send(
            app.clone(),
            "GET",
            "/v1/graph/spoke/nodes?kinds=external_ref",
        )
        .await;
        assert_eq!(after2["total"], 2, "no duplicate nodes on re-write");

        // The persisted links now render as inferred in the topology (matched keys
        // resolve; the orphan still drifts).
        let (_, top) = send(app.clone(), "GET", "/v1/graph/topology").await;
        assert_eq!(top["links"].as_array().unwrap().len(), 2);
        assert_eq!(top["spokes"][0]["driftCount"], 1);
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

    #[tokio::test]
    async fn project_links_report_multiple_links_into_one_target_distinctly() {
        // Two spoke config keys point at the SAME hub key `serve.addr` (one inferred,
        // one authored), plus a drift key. They share one external-ref node, so a
        // `to`-keyed index would collapse them — the payload must instead carry BOTH
        // as distinct links, each with its own `from` and provenance. This is the
        // data guarantee the per-edge JS styling and the per-node chips rely on.
        let ws = Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_shared_target_and_drift()),
        ]);
        let (status, json) = get(single_set(ws), None, "/v1/graph/spoke/links").await;
        assert_eq!(status, StatusCode::OK);
        let links = json["links"].as_array().unwrap();
        assert_eq!(
            links.len(),
            3,
            "two links into the shared target + one drift"
        );

        // Both links into `serve.addr` survive — the shared target is not collapsed.
        let into_addr: Vec<&Value> = links
            .iter()
            .filter(|l| l["toQualified"] == "hub::cfgkey:config.toml#serve.addr")
            .collect();
        assert_eq!(
            into_addr.len(),
            2,
            "both links into the one target are reported"
        );
        let froms: std::collections::BTreeSet<&str> = into_addr
            .iter()
            .filter_map(|l| l["fromName"].as_str())
            .collect();
        assert_eq!(
            froms,
            ["PROXY_ADDR", "SERVE_ADDR"].into_iter().collect(),
            "each link keeps its own source config key"
        );
        let provs: std::collections::BTreeSet<&str> = into_addr
            .iter()
            .filter_map(|l| l["provenance"].as_str())
            .collect();
        assert_eq!(
            provs,
            ["authored", "inferred"].into_iter().collect(),
            "each edge into the shared target keeps its own provenance"
        );
        // Both share the (live) target, so neither drifts and both point at the same
        // external-ref node key.
        assert!(into_addr.iter().all(|l| l["drift"] == false));
        assert!(
            into_addr
                .iter()
                .all(|l| l["to"] == "extref:hub::cfgkey:config.toml#serve.addr"),
            "both links share the one external-ref node"
        );

        // The unrelated legacy key still drifts on its own.
        let drift = links
            .iter()
            .find(|l| l["fromName"] == "LEGACY_ADDR")
            .unwrap();
        assert_eq!(drift["drift"], true);
        assert_eq!(drift["toName"], Value::Null);
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

    // -- selector/route-by-type: the payload shapes the landing relies on -----
    //
    // The workspace-selector landing routes BY PROJECT COUNT: a workspace with
    // MORE THAN ONE project opens the cross-repo workspace view; one with exactly
    // ONE project drills straight into that project's graph. And when there is only
    // ONE workspace total, the selector is skipped and the app auto-enters by the
    // same rule. These reuse the in-memory sets above to pin the three payload
    // shapes the JS selector/routing consume (the routing itself is JS+DOM, so it
    // needs a browser for full QA — see the PR notes).

    #[tokio::test]
    async fn workspaces_payload_carries_both_shapes_the_selector_routes_on() {
        // The multi set holds both shapes at once: a multi-repo hub (>1 project →
        // workspace view) and a standalone singleton (exactly 1 project → project).
        let (status, _ct, body) =
            get_app(explorer_router(multi_set(), None), "/v1/graph/workspaces").await;
        assert_eq!(status, StatusCode::OK);
        let arr: Value = serde_json::from_str(&body).unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 2, "both workspaces are offered in the selector");
        let linked = arr.iter().find(|w| w["name"] == "linked").unwrap();
        assert_eq!(linked["linked"], true);
        assert!(
            linked["projects"].as_array().unwrap().len() > 1,
            "a hub has more than one project → routes to the cross-repo view"
        );
        let solo = arr.iter().find(|w| w["name"] == "solo").unwrap();
        assert_eq!(solo["linked"], false);
        assert_eq!(
            solo["projects"].as_array().unwrap().len(),
            1,
            "a standalone repo has exactly one project → drills straight in"
        );
    }

    #[tokio::test]
    async fn standalone_only_is_a_single_one_project_workspace_for_auto_enter() {
        // A lone standalone workspace with exactly one project: the selector is
        // skipped (nothing to choose) and the app auto-enters that project's graph.
        let (status, _ct, body) = get_app(
            explorer_router(standalone_only_set(), Some("solo")),
            "/v1/graph/workspaces",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let arr: Value = serde_json::from_str(&body).unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 1, "a single workspace → auto-enter, no selector");
        assert_eq!(arr[0]["linked"], false);
        assert_eq!(arr[0]["projects"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn single_multi_repo_workspace_auto_enters_the_cross_repo_view() {
        // A lone multi-repo workspace (>1 project): still no choice, so the selector
        // is skipped — but auto-enter lands on the cross-repo view, not a project.
        let (status, _ct, body) = get_app(
            explorer_router(single_set(linked_workspace()), None),
            "/v1/graph/workspaces",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let arr: Value = serde_json::from_str(&body).unwrap();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 1, "a single workspace → auto-enter, no selector");
        assert!(
            arr[0]["projects"].as_array().unwrap().len() > 1,
            "a lone hub still routes to the cross-repo workspace view"
        );
    }
}
