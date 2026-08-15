//! The invariants that make analyzer findings a *separate* artifact store rather
//! than a fourth provenance class (ADR-0012).
//!
//! Every assertion here exists because the alternative is mechanically possible.
//! Writing findings as nodes would compile, pass, inherit
//! `nodes.provenance DEFAULT 'derived'`, and be swept into `export_factset` —
//! silently publishing tool output into an artifact that must stay a pure
//! function of the tree. These tests are what stop that from happening quietly.

use rto_graph::{
    AnalysisRun, CommandPolicy, Edge, EdgeKind, FactSet, Finding, FindingKey, FindingsLayer,
    GraphArtifact, Isolation, Node, NodeKind, Provenance, RunnerKind, Severity, SourceIdentity,
    Store, WorktreeId, layer_key, search,
};

/// A small graph standing in for a real repository's derived + authored layers.
fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    facts.nodes = vec![
        Node::new("adr:0001", NodeKind::Adr, "Build Roteiro").with_provenance(Provenance::Authored),
        Node::new("sym:rust:src/tls.rs#connect", NodeKind::Fn, "connect"),
        Node::new("file:Cargo.lock", NodeKind::File, "Cargo.lock"),
    ];
    facts.edges = vec![
        Edge::authored(
            "adr:0001",
            "sym:rust:src/tls.rs#connect",
            EdgeKind::References,
        ),
        Edge::derived(
            "sym:rust:src/tls.rs#connect",
            "file:Cargo.lock",
            EdgeKind::References,
        ),
    ];
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
}

fn worktree() -> WorktreeId {
    WorktreeId::new("ab12cd34").expect("worktree id")
}

fn run(analyzer: &str, report_digest: &str) -> AnalysisRun {
    AnalysisRun {
        layer: layer_key(analyzer, &worktree()).expect("layer key"),
        analyzer: analyzer.to_owned(),
        analyzer_version: "0.21.0".to_owned(),
        runner: RunnerKind::Ingested,
        isolation: Isolation::Ingested,
        image_digest: None,
        rules_digest: Some("rules-abc".to_owned()),
        advisory_db: None,
        command_policy: CommandPolicy::default(),
        source: SourceIdentity {
            commit: Some("c0ffee".to_owned()),
            tree: Some("treeabc".to_owned()),
            lockfile_blob: Some("lock123".to_owned()),
        },
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:04Z".to_owned(),
        exit_status: 1,
        report_digest: report_digest.to_owned(),
    }
}

fn finding(advisory: &str, package: &str) -> Finding {
    Finding {
        key: FindingKey::new("cargo-audit", &[advisory, package, "0.10.5", "lock123"])
            .expect("key"),
        rule: advisory.to_owned(),
        severity: Severity::High,
        title: format!("{package} is vulnerable"),
        message: format!("{advisory}: upgrade {package}"),
        path: Some("Cargo.lock".to_owned()),
        span: None,
        meta: serde_json::json!({ "package": package }),
    }
}

/// Ingesting the same report twice must change nothing: same findings, same
/// counts, no duplicates, no growth.
#[test]
fn re_ingesting_the_same_report_is_idempotent() {
    let mut store = Store::open_in_memory().expect("store");
    let run = run("cargo-audit", "digest-1");
    let findings = vec![
        finding("RUSTSEC-2024-0001", "openssl"),
        finding("RUSTSEC-2024-0002", "time"),
    ];

    let first = store
        .replace_findings_layer(&run, &findings)
        .expect("first ingest");
    assert_eq!(first.findings, 2);
    assert_eq!(first.removed, 0);
    assert!(!first.replaced, "nothing to replace on a first ingest");

    let before = store.findings_layers(None).expect("layers");

    let second = store
        .replace_findings_layer(&run, &findings)
        .expect("second ingest");
    assert_eq!(second.findings, 2);
    assert_eq!(second.removed, 2, "the previous layer's rows are removed");
    assert!(second.replaced);

    assert_eq!(store.finding_count().expect("count"), 2, "no growth");
    assert_eq!(store.analysis_run_count().expect("runs"), 1, "one live run");
    assert_eq!(
        store.findings_layers(None).expect("layers"),
        before,
        "a repeat ingest is observationally a no-op"
    );
}

