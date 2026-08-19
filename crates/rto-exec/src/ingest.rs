//! The ingest backend: a normalized report produced elsewhere, read in as if it
//! had been produced here.
//!
//! This is the zero-install default of ADR-0014, and the first implementation of
//! the [`AnalyzerRunner`] contract. It performs no execution and opens no
//! network connection: the analyzer already ran, in CI or in a developer's own
//! tooling, and what arrives is its normalized output. What it *does* do is
//! validate that output strictly — a report is untrusted input, and a malformed
//! or hostile one must be refused with a clear error, before anything is written.

use std::collections::HashSet;

use rto_graph::{
    AdvisoryDb, AnalysisRun, CommandPolicy, EnvironmentPolicy, Finding, FindingKey, Isolation,
    RunnerKind, Severity, SourceIdentity, Span, is_valid_analyzer_id, layer_key,
};
use serde::{Deserialize, Serialize};

use crate::adapter::{NativeContext, adapter_for, known_analyzers};
use crate::runner::{
    AnalysisRequest, AnalysisResponse, AnalyzerRunner, ExecError, check_reported_path,
    check_request,
};
use crate::sha256_hex;

/// Schema tag every normalized report must carry. Bump on a breaking change to
/// the report format, exactly as [`rto_graph::ARTIFACT_SCHEMA`] does for the
/// graph artifact.
pub const REPORT_SCHEMA: &str = "roteiro.findings/v1";

/// The most findings accepted from one report.
///
/// A ceiling, not a target: a report claiming more than this is a runaway or
/// hostile producer, and refusing it up front is better than letting it bloat the
/// store one row at a time.
pub const MAX_REPORT_FINDINGS: usize = 100_000;

/// One finding as it appears in a normalized report.
///
/// `identity` is the analyzer's **own** ordered identity recipe, not a fixed set
/// of fields, which is what lets a new analyzer slot in without a schema change:
///
/// ```text
/// semgrep:     ["<rule>", "<path>", "<start-byte>", "<snippet-hash>"]
/// cargo-audit: ["<advisory>", "<pkg>", "<version>", "<lockfile-blob>"]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFinding {
    /// The analyzer's ordered identity components for this finding.
    pub identity: Vec<String>,
    /// The rule, advisory or check id that fired.
    pub rule: String,
    /// The severity the analyzer assigned.
    pub severity: Severity,
    /// One-line summary.
    pub title: String,
    /// The analyzer's full message.
    #[serde(default)]
    pub message: String,
    /// Repository-relative path the finding is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Byte span within that path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// Anything else the analyzer reported, kept verbatim.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub meta: serde_json::Value,
}

/// A normalized analyzer report — the interchange format `roteiro security
/// ingest` consumes and every analyzer adapter emits.
///
/// Unknown fields are **not** rejected: the schema tag carries versioning, so a
/// producer may add diagnostics without breaking older readers. Everything the
/// evidence chain needs is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedReport {
    /// Schema tag ([`REPORT_SCHEMA`]).
    pub schema: String,
    /// The analyzer id.
    pub analyzer: String,
    /// The analyzer's version.
    pub analyzer_version: String,
    /// When the analyzer started, as the producer recorded it.
    pub started_at: String,
    /// When it finished.
    pub ended_at: String,
    /// Its process exit status.
    #[serde(default)]
    pub exit_status: i32,
    /// Digest of the rule set it ran with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_digest: Option<String>,
    /// Digest of the container image it ran in, where one was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// The pinned advisory database it consulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_db: Option<AdvisoryDb>,
    /// The source identity it ran against.
    #[serde(default)]
    pub source: SourceIdentity,
    /// The findings it produced.
    #[serde(default)]
    pub findings: Vec<ReportFinding>,
}

/// Consumes a normalized report and yields the same values any other backend
/// would.
///
/// The report bytes are held verbatim rather than pre-parsed, because the run's
/// `report_digest` must be the digest of exactly what arrived — the tie between
/// the stored findings and the file they came from.
#[derive(Debug, Clone)]
pub struct IngestRunner {
    report: Vec<u8>,
}

