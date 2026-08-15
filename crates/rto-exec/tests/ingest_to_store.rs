//! The whole ingest path, end to end: a normalized report goes in through the
//! [`AnalyzerRunner`] contract and comes out as a persisted, replaceable findings
//! layer — with the graph provably untouched.
//!
//! The unit tests in each crate check the parts. These check the seam: that what
//! a future sandboxed backend will hand to the store is exactly what ingest hands
//! it today, and that nothing along the way can move the published artifact.

use rto_exec::{AnalysisRequest, AnalyzerRunner, Consent, ExecError, IngestRunner, Worktree};
use rto_graph::{
    Edge, EdgeKind, FactSet, GraphArtifact, Node, NodeKind, Provenance, SourceIdentity, Store,
};

fn request() -> AnalysisRequest {
    AnalysisRequest {
        analyzer: "cargo-audit".to_owned(),
        worktree: Worktree::read_only("/repo".as_ref()).expect("worktree"),
        network: rto_graph::NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: SourceIdentity {
            commit: Some("c0ffee".to_owned()),
            tree: Some("treeabc".to_owned()),
            lockfile_blob: Some("lock123".to_owned()),
        },
    }
}

/// A report naming the given advisories, in the interchange format producers
/// emit. Written as text on purpose: this is what a CI job actually hands over,
/// and a change that broke the wire format would be invisible in a builder.
fn report(advisories: &[(&str, &str)]) -> Vec<u8> {
    let findings: Vec<String> = advisories
        .iter()
        .map(|(advisory, package)| {
            format!(
                r#"{{
                    "identity": ["{advisory}", "{package}", "0.10.5", "lock123"],
                    "rule": "{advisory}",
                    "severity": "high",
                    "title": "{package} is vulnerable",
                    "message": "{advisory}: upgrade {package}",
                    "path": "Cargo.lock"
                }}"#
            )
        })
        .collect();
    format!(
        r#"{{
            "schema": "roteiro.findings/v1",
            "analyzer": "cargo-audit",
            "analyzer_version": "0.21.0",
            "started_at": "2026-08-15T09:00:00Z",
            "ended_at": "2026-08-15T09:00:04Z",
            "exit_status": {status},
            "advisory_db": {{ "digest": "advisory-abc", "published_at": "2026-08-01T00:00:00Z" }},
            "findings": [{findings}]
        }}"#,
        status = i32::from(!advisories.is_empty()),
        findings = findings.join(",")
    )
    .into_bytes()
}

/// Ingest a report into `store` exactly as the CLI does.
fn ingest(store: &mut Store, bytes: Vec<u8>) -> Result<rto_graph::FindingsApplied, ExecError> {
    let response = IngestRunner::new(bytes).run(&request())?;
    Ok(store
        .replace_findings_layer(&response.run, &response.findings)
        .expect("persist"))
}

fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    facts.nodes = vec![
        Node::new("adr:0001", NodeKind::Adr, "Build Roteiro").with_provenance(Provenance::Authored),
        Node::new("file:Cargo.lock", NodeKind::File, "Cargo.lock"),
    ];
    facts.edges = vec![Edge::authored(
        "adr:0001",
        "file:Cargo.lock",
        EdgeKind::References,
    )];
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
}

#[test]
fn ingesting_the_same_report_twice_changes_nothing() {
    let mut store = Store::open_in_memory().expect("store");
    let bytes = report(&[
        ("RUSTSEC-2024-0001", "openssl"),
        ("RUSTSEC-2024-0002", "time"),
    ]);

    let first = ingest(&mut store, bytes.clone()).expect("first");
    assert_eq!(first.findings, 2);
    assert!(!first.replaced);
    let after_first = store.findings_layers(None).expect("layers");

    let second = ingest(&mut store, bytes).expect("second");
    assert!(second.replaced);
    assert_eq!(store.finding_count().expect("count"), 2, "no growth");
    assert_eq!(store.analysis_run_count().expect("runs"), 1);
    assert_eq!(store.findings_layers(None).expect("layers"), after_first);
}

#[test]
fn a_fixed_finding_disappears_on_the_next_ingest() {
    let mut store = Store::open_in_memory().expect("store");
    ingest(
        &mut store,
        report(&[
            ("RUSTSEC-2024-0001", "openssl"),
            ("RUSTSEC-2024-0002", "time"),
        ]),
    )
    .expect("run 1");

    // `time` was upgraded between runs; CI's next report simply omits it.
    let applied = ingest(&mut store, report(&[("RUSTSEC-2024-0001", "openssl")])).expect("run 2");
    assert_eq!(applied.removed, 2, "the whole previous layer was cleared");

    let layers = store.findings_layers(None).expect("layers");
    assert_eq!(layers[0].findings.len(), 1);
    assert_eq!(layers[0].findings[0].rule, "RUSTSEC-2024-0001");
    assert_eq!(store.finding_count().expect("count"), 1);
    assert_eq!(
        store.orphan_finding_count().expect("orphans"),
        0,
        "the removed finding's row must be gone, not orphaned"
    );

    // A clean run empties the layer entirely rather than leaving stale findings.
    ingest(&mut store, report(&[])).expect("clean run");
    assert_eq!(store.finding_count().expect("count"), 0);
    assert_eq!(store.analysis_run_count().expect("runs"), 1);
    assert_eq!(
        store.findings_layers(None).expect("layers")[0]
            .run
            .exit_status,
        0
    );
}

