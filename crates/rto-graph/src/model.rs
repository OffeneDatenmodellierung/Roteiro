//! In-memory graph domain types: nodes, edges, and the [`FactSet`] that groups
//! the facts extracted from a single source blob.
//!
//! Node and edge *kinds* are open sets: known variants have stable string
//! tokens, and any other token round-trips through [`NodeKind::Other`] /
//! [`EdgeKind::Other`] so new extractors can introduce kinds without a schema
//! change. Nodes are addressed by a deterministic natural [`Node::key`]; edges
//! reference their endpoints by that key, and the store resolves keys to row
//! ids on insert.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Provenance;

/// The kind of a graph node.
///
/// Known kinds have stable tokens (`fn`, `struct`, …); unrecognised tokens are
/// preserved verbatim in [`NodeKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A function or method.
    Fn,
    /// A struct type.
    Struct,
    /// An enum type.
    Enum,
    /// A trait / interface.
    Trait,
    /// A module or namespace.
    Module,
    /// A source file.
    File,
    /// An Architecture Decision Record.
    Adr,
    /// A section within an ADR.
    AdrSection,
    /// A blueprint document.
    Blueprint,
    /// A free-form documentation artifact.
    Doc,
    /// Any kind not covered above, kept verbatim.
    Other(String),
}

impl NodeKind {
    /// The stable string token for this kind, as stored in the database.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Fn => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::File => "file",
            Self::Adr => "adr",
            Self::AdrSection => "adr_section",
            Self::Blueprint => "blueprint",
            Self::Doc => "doc",
            Self::Other(s) => s,
        }
    }

    /// Parse a kind from its string token. Unknown tokens become
    /// [`NodeKind::Other`], so this is infallible.
    #[must_use]
    pub fn from_token(s: &str) -> Self {
        match s {
            "fn" => Self::Fn,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "module" => Self::Module,
            "file" => Self::File,
            "adr" => Self::Adr,
            "adr_section" => Self::AdrSection,
            "blueprint" => Self::Blueprint,
            "doc" => Self::Doc,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// The kind of a graph edge (the relationship it records).
///
/// Known kinds have stable tokens; unrecognised tokens are preserved in
/// [`EdgeKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    /// Source calls target.
    Calls,
    /// Source imports target.
    Imports,
    /// Source defines target.
    Defines,
    /// Source contains target (structural nesting).
    Contains,
    /// Source references target (unspecified use).
    References,
    /// Source supersedes target (e.g. a later ADR).
    Supersedes,
    /// Target is authored by / documented in source.
    AuthoredBy,
    /// Target is inferred from source.
    InferredFrom,
    /// Source and target are semantically related (inferred by similarity).
    Related,
    /// Any kind not covered above, kept verbatim.
    Other(String),
}

impl EdgeKind {
    /// The stable string token for this kind, as stored in the database.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Defines => "defines",
            Self::Contains => "contains",
            Self::References => "references",
            Self::Supersedes => "supersedes",
            Self::AuthoredBy => "authored_by",
            Self::InferredFrom => "inferred_from",
            Self::Related => "related",
            Self::Other(s) => s,
        }
    }

    /// Parse a kind from its string token. Unknown tokens become
    /// [`EdgeKind::Other`], so this is infallible.
    #[must_use]
    pub fn from_token(s: &str) -> Self {
        match s {
            "calls" => Self::Calls,
            "imports" => Self::Imports,
            "defines" => Self::Defines,
            "contains" => Self::Contains,
            "references" => Self::References,
            "supersedes" => Self::Supersedes,
            "authored_by" => Self::AuthoredBy,
            "inferred_from" => Self::InferredFrom,
            "related" => Self::Related,
            other => Self::Other(other.to_owned()),
        }
    }
}

// Kinds (de)serialize as their bare string token so on-disk fact sets and the
// database agree on representation.
impl Serialize for NodeKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NodeKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_token(&s))
    }
}

impl Serialize for EdgeKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EdgeKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_token(&s))
    }
}

/// A byte-offset range within a source blob (`start..end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

impl Span {
    /// Construct a span from a start and end byte offset.
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// A node in the knowledge graph.
///
/// [`Node::key`] is the deterministic natural identity used for upserts (e.g.
/// `sym:rust:src/lib.rs#Store`); the database row id is an internal detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Deterministic, unique natural key.
    pub key: String,
    /// The kind of thing this node represents.
    pub kind: NodeKind,
    /// Human-facing name (need not be unique).
    pub name: String,
    /// Repository-relative source path, if any.
    pub path: Option<String>,
    /// Language token (e.g. `rust`), if applicable.
    pub lang: Option<String>,
    /// Git blob hash this node was extracted from, if applicable.
    pub blob_hash: Option<String>,
    /// Byte span within the source blob, if applicable.
    pub span: Option<Span>,
    /// Arbitrary structured metadata.
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl Node {
    /// Construct a node with the given key, kind, and name; all optional fields
    /// unset and `meta` null.
    #[must_use]
    pub fn new(key: impl Into<String>, kind: NodeKind, name: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind,
            name: name.into(),
            path: None,
            lang: None,
            blob_hash: None,
            span: None,
            meta: serde_json::Value::Null,
        }
    }
}

/// An edge in the knowledge graph, connecting two nodes by their keys.
///
/// The [`Provenance`] invariant is enforced on insert: `confidence` is present
/// if and only if the provenance is [`Provenance::Inferred`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Natural key of the source node.
    pub src: String,
    /// Natural key of the destination node.
    pub dst: String,
    /// The relationship this edge records.
    pub kind: EdgeKind,
    /// How this edge was produced.
    pub provenance: Provenance,
    /// Confidence score in `0.0..=1.0`; `Some` iff `provenance` is inferred.
    pub confidence: Option<f64>,
    /// Where the fact came from (e.g. `blob#span`, or an ADR id).
    pub src_ref: Option<String>,
}