impl IngestRunner {
    /// Build a runner over the raw bytes of a normalized report.
    #[must_use]
    pub fn new(report: impl Into<Vec<u8>>) -> Self {
        Self {
            report: report.into(),
        }
    }
}

impl AnalyzerRunner for IngestRunner {
    fn kind(&self) -> RunnerKind {
        RunnerKind::Ingested
    }

    fn isolation(&self) -> Isolation {
        // Nothing executed locally, so there is no boundary to claim — and a
        // report from an unknown CI job is exactly the case where an
        // over-claimed isolation label would be misleading.
        Isolation::Ingested
    }

    fn run(&self, request: &AnalysisRequest) -> Result<AnalysisResponse, ExecError> {
        check_request(request)?;
        let report: NormalizedReport = serde_json::from_slice(&self.report)?;
        assemble(report, request, self.kind(), self.isolation(), &self.report)
    }
}

/// Turn one analyzer's **native** output into a normalized report, using that
/// analyzer's adapter.
///
/// This is the single conversion both execution paths go through: a subprocess
/// run hands it the bytes it captured from the analyzer's stdout, and
/// `roteiro security ingest` hands it the bytes of a report file produced by the
/// same analyzer in CI. The resulting [`Finding`]s are equal because they came
/// out of the same function, not because two implementations were checked
/// against each other.
///
/// # Errors
/// Returns [`ExecError::UnknownAnalyzer`] if this build has no adapter for
/// `analyzer`, or whatever the adapter raises for output it cannot read.
pub fn normalize_native(
    analyzer: &str,
    native: &[u8],
    ctx: &NativeContext<'_>,
) -> Result<NormalizedReport, ExecError> {
    let adapter = adapter_for(analyzer).ok_or_else(|| ExecError::UnknownAnalyzer {
        requested: analyzer.to_owned(),
        known: known_analyzers().join(", "),
    })?;
    adapter.normalize(native, ctx)
}

/// Validate a normalized report and build the response a backend returns.
///
/// Shared by every backend so the validation, the identity keys, the ordering
/// and the evidence chain are written once. `raw` is the exact bytes the report
/// was derived from — the analyzer's stdout for a subprocess run, the file's
/// contents for an ingest — because `report_digest` identifies *those bytes*,
/// not the parsed value.
pub(crate) fn assemble(
    report: NormalizedReport,
    request: &AnalysisRequest,
    runner: RunnerKind,
    isolation: Isolation,
    raw: &[u8],
) -> Result<AnalysisResponse, ExecError> {
    validate_report(&report, &request.analyzer)?;
    let findings = normalize_findings(&report)?;
    let layer = layer_key(&request.analyzer, &request.worktree.id)?;
    let run = AnalysisRun {
        layer,
        analyzer: report.analyzer,
        analyzer_version: report.analyzer_version,
        runner,
        isolation,
        image_digest: report.image_digest,
        rules_digest: report.rules_digest,
        advisory_db: report.advisory_db,
        // The policy the run was executed under. For ingest that is trivially
        // honoured — it opened no socket and did not write the tree. A backend
        // that really executes something records what it enforced, and says so
        // in its own documentation where "enforced" overstates the case.
        command_policy: CommandPolicy {
            network: request.network,
            worktree: request.worktree.access,
            environment: EnvironmentPolicy::Scrubbed,
        },
        // The caller's knowledge of the source identity wins where it has any;
        // otherwise the report's own record stands.
        source: merge_source(&request.source, report.source),
        started_at: report.started_at,
        ended_at: report.ended_at,
        exit_status: report.exit_status,
        report_digest: sha256_hex(raw),
    };
    Ok(AnalysisResponse { run, findings })
}

/// Prefer the caller's source identity component-by-component, falling back to
/// the report's. A producer knows the lockfile blob it resolved; a caller knows
/// which checkout it is standing in.
fn merge_source(requested: &SourceIdentity, reported: SourceIdentity) -> SourceIdentity {
    SourceIdentity {
        commit: requested.commit.clone().or(reported.commit),
        tree: requested.tree.clone().or(reported.tree),
        lockfile_blob: requested.lockfile_blob.clone().or(reported.lockfile_blob),
    }
}

