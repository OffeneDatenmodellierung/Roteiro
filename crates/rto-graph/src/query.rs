//! The agent- and human-facing query surface over the graph.
//!
//! Everything here is a read-only view built from the store's typed queries,
//! serialised under a **stable, versioned** JSON schema ([`SCHEMA`]) so agents
//! can depend on the shape. Three primitives are provided: [`explain`] (a node
//! and its provenance-labelled neighbourhood), [`list_kind`] (all nodes of a
//! kind), and [`path`] (a shortest path between two nodes). All return
//! mixed-provenance results — the "one query surface" from ADR-0001 — with every
//! edge carrying its `provenance`.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use crate::store::{Store, StoreError};
use crate::{Edge, NodeKind};

/// The versioned schema tag emitted on every query result. Bump the version on
/// any breaking change to the shape.
pub const SCHEMA: &str = "roteiro.query/v1";

/// A compact node summary (used in listings and as the subject of an
/// [`Explanation`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeSummary {
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

impl NodeSummary {
    fn from_node(node: &crate::Node) -> Self {
        Self {
            key: node.key.clone(),
            kind: node.kind.as_str().to_owned(),
            name: node.name.clone(),
            path: node.path.clone(),
            lang: node.lang.clone(),
        }
    }
}

/// One end of an edge as seen from a subject node: the relationship, how it was
/// produced, and the node on the other end.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRef {
    /// Edge kind token (e.g. `calls`, `references`).
    pub kind: String,
    /// How the edge was produced (`derived` | `authored` | `inferred`).
    pub provenance: &'static str,
    /// Confidence score, present only for inferred edges.
    pub confidence: Option<f64>,
    /// The natural key of the node at the other end.
    pub node: String,
}

/// A node together with its provenance-labelled neighbourhood.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The subject node.
    pub node: NodeSummary,
    /// Structured metadata attached to the node.
    pub meta: serde_json::Value,
    /// Edges where the subject is the source.
    pub outgoing: Vec<EdgeRef>,
    /// Edges where the subject is the destination.
    pub incoming: Vec<EdgeRef>,
}

/// A listing of all nodes of one kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The kind that was listed.
    pub kind: String,
    /// Matching nodes, ordered by key.
    pub nodes: Vec<NodeSummary>,
}

/// One step along a [`Path`]: the edge traversed and the node it leads to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathHop {
    /// Edge kind token (e.g. `calls`, `contains`).
    pub kind: String,
    /// How the edge was produced.
    pub provenance: &'static str,
    /// Confidence score, present only for inferred edges.
    pub confidence: Option<f64>,
    /// The direction the edge was traversed relative to the previous node
    /// (`outgoing` = along the edge, `incoming` = against it).
    pub direction: &'static str,
    /// The natural key of the node this hop arrives at.
    pub node: String,
}

/// A shortest path between two nodes. Edges are followed in either direction
/// (the graph is treated as undirected for reachability), and each hop records
/// the actual direction and provenance of the edge used.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Path {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// Natural key of the start node.
    pub from: String,
    /// Natural key of the goal node.
    pub to: String,
    /// Whether a path (including the trivial empty one) was found.
    pub found: bool,
    /// Number of hops (edges) in the path; `0` when `from == to`.
    pub length: usize,
    /// The hops from `from` to `to`, in order.
    pub hops: Vec<PathHop>,
}

fn out_ref(edge: &Edge) -> EdgeRef {
    EdgeRef {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        node: edge.dst.clone(),
    }
}

fn in_ref(edge: &Edge) -> EdgeRef {
    EdgeRef {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        node: edge.src.clone(),
    }
}

fn sort_refs(refs: &mut [EdgeRef]) {
    // Include provenance so edges differing only in provenance have a total,
    // stable order; with the edge-uniqueness constraint this key is unique.
    refs.sort_by(|a, b| (&a.kind, &a.node, a.provenance).cmp(&(&b.kind, &b.node, b.provenance)));
}

/// Explain a node: its record plus every incoming and outgoing edge, each
/// labelled with provenance. Returns `None` if no node has that key.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn explain(store: &Store, key: &str) -> Result<Option<Explanation>, StoreError> {
    let Some(node) = store.get_node(key)? else {
        return Ok(None);
    };
    let mut outgoing: Vec<EdgeRef> = store.edges_from(key)?.iter().map(out_ref).collect();
    let mut incoming: Vec<EdgeRef> = store.edges_to(key)?.iter().map(in_ref).collect();
    sort_refs(&mut outgoing);
    sort_refs(&mut incoming);
    Ok(Some(Explanation {
        schema: SCHEMA,
        node: NodeSummary::from_node(&node),
        meta: node.meta,
        outgoing,
        incoming,
    }))
}