/// The point of a replaceable layer: a finding that has been fixed disappears,
/// and the rows it owned are actually gone — not orphaned behind a deleted run.
#[test]
fn a_fixed_finding_disappears_and_orphans_nothing() {
    let mut store = Store::open_in_memory().expect("store");
    let first = vec![
        finding("RUSTSEC-2024-0001", "openssl"),
        finding("RUSTSEC-2024-0002", "time"),
    ];
    store
        .replace_findings_layer(&run("cargo-audit", "digest-1"), &first)
        .expect("run 1");
    assert_eq!(store.finding_count().expect("count"), 2);

    // Run 2: `time` was upgraded, so only the openssl advisory remains.
    let second = vec![finding("RUSTSEC-2024-0001", "openssl")];
    let applied = store
        .replace_findings_layer(&run("cargo-audit", "digest-2"), &second)
        .expect("run 2");
    assert_eq!(applied.removed, 2, "both previous rows were deleted");
    assert_eq!(applied.findings, 1);

    let layers = store.findings_layers(None).expect("layers");
    assert_eq!(layers.len(), 1);
    let keys: Vec<String> = layers[0].findings.iter().map(|f| f.key.render()).collect();
    assert_eq!(keys.len(), 1);
    assert!(
        keys[0].contains("RUSTSEC-2024-0001"),
        "the fixed finding must be gone, got {keys:?}"
    );

    // The owned-record cleanup this store had to implement itself: no row is
    // left pointing at a run that no longer exists.
    assert_eq!(store.finding_count().expect("count"), 1);
    assert_eq!(
        store.orphan_finding_count().expect("orphans"),
        0,
        "obsolete findings must be deleted, not orphaned"
    );
    // And the run itself was replaced, not accumulated.
    assert_eq!(store.analysis_run_count().expect("runs"), 1);
    assert_eq!(layers[0].run.report_digest, "digest-2");
}

/// The single most important assertion in this change: the published artifact is
/// a pure function of the tree, so ingesting findings cannot alter it by one byte.
#[test]
fn export_factset_is_byte_identical_across_an_ingest() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let before = GraphArtifact::from_store(&store)
        .expect("capture")
        .to_json()
        .expect("json");

    store
        .replace_findings_layer(
            &run("cargo-audit", "digest-1"),
            &[finding("RUSTSEC-2024-0001", "openssl")],
        )
        .expect("ingest");
    // A second, *different* ingest — including a removal — must not move it either.
    store
        .replace_findings_layer(&run("cargo-audit", "digest-2"), &[])
        .expect("second ingest");

    let after = GraphArtifact::from_store(&store)
        .expect("capture")
        .to_json()
        .expect("json");

    assert_eq!(
        before, after,
        "GraphArtifact must remain byte-identical across findings ingest"
    );
    assert_eq!(
        store.export_factset().expect("factset"),
        FactSet {
            nodes: store.all_nodes().expect("nodes"),
            edges: store.all_edges().expect("edges"),
        }
    );
}

/// Nothing an analyzer reports may become a node or an edge. Asserted as counts,
/// because the failure mode is a single extra row, not a visible crash.
#[test]
fn ingest_adds_no_nodes_and_no_edges() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let nodes = store.node_count().expect("nodes");
    let edges = store.edge_count().expect("edges");

    store
        .replace_findings_layer(
            &run("cargo-audit", "digest-1"),
            &[
                finding("RUSTSEC-2024-0001", "openssl"),
                finding("RUSTSEC-2024-0002", "time"),
            ],
        )
        .expect("ingest");

    assert_eq!(store.node_count().expect("nodes"), nodes);
    assert_eq!(store.edge_count().expect("edges"), edges);
    assert_eq!(store.finding_count().expect("findings"), 2);
}

/// `authored` means a human or agent deliberately wrote this in a reviewed file,
/// and carries a +40 relevance boost. Unreviewed tool output must never ride it.
#[test]
fn findings_do_not_appear_in_search_and_never_rank_as_authored() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .replace_findings_layer(
            &run("cargo-audit", "digest-1"),
            &[finding("RUSTSEC-2024-0001", "openssl")],
        )
        .expect("ingest");

    // The finding's own words find nothing: it is not in the ranked corpus at all.
    assert!(
        search(&store, "RUSTSEC-2024-0001", 20)
            .expect("search")
            .is_empty(),
        "a finding must not be searchable as a graph fact"
    );
    for query in ["openssl", "vulnerable", "finding"] {
        let hits = search(&store, query, 20).expect("search");
        assert!(
            hits.iter().all(|h| !h.node.key.starts_with("finding:")),
            "`{query}` surfaced a finding as a search hit"
        );
    }
    // The authored boost still belongs to the authored layer, and only to it.
    let adr = search(&store, "Build Roteiro", 5).expect("search");
    assert_eq!(adr.first().map(|h| h.node.key.as_str()), Some("adr:0001"));
}

