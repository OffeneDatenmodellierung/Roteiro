//! Read-only JSON HTTP API over the workspace graph (`/v1/graph/*`).
//!
//! The interactive workspace explorer's **data foundation** (PR 1/5): a small,
//! side-effect-free axum router that surfaces the graph a `roteiro serve` process
//! already holds in memory as an [`Arc<Workspace>`]. Every route is a `GET`
//! returning JSON — no mutation, no model, no llama.cpp.
//!
//! It mirrors [`rto_render::mcp::mcp_router`]: a `router(workspace)` builder that
//! bakes the shared [`Workspace`] into axum state and is merged into the main app
//! alongside `/v1` and `/mcp` (see `serve_v1_tail` in `main.rs`). It lives in the
//! `roteiro` binary — not `rto-render` — because two routes reuse binary-local
//! code: the override matrix reuses [`crate::overview::build`], and the cross-repo
//! views reconstruct the persisted external-ref edges the workspace resolver
//! walks. Keeping the API here avoids relocating that code across crate
//! boundaries just to satisfy a router builder.
//!
//! Cross-repo semantics are read straight from the stores (so the API is fully
//! testable over an in-memory [`Workspace`], with no config-file scan): an
//! inferred **external-ref** edge that still resolves to its hub node is a
//! *match*; one whose hub node is gone is *drift* — the same "orphan" the
//! `/resolve` route reports as `{ "drift": true, "target": null }`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rto_graph::{
    EXTERNAL_REF_KIND, Edge, Node, NodeKind, Provenance, Store, StoreError, Workspace,
    WorkspaceError, debt, explain, external_ref_target, parse_qualified,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::overview;

/// The shared, read-only workspace handle every handler queries.
type Ws = Arc<Workspace>;

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

