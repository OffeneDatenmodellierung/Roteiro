//! Round-trip property test for the graph store.
//!
//! Rather than pull in a property-testing dependency (Stage 1 budgets only
//! `serde_json`), this builds a fact set that exhaustively covers every node
//! kind, edge kind, and provenance class — with and without optional fields and
//! across several `meta` shapes — applies it to a store, and asserts that every
//! node and edge reads back byte-for-byte. The invariant under test: for any
//! valid fact set, `apply → read == input`.

use rto_graph::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Provenance, Span, Store};

fn all_node_kinds() -> Vec<NodeKind> {
    vec![
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
        // `Other` with a token that is deliberately not a known one, so it
        // canonicalises back to `Other` and not a named variant.
        NodeKind::Other("xcustomkind".to_owned()),
    ]
}

fn all_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Calls,
        EdgeKind::Imports,
        EdgeKind::Defines,
        EdgeKind::Contains,
        EdgeKind::References,
        EdgeKind::Supersedes,
        EdgeKind::AuthoredBy,
        EdgeKind::InferredFrom,
        EdgeKind::Other("xcustomedge".to_owned()),
    ]
}

fn meta_variants() -> Vec<serde_json::Value> {
    vec![
        serde_json::Value::Null,
        serde_json::json!({"vis": "pub", "async": true}),
        serde_json::json!([1, 2, 3]),
        serde_json::json!("scalar"),
    ]
}

/// Build a node whose optional fields are present or absent based on `full`.
fn make_node(i: usize, kind: NodeKind, full: bool) -> Node {
    let key = format!("k{i}");
    let meta = meta_variants()[i % meta_variants().len()].clone();
    let offset = u32::try_from(i).expect("index fits u32");
    // Cycle the node provenance so the round-trip covers all three layers being
    // persisted and reloaded (Derived/Authored/Inferred).
    let provenance = [
        Provenance::Derived,
        Provenance::Authored,
        Provenance::Inferred,
    ][i % 3];
    if full {
        Node {
            key,
            kind,
            name: format!("name{i}"),
            path: Some(format!("src/f{i}.rs")),
            lang: Some("rust".to_owned()),
            blob_hash: Some(format!("blob{i}")),
            span: Some(Span::new(offset, offset + 7)),
            provenance,
            meta,
        }
    } else {
        let mut n = Node::new(key, kind, format!("name{i}")).with_provenance(provenance);
        n.meta = meta;
        n
    }
}

/// Assemble a fact set covering the full cross-product of kinds/provenance.
fn build_factset() -> FactSet {
    let node_kinds = all_node_kinds();
    let mut facts = FactSet::new();

    // One node per kind, alternating full/sparse optional fields.
    for (i, kind) in node_kinds.iter().cloned().enumerate() {
        facts = facts.with_node(make_node(i, kind, i % 2 == 0));
    }
    let n = node_kinds.len();

    // Edges: every edge kind × every provenance, wired between existing nodes.
    //
    // **Every** provenance, including the three imported tiers. This fixture is
    // what proves a value survives the store, and the store is where the
    // `external-*` tokens actually live — a list that named only the local three
    // would leave the newest half of the vocabulary untested precisely where
    // it is written to disk.
    let mut e = 0usize;
    for ekind in all_edge_kinds() {
        for prov in [
            Provenance::Derived,
            Provenance::Authored,
            Provenance::Inferred,
            Provenance::ExternalDerived,
            Provenance::ExternalAuthored,
            Provenance::ExternalInferred,
        ] {
            let src = format!("k{}", e % n);
            let dst = format!("k{}", (e + 1) % n);
            let edge = if prov == Provenance::Inferred {
                let bump = f64::from(u32::try_from(e).expect("fits")) * 0.01;
                let mut ed = Edge::inferred(src, dst, ekind.clone(), 0.25 + bump);
                ed.src_ref = Some(format!("blob{e}#0..1"));
                ed
            } else {
                // No confidence on any other class — including the imported
                // ones, which have no score of their own to carry: OKF records
                // none for a relationship. `Edge::is_valid` and the store's
                // `CHECK` both say so, and an edge built otherwise would be
                // rejected on insert rather than round-tripped.
                let mut ed = Edge::derived(src, dst, ekind.clone());
                ed.provenance = prov;
                ed
            };
            assert!(edge.is_valid(), "fixture edge {e} is not storable");
            facts = facts.with_edge(edge);
            e += 1;
        }
    }
    facts
}

#[test]
fn factset_round_trips_through_store() {
    let facts = build_factset();
    let mut store = Store::open_in_memory().expect("open");
    store.apply_factset(&facts).expect("apply");

    assert_eq!(store.node_count().expect("nc"), facts.nodes.len() as u64);
    assert_eq!(store.edge_count().expect("ec"), facts.edges.len() as u64);

    // Every node reads back identically.
    for node in &facts.nodes {
        let got = store.get_node(&node.key).expect("get").expect("present");
        assert_eq!(&got, node, "node {} did not round-trip", node.key);
    }

    // Every edge is retrievable from its source, byte-for-byte.
    for edge in &facts.edges {
        let from = store.edges_from(&edge.src).expect("edges_from");
        assert!(
            from.iter().any(|e| e == edge),
            "edge {:?} -> {:?} ({}) missing after round-trip",
            edge.src,
            edge.dst,
            edge.kind.as_str()
        );
    }

    // Provenance partitions the edge set exactly.
    //
    // The classes come from `Provenance::tokens()`, not a list written here. A
    // written-down list makes this assertion silently *weaker* every time the
    // vocabulary grows — it kept summing three of six and still read as a
    // partition check — where reading the vocabulary makes it impossible to
    // widen the enum without widening the partition with it.
    let total: usize = Provenance::tokens()
        .iter()
        .map(|t| Provenance::from_token(t).expect("a listed token must parse"))
        .map(|p| store.edges_by_provenance(p).expect("by prov").len())
        .sum();
    assert_eq!(total, facts.edges.len());
}

#[test]
fn neighbors_reflect_applied_edges() {
    let facts = build_factset();
    let mut store = Store::open_in_memory().expect("open");
    store.apply_factset(&facts).expect("apply");

    // Cross-check neighbours against the raw edge list for the first node.
    let key = "k0";
    let expected_out: std::collections::BTreeSet<_> = facts
        .edges
        .iter()
        .filter(|e| e.src == key)
        .map(|e| e.dst.clone())
        .collect();
    let got_out: std::collections::BTreeSet<_> = store
        .neighbors(key, Direction::Outgoing)
        .expect("out")
        .into_iter()
        .map(|n| n.key)
        .collect();
    assert_eq!(got_out, expected_out);
}