#[test]
fn the_graph_artifact_is_byte_identical_across_the_whole_ingest_path() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let before = GraphArtifact::from_store(&store)
        .expect("capture")
        .to_json()
        .expect("json");
    let nodes = store.node_count().expect("nodes");
    let edges = store.edge_count().expect("edges");

    ingest(
        &mut store,
        report(&[
            ("RUSTSEC-2024-0001", "openssl"),
            ("RUSTSEC-2024-0002", "time"),
        ]),
    )
    .expect("ingest");
    ingest(&mut store, report(&[("RUSTSEC-2024-0001", "openssl")])).expect("re-ingest");

    let after = GraphArtifact::from_store(&store)
        .expect("capture")
        .to_json()
        .expect("json");
    assert_eq!(before, after, "ingest must not move the published artifact");
    assert_eq!(store.node_count().expect("nodes"), nodes);
    assert_eq!(store.edge_count().expect("edges"), edges);
    assert_eq!(store.finding_count().expect("findings"), 1);
}

#[test]
fn a_hostile_report_is_refused_with_a_clear_error_and_no_partial_write() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    ingest(&mut store, report(&[("RUSTSEC-2024-0001", "openssl")])).expect("good report");
    let before = store.findings_layers(None).expect("layers");
    let artifact = GraphArtifact::from_store(&store)
        .expect("capture")
        .to_json()
        .expect("json");

    // A report claiming a finding outside the analyzed tree, with a rule id
    // chosen to look plausible. It must not reach the store at all.
    let hostile = br#"{
        "schema": "roteiro.findings/v1",
        "analyzer": "cargo-audit",
        "analyzer_version": "0.21.0",
        "started_at": "2026-08-15T10:00:00Z",
        "ended_at": "2026-08-15T10:00:01Z",
        "exit_status": 1,
        "findings": [{
            "identity": ["RUSTSEC-2024-9999", "evil", "1.0.0", "lock123"],
            "rule": "RUSTSEC-2024-9999",
            "severity": "critical",
            "title": "look over here",
            "path": "../../../etc/passwd"
        }]
    }"#;
    let err = ingest(&mut store, hostile.to_vec()).expect_err("must be refused");
    assert!(
        matches!(err, ExecError::PathEscapesWorktree(_)),
        "expected a path-escape error, got {err}"
    );
    assert!(
        err.to_string().contains("escapes the worktree"),
        "the error must say what was wrong: {err}"
    );

    assert_eq!(
        store.findings_layers(None).expect("layers"),
        before,
        "a refused report must leave the previous layer untouched"
    );
    assert_eq!(store.finding_count().expect("count"), 1);
    assert_eq!(
        GraphArtifact::from_store(&store)
            .expect("capture")
            .to_json()
            .expect("json"),
        artifact
    );

    // Truncated JSON is refused the same way: an error, never a panic.
    let truncated = ingest(&mut store, b"{\"schema\": \"roteiro.find".to_vec());
    assert!(matches!(truncated, Err(ExecError::Json(_))));
    assert_eq!(store.finding_count().expect("count"), 1);
}

#[test]
fn an_ingested_run_records_the_evidence_a_later_backend_will_record_too() {
    let mut store = Store::open_in_memory().expect("store");
    ingest(&mut store, report(&[("RUSTSEC-2024-0001", "openssl")])).expect("ingest");

    let layers = store.findings_layers(Some("cargo-audit")).expect("layers");
    assert_eq!(layers.len(), 1);
    let run = &layers[0].run;
    assert_eq!(run.runner, rto_graph::RunnerKind::Ingested);
    assert_eq!(run.isolation, rto_graph::Isolation::Ingested);
    assert_eq!(run.analyzer_version, "0.21.0");
    assert_eq!(
        run.advisory_db.as_ref().map(|db| db.digest.as_str()),
        Some("advisory-abc")
    );
    assert_eq!(
        run.advisory_db
            .as_ref()
            .and_then(|db| db.published_at.as_deref()),
        Some("2026-08-01T00:00:00Z"),
        "staleness must be recordable, so a result is never labelled `current` by default"
    );
    assert_eq!(run.command_policy.network, rto_graph::NetworkPolicy::Deny);
    assert_eq!(
        run.command_policy.worktree,
        rto_graph::WorktreeAccess::ReadOnly
    );
    assert_eq!(run.source.commit.as_deref(), Some("c0ffee"));
    assert_eq!(run.source.lockfile_blob.as_deref(), Some("lock123"));
    assert_eq!(run.report_digest.len(), 64, "a full SHA-256 of the report");
    assert!(run.layer.starts_with("security:cargo-audit:"));
}
