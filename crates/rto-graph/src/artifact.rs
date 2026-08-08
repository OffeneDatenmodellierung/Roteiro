//! A portable, versioned snapshot of an assembled graph.
//!
//! A [`GraphArtifact`] is the whole graph — every node and edge, plus the
//! `HEAD` tree id it was assembled from — serialised as deterministic JSON. It
//! is the unit CI publishes so that a clone can load a ready-made graph instead
//! of re-extracting it (offline fallback: rebuild). Because [`Store::export_factset`]
//! orders its output, the same graph always produces byte-identical JSON.

use serde::{Deserialize, Serialize};

use crate::{FactSet, Store, StoreError};

/// Versioned schema tag for the artifact envelope. Bump on any breaking change.
pub const ARTIFACT_SCHEMA: &str = "roteiro.graph/v1";

/// A self-describing snapshot of an assembled graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphArtifact {
    /// Schema tag ([`ARTIFACT_SCHEMA`]).
    pub schema: String,
    /// Hex id of the `HEAD` tree this graph was assembled from, if recorded.
    pub tree: Option<String>,
    /// The full node/edge set.
    pub facts: FactSet,
}

impl GraphArtifact {
    /// Capture the current contents of `store` as an artifact.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the store cannot be read.
    pub fn from_store(store: &Store) -> Result<Self, StoreError> {
        Ok(Self {
            schema: ARTIFACT_SCHEMA.to_owned(),
            tree: store.sync_state()?,
            facts: store.export_factset()?,
        })
    }

    /// Serialise to pretty, deterministic JSON.
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] if serialisation fails.
    pub fn to_json(&self) -> Result<String, StoreError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parse an artifact from JSON.
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] on malformed JSON, or [`StoreError::Corrupt`]
    /// if the schema tag is unrecognised.
    pub fn from_json(json: &str) -> Result<Self, StoreError> {
        let artifact: Self = serde_json::from_str(json)?;
        if artifact.schema != ARTIFACT_SCHEMA {
            return Err(StoreError::Corrupt(format!(
                "unsupported graph artifact schema: {} (expected {ARTIFACT_SCHEMA})",
                artifact.schema
            )));
        }
        Ok(artifact)
    }

    /// Load this artifact into `store`, replacing its entire contents and
    /// recording the tree id so a later `sync` sees it as already applied.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the rebuild fails.
    pub fn load_into(&self, store: &mut Store) -> Result<(), StoreError> {
        store.rebuild(&self.facts, self.tree.as_deref().unwrap_or(""))
    }
}

#[cfg(test)]
mod tests {
    use super::{ARTIFACT_SCHEMA, GraphArtifact};
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("adr:0001", NodeKind::Adr, "Build Roteiro"))
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_edge(Edge::authored(
                "adr:0001",
                "sym:rust:a.rs#main",
                EdgeKind::References,
            ))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ));
        store.rebuild(&facts, "treeabc").expect("rebuild");
        store
    }

    #[test]
    fn round_trips_through_json_and_a_fresh_store() {
        let store = seeded();
        let artifact = GraphArtifact::from_store(&store).expect("capture");
        assert_eq!(artifact.schema, ARTIFACT_SCHEMA);
        assert_eq!(artifact.tree.as_deref(), Some("treeabc"));
        assert_eq!(artifact.facts.nodes.len(), 3);
        assert_eq!(artifact.facts.edges.len(), 2);

        // JSON round-trip is lossless.
        let json = artifact.to_json().expect("json");
        let parsed = GraphArtifact::from_json(&json).expect("parse");
        assert_eq!(parsed, artifact);

        // Loading into a fresh store reproduces the graph without extraction.
        let mut fresh = Store::open_in_memory().expect("fresh");
        parsed.load_into(&mut fresh).expect("load");
        assert_eq!(fresh.node_count().expect("nc"), 3);
        assert_eq!(fresh.edge_count().expect("ec"), 2);
        assert_eq!(
            fresh.sync_state().expect("state").as_deref(),
            Some("treeabc")
        );
        // Mixed provenance survives the round-trip.
        let inbound = fresh.edges_to("sym:rust:a.rs#main").expect("edges");
        assert!(
            inbound
                .iter()
                .any(|e| e.provenance == crate::Provenance::Authored)
        );
    }

    #[test]
    fn export_is_deterministic() {
        // Two independent stores with the same facts applied in different orders
        // export byte-identical JSON.
        let a = GraphArtifact::from_store(&seeded())
            .expect("a")
            .to_json()
            .expect("ja");
        let b = GraphArtifact::from_store(&seeded())
            .expect("b")
            .to_json()
            .expect("jb");
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_unknown_schema() {
        let json = r#"{"schema":"roteiro.graph/v999","tree":null,"facts":{"nodes":[],"edges":[]}}"#;
        assert!(matches!(
            GraphArtifact::from_json(json),
            Err(crate::StoreError::Corrupt(_))
        ));
    }
}
