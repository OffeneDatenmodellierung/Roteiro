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

    /// Load this artifact into `store`, replacing its entire contents. If the
    /// artifact carries a tree id it is recorded, so a `sync` at the matching
    /// commit sees the graph as already applied; a tree-less artifact records no
    /// synced state.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the rebuild fails.
    pub fn load_into(&self, store: &mut Store) -> Result<(), StoreError> {
        store.rebuild(&self.facts, self.tree.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::{ARTIFACT_SCHEMA, GraphArtifact};
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    /// The three nodes and two edges of the sample graph, as builder closures so
    /// tests can apply them in any order.
    fn sample_nodes() -> Vec<Node> {
        vec![
            Node::new("adr:0001", NodeKind::Adr, "Build Roteiro"),
            Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"),
            Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"),
        ]
    }

    fn sample_edges() -> Vec<Edge> {
        vec![
            Edge::authored("adr:0001", "sym:rust:a.rs#main", EdgeKind::References),
            Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ),
        ]
    }

    /// Build a store from the sample graph, applying nodes/edges in the given
    /// order (to exercise insertion-order independence) and recording `tree`.
    fn seeded_ordered(reversed: bool, tree: Option<&str>) -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let mut nodes = sample_nodes();
        let mut edges = sample_edges();
        if reversed {
            nodes.reverse();
            edges.reverse();
        }
        let mut facts = FactSet::new();
        facts.nodes = nodes;
        facts.edges = edges;
        store.rebuild(&facts, tree).expect("rebuild");
        store
    }

    fn seeded() -> Store {
        seeded_ordered(false, Some("treeabc"))
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
    fn export_is_deterministic_regardless_of_insertion_order() {
        // The same graph, inserted forwards vs. reversed, must export to
        // byte-identical JSON — proving the ordering comes from the export, not
        // from insertion order.
        let a = GraphArtifact::from_store(&seeded_ordered(false, Some("treeabc")))
            .expect("a")
            .to_json()
            .expect("ja");
        let b = GraphArtifact::from_store(&seeded_ordered(true, Some("treeabc")))
            .expect("b")
            .to_json()
            .expect("jb");
        assert_eq!(a, b);
    }

    #[test]
    fn artifact_without_tree_records_no_sync_state() {
        // An artifact carrying no tree id must load as `sync_state == None`, not
        // an empty string, so a later `sync` does not spuriously short-circuit.
        let store = seeded_ordered(false, None);
        let artifact = GraphArtifact::from_store(&store).expect("capture");
        assert_eq!(artifact.tree, None);

        let mut fresh = Store::open_in_memory().expect("fresh");
        artifact.load_into(&mut fresh).expect("load");
        assert_eq!(fresh.node_count().expect("nc"), 3);
        assert_eq!(fresh.sync_state().expect("state"), None);
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