/// List every node of the given `kind`, ordered by key.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn list_kind(store: &Store, kind: &NodeKind) -> Result<Listing, StoreError> {
    let nodes = store
        .nodes_by_kind(kind)?
        .iter()
        .map(NodeSummary::from_node)
        .collect();
    Ok(Listing {
        schema: SCHEMA,
        kind: kind.as_str().to_owned(),
        nodes,
    })
}

/// A candidate step out of a node during traversal: the edge used and the node
/// on the other end. Ordered so BFS expansion is deterministic.
struct Step {
    node: String,
    hop: PathHop,
}

/// All one-hop steps out of `key`, following edges in either direction, sorted
/// for deterministic traversal.
fn steps_from(store: &Store, key: &str) -> Result<Vec<Step>, StoreError> {
    let mut steps = Vec::new();
    for edge in store.edges_from(key)? {
        steps.push(Step {
            node: edge.dst.clone(),
            hop: hop(&edge, "outgoing", edge.dst.clone()),
        });
    }
    for edge in store.edges_to(key)? {
        steps.push(Step {
            node: edge.src.clone(),
            hop: hop(&edge, "incoming", edge.src.clone()),
        });
    }
    steps.sort_by(|a, b| {
        (&a.node, &a.hop.kind, a.hop.provenance, a.hop.direction).cmp(&(
            &b.node,
            &b.hop.kind,
            b.hop.provenance,
            b.hop.direction,
        ))
    });
    Ok(steps)
}

fn hop(edge: &Edge, direction: &'static str, node: String) -> PathHop {
    PathHop {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        direction,
        node,
    }
}

/// Find a shortest path from `from` to `to`, following edges in either
/// direction. Returns a [`Path`] with `found = false` (and no hops) if either
/// endpoint is absent or `to` is unreachable; `from == to` yields the trivial
/// zero-length path.
///
/// The search is breadth-first with deterministic neighbour ordering, so the
/// returned path is stable for a given graph.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn path(store: &Store, from: &str, to: &str) -> Result<Path, StoreError> {
    let not_found = |found: bool, hops: Vec<PathHop>| Path {
        schema: SCHEMA,
        from: from.to_owned(),
        to: to.to_owned(),
        found,
        length: hops.len(),
        hops,
    };

    // Both endpoints must exist in the graph.
    if store.get_node(from)?.is_none() || store.get_node(to)?.is_none() {
        return Ok(not_found(false, Vec::new()));
    }
    if from == to {
        return Ok(not_found(true, Vec::new()));
    }

    // BFS, recording for each visited node the (predecessor, hop) that reached
    // it so the path can be reconstructed.
    let mut came_from: BTreeMap<String, (String, PathHop)> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(from.to_owned());
    came_from.insert(from.to_owned(), (String::new(), placeholder_hop()));

    while let Some(current) = queue.pop_front() {
        if current == to {
            break;
        }
        for step in steps_from(store, &current)? {
            if came_from.contains_key(&step.node) {
                continue;
            }
            came_from.insert(step.node.clone(), (current.clone(), step.hop));
            queue.push_back(step.node);
        }
    }

    // Walk predecessors back from `to` to `from`, then reverse. Every node in
    // `came_from` other than `from` has a real predecessor, so this terminates
    // at `from`. If the chain is ever broken (an invariant violation), treat it
    // as no path rather than silently returning a partial one.
    let mut hops = Vec::new();
    let mut cursor = to.to_owned();
    while cursor != from {
        let Some((prev, hop)) = came_from.get(&cursor) else {
            return Ok(not_found(false, Vec::new()));
        };
        hops.push(hop.clone());
        cursor = prev.clone();
    }
    hops.reverse();
    Ok(not_found(true, hops))
}

