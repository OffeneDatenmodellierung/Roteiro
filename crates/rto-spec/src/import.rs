//! Importers that map an external tool's graph into Roteiro's provenance model.
//!
//! Currently: **Graphify** (a `NetworkX` node-link JSON graph). Per ADR-0001,
//! Graphify's doc/media/concept knowledge is imported as `inferred` facts, while
//! its code-structure (AST) nodes and edges are **dropped** in favour of
//! Roteiro's own more precise derivation. Each import returns a
//! [`ImportReport`] so the migration is auditable.

use std::collections::{BTreeMap, HashSet};

use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance};
use serde::Deserialize;

/// `src_ref` stamped on every edge imported from Graphify, so it can be told
/// apart from other `inferred` edges (e.g. the embedding layer's).
pub const GRAPHIFY_REF: &str = "import:graphify";

/// Errors raised while importing.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The source JSON could not be parsed.
    #[error("invalid graphify json: {0}")]
    Json(#[from] serde_json::Error),
}

/// The result of importing a Graphify graph: the facts to apply and a report.
#[derive(Debug, Clone)]
pub struct GraphifyImport {
    /// Nodes and `inferred` edges to apply to the store.
    pub facts: FactSet,
    /// A summary of what was imported vs. dropped.
    pub report: ImportReport,
}

/// An auditable summary of a Graphify import.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportReport {
    /// Total nodes in the source.
    pub nodes_total: usize,
    /// Doc/media/concept nodes imported.
    pub nodes_imported: usize,
    /// Code (AST) nodes dropped in favour of re-derivation.
    pub nodes_dropped_code: usize,
    /// Imported node count by Graphify `file_type`.
    pub nodes_by_type: BTreeMap<String, usize>,
    /// Total links in the source.
    pub links_total: usize,
    /// Semantic/inferred edges imported.
    pub edges_imported: usize,
    /// AST (code-structure) edges dropped in favour of re-derivation.
    pub edges_dropped_ast: usize,
    /// Semantic edges skipped because an endpoint was a dropped code node.
    pub edges_skipped_dangling: usize,
    /// Total hyperedges in the source.
    pub hyperedges_total: usize,
    /// Hyperedges imported as grouping nodes.
    pub hyperedges_imported: usize,
}

// --- Graphify's NetworkX node-link schema (only the fields we use) ---

#[derive(Deserialize)]
struct GraphifyGraph {
    #[serde(default)]
    nodes: Vec<GNode>,
    #[serde(default)]
    links: Vec<GLink>,
    #[serde(default)]
    hyperedges: Vec<GHyper>,
}

#[derive(Deserialize)]
struct GNode {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    file_type: String,
    #[serde(default)]
    source_file: Option<String>,
    #[serde(rename = "_origin", default)]
    origin: String,
    #[serde(default)]
    community_name: Option<String>,
}

#[derive(Deserialize)]
struct GLink {
    source: String,
    target: String,
    #[serde(default)]
    relation: String,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    confidence_score: Option<f64>,
    #[serde(rename = "_origin", default)]
    origin: String,
}

#[derive(Deserialize)]
struct GHyper {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    nodes: Vec<String>,
    #[serde(default)]
    confidence_score: Option<f64>,
}

/// A Graphify node is *code structure* (dropped, re-derived) when its file type
/// is `code`. Everything else (document/concept/rationale/image) is imported.
fn is_code_node(n: &GNode) -> bool {
    n.file_type == "code"
}

/// A link is a *semantic/inferred* relationship (imported) rather than plain
/// code-structure (dropped) when it did **not** come from the AST, **or** it is
/// explicitly marked `INFERRED` confidence (a fuzzy suggestion is worth keeping
/// even if Graphify tagged its origin as `ast`).
fn is_semantic_link(l: &GLink) -> bool {
    l.origin != "ast" || l.confidence.eq_ignore_ascii_case("inferred")
}

/// Map a Graphify `file_type` to a Roteiro node kind. An unset type is treated
/// as a plain document (mapping to the real [`NodeKind::Doc`], not an `Other`
/// token that would collide with `Doc`'s stable token on round-trip).
fn node_kind(file_type: &str) -> NodeKind {
    match file_type {
        "document" | "" => NodeKind::Doc,
        other => NodeKind::Other(other.to_owned()),
    }
}

/// Map a Graphify relation to a Roteiro edge kind.
fn edge_kind(relation: &str) -> EdgeKind {
    match relation {
        "conceptually_related_to" | "semantically_similar_to" | "" => EdgeKind::Related,
        "references" | "rationale_for" => EdgeKind::References,
        other => EdgeKind::Other(other.to_owned()),
    }
}

/// The Roteiro node key for a Graphify node id.
fn key(id: &str) -> String {
    format!("graphify:{id}")
}