/// Build the read-only `/v1/graph/*` router over a shared [`Workspace`].
///
/// Merge it into the main axum app the same way the MCP router is merged, so the
/// explorer API, the model endpoint (`/v1`) and the MCP server share one port and
/// one workspace.
pub fn router(workspace: Ws) -> Router {
    Router::new()
        .route("/v1/graph/projects", get(projects))
        .route("/v1/graph/topology", get(topology))
        .route("/v1/graph/matrix", get(matrix))
        .route("/v1/graph/resolve", get(resolve))
        .route("/v1/graph/{project}", get(project_graph))
        .route("/v1/graph/{project}/nodes", get(project_nodes))
        .route("/v1/graph/{project}/node/{*key}", get(node_detail))
        .route(
            "/v1/graph/{project}/neighbourhood/{*key}",
            get(neighbourhood),
        )
        .route("/v1/graph/{project}/debt", get(project_debt))
        .route("/v1/graph/{project}/hotspots", get(hotspots))
        .with_state(workspace)
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

/// A handler failure, carrying the HTTP status it maps to. Rendered as
/// `{ "error": "<message>" }`.
enum ApiError {
    /// 400 — a malformed or missing query parameter.
    BadRequest(String),
    /// 404 — an unknown project, node, or key.
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
            // The named project isn't hosted, or it has no graph yet → 404.
            WorkspaceError::UnknownProject { .. } | WorkspaceError::NoGraph { .. } => {
                ApiError::NotFound(e.to_string())
            }
            // The caller's input was malformed or under-specified → 400.
            WorkspaceError::Unqualified { .. }
            | WorkspaceError::AmbiguousProject { .. }
            | WorkspaceError::Empty => ApiError::BadRequest(e.to_string()),
            // Poisoned lock, git, store, prepare-hook failures → 500.
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

/// `GET /v1/graph/projects` → the hosted project names and whether more than one
/// is hosted (so a client knows to offer project selection).
async fn projects(State(ws): State<Ws>) -> ApiResult {
    Ok(Json(json!({
        "projects": ws.names(),
        "isMulti": ws.is_multi(),
    }))
    .into_response())
}

/// `GET /v1/graph/{project}` → the whole graph as `{ nodes, edges, counts }`.
async fn project_graph(State(ws): State<Ws>, Path(project): Path<String>) -> ApiResult {
    let facts = ws.with_store(Some(&project), Store::export_factset)??;
    let (nodes, edges) = (facts.nodes.len(), facts.edges.len());
    Ok(Json(json!({
        "nodes": facts.nodes,
        "edges": facts.edges,
        "counts": { "nodes": nodes, "edges": edges },
    }))
    .into_response())
}

/// `GET /v1/graph/{project}/nodes?kinds=&provenance=&q=&limit=&offset=` →
/// filtered, paged nodes plus the pre-paging `total`.
async fn project_nodes(
    State(ws): State<Ws>,
    Path(project): Path<String>,
    Query(p): Query<NodesQuery>,
) -> ApiResult {
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

    let mut nodes = ws.with_store(Some(&project), Store::all_nodes)??;
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

/// `GET /v1/graph/{project}/node/{key}` → the node plus its in/out edges
/// (`query::explain`). 404 when the key is unknown.
async fn node_detail(
    State(ws): State<Ws>,
    Path((project, key)): Path<(String, String)>,
) -> ApiResult {
    let explanation = ws.with_store(Some(&project), |s| explain(s, &key))??;
    match explanation {
        Some(e) => Ok(Json(e).into_response()),
        None => Err(ApiError::NotFound(format!(
            "no node `{key}` in project `{project}`"
        ))),
    }
}

/// `GET /v1/graph/{project}/neighbourhood/{key}?depth=1` → the subgraph within
/// `depth` hops of `key`. 404 when the root key is unknown.
async fn neighbourhood(
    State(ws): State<Ws>,
    Path((project, key)): Path<(String, String)>,
    Query(dq): Query<DepthQuery>,
) -> ApiResult {
    let depth = dq.depth.unwrap_or(1).min(MAX_DEPTH);
    let sub = ws.with_store(Some(&project), |s| neighbourhood_subgraph(s, &key, depth))??;
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

/// `GET /v1/graph/{project}/debt` → the intent-debt report (`query::debt`).
async fn project_debt(State(ws): State<Ws>, Path(project): Path<String>) -> ApiResult {
    let report = ws.with_store(Some(&project), |s| debt(s, &[], &[]))??;
    Ok(Json(report).into_response())
}

/// `GET /v1/graph/{project}/hotspots?limit=` → the top-`limit` nodes by total
/// degree (in + out edges).
async fn hotspots(
    State(ws): State<Ws>,
    Path(project): Path<String>,
    Query(lq): Query<LimitQuery>,
) -> ApiResult {
    let limit = lq.limit.unwrap_or(DEFAULT_HOTSPOTS);
    let ranked = ws.with_store(Some(&project), |s| compute_hotspots(s, limit))??;
    Ok(Json(json!({ "hotspots": ranked, "limit": limit })).into_response())
}

/// `GET /v1/graph/resolve?qualified=<project>::<key>` → the target node and a
/// `drift` flag: `{ target: null, drift: true }` when the key is well-formed but
/// its node is gone (a removed or renamed cross-repo target).
async fn resolve(State(ws): State<Ws>, Query(rq): Query<ResolveQuery>) -> ApiResult {
    let qualified = rq
        .qualified
        .ok_or_else(|| ApiError::BadRequest("missing `qualified` query parameter".to_owned()))?;
    let target = ws.resolve_qualified(&qualified)?;
    let drift = target.is_none();
    Ok(Json(json!({ "target": target, "drift": drift })).into_response())
}

/// `GET /v1/graph/topology` → the cross-repo hub-and-spoke shape: the hub project,
/// a summary per spoke (`keyCount`, `driftCount`), and the inferred cross-repo
/// links (`from`/`to` node keys, provenance, confidence).
async fn topology(State(ws): State<Ws>) -> ApiResult {
    let names = ws.names();
    let hub = determine_hub(&ws, &names)?;

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
            confidence,
            ..
        } in &refs
        {
            if let Some(target) = external_ref_target(node) {
                links.push(json!({
                    "from": format!("{name}::{src}"),
                    "to": target,
                    "provenance": "inferred",
                    "confidence": confidence,
                }));
            }
            if ws.follow_external_ref(node)?.is_none() {
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

/// `GET /v1/graph/matrix` → the cross-repo config override matrix + drift
/// ([`overview::OverrideMatrix`]), reconstructed from the persisted external-ref
/// edges: a resolving edge is an override cell, a dangling one is drift.
async fn matrix(State(ws): State<Ws>) -> ApiResult {
    let names = ws.names();
    let Some(hub) = determine_hub(&ws, &names)? else {
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
            confidence,
        } in &refs
        {
            let (spoke_key, spoke_value) = spoke_cfg
                .get(src)
                .cloned()
                .unwrap_or_else(|| (src.clone(), String::new()));
            match ws.follow_external_ref(node)? {
                // Resolves to its hub node → a real override cell.
                Some(hub_node) => matches.push(overview::MatchInput {
                    hub_key: hub_node.name,
                    spoke_key,
                    spoke_value,
                    confidence: confidence.unwrap_or(0.0),
                }),
                // Hub node gone → drift (an orphan spoke key).
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
    /// The inferred edge's confidence.
    confidence: Option<f64>,
}

/// The inferred external-ref links persisted in `store` (ADR-0009): every
/// external-ref placeholder node and the inferred edge that points at it.
fn external_refs(store: &Store) -> Result<Vec<ExternalRef>, StoreError> {
    let placeholders = store.nodes_by_kind(&NodeKind::Other(EXTERNAL_REF_KIND.to_owned()))?;
    let mut out = Vec::new();
    for node in placeholders {
        for edge in store.edges_to(&node.key)? {
            if edge.provenance == Provenance::Inferred {
                out.push(ExternalRef {
                    src: edge.src,
                    node: node.clone(),
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

/// The hub project: the one most external-ref edges across the workspace point
/// into. `None` when nothing references anything (a single-repo or unlinked
/// workspace).
fn determine_hub(ws: &Workspace, names: &[String]) -> Result<Option<String>, ApiError> {
    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        for ExternalRef { node, .. } in ws.with_store(Some(name), external_refs)?? {
            if let Some(qualified) = external_ref_target(&node)
                && let Some((project, _)) = parse_qualified(&qualified)
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

    // -- synthetic in-memory workspace ------------------------------------

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

    fn apply(mut store: Store, facts: &FactSet) -> Store {
        store.apply_factset(facts).expect("apply factset");
        store
    }

    /// A two-project workspace (hub + spoke) over pre-opened in-memory stores.
    fn workspace() -> Ws {
        Arc::new(Workspace::from_stores([
            (HUB.to_owned(), hub_store()),
            (SPOKE.to_owned(), spoke_store()),
        ]))
    }

    /// A single-project workspace, for the routes that don't need cross-repo data.
    fn single_workspace() -> Ws {
        Arc::new(Workspace::single(HUB, hub_store()))
    }

    async fn get(ws: Ws, uri: &str) -> (StatusCode, Value) {
        let resp = router(ws)
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

    // -- tests -------------------------------------------------------------

    #[tokio::test]
    async fn projects_reports_names_and_multiplicity() {
        let (status, json) = get(workspace(), "/v1/graph/projects").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["isMulti"], true);
        let names: Vec<String> =
            serde_json::from_value(json["projects"].clone()).expect("projects array");
        assert!(names.contains(&HUB.to_owned()) && names.contains(&SPOKE.to_owned()));

        let (_, single) = get(single_workspace(), "/v1/graph/projects").await;
        assert_eq!(single["isMulti"], false);
    }

    #[tokio::test]
    async fn project_graph_returns_nodes_edges_and_counts() {
        let (status, json) = get(single_workspace(), "/v1/graph/hub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["counts"]["nodes"], 4);
        assert_eq!(json["counts"]["edges"], 1);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn unknown_project_is_404() {
        let (status, _) = get(single_workspace(), "/v1/graph/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn nodes_filter_by_kind_and_page() {
        // Two `fn` nodes in the hub; filter to them, then page one at a time.
        let (status, all) = get(single_workspace(), "/v1/graph/hub/nodes?kinds=fn").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all["total"], 2);
        assert_eq!(all["nodes"].as_array().unwrap().len(), 2);

        let (_, page) = get(
            single_workspace(),
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
        let (_, json) = get(single_workspace(), "/v1/graph/hub/nodes?q=help").await;
        assert_eq!(json["total"], 1);
        assert_eq!(json["nodes"][0]["name"], "helper");
    }

    #[tokio::test]
    async fn nodes_bad_provenance_is_400() {
        let (status, _) = get(single_workspace(), "/v1/graph/hub/nodes?provenance=bogus").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn node_detail_explains_and_404s_unknown_key() {
        let (status, json) = get(single_workspace(), "/v1/graph/hub/node/sym:main").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["node"]["key"], "sym:main");
        // `main` calls `helper` → one outgoing edge.
        assert_eq!(json["outgoing"].as_array().unwrap().len(), 1);

        let (missing, _) = get(single_workspace(), "/v1/graph/hub/node/sym:ghost").await;
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn neighbourhood_returns_root_and_neighbour() {
        let (status, json) = get(single_workspace(), "/v1/graph/hub/neighbourhood/sym:main").await;
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
        let (status, json) = get(single_workspace(), "/v1/graph/hub/debt").await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["schema"].is_string());
        assert!(json["total"].is_number());
        assert!(json["items"].is_array());
    }

    #[tokio::test]
    async fn hotspots_ranks_by_degree() {
        let (status, json) = get(single_workspace(), "/v1/graph/hub/hotspots?limit=1").await;
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
            workspace(),
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
            workspace(),
            "/v1/graph/resolve?qualified=hub::cfgkey:config.toml%23serve.legacy",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["drift"], true);
        assert_eq!(json["target"], Value::Null);
    }

    #[tokio::test]
    async fn resolve_unqualified_key_is_400() {
        let (status, _) = get(workspace(), "/v1/graph/resolve?qualified=notqualified").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn topology_shows_hub_spokes_and_links() {
        let (status, json) = get(workspace(), "/v1/graph/topology").await;
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
        let (status, json) = get(workspace(), "/v1/graph/matrix").await;
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
}