/// A sentinel hop for the BFS start node (never emitted in a result).
fn placeholder_hop() -> PathHop {
    PathHop {
        kind: String::new(),
        provenance: "derived",
        confidence: None,
        direction: "outgoing",
        node: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{SCHEMA, explain, list_kind, path};
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_node(Node::new("adr:0001", NodeKind::Adr, "Build Roteiro"))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::authored(
                "adr:0001",
                "sym:rust:a.rs#main",
                EdgeKind::References,
            ));
        store.apply_factset(&facts).expect("apply");
        store
    }

    #[test]
    fn explain_reports_labelled_neighbourhood() {
        let store = seeded();
        let ex = explain(&store, "sym:rust:a.rs#main")
            .expect("query")
            .expect("present");
        assert_eq!(ex.schema, SCHEMA);
        assert_eq!(ex.node.kind, "fn");

        // Outgoing: derived call to helper.
        assert_eq!(ex.outgoing.len(), 1);
        assert_eq!(ex.outgoing[0].kind, "calls");
        assert_eq!(ex.outgoing[0].provenance, "derived");
        assert_eq!(ex.outgoing[0].node, "sym:rust:a.rs#helper");

        // Incoming: authored reference from the ADR.
        assert_eq!(ex.incoming.len(), 1);
        assert_eq!(ex.incoming[0].provenance, "authored");
        assert_eq!(ex.incoming[0].node, "adr:0001");
    }

    #[test]
    fn explain_missing_node_is_none() {
        let store = seeded();
        assert!(explain(&store, "sym:rust:a.rs#ghost").expect("q").is_none());
    }

    #[test]
    fn edges_differing_only_in_provenance_are_ordered() {
        // Two edges A->B with the same kind but different provenance must sort
        // into a stable, deterministic order (authored before derived).
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_edge(Edge::derived("a", "b", EdgeKind::References))
            .with_edge(Edge::authored("a", "b", EdgeKind::References));
        store.apply_factset(&facts).expect("apply");

        let ex = explain(&store, "a").expect("q").expect("present");
        let provs: Vec<_> = ex.outgoing.iter().map(|e| e.provenance).collect();
        assert_eq!(provs, ["authored", "derived"]);
    }

    #[test]
    fn list_kind_is_ordered() {
        let store = seeded();
        let listing = list_kind(&store, &NodeKind::Fn).expect("list");
        let keys: Vec<_> = listing.nodes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["sym:rust:a.rs#helper", "sym:rust:a.rs#main"]);
    }

    #[test]
    fn json_schema_is_stable() {
        let store = seeded();
        let ex = explain(&store, "adr:0001").expect("q").expect("present");
        let json = serde_json::to_value(&ex).expect("json");
        assert_eq!(json["schema"], SCHEMA);
        assert_eq!(json["node"]["key"], "adr:0001");
        assert_eq!(json["node"]["kind"], "adr");
        // Outgoing authored reference is present with its provenance label.
        assert_eq!(json["outgoing"][0]["kind"], "references");
        assert_eq!(json["outgoing"][0]["provenance"], "authored");
        assert_eq!(json["outgoing"][0]["node"], "sym:rust:a.rs#main");
        assert!(json["outgoing"][0]["confidence"].is_null());
    }

    #[test]
    fn path_crosses_provenance_and_direction() {
        // adr:0001 --authored/references--> main --derived/calls--> helper.
        // A path from the ADR to helper must traverse both, each hop labelled.
        let store = seeded();
        let p = path(&store, "adr:0001", "sym:rust:a.rs#helper").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 2);
        assert_eq!(p.schema, SCHEMA);

        assert_eq!(p.hops[0].kind, "references");
        assert_eq!(p.hops[0].provenance, "authored");
        assert_eq!(p.hops[0].direction, "outgoing");
        assert_eq!(p.hops[0].node, "sym:rust:a.rs#main");

        assert_eq!(p.hops[1].kind, "calls");
        assert_eq!(p.hops[1].provenance, "derived");
        assert_eq!(p.hops[1].node, "sym:rust:a.rs#helper");
    }

    #[test]
    fn path_follows_edges_against_direction() {
        // From helper back to the ADR: both edges are traversed against their
        // stored direction, so each hop is `incoming`.
        let store = seeded();
        let p = path(&store, "sym:rust:a.rs#helper", "adr:0001").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 2);
        assert!(p.hops.iter().all(|h| h.direction == "incoming"));
        assert_eq!(p.hops.last().unwrap().node, "adr:0001");
    }

    #[test]
    fn path_same_node_is_trivial() {
        let store = seeded();
        let p = path(&store, "adr:0001", "adr:0001").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 0);
        assert!(p.hops.is_empty());
    }

    #[test]
    fn path_missing_endpoint_or_unreachable_is_not_found() {
        let mut store = Store::open_in_memory().expect("store");
        // Two disconnected components: a-b and an isolated island.
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_node(Node::new("island", NodeKind::Fn, "island"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls));
        store.apply_factset(&facts).expect("apply");

        // Absent endpoint.
        let missing = path(&store, "a", "ghost").expect("path");
        assert!(!missing.found);
        assert!(missing.hops.is_empty());

        // Present but unreachable.
        let unreachable = path(&store, "a", "island").expect("path");
        assert!(!unreachable.found);
        assert!(unreachable.hops.is_empty());
    }

    #[test]
    fn path_is_shortest() {
        // a-b-c-d chain plus a direct a-d edge: the path must take the shortcut.
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_node(Node::new("c", NodeKind::Fn, "c"))
            .with_node(Node::new("d", NodeKind::Fn, "d"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls))
            .with_edge(Edge::derived("b", "c", EdgeKind::Calls))
            .with_edge(Edge::derived("c", "d", EdgeKind::Calls))
            .with_edge(Edge::derived("a", "d", EdgeKind::Calls));
        store.apply_factset(&facts).expect("apply");

        let p = path(&store, "a", "d").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 1, "the direct a->d edge is the shortest path");
        assert_eq!(p.hops[0].node, "d");
    }
}