/// Deleting a layer removes its findings too — the same owned-record rule as
/// replacement, exercised on the explicit path.
#[test]
fn deleting_a_layer_removes_the_rows_it_owned() {
    let mut store = Store::open_in_memory().expect("store");
    let layer = layer_key("cargo-audit", &worktree()).expect("layer");
    store
        .replace_findings_layer(
            &run("cargo-audit", "digest-1"),
            &[finding("RUSTSEC-2024-0001", "openssl")],
        )
        .expect("ingest");

    assert_eq!(
        store.delete_findings_layer(&layer).expect("delete"),
        Some(1)
    );
    assert_eq!(store.finding_count().expect("count"), 0);
    assert_eq!(store.analysis_run_count().expect("runs"), 0);
    assert_eq!(store.orphan_finding_count().expect("orphans"), 0);
    assert_eq!(
        store.delete_findings_layer(&layer).expect("delete again"),
        None,
        "deleting an absent layer is not an error"
    );
}

/// One analyzer's layer is independent of another's: re-running `semgrep` must
/// not disturb `cargo-audit`'s findings, and listing can narrow to either.
#[test]
fn layers_are_scoped_per_analyzer() {
    let mut store = Store::open_in_memory().expect("store");
    store
        .replace_findings_layer(
            &run("cargo-audit", "digest-1"),
            &[finding("RUSTSEC-2024-0001", "openssl")],
        )
        .expect("audit");

    let semgrep_run = run("semgrep", "digest-s1");
    let semgrep_finding = Finding {
        key: FindingKey::new(
            "semgrep",
            &["rules.rust.tls", "src/tls.rs", "1024", "9f8e7d"],
        )
        .expect("key"),
        rule: "rules.rust.tls".to_owned(),
        severity: Severity::Medium,
        title: "TLS verification disabled".to_owned(),
        message: "danger_accept_invalid_certs".to_owned(),
        path: Some("src/tls.rs".to_owned()),
        span: Some(rto_graph::Span::new(1024, 1088)),
        meta: serde_json::Value::Null,
    };
    store
        .replace_findings_layer(&semgrep_run, std::slice::from_ref(&semgrep_finding))
        .expect("semgrep");

    assert_eq!(store.finding_count().expect("count"), 2);
    let all = store.findings_layers(None).expect("all");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].run.analyzer, "cargo-audit", "ordered by layer key");

    let only_semgrep = store.findings_layers(Some("semgrep")).expect("filtered");
    assert_eq!(only_semgrep.len(), 1);
    assert_eq!(only_semgrep[0].findings, vec![semgrep_finding]);
    // Round-tripping through storage preserves the span and the typed key.
    assert_eq!(
        only_semgrep[0].findings[0].span.map(|s| (s.start, s.end)),
        Some((1024, 1088))
    );

    // Re-running semgrep leaves cargo-audit alone.
    store
        .replace_findings_layer(&run("semgrep", "digest-s2"), &[])
        .expect("semgrep again");
    assert_eq!(
        store
            .findings_layers(Some("cargo-audit"))
            .expect("audit")
            .len(),
        1
    );
    assert_eq!(store.finding_count().expect("count"), 1);
}

/// A duplicate identity within one layer is a producer bug. It must be refused,
/// and — because it is refused inside the transaction — must leave the previously
/// stored layer exactly as it was.
#[test]
fn a_duplicate_identity_is_refused_without_a_partial_write() {
    let mut store = Store::open_in_memory().expect("store");
    let good = vec![finding("RUSTSEC-2024-0001", "openssl")];
    store
        .replace_findings_layer(&run("cargo-audit", "digest-1"), &good)
        .expect("ingest");
    let before = store.findings_layers(None).expect("layers");

    let dupes = vec![
        finding("RUSTSEC-2024-0002", "time"),
        finding("RUSTSEC-2024-0002", "time"),
    ];
    assert!(
        store
            .replace_findings_layer(&run("cargo-audit", "digest-2"), &dupes)
            .is_err(),
        "a duplicate finding identity must be refused"
    );

    assert_eq!(
        store.findings_layers(None).expect("layers"),
        before,
        "a refused ingest must not disturb the stored layer"
    );
    assert_eq!(store.finding_count().expect("count"), 1);
    assert_eq!(store.orphan_finding_count().expect("orphans"), 0);
}

/// A stored layer decodes back to exactly what was written — the evidence chain
/// (policy, digests, source identity, timestamps, exit status) survives storage.
#[test]
fn the_evidence_chain_round_trips() {
    let mut store = Store::open_in_memory().expect("store");
    let mut written = run("cargo-audit", "digest-1");
    written.advisory_db = Some(rto_graph::AdvisoryDb {
        digest: "advisory-abc".to_owned(),
        published_at: Some("2026-08-01T00:00:00Z".to_owned()),
    });
    written.image_digest = Some("sha256:deadbeef".to_owned());
    let findings = vec![finding("RUSTSEC-2024-0001", "openssl")];
    store
        .replace_findings_layer(&written, &findings)
        .expect("ingest");

    let layers = store.findings_layers(None).expect("layers");
    assert_eq!(
        layers,
        vec![FindingsLayer {
            run: written,
            findings
        }]
    );
}