impl Edge {
    /// Construct a `derived` edge (no confidence).
    #[must_use]
    pub fn derived(src: impl Into<String>, dst: impl Into<String>, kind: EdgeKind) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            kind,
            provenance: Provenance::Derived,
            confidence: None,
            src_ref: None,
        }
    }

    /// Construct an `authored` edge (no confidence).
    #[must_use]
    pub fn authored(src: impl Into<String>, dst: impl Into<String>, kind: EdgeKind) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            kind,
            provenance: Provenance::Authored,
            confidence: None,
            src_ref: None,
        }
    }

    /// Construct an `inferred` edge carrying a confidence score.
    #[must_use]
    pub fn inferred(
        src: impl Into<String>,
        dst: impl Into<String>,
        kind: EdgeKind,
        confidence: f64,
    ) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            kind,
            provenance: Provenance::Inferred,
            confidence: Some(confidence),
            src_ref: None,
        }
    }

    /// Whether this edge is valid for storage: a confidence score is present
    /// exactly when the edge is inferred, and any present score is a finite
    /// value in `0.0..=1.0` (rejecting NaN, infinities, and out-of-range).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let inferred = matches!(self.provenance, Provenance::Inferred);
        match self.confidence {
            Some(c) => inferred && (0.0..=1.0).contains(&c),
            None => !inferred,
        }
    }
}

/// The set of nodes and edges extracted from a single source blob (or otherwise
/// assembled together). Applying a fact set to a [`crate::Store`] is atomic.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FactSet {
    /// Nodes to upsert.
    pub nodes: Vec<Node>,
    /// Edges to insert (endpoints must resolve to nodes in this set or already
    /// present in the store).
    pub edges: Vec<Edge>,
}

impl FactSet {
    /// An empty fact set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node, returning `self` for chaining.
    #[must_use]
    pub fn with_node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }

    /// Add an edge, returning `self` for chaining.
    #[must_use]
    pub fn with_edge(mut self, edge: Edge) -> Self {
        self.edges.push(edge);
        self
    }

    /// Whether the fact set has no nodes and no edges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }
}

/// Direction of traversal when querying a node's neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow edges where the node is the source.
    Outgoing,
    /// Follow edges where the node is the destination.
    Incoming,
    /// Follow edges in either direction.
    Both,
}

#[cfg(test)]
mod tests {
    use super::{Edge, EdgeKind, FactSet, Node, NodeKind};
    use crate::Provenance;

    #[test]
    fn node_kind_tokens_round_trip() {
        let kinds = [
            NodeKind::Fn,
            NodeKind::Struct,
            NodeKind::Enum,
            NodeKind::Trait,
            NodeKind::Module,
            NodeKind::File,
            NodeKind::Adr,
            NodeKind::AdrSection,
            NodeKind::Blueprint,
            NodeKind::Doc,
            NodeKind::Other("weird".to_owned()),
        ];
        for k in kinds {
            assert_eq!(NodeKind::from_token(k.as_str()), k);
        }
    }

    #[test]
    fn edge_kind_tokens_round_trip() {
        let kinds = [
            EdgeKind::Calls,
            EdgeKind::Imports,
            EdgeKind::Defines,
            EdgeKind::Contains,
            EdgeKind::References,
            EdgeKind::Supersedes,
            EdgeKind::AuthoredBy,
            EdgeKind::InferredFrom,
            EdgeKind::Other("weird".to_owned()),
        ];
        for k in kinds {
            assert_eq!(EdgeKind::from_token(k.as_str()), k);
        }
    }

    #[test]
    fn kinds_serialize_as_bare_tokens() {
        assert_eq!(
            serde_json::to_string(&NodeKind::AdrSection).unwrap(),
            "\"adr_section\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::AuthoredBy).unwrap(),
            "\"authored_by\""
        );
        let k: NodeKind = serde_json::from_str("\"struct\"").unwrap();
        assert_eq!(k, NodeKind::Struct);
    }

    #[test]
    fn edge_validity_tracks_provenance() {
        assert!(Edge::derived("a", "b", EdgeKind::Calls).is_valid());
        assert!(Edge::authored("a", "b", EdgeKind::AuthoredBy).is_valid());
        assert!(Edge::inferred("a", "b", EdgeKind::References, 0.5).is_valid());
        // Boundary values are valid.
        assert!(Edge::inferred("a", "b", EdgeKind::References, 0.0).is_valid());
        assert!(Edge::inferred("a", "b", EdgeKind::References, 1.0).is_valid());

        let inferred = Edge::inferred("a", "b", EdgeKind::References, 0.5);
        // Non-inferred edge carrying confidence is invalid.
        let bad = Edge {
            provenance: Provenance::Derived,
            confidence: Some(0.9),
            ..Edge::derived("a", "b", EdgeKind::Calls)
        };
        assert!(!bad.is_valid());
        // Inferred edge without confidence is invalid.
        assert!(
            !Edge {
                confidence: None,
                ..inferred.clone()
            }
            .is_valid()
        );
        // Out-of-range and non-finite confidences are invalid.
        for c in [-0.1, 1.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                !Edge {
                    confidence: Some(c),
                    ..inferred.clone()
                }
                .is_valid(),
                "confidence {c} should be rejected"
            );
        }
    }

    #[test]
    fn factset_builders() {
        let fs = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_edge(Edge::derived("a", "a", EdgeKind::Calls));
        assert_eq!(fs.nodes.len(), 1);
        assert_eq!(fs.edges.len(), 1);
        assert!(!fs.is_empty());
        assert!(FactSet::new().is_empty());
    }
}