/// Check everything about a report that must hold before any of it is trusted.
fn validate_report(report: &NormalizedReport, requested: &str) -> Result<(), ExecError> {
    if report.schema != REPORT_SCHEMA {
        return Err(ExecError::UnsupportedSchema {
            found: report.schema.clone(),
            expected: REPORT_SCHEMA,
        });
    }
    if !is_valid_analyzer_id(&report.analyzer) {
        return Err(ExecError::InvalidAnalyzerId(report.analyzer.clone()));
    }
    if report.analyzer != requested {
        return Err(ExecError::AnalyzerMismatch {
            requested: requested.to_owned(),
            reported: report.analyzer.clone(),
        });
    }
    // The evidence chain is the reason this store exists; a run that cannot say
    // what version ran, or when, is not evidence.
    for (field, value) in [
        ("analyzer_version", &report.analyzer_version),
        ("started_at", &report.started_at),
        ("ended_at", &report.ended_at),
    ] {
        if value.trim().is_empty() {
            return Err(ExecError::MalformedReport(format!("{field} is empty")));
        }
    }
    if report.findings.len() > MAX_REPORT_FINDINGS {
        return Err(ExecError::TooManyFindings {
            count: report.findings.len(),
            max: MAX_REPORT_FINDINGS,
        });
    }
    Ok(())
}