/// The Roteiro node key for a Graphify **hyperedge** group id, in a distinct
/// namespace so a hyperedge can never collide with (and clobber) a regular node
/// that happens to share its id.
fn group_key(id: &str) -> String {
    format!("graphify:group:{id}")
}

/// Confidence in `0.0..=1.0` for an imported edge (defaulting mid-scale).
fn confidence(score: Option<f64>) -> f64 {
    score.unwrap_or(0.5).clamp(0.0, 1.0)
}

/// Import a Graphify node-link JSON graph into Roteiro facts.
///
/// Doc/concept/rationale/image nodes become nodes keyed `graphify:<id>`;
/// semantic/inferred links between two imported nodes become `inferred` edges
/// (stamped [`GRAPHIFY_REF`]); hyperedges become grouping nodes with `related`
/// edges to their imported members. Code/AST nodes and edges are dropped.
///
/// # Errors
/// Returns [`ImportError::Json`] if `json` is not a valid Graphify graph.
pub fn import_graphify(json: &str) -> Result<GraphifyImport, ImportError> {
    let graph: GraphifyGraph = serde_json::from_str(json)?;
    let mut report = ImportReport {
        nodes_total: graph.nodes.len(),
        links_total: graph.links.len(),
        hyperedges_total: graph.hyperedges.len(),
        ..ImportReport::default()
    };
    let mut facts = FactSet::new();
    let mut imported: HashSet<String> = HashSet::new();

    // Nodes.
    for n in &graph.nodes {
        if is_code_node(n) {
            report.nodes_dropped_code += 1;
            continue;
        }
        let node_key = key(&n.id);
        let name = if n.label.is_empty() {
            n.id.clone()
        } else {
            n.label.clone()
        };
        let mut node = Node::new(node_key.clone(), node_kind(&n.file_type), name)
            .with_provenance(Provenance::Inferred);
        node.path.clone_from(&n.source_file);
        node.meta = serde_json::json!({
            "graphify_id": n.id,
            "file_type": n.file_type,
            "origin": n.origin,
            "community": n.community_name,
        });
        facts.nodes.push(node);
        imported.insert(node_key);
        *report
            .nodes_by_type
            .entry(if n.file_type.is_empty() {
                "unknown".to_owned()
            } else {
                n.file_type.clone()
            })
            .or_default() += 1;
        report.nodes_imported += 1;
    }

    // Links.
    for l in &graph.links {
        if !is_semantic_link(l) {
            report.edges_dropped_ast += 1;
            continue;
        }
        let (src, dst) = (key(&l.source), key(&l.target));
        if !imported.contains(&src) || !imported.contains(&dst) {
            // A semantic edge that touches a dropped code node.
            report.edges_skipped_dangling += 1;
            continue;
        }
        let mut edge = Edge::inferred(
            src,
            dst,
            edge_kind(&l.relation),
            confidence(l.confidence_score),
        );
        edge.src_ref = Some(GRAPHIFY_REF.to_owned());
        facts.edges.push(edge);
        report.edges_imported += 1;
    }

    // Hyperedges → a grouping node + `related` edges to imported members.
    for h in &graph.hyperedges {
        let members: Vec<String> = h
            .nodes
            .iter()
            .map(|m| key(m))
            .filter(|m| imported.contains(m))
            .collect();
        if members.is_empty() {
            continue;
        }
        let gkey = group_key(&h.id);
        let name = if h.label.is_empty() {
            h.id.clone()
        } else {
            h.label.clone()
        };
        let mut group = Node::new(gkey.clone(), NodeKind::Other("group".to_owned()), name)
            .with_provenance(Provenance::Inferred);
        group.meta = serde_json::json!({ "graphify_id": h.id, "kind": "hyperedge" });
        facts.nodes.push(group);
        for member in members {
            let mut edge = Edge::inferred(
                gkey.clone(),
                member,
                EdgeKind::Related,
                confidence(h.confidence_score),
            );
            edge.src_ref = Some(GRAPHIFY_REF.to_owned());
            facts.edges.push(edge);
        }
        report.hyperedges_imported += 1;
    }

    Ok(GraphifyImport { facts, report })
}

#[cfg(test)]
mod tests {
    use super::{GRAPHIFY_REF, import_graphify};
    use rto_graph::{EdgeKind, NodeKind, Provenance};

