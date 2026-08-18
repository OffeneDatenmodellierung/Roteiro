//! Per-node **context bundles** with a dependency-aware cache.
//!
//! A node's *context* is the node plus its one-hop, provenance-labelled
//! neighbourhood — the same shape as [`crate::query::explain`], but **cached**
//! and **fingerprinted**. The fingerprint folds in the node's own content *and*
//! every neighbour's content signature, so a change to the node or to any of its
//! neighbours (callers, callees, referencing docs) moves the fingerprint and the
//! cached entry is rebuilt on the next read. This is the codegraph-style
//! "dirty-propagation" invalidation: because context reaches one hop out, a
//! changed symbol invalidates exactly its dependents' cached context.
//!
//! The cache is content-addressed (the fingerprint *is* the validity check), so
//! it needs no manual bookkeeping beyond pruning entries for deleted nodes. The
//! bundle itself is cheap to rebuild today; the cache slot is the durable place a
//! future, expensive per-node summary would live.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::store::{Store, StoreError};
use crate::{Edge, Node, SCHEMA};

/// A compact node summary within a [`NodeContext`]. Owned and round-trippable so
/// the whole bundle can be cached as JSON and read back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextNode {
    /// Natural key.
    pub key: String,
    /// Kind token (e.g. `fn`, `adr`).
    pub kind: String,
    /// Human-facing name.
    pub name: String,
    /// Repository-relative path, if any.
    pub path: Option<String>,
    /// Language token, if any.
    pub lang: Option<String>,
}

impl ContextNode {
    fn from_node(node: &Node) -> Self {
        Self {
            key: node.key.clone(),
            kind: node.kind.as_str().to_owned(),
            name: node.name.clone(),
            path: node.path.clone(),
            lang: node.lang.clone(),
        }
    }
}

/// One incident edge as seen from the subject: the relationship, its provenance,
/// and the node on the other end. Owned/round-trippable for caching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEdge {
    /// Edge kind token (e.g. `calls`, `references`).
    pub kind: String,
    /// How the edge was produced (`derived` | `authored` | `inferred`).
    pub provenance: String,
    /// Confidence score, present only for inferred edges.
    pub confidence: Option<f64>,
    /// The natural key of the node at the other end.
    pub node: String,
}

/// A node together with its one-hop neighbourhood and a validity `fingerprint`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeContext {
    /// Stable schema tag ([`crate::SCHEMA`]).
    pub schema: String,
    /// Fingerprint over the node's content and its neighbours' content; a change
    /// to either moves it, invalidating a cached entry.
    pub fingerprint: String,
    /// The subject node.
    pub node: ContextNode,
    /// Structured metadata attached to the node.
    pub meta: serde_json::Value,
    /// Edges where the subject is the source.
    pub outgoing: Vec<ContextEdge>,
    /// Edges where the subject is the destination.
    pub incoming: Vec<ContextEdge>,
}

/// Counts from a [`refresh_contexts`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ContextRefresh {
    /// Cached entries that were rebuilt because their fingerprint had changed
    /// (the node or a neighbour changed).
    pub rebuilt: usize,
    /// Cached entries that were still fresh and reused as-is.
    pub reused: usize,
    /// Stale entries pruned because their node no longer exists.
    pub pruned: usize,
}

/// FNV-1a (64-bit). Dependency-free and deterministic; used only to fold content
/// signatures into a fingerprint, so it needs no cryptographic properties.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A content signature for a node: its git blob hash when present (whole-content
/// identity), else a hash of its kind, name, and metadata (which includes any
/// captured `content`). Any change to what the node *means* moves this value.
fn node_signature(node: &Node) -> u64 {
    if let Some(blob) = &node.blob_hash {
        return fnv1a64(blob.as_bytes());
    }
    let mut s = String::new();
    s.push_str(node.kind.as_str());
    s.push('\u{0}');
    s.push_str(&node.name);
    s.push('\u{0}');
    // serde_json serialises object keys in sorted order, so this is stable.
    s.push_str(&serde_json::to_string(&node.meta).unwrap_or_default());
    fnv1a64(s.as_bytes())
}