/// Turn a validated report's findings into normalized [`Finding`]s, ordered by
/// their stable identity so an unchanged report always produces an identical
/// sequence.
fn normalize_findings(report: &NormalizedReport) -> Result<Vec<Finding>, ExecError> {
    let mut seen: HashSet<String> = HashSet::with_capacity(report.findings.len());
    let mut out = Vec::with_capacity(report.findings.len());
    for reported in &report.findings {
        if reported.rule.trim().is_empty() {
            return Err(ExecError::MalformedReport(
                "a finding has an empty rule id".to_owned(),
            ));
        }
        if reported.title.trim().is_empty() {
            return Err(ExecError::MalformedReport(format!(
                "finding {:?} has an empty title",
                reported.rule
            )));
        }
        if let Some(path) = &reported.path {
            check_reported_path(path)?;
        }
        if let Some(span) = reported.span
            && span.end < span.start
        {
            return Err(ExecError::MalformedReport(format!(
                "finding {:?} has a span that runs backwards ({}..{})",
                reported.rule, span.start, span.end
            )));
        }
        let key = FindingKey::new(&report.analyzer, &reported.identity)?;
        let rendered = key.render();
        if !seen.insert(rendered.clone()) {
            return Err(ExecError::DuplicateFinding(rendered));
        }
        out.push(Finding {
            key,
            rule: reported.rule.clone(),
            severity: reported.severity.clone(),
            title: reported.title.clone(),
            message: reported.message.clone(),
            path: reported.path.clone(),
            span: reported.span,
            meta: reported.meta.clone(),
        });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        IngestRunner, MAX_REPORT_FINDINGS, NormalizedReport, REPORT_SCHEMA, ReportFinding,
    };
    use crate::runner::{
        AnalysisRequest, AnalysisResponse, AnalyzerRunner, Consent, ExecError, Worktree,
    };
    use rto_graph::{Isolation, NetworkPolicy, RunnerKind, Severity, SourceIdentity, Span};

    fn request() -> AnalysisRequest {
        AnalysisRequest {
            analyzer: "cargo-audit".to_owned(),
            worktree: Worktree::read_only("/repo".as_ref()).expect("worktree"),
            network: NetworkPolicy::Deny,
            consent: Consent::Granted,
            source: SourceIdentity::default(),
        }
    }

    fn report() -> NormalizedReport {
        NormalizedReport {
            schema: REPORT_SCHEMA.to_owned(),
            analyzer: "cargo-audit".to_owned(),
            analyzer_version: "0.21.0".to_owned(),
            started_at: "2026-08-15T09:00:00Z".to_owned(),
            ended_at: "2026-08-15T09:00:04Z".to_owned(),
            exit_status: 1,
            rules_digest: None,
            image_digest: None,
            advisory_db: None,
            source: SourceIdentity::default(),
            findings: vec![
                ReportFinding {
                    identity: vec![
                        "RUSTSEC-2024-0002".to_owned(),
                        "time".to_owned(),
                        "0.1.44".to_owned(),
                        "lock123".to_owned(),
                    ],
                    rule: "RUSTSEC-2024-0002".to_owned(),
                    severity: Severity::Medium,
                    title: "time is vulnerable".to_owned(),
                    message: "segfault".to_owned(),
                    path: Some("Cargo.lock".to_owned()),
                    span: None,
                    meta: serde_json::Value::Null,
                },
                ReportFinding {
                    identity: vec![
                        "RUSTSEC-2024-0001".to_owned(),
                        "openssl".to_owned(),
                        "0.10.5".to_owned(),
                        "lock123".to_owned(),
                    ],
                    rule: "RUSTSEC-2024-0001".to_owned(),
                    severity: Severity::High,
                    title: "openssl is vulnerable".to_owned(),
                    message: "upgrade".to_owned(),
                    path: Some("Cargo.lock".to_owned()),
                    span: Some(Span::new(10, 20)),
                    meta: serde_json::json!({"cvss": 9.1}),
                },
            ],
        }
    }

    fn ingest(report: &NormalizedReport) -> Result<AnalysisResponse, ExecError> {
        let bytes = serde_json::to_vec(report).expect("serialize");
        IngestRunner::new(bytes).run(&request())
    }

    #[test]
    fn ingests_a_well_formed_report_deterministically() {
        let response = ingest(&report()).expect("ingest");
        assert_eq!(response.run.runner, RunnerKind::Ingested);
        assert_eq!(response.run.isolation, Isolation::Ingested);
        assert_eq!(response.run.analyzer_version, "0.21.0");
        assert_eq!(response.run.exit_status, 1);
        assert_eq!(response.run.command_policy.network, NetworkPolicy::Deny);
        assert!(
            response.run.layer.starts_with("security:cargo-audit:"),
            "layer key was {}",
            response.run.layer
        );
        // Findings come back ordered by identity, not in report order, so an
        // unchanged report always produces an identical sequence.
        let keys: Vec<String> = response.findings.iter().map(|f| f.key.render()).collect();
        assert_eq!(
            keys,
            vec![
                "finding:cargo-audit:RUSTSEC-2024-0001:openssl:0.10.5:lock123",
                "finding:cargo-audit:RUSTSEC-2024-0002:time:0.1.44:lock123",
            ]
        );
        assert_eq!(response.findings[0].span.map(|s| s.start), Some(10));
    }

    #[test]
    fn the_report_digest_is_over_the_exact_bytes_received() {
        let bytes = serde_json::to_vec(&report()).expect("serialize");
        let digest = IngestRunner::new(bytes.clone())
            .run(&request())
            .expect("ingest")
            .run
            .report_digest;
        assert_eq!(digest, crate::sha256_hex(&bytes));

        // Whitespace changes the bytes, so it changes the digest — the digest
        // identifies the file, not the parsed content.
        let spaced = serde_json::to_vec_pretty(&report()).expect("serialize");
        let other = IngestRunner::new(spaced)
            .run(&request())
            .expect("ingest")
            .run
            .report_digest;
        assert_ne!(digest, other);
    }

    #[test]
    fn a_run_carries_the_source_identity_of_the_caller_not_the_report() {
        let mut req = request();
        req.source.commit = Some("c0ffee".to_owned());
        let mut rep = report();
        rep.source.commit = Some("stale".to_owned());
        rep.source.lockfile_blob = Some("lock123".to_owned());
        let bytes = serde_json::to_vec(&rep).expect("serialize");
        let run = IngestRunner::new(bytes).run(&req).expect("ingest").run;
        assert_eq!(run.source.commit.as_deref(), Some("c0ffee"));
        // …but keeps what only the producer knew.
        assert_eq!(run.source.lockfile_blob.as_deref(), Some("lock123"));
    }

    #[test]
    fn rejects_a_report_with_the_wrong_schema_tag() {
        let mut rep = report();
        rep.schema = "roteiro.findings/v999".to_owned();
        assert!(matches!(
            ingest(&rep),
            Err(ExecError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn rejects_a_report_from_a_different_analyzer() {
        let mut rep = report();
        rep.analyzer = "semgrep".to_owned();
        assert!(matches!(
            ingest(&rep),
            Err(ExecError::AnalyzerMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_report_missing_its_evidence() {
        for mutate in [
            (|r: &mut NormalizedReport| r.analyzer_version = String::new()) as fn(&mut _),
            |r: &mut NormalizedReport| r.started_at = "  ".to_owned(),
            |r: &mut NormalizedReport| r.ended_at = String::new(),
        ] {
            let mut rep = report();
            mutate(&mut rep);
            assert!(
                matches!(ingest(&rep), Err(ExecError::MalformedReport(_))),
                "a run with no evidence must be refused"
            );
        }
    }

    #[test]
    fn rejects_a_finding_with_no_stable_identity() {
        let mut rep = report();
        rep.findings[0].identity.clear();
        assert!(matches!(ingest(&rep), Err(ExecError::Identity(_))));
    }

    #[test]
    fn rejects_duplicate_identities_within_one_report() {
        let mut rep = report();
        rep.findings[1].identity = rep.findings[0].identity.clone();
        assert!(matches!(ingest(&rep), Err(ExecError::DuplicateFinding(_))));
    }

    #[test]
    fn rejects_a_finding_claiming_a_path_outside_the_worktree() {
        for hostile in ["/etc/shadow", "../../../etc/passwd"] {
            let mut rep = report();
            rep.findings[0].path = Some(hostile.to_owned());
            assert!(
                matches!(ingest(&rep), Err(ExecError::PathEscapesWorktree(_))),
                "{hostile:?} should be refused"
            );
        }
    }

    #[test]
    fn rejects_empty_rules_titles_and_backwards_spans() {
        let mut rep = report();
        rep.findings[0].rule = "  ".to_owned();
        assert!(matches!(ingest(&rep), Err(ExecError::MalformedReport(_))));

        let mut rep = report();
        rep.findings[0].title = String::new();
        assert!(matches!(ingest(&rep), Err(ExecError::MalformedReport(_))));

        let mut rep = report();
        rep.findings[0].span = Some(Span::new(90, 10));
        assert!(matches!(ingest(&rep), Err(ExecError::MalformedReport(_))));
    }

    #[test]
    fn rejects_a_runaway_report() {
        let mut rep = report();
        let template = rep.findings[0].clone();
        rep.findings = (0..=MAX_REPORT_FINDINGS)
            .map(|i| {
                let mut f = template.clone();
                f.identity[1] = format!("pkg{i}");
                f
            })
            .collect();
        assert!(matches!(
            ingest(&rep),
            Err(ExecError::TooManyFindings { .. })
        ));
    }

    #[test]
    fn rejects_bytes_that_are_not_a_report_at_all() {
        for junk in [
            &b"not json at all"[..],
            &b"[]"[..],
            &b"null"[..],
            &b"{\"schema\":\"roteiro.findings/v1\"}"[..],
        ] {
            assert!(
                matches!(
                    IngestRunner::new(junk.to_vec()).run(&request()),
                    Err(ExecError::Json(_))
                ),
                "{:?} should be refused as JSON",
                String::from_utf8_lossy(junk)
            );
        }
    }

    #[test]
    fn refuses_to_run_without_consent() {
        let mut req = request();
        req.consent = Consent::Withheld;
        let bytes = serde_json::to_vec(&report()).expect("serialize");
        assert!(matches!(
            IngestRunner::new(bytes).run(&req),
            Err(ExecError::ConsentRequired)
        ));
    }

    #[test]
    fn a_report_with_no_findings_is_a_valid_clean_run() {
        let mut rep = report();
        rep.findings.clear();
        rep.exit_status = 0;
        let response = ingest(&rep).expect("ingest");
        assert!(response.findings.is_empty());
        assert_eq!(response.run.exit_status, 0);
    }

    #[test]
    fn the_report_format_round_trips_through_json() {
        let rep = report();
        let json = serde_json::to_string(&rep).expect("serialize");
        assert_eq!(
            serde_json::from_str::<NormalizedReport>(&json).expect("deserialize"),
            rep
        );
    }
}
