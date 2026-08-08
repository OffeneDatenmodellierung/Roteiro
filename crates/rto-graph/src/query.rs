//! The agent- and human-facing query surface over the graph.
//!
//! Everything here is a read-only view built from the store's typed queries,
//! serialised under a **stable, versioned** JSON schema ([`SCHEMA`]) so agents
//! can depend on the shape. Two primitives are provided: [`explain`] (a node
//! and its provenance-labelled neighbourhood) and [`list_kind`] (all nodes of a
//! kind). Both return mixed-provenance results — the "one query surface" from
//! ADR-0001 — with every edge carrying its `provenance`.

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

#[cfg(test)]
mod tests {
    use super::{SCHEMA, explain, list_kind};
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
}