/// A canonical, sortable descriptor of one incident edge for the fingerprint:
/// direction, kind, provenance, confidence, the neighbour's key, and the
/// neighbour's content signature. The confidence is captured by its exact bit
/// pattern, so any change to it — however small — moves the fingerprint.
fn edge_descriptor(edge: &Edge, direction: &str, neighbour: &str, neighbour_sig: u64) -> String {
    let confidence = edge
        .confidence
        .map_or_else(String::new, |c| format!("{:016x}", c.to_bits()));
    format!(
        "{direction}|{}|{}|{confidence}|{neighbour}|{neighbour_sig:016x}",
        edge.kind.as_str(),
        edge.provenance.as_str(),
    )
}

/// Compute the fingerprint of `node`'s context: its own signature plus a sorted
/// list of its incident edges' descriptors (each carrying the neighbour's
/// signature). Deterministic for a given graph state.
fn compute_fingerprint(store: &Store, node: &Node) -> Result<String, StoreError> {
    let mut descriptors: Vec<String> = Vec::new();
    for edge in store.edges_from(&node.key)? {
        let sig = store.get_node(&edge.dst)?.map_or(0, |n| node_signature(&n));
        descriptors.push(edge_descriptor(&edge, "out", &edge.dst, sig));
    }
    for edge in store.edges_to(&node.key)? {
        let sig = store.get_node(&edge.src)?.map_or(0, |n| node_signature(&n));
        descriptors.push(edge_descriptor(&edge, "in", &edge.src, sig));
    }
    // Sort so the fingerprint is independent of edge storage/query order.
    descriptors.sort();
    let mut buf = format!("ctx/v1|{:016x}", node_signature(node));
    for d in &descriptors {
        buf.push('\n');
        buf.push_str(d);
    }
    Ok(format!("{:016x}", fnv1a64(buf.as_bytes())))
}

fn out_ref(edge: &Edge) -> ContextEdge {
    ContextEdge {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str().to_owned(),
        confidence: edge.confidence,
        node: edge.dst.clone(),
    }
}

fn in_ref(edge: &Edge) -> ContextEdge {
    ContextEdge {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str().to_owned(),
        confidence: edge.confidence,
        node: edge.src.clone(),
    }
}

fn sort_refs(refs: &mut [ContextEdge]) {
    refs.sort_by(|a, b| (&a.kind, &a.node, &a.provenance).cmp(&(&b.kind, &b.node, &b.provenance)));
}

/// Assemble a fresh bundle for `node` from the current graph, with the given
/// `fingerprint`. Shared by [`build_context`] and the cache-miss path.
fn fresh_bundle(
    store: &Store,
    node: &Node,
    fingerprint: String,
) -> Result<NodeContext, StoreError> {
    let mut outgoing: Vec<ContextEdge> = store.edges_from(&node.key)?.iter().map(out_ref).collect();
    let mut incoming: Vec<ContextEdge> = store.edges_to(&node.key)?.iter().map(in_ref).collect();
    sort_refs(&mut outgoing);
    sort_refs(&mut incoming);
    Ok(NodeContext {
        schema: SCHEMA.to_owned(),
        fingerprint,
        node: ContextNode::from_node(node),
        meta: node.meta.clone(),
        outgoing,
        incoming,
    })
}

/// Build a node's context bundle from the current graph (ignoring the cache).
/// Returns `None` if no node has that key.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn build_context(store: &Store, key: &str) -> Result<Option<NodeContext>, StoreError> {
    let Some(node) = store.get_node(key)? else {
        return Ok(None);
    };
    let fingerprint = compute_fingerprint(store, &node)?;
    Ok(Some(fresh_bundle(store, &node, fingerprint)?))
}