    // A miniature Graphify graph exercising each rule.
    const SAMPLE: &str = r#"{
      "directed": false, "multigraph": false,
      "nodes": [
        {"id": "adr59", "label": "ADR-0059", "file_type": "concept", "source_file": "docs/adr/0059.md", "_origin": "semantic", "community_name": "adrs"},
        {"id": "doc1", "label": "Design note", "file_type": "document", "source_file": "docs/design.md", "_origin": "semantic"},
        {"id": "codeA", "label": "fn a", "file_type": "code", "source_file": "src/a.rs", "_origin": "ast"}
      ],
      "links": [
        {"source": "adr59", "target": "doc1", "relation": "conceptually_related_to", "confidence": "INFERRED", "confidence_score": 0.82, "_origin": "semantic"},
        {"source": "codeA", "target": "doc1", "relation": "references", "confidence": "EXTRACTED", "confidence_score": 1.0, "_origin": "ast"},
        {"source": "adr59", "target": "codeA", "relation": "references", "confidence": "EXTRACTED", "confidence_score": 1.0, "_origin": "semantic"}
      ],
      "hyperedges": [
        {"id": "grp1", "label": "ADR cluster", "nodes": ["adr59", "doc1", "codeA"], "confidence_score": 0.9}
      ]
    }"#;

    #[test]
    fn imports_docs_and_semantic_edges_drops_code() {
        let out = import_graphify(SAMPLE).expect("import");
        let r = &out.report;

        // Two doc/concept nodes imported; one code node dropped.
        assert_eq!(r.nodes_total, 3);
        assert_eq!(r.nodes_imported, 2);
        assert_eq!(r.nodes_dropped_code, 1);
        assert_eq!(r.nodes_by_type.get("concept"), Some(&1));
        assert_eq!(r.nodes_by_type.get("document"), Some(&1));

        // Edges: adr59→doc1 (semantic) imported; codeA→doc1 (ast) dropped;
        // adr59→codeA (semantic but code endpoint) skipped as dangling.
        assert_eq!(r.edges_imported, 1);
        assert_eq!(r.edges_dropped_ast, 1);
        assert_eq!(r.edges_skipped_dangling, 1);

        // The hyperedge keeps only its two imported members.
        assert_eq!(r.hyperedges_imported, 1);

        // The imported semantic edge is inferred, related-kind, correct
        // confidence, and stamped with the graphify src_ref.
        let e = out
            .facts
            .edges
            .iter()
            .find(|e| e.src == "graphify:adr59" && e.dst == "graphify:doc1")
            .expect("semantic edge");
        assert_eq!(e.provenance, Provenance::Inferred);
        assert_eq!(e.kind, EdgeKind::Related);
        assert_eq!(e.confidence, Some(0.82));
        assert_eq!(e.src_ref.as_deref(), Some(GRAPHIFY_REF));

        // The concept node carries its path and a Doc/Other kind.
        let n = out
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "graphify:adr59")
            .expect("concept node");
        assert_eq!(n.kind, NodeKind::Other("concept".to_owned()));
        assert_eq!(n.path.as_deref(), Some("docs/adr/0059.md"));
        assert_eq!(n.meta["graphify_id"], "adr59");
        // Graphify nodes are the inferred layer (heuristic import).
        assert_eq!(n.provenance, Provenance::Inferred);

        // A `document` node maps to NodeKind::Doc.
        let d = out
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "graphify:doc1")
            .expect("doc node");
        assert_eq!(d.kind, NodeKind::Doc);

        // Every applied fact is valid for the store (invariants hold).
        for edge in &out.facts.edges {
            assert!(edge.is_valid());
        }
    }

    #[test]
    fn hyperedge_group_links_only_imported_members() {
        let out = import_graphify(SAMPLE).expect("import");
        // The group lives in a distinct `graphify:group:` namespace.
        let group = out
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "graphify:group:grp1")
            .expect("group node");
        assert_eq!(group.kind, NodeKind::Other("group".to_owned()));
        // Group → adr59 and doc1 (imported), not codeA (dropped).
        let group_edges: Vec<_> = out
            .facts
            .edges
            .iter()
            .filter(|e| e.src == "graphify:group:grp1")
            .map(|e| e.dst.as_str())
            .collect();
        assert_eq!(group_edges.len(), 2);
        assert!(group_edges.contains(&"graphify:adr59"));
        assert!(group_edges.contains(&"graphify:doc1"));
        assert!(!group_edges.contains(&"graphify:codeA"));
    }

    #[test]
    fn group_id_colliding_with_a_node_id_does_not_clobber() {
        // A node and a hyperedge share the id "x": the group must land under
        // `graphify:group:x`, leaving the real node `graphify:x` intact.
        let json = r#"{
          "nodes": [
            {"id": "x", "label": "real node", "file_type": "document", "_origin": "semantic"},
            {"id": "y", "label": "other", "file_type": "concept", "_origin": "semantic"}
          ],
          "links": [],
          "hyperedges": [
            {"id": "x", "label": "group named x", "nodes": ["y"], "confidence_score": 0.9}
          ]
        }"#;
        let out = import_graphify(json).expect("import");
        let real = out
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "graphify:x")
            .expect("real node survives");
        assert_eq!(real.name, "real node");
        let group = out
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "graphify:group:x")
            .expect("group in its own namespace");
        assert_eq!(group.name, "group named x");
    }

    #[test]
    fn invalid_json_errors() {
        assert!(import_graphify("not json").is_err());
    }
}
