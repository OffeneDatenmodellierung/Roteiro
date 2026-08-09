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

use crate::query::SCHEMA;
use crate::store::{Store, StoreError};
use crate::{Edge, Node};

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
/// neighbour's content signature.
fn edge_descriptor(edge: &Edge, direction: &str, neighbour: &str, neighbour_sig: u64) -> String {
    let confidence = edge
        .confidence
        .map_or_else(String::new, |c| format!("{c:.6}"));
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
        let fresh = store
            .context_cache_get(&key)?
            .is_some_and(|(fp, _)| fp == fingerprint);
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
    use super::{build_context, context, dependents, refresh_contexts};
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