/// Fetch a node's context through the cache: return the cached bundle when its
/// fingerprint still matches the current graph, otherwise rebuild it, store it,
/// and return the fresh bundle. Returns `None` (and prunes any stale entry) if
/// the node no longer exists.
///
/// # Errors
/// Returns [`StoreError`] on query failure, or if a cached entry cannot be
/// decoded.
pub fn context(store: &Store, key: &str) -> Result<Option<NodeContext>, StoreError> {
    let Some(node) = store.get_node(key)? else {
        store.context_cache_delete(key)?;
        return Ok(None);
    };
    let fingerprint = compute_fingerprint(store, &node)?;
    if let Some((cached_fp, json)) = store.context_cache_get(key)?
        && cached_fp == fingerprint
    {
        return Ok(Some(serde_json::from_str(&json)?));
    }
    // Miss or stale: rebuild from the current graph and cache it.
    let bundle = fresh_bundle(store, &node, fingerprint.clone())?;
    store.context_cache_put(key, &fingerprint, &serde_json::to_string(&bundle)?)?;
    Ok(Some(bundle))
}

/// The set of nodes whose cached context a change to any of `changed` would
/// invalidate: the changed nodes themselves plus their one-hop neighbours in
/// either direction (a node's context reaches exactly one hop out). This makes
/// the dependency-propagation contract explicit; [`refresh_contexts`] realises
/// it via fingerprints.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn dependents(store: &Store, changed: &[String]) -> Result<BTreeSet<String>, StoreError> {
    let mut set = BTreeSet::new();
    for key in changed {
        // A changed node's own context is dirty, and so is each neighbour's
        // (their context reaches one hop and includes this node). This holds even
        // if the node was deleted — its former neighbours are still reachable via
        // their edges to/from it.
        set.insert(key.clone());
        for edge in store.edges_from(key)? {
            set.insert(edge.dst);
        }
        for edge in store.edges_to(key)? {
            set.insert(edge.src);
        }
    }
    Ok(set)
}

/// The largest number of edges a [`tool_context`] bundle carries **per
/// direction**.
///
/// This surface has no `limit` parameter — a context bundle is one node's
/// neighbourhood, so the only honest argument is the node key — which means the
/// bound is fixed here rather than negotiated per call, and the tool's
/// description states this number because a model reads that even when it does
/// not read a schema.
///
/// Why a bound exists at all: `context` on a large file node is the biggest
/// answer this graph can produce for a single key. In this repository
/// `file:crates/roteiro/src/main.rs` returns 269 outgoing edges — 44,556 bytes of
/// pretty JSON, roughly 11k tokens — of which 244 are `defines`. The node's own
/// `meta` is already bounded (extraction caps captured content), so the edge
/// lists are the whole of the variance, and capping them is the whole of the fix.
///
/// 50 per direction holds the worst case near 18 KB while leaving every ADR,
/// every symbol and every ordinary file untouched — in this repository only file
/// nodes reach it at all.
pub const TOOL_CONTEXT_EDGE_CAP: usize = 50;

/// How many edges of one kind were left out of a truncated direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedEdges {
    /// Edge kind token (e.g. `defines`).
    pub kind: String,
    /// How many edges of that kind the bundle does not carry.
    pub omitted: usize,
}

/// One direction of a bounded bundle: the edges kept, and an exact account of
/// what was dropped to fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundedEdges {
    /// How many edges this node has in this direction, before the cap.
    pub total: usize,
    /// Whether `edges` is a subset of them.
    pub truncated: bool,
    /// What was dropped, by edge kind. Empty when `truncated` is false.
    ///
    /// This is the difference between a bounded answer and a misleading one: a
    /// model that asks what `main.rs` imports and is handed 50 of its 269 edges
    /// must be able to see that 23 `imports` edges exist, rather than conclude
    /// from their absence that there are none.
    pub omitted: Vec<OmittedEdges>,
    /// The edges carried, in the same order [`context`] and [`explain`](crate::explain)
    /// produce them.
    pub edges: Vec<ContextEdge>,
}

/// A node's context bundle, bounded for a model-facing tool surface.
///
/// The same shape as [`NodeContext`] with each edge list replaced by a
/// [`BoundedEdges`] that says how much of it you are looking at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolContext {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: String,
    /// Fingerprint over the node's content and its neighbours' content.
    pub fingerprint: String,
    /// The cap applied to each direction ([`TOOL_CONTEXT_EDGE_CAP`]).
    pub edge_cap: usize,
    /// Whether either direction was truncated — the one field a caller has to
    /// read to know the bundle is partial.
    pub truncated: bool,
    /// The subject node.
    pub node: ContextNode,
    /// Structured metadata attached to the node.
    pub meta: serde_json::Value,
    /// Edges where the subject is the source.
    pub outgoing: BoundedEdges,
    /// Edges where the subject is the destination.
    pub incoming: BoundedEdges,
}

