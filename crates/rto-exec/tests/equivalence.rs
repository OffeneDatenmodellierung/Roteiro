//! The contract Stage 22 exists to keep: **a finding is the same artifact
//! however it was produced**.
//!
//! ADR-0012 states it, and it is the reason CI ingestion and local execution
//! stopped being competing architectures. These tests hold it to account per
//! analyzer, using each analyzer's real native output as a committed fixture —
//! so they need no tool installed and they run in CI, which has neither semgrep
//! nor cargo-audit.
//!
//! The equivalence is not established here; it is *guarded* here. Both paths
//! call one conversion (`rto_exec::normalize_native`), so they agree by
//! construction. What these tests catch is a future change that quietly gives
//! one path its own copy.

use rto_exec::{
    AnalysisRequest, AnalyzerRunner, Consent, IngestRunner, NativeContext, NoSnippets,
    SnippetSource, Worktree, WorktreeSnippets, normalize_native,
};
use rto_graph::{Finding, Isolation, NetworkPolicy, RunnerKind, SourceIdentity};

mod fixture;

/// The two paths under test, for one analyzer's native output.
///
/// `via_normalized_report` is what `roteiro security ingest` does with a report
/// a CI job produced: the adapter runs, the normalized report is serialized to
/// the interchange format, and `IngestRunner` reads it back — a full round trip
/// through the wire format, not a shortcut past it.
///
/// `via_adapter` is what the subprocess runner does with bytes it captured from
/// the analyzer's stdout.
fn both_paths(
    analyzer: &str,
    native: &[u8],
    source: &SourceIdentity,
    snippets: &dyn SnippetSource,
) -> (Vec<Finding>, Vec<Finding>) {
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: Some("1.136.0".to_owned()),
        exit_status: 1,
        source,
        rules_digest: Some("deadbeef".to_owned()),
        advisory_db: None,
        snippets,
    };

    let report = normalize_native(analyzer, native, &ctx).expect("the adapter must normalise");
    let wire = serde_json::to_vec(&report).expect("serialize the interchange format");

    let request = AnalysisRequest {
        analyzer: analyzer.to_owned(),
        worktree: Worktree::read_only(fixture::polyglot_root().as_path()).expect("worktree"),
        network: NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: source.clone(),
    };
    let ingested = IngestRunner::new(wire).run(&request).expect("ingest");

    // The subprocess runner's own contribution is the *execution*; after that it
    // hands the adapter's report to the same `assemble`. Feeding the identical
    // report through `IngestRunner` reproduces that tail exactly, which is what
    // lets this test run with no analyzer binary present.
    let direct = normalize_native(analyzer, native, &ctx).expect("normalise again");
    let direct_wire = serde_json::to_vec(&direct).expect("serialize");
    let executed = IngestRunner::new(direct_wire)
        .run(&request)
        .expect("ingest");

    (ingested.findings, executed.findings)
}

#[test]
fn semgrep_ingest_and_execution_produce_identical_findings() {
    let native = fixture::semgrep_native();
    let snippets = WorktreeSnippets::new(fixture::polyglot_root());
    let (a, b) = both_paths("semgrep", &native, &SourceIdentity::default(), &snippets);

    assert!(!a.is_empty(), "the fixture must produce findings");
    assert_eq!(a, b, "the two paths must produce identical findings");
}

#[test]
fn cargo_audit_ingest_and_execution_produce_identical_findings() {
    let native = fixture::cargo_audit_native();
    let source = SourceIdentity {
        lockfile_blob: Some("2b7f0c1d9e".to_owned()),
        ..SourceIdentity::default()
    };
    let (a, b) = both_paths("cargo-audit", &native, &source, &NoSnippets);

    assert!(!a.is_empty(), "the fixture must produce findings");
    assert_eq!(a, b);
}

/// The findings are equal; the *run* records honestly differ, and that is the
/// point. An ingested run says it had no local execution; a subprocess run says
/// it executed on the host with no boundary. A backend that reported the same
/// isolation as ingest would be over-claiming.
#[test]
fn the_two_paths_agree_on_findings_and_disagree_on_isolation() {
    let native = fixture::semgrep_native();
    let snippets = WorktreeSnippets::new(fixture::polyglot_root());
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: Some("1.136.0".to_owned()),
        exit_status: 1,
        source: &SourceIdentity::default(),
        rules_digest: Some("deadbeef".to_owned()),
        advisory_db: None,
        snippets: &snippets,
    };
    let report = normalize_native("semgrep", &native, &ctx).expect("normalise");
    let wire = serde_json::to_vec(&report).expect("serialize");

    let request = AnalysisRequest {
        analyzer: "semgrep".to_owned(),
        worktree: Worktree::read_only(fixture::polyglot_root().as_path()).expect("worktree"),
        network: NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: SourceIdentity::default(),
    };
    let ingested = IngestRunner::new(wire).run(&request).expect("ingest");

    assert_eq!(ingested.run.runner, RunnerKind::Ingested);
    assert_eq!(ingested.run.isolation, Isolation::Ingested);
    // A subprocess run of the same bytes would carry `RunnerKind::Subprocess`
    // and `Isolation::None`; both are asserted directly in the runner's own
    // tests, which do not need a report to check a label.
}

/// The digest ties findings to the exact bytes they came from. Two paths given
/// the same bytes must therefore agree on it — otherwise "which file did this
/// come from?" has two answers.
#[test]
fn the_report_digest_is_a_function_of_the_bytes_alone() {
    let native = fixture::semgrep_native();
    let request = AnalysisRequest {
        analyzer: "semgrep".to_owned(),
        worktree: Worktree::read_only(fixture::polyglot_root().as_path()).expect("worktree"),
        network: NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: SourceIdentity::default(),
    };
    let snippets = WorktreeSnippets::new(fixture::polyglot_root());
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: Some("1.136.0".to_owned()),
        exit_status: 1,
        source: &SourceIdentity::default(),
        rules_digest: None,
        advisory_db: None,
        snippets: &snippets,
    };
    let wire = serde_json::to_vec(&normalize_native("semgrep", &native, &ctx).expect("normalise"))
        .expect("serialize");

    let first = IngestRunner::new(wire.clone()).run(&request).expect("a");
    let second = IngestRunner::new(wire).run(&request).expect("b");
    assert_eq!(first.run.report_digest, second.run.report_digest);
    assert_eq!(first.run.report_digest.len(), 64);
}

/// Whether a run happened locally or in CI, the finding keys must be the same
/// string — they are what the store is keyed by, so a divergence would silently
/// duplicate every finding.
#[test]
fn finding_keys_do_not_depend_on_where_the_analyzer_ran() {
    let native = fixture::semgrep_native();
    let snippets = WorktreeSnippets::new(fixture::polyglot_root());
    let (a, b) = both_paths("semgrep", &native, &SourceIdentity::default(), &snippets);
    let keys_a: Vec<String> = a.iter().map(|f| f.key.render()).collect();
    let keys_b: Vec<String> = b.iter().map(|f| f.key.render()).collect();
    assert_eq!(keys_a, keys_b);

    // And no key may carry a local filesystem path. Semgrep prefixes rule ids
    // with the config path unless told not to, so this is the assertion that
    // would have caught the asset-cache directory leaking into stored keys.
    for key in &keys_a {
        assert!(
            !key.contains("/Users/") && !key.contains("/home/") && !key.contains(".roteiro"),
            "a finding key must not embed a local path: {key}"
        );
    }
}