/// Keep at most `cap` of `refs`, drawing **round-robin across edge kinds** so
/// every kind present survives, and account for the rest.
///
/// Plain truncation of the sorted list would be simpler and worse. The sort is
/// by `(kind, node, provenance)`, so on `file:…/main.rs` — `contains` 1,
/// `defines` 244, `imports` 23, `references` 1 — the first 50 are one `contains`
/// and 49 `defines`, and the bundle silently contains no `imports` or
/// `references` edge at all. Round-robin gives the small kinds their edges back;
/// `omitted` then says exactly how much of each large one is missing.
fn bound_edges(refs: Vec<ContextEdge>, cap: usize) -> BoundedEdges {
    let total = refs.len();
    if total <= cap {
        return BoundedEdges {
            total,
            truncated: false,
            omitted: Vec::new(),
            edges: refs,
        };
    }
    // `refs` is sorted by kind, so equal kinds are already adjacent: walk it once
    // into per-kind queues, then deal one from each in turn until the cap is met.
    let mut by_kind: Vec<(String, Vec<ContextEdge>)> = Vec::new();
    for r in refs {
        match by_kind.last_mut() {
            Some((kind, group)) if *kind == r.kind => group.push(r),
            _ => by_kind.push((r.kind.clone(), vec![r])),
        }
    }
    let mut kept: Vec<ContextEdge> = Vec::with_capacity(cap);
    let mut round = 0usize;
    while kept.len() < cap {
        let mut dealt = false;
        for (_, group) in &by_kind {
            if kept.len() == cap {
                break;
            }
            if round < group.len() {
                kept.push(group[round].clone());
                dealt = true;
            }
        }
        // Every queue is exhausted — impossible while `total > cap`, but a `while`
        // that can only end on a counter is a hang waiting for a future change.
        if !dealt {
            break;
        }
        round += 1;
    }
    let omitted: Vec<OmittedEdges> = by_kind
        .iter()
        .filter_map(|(kind, group)| {
            let carried = kept.iter().filter(|e| e.kind == *kind).count();
            (group.len() > carried).then(|| OmittedEdges {
                kind: kind.clone(),
                omitted: group.len() - carried,
            })
        })
        .collect();
    // Restore the canonical order, so a bounded bundle reads like a whole one.
    sort_refs(&mut kept);
    BoundedEdges {
        total,
        truncated: true,
        omitted,
        edges: kept,
    }
}

/// A node's context bundle for a model-facing tool surface: **read-only** and
/// **bounded**. Returns `None` if no node has that key.
///
/// # Read-only, deliberately
///
/// This builds on [`build_context`] and not on [`context`]. `context` is the
/// cached read, and the cache is not free of side effects: a hit writes nothing
/// but a miss calls `context_cache_put`, and a key whose node has been deleted
/// calls `context_cache_delete` — a prune. Pruning is the *maintenance* half of
/// `roteiro context --refresh`, which exists so that ordinary reads never mutate
/// the store (ADR-0013), and a tool surface is the last place it should reappear.
/// `build_context` assembles the identical bundle from the live graph and touches
/// nothing, which is the whole difference and the reason for the choice.
///
/// # Bounded, and it says so
///
/// Each direction is capped at [`TOOL_CONTEXT_EDGE_CAP`] and the result carries
/// `truncated`, each direction's `total`, and per-kind `omitted` counts. A
/// shortened bundle that did not say it was shortened would be the same defect as
/// a `limit` that silently returns nothing (issue #393): an answer a caller
/// cannot tell from a complete one.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn tool_context(store: &Store, key: &str) -> Result<Option<ToolContext>, StoreError> {
    let Some(bundle) = build_context(store, key)? else {
        return Ok(None);
    };
    let outgoing = bound_edges(bundle.outgoing, TOOL_CONTEXT_EDGE_CAP);
    let incoming = bound_edges(bundle.incoming, TOOL_CONTEXT_EDGE_CAP);
    Ok(Some(ToolContext {
        schema: bundle.schema,
        fingerprint: bundle.fingerprint,
        edge_cap: TOOL_CONTEXT_EDGE_CAP,
        truncated: outgoing.truncated || incoming.truncated,
        node: bundle.node,
        meta: bundle.meta,
        outgoing,
        incoming,
    }))
}

/// Refresh every cached context that has gone stale (its node or a neighbour
/// changed) and prune entries whose node no longer exists. Only existing nodes
/// that already have a cache entry are considered — this reconciles the cache
/// with the current graph without eagerly materialising context for every node.
///
/// # Errors
/// Returns [`StoreError`] on query failure or if a cached entry cannot be
/// decoded.
pub fn refresh_contexts(store: &Store) -> Result<ContextRefresh, StoreError> {
    let mut out = ContextRefresh {
        rebuilt: 0,
        reused: 0,
        pruned: 0,
    };
    for key in store.context_cache_keys()? {
        let Some(node) = store.get_node(&key)? else {
            store.context_cache_delete(&key)?;
            out.pruned += 1;
            continue;
        };
        let fingerprint = compute_fingerprint(store, &node)?;
        // Only the fingerprint is needed to decide freshness — avoid reading the
        // full cached JSON payload for entries that turn out to be fresh.
        let fresh = store
            .context_cache_fingerprint(&key)?
            .is_some_and(|fp| fp == fingerprint);
        if fresh {
            out.reused += 1;
        } else {
            context(store, &key)?; // rebuilds and stores
            out.rebuilt += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        TOOL_CONTEXT_EDGE_CAP, build_context, context, dependents, refresh_contexts, tool_context,
    };
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    /// A store: doc --references--> caller --calls--> callee.
    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let mut caller = Node::new("sym:rust:a.rs#caller", NodeKind::Fn, "caller");
        caller.blob_hash = Some("BLOB_A".to_owned());
        let mut target = Node::new("sym:rust:a.rs#callee", NodeKind::Fn, "callee");
        target.blob_hash = Some("BLOB_A".to_owned());
        let mut doc = Node::new("file:docs/g.md", NodeKind::Doc, "g.md");
        doc.blob_hash = Some("BLOB_DOC".to_owned());
        let facts = FactSet::new()
            .with_node(caller)
            .with_node(target)
            .with_node(doc)
            .with_edge(Edge::derived(
                "sym:rust:a.rs#caller",
                "sym:rust:a.rs#callee",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::authored(
                "file:docs/g.md",
                "sym:rust:a.rs#caller",
                EdgeKind::References,
            ));
        store.apply_factset(&facts).expect("apply");
        store
    }

    /// A store with `count` symbols of each of three edge kinds hanging off one
    /// file node — the shape a large source file has, at a size that trips the cap.
    fn wide(count: usize) -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let mut facts =
            FactSet::new().with_node(Node::new("file:wide.rs", NodeKind::File, "wide.rs"));
        // Deliberately lopsided, as a real file is: many `defines`, few of the
        // rest. Sorted order puts every `defines` before every `imports`, which is
        // what plain truncation would drop whole.
        for (kind, n) in [
            (EdgeKind::Defines, count),
            (EdgeKind::Imports, 3),
            (EdgeKind::References, 2),
        ] {
            for i in 0..n {
                let key = format!("sym:rust:wide.rs#{}{i:04}", kind.as_str());
                facts = facts
                    .with_node(Node::new(key.clone(), NodeKind::Fn, "s"))
                    .with_edge(Edge::derived("file:wide.rs", key, kind.clone()));
            }
        }
        store.apply_factset(&facts).expect("apply");
        store
    }

    #[test]
    fn a_small_bundle_is_carried_whole_and_says_it_was_not_truncated() {
        let store = seeded();
        let out = tool_context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert!(!out.truncated);
        assert_eq!(out.edge_cap, TOOL_CONTEXT_EDGE_CAP);
        assert_eq!(out.outgoing.total, 1);
        assert!(!out.outgoing.truncated);
        assert!(out.outgoing.omitted.is_empty());
        assert_eq!(out.outgoing.edges.len(), 1);
        assert_eq!(out.incoming.total, 1);
        assert_eq!(out.incoming.edges.len(), 1);
        // Identical to the unbounded bundle it wraps, edge for edge.
        let whole = build_context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert_eq!(out.outgoing.edges, whole.outgoing);
        assert_eq!(out.incoming.edges, whole.incoming);
        assert_eq!(out.fingerprint, whole.fingerprint);
    }

    /// The bound, and the accounting that keeps it from being a silent answer.
    #[test]
    fn a_truncated_bundle_reports_the_cap_the_totals_and_what_it_dropped() {
        let store = wide(200);
        let out = tool_context(&store, "file:wide.rs")
            .expect("ctx")
            .expect("present");

        assert!(out.truncated, "205 edges must not fit under the cap");
        assert_eq!(out.outgoing.total, 205, "the count before the cap");
        assert!(out.outgoing.truncated);
        assert_eq!(out.outgoing.edges.len(), TOOL_CONTEXT_EDGE_CAP);
        // The dropped edges are accounted for by kind, and the numbers reconcile:
        // total = carried + omitted.
        let omitted: usize = out.outgoing.omitted.iter().map(|o| o.omitted).sum();
        assert_eq!(omitted + out.outgoing.edges.len(), out.outgoing.total);
        assert_eq!(
            out.outgoing
                .omitted
                .iter()
                .find(|o| o.kind == "defines")
                .map(|o| o.omitted),
            // Round-robin fills the two small kinds first (3 + 2), so the cap
            // leaves `cap - 5` of the 200 `defines`. Written as a saturating
            // expression so raising the cap fails this test rather than failing
            // to compile it.
            Some(205usize.saturating_sub(TOOL_CONTEXT_EDGE_CAP)),
            "{:?}",
            out.outgoing.omitted
        );

        // The other direction is untouched and says so.
        assert_eq!(out.incoming.total, 0);
        assert!(!out.incoming.truncated);
    }

    /// Round-robin, not head-of-list. Sorted order is `defines` … then `imports`
    /// then `references`, so a plain `truncate(50)` would hand back 50 `defines`
    /// and let a model conclude the file imports nothing. Every kind present must
    /// survive the cap.
    #[test]
    fn truncation_keeps_every_edge_kind_rather_than_the_first_fifty() {
        let store = wide(200);
        let out = tool_context(&store, "file:wide.rs")
            .expect("ctx")
            .expect("present");
        for kind in ["defines", "imports", "references"] {
            assert!(
                out.outgoing.edges.iter().any(|e| e.kind == kind),
                "`{kind}` must survive truncation: {:?}",
                out.outgoing
                    .edges
                    .iter()
                    .map(|e| &e.kind)
                    .collect::<std::collections::BTreeSet<_>>()
            );
        }
        // The small kinds fit entirely, so they are not in `omitted` at all.
        assert!(
            !out.outgoing
                .omitted
                .iter()
                .any(|o| o.kind == "imports" || o.kind == "references"),
            "{:?}",
            out.outgoing.omitted
        );
        // And the kept edges are still in canonical order.
        let mut sorted = out.outgoing.edges.clone();
        super::sort_refs(&mut sorted);
        assert_eq!(sorted, out.outgoing.edges);
    }

    /// The read-only contract. `context` writes a cache entry on a miss and
    /// *prunes* one for a deleted node; `tool_context` must do neither, so a tool
    /// call never mutates the store (ADR-0013).
    #[test]
    fn tool_context_never_writes_to_the_cache() {
        let store = seeded();
        assert!(store.context_cache_keys().unwrap().is_empty());

        tool_context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert!(
            store.context_cache_keys().unwrap().is_empty(),
            "a tool read must not populate the cache",
        );

        // And a hit on a key with a *stale* cache entry must not prune it: pruning
        // is `roteiro context --refresh`'s maintenance, not a read's.
        store
            .context_cache_put("sym:rust:a.rs#ghost", "stale-fingerprint", "{}")
            .expect("put");
        let missing = tool_context(&store, "sym:rust:a.rs#ghost").expect("ctx");
        assert!(missing.is_none(), "no such node");
        assert_eq!(
            store.context_cache_keys().unwrap(),
            vec!["sym:rust:a.rs#ghost".to_owned()],
            "a missing node must not prune its cache entry",
        );
    }

    #[test]
    fn context_is_cached_then_served_from_cache() {
        let store = seeded();
        // First read is a miss: it populates the cache.
        let first = context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert_eq!(first.node.key, "sym:rust:a.rs#caller");
        assert_eq!(first.outgoing.len(), 1, "calls callee");
        assert_eq!(first.incoming.len(), 1, "referenced by doc");
        assert_eq!(
            store
                .context_cache_get("sym:rust:a.rs#caller")
                .expect("get")
                .expect("cached")
                .0,
            first.fingerprint,
        );
        // Second read returns the identical bundle from the cache.
        let second = context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert_eq!(first, second);
    }

    #[test]
    fn changing_a_dependency_invalidates_dependent_context() {
        let store = seeded();
        // Warm the cache for all three nodes.
        let before = refresh_first_read(&store);
        assert_eq!(before.rebuilt, 0, "warming reads are misses, not refreshes");

        // The caller's cached fingerprint before the change.
        let caller_fp_before = context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present")
            .fingerprint;

        // Change the *callee*'s content (new blob). The caller depends on it.
        let mut target = store
            .get_node("sym:rust:a.rs#callee")
            .expect("get")
            .expect("present");
        target.blob_hash = Some("BLOB_A2".to_owned());
        store.upsert_node(&target).expect("upsert");

        // `dependents` names exactly who should be dirtied: the callee and its
        // neighbour, the caller.
        let deps = dependents(&store, &["sym:rust:a.rs#callee".to_owned()]).expect("deps");
        assert!(deps.contains("sym:rust:a.rs#caller"));

        // The caller's fingerprint has moved (its neighbour changed), so a fresh
        // read rebuilds it — its cached context is invalidated.
        let caller_fp_after = context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present")
            .fingerprint;
        assert_ne!(
            caller_fp_before, caller_fp_after,
            "dependent context must be invalidated when a dependency changes",
        );

        // The unrelated doc did not change and is not a dependent of the callee,
        // so a refresh reuses it while rebuilding the affected nodes.
        let report = refresh_contexts(&store).expect("refresh");
        assert!(report.rebuilt >= 1, "affected contexts rebuilt");
        assert_eq!(report.pruned, 0);
    }

    /// Read context for every node once, warming the cache; returns a refresh
    /// report taken immediately after (which should show everything fresh).
    fn refresh_first_read(store: &Store) -> super::ContextRefresh {
        for key in store.all_keys().expect("keys") {
            context(store, &key).expect("ctx");
        }
        refresh_contexts(store).expect("refresh")
    }

    #[test]
    fn deleted_node_context_is_pruned() {
        let mut store = seeded();
        context(&store, "file:docs/g.md")
            .expect("ctx")
            .expect("present");
        assert!(
            store
                .context_cache_get("file:docs/g.md")
                .expect("get")
                .is_some()
        );
        // Rebuild the graph without the doc node.
        let mut caller = Node::new("sym:rust:a.rs#caller", NodeKind::Fn, "caller");
        caller.blob_hash = Some("BLOB_A".to_owned());
        store
            .rebuild(&FactSet::new().with_node(caller), None)
            .expect("rebuild");
        // The cache entry survives rebuild but is pruned on refresh; a direct read
        // also returns None and clears it.
        assert!(context(&store, "file:docs/g.md").expect("ctx").is_none());
        assert!(
            store
                .context_cache_get("file:docs/g.md")
                .expect("get")
                .is_none()
        );
    }

    #[test]
    fn build_context_matches_cached() {
        let store = seeded();
        let built = build_context(&store, "sym:rust:a.rs#caller")
            .expect("build")
            .expect("present");
        let cached = context(&store, "sym:rust:a.rs#caller")
            .expect("ctx")
            .expect("present");
        assert_eq!(built, cached);
        assert!(
            build_context(&store, "sym:rust:a.rs#ghost")
                .expect("b")
                .is_none()
        );
    }
}
