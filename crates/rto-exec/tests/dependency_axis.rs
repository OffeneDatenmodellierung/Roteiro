//! The dependency axis: `osv-scanner` over Python, Java, Node and Rust
//! manifests, and the Rust overlap with `cargo-audit`.
//!
//! This is ADR-0018's dependency column as a test rather than a claim. Three
//! kinds of test live here, and the difference matters:
//!
//! - **Fixture-driven** tests normalise real `osv-scanner` output captured once
//!   and committed (`tests/fixtures/native/osv-scanner-deps.json`). They run
//!   everywhere, including CI, which has no `osv-scanner`.
//! - **Cross-reference** tests build the two analyzers' layers and assert the
//!   join ADR-0018 v1.1 decided — including that it never halves a count.
//! - **Live** tests re-run the real binary and check the committed fixture still
//!   describes what it emits. They **self-skip with a visible message** when no
//!   `osv-scanner` is on `PATH` or no database is provisioned, so a skip is never
//!   silent and never passes for the wrong reason.
//!
//! Because the OSV databases are rebuilt daily, nothing here asserts an advisory
//! count. A re-capture legitimately reports more advisories than the last one.

use rto_exec::{NativeContext, NoSnippets, normalize_native};
use rto_graph::{Severity, SourceIdentity};

mod fixture;

fn ctx(worktree: Option<&std::path::Path>) -> NativeContext<'_> {
    static SOURCE: std::sync::LazyLock<SourceIdentity> =
        std::sync::LazyLock::new(SourceIdentity::default);
    NativeContext {
        started_at: "2026-08-16T09:00:00Z".to_owned(),
        ended_at: "2026-08-16T09:00:11Z".to_owned(),
        analyzer_version: Some("2.5.0".to_owned()),
        exit_status: 1,
        source: &SOURCE,
        rules_digest: None,
        advisory_db: None,
        worktree,
        snippets: &NoSnippets,
    }
}

fn normalize_fixture() -> rto_exec::NormalizedReport {
    let root = std::path::Path::new(fixture::CAPTURE_ROOT);
    normalize_native(
        "osv-scanner",
        &fixture::osv_scanner_native(),
        &ctx(Some(root)),
    )
    .expect("the adapter must normalise real osv-scanner output")
}

/// The headline requirement of this stage: the dependency axis, which was
/// Rust-only, now produces findings for Python, Java and Node as well.
#[test]
fn every_required_ecosystem_yields_at_least_one_finding() {
    let report = normalize_fixture();
    for (ecosystem, manifest) in fixture::REQUIRED_ECOSYSTEMS {
        let found = report
            .findings
            .iter()
            .filter(|f| f.meta["ecosystem"] == *ecosystem)
            .count();
        assert!(
            found > 0,
            "no {ecosystem} finding from {manifest}; the dependency axis claim in ADR-0018 \
             is not met by this build"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.path.as_deref() == Some(*manifest)),
            "no finding points at {manifest}"
        );
    }
}

/// The three ecosystems this stage exists to add. Stated separately from the
/// table above so the *gap being closed* is named in the failure message, not
/// just the general requirement.
#[test]
fn python_java_and_node_dependency_vulnerabilities_are_covered() {
    let report = normalize_fixture();
    for ecosystem in ["PyPI", "Maven", "npm"] {
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.meta["ecosystem"] == ecosystem),
            "{ecosystem} was the headline gap ADR-0018 named, and it is still open"
        );
    }
}

/// `osv-scanner` reports absolute paths even when told to scan `.`. Every stored
/// path must have been placed back inside the worktree — an absolute one would
/// carry the scanning machine's home directory into a persisted finding key, and
/// the shared preflight refuses it besides.
#[test]
fn no_finding_carries_an_absolute_path_from_the_scanning_machine() {
    let report = normalize_fixture();
    for finding in &report.findings {
        if let Some(path) = &finding.path {
            assert!(!path.starts_with('/'), "{path} is absolute");
            assert!(!path.contains(".."), "{path} climbs out of the tree");
            rto_exec::check_reported_path(path).expect("must pass the shared preflight");
        }
        assert!(
            !finding.identity_debug().contains(fixture::CAPTURE_ROOT),
            "the capture machine's root leaked into a finding identity"
        );
    }
}

/// The intra-analyzer duplicate: OSV carries a Rust advisory under both its
/// RUSTSEC and its GHSA id, and `osv-scanner` lists both. `groups` says they are
/// one advisory, and one finding is what must come out.
#[test]
fn an_advisory_listed_under_two_ids_becomes_one_finding() {
    let report = normalize_fixture();
    let time_findings: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.meta["package"] == "time")
        .collect();
    assert_eq!(
        time_findings.len(),
        1,
        "RUSTSEC-2020-0071 and GHSA-wcg3-cvx6-7396 are one advisory, not two"
    );
    let aliases: Vec<&str> = time_findings[0].meta["aliases"]
        .as_array()
        .expect("aliases")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    // Both ids stay addressable on the surviving finding, which is what the
    // cross-reference joins on.
    assert!(aliases.contains(&"RUSTSEC-2020-0071"), "{aliases:?}");
    assert!(aliases.contains(&"GHSA-wcg3-cvx6-7396"), "{aliases:?}");
    assert!(aliases.contains(&"CVE-2020-26235"), "{aliases:?}");
}

/// **Measured, not assumed.** ADR-0018 v1.0 said `osv-scanner` "does not report
/// `RustSec`'s `unmaintained`/`unsound`/`yanked` kinds". It does report
/// `unmaintained`: this fixture is real output from a default-flag run, and
/// `RUSTSEC-2024-0388` (`derivative` is unmaintained) is in it. v1.0 conflated
/// the database with the tool; v1.2 corrects it.
#[test]
fn informational_advisories_are_reported_by_the_scanner_not_only_carried_by_osv() {
    let report = normalize_fixture();
    let unmaintained = report
        .findings
        .iter()
        .find(|f| f.meta["package"] == "derivative")
        .expect("RUSTSEC-2024-0388 must be reported by a default-flag osv-scanner run");
    assert_eq!(unmaintained.meta["informational"], "unmaintained");
    // …and graded exactly as `cargo-audit` grades the same kind, so a
    // cross-referenced pair does not read as two different severities.
    assert_eq!(unmaintained.severity, Severity::Low);
}

/// Real output carries what a reader needs to act, not just an id.
#[test]
fn findings_carry_the_package_version_and_identifier_set() {
    let report = normalize_fixture();
    assert!(!report.findings.is_empty());
    for finding in &report.findings {
        assert!(finding.meta["package"].is_string(), "{:?}", finding.meta);
        assert!(finding.meta["version"].is_string());
        assert!(finding.meta["aliases"].is_array());
        assert!(!finding.title.trim().is_empty());
        assert!(!finding.rule.trim().is_empty());
        // Five identity components: advisory, ecosystem, package, version,
        // manifest.
        assert_eq!(finding.identity.len(), 5, "{:?}", finding.identity);
        assert!(finding.identity.iter().all(|c| !c.trim().is_empty()));
    }
}

/// A new analyzer needs no schema change. Stage 21 designed for exactly this:
/// `FindingKey` takes each analyzer's own ordered identity components, and the
/// schema never learns what they mean.
#[test]
fn the_new_analyzer_needs_no_migration() {
    let report = normalize_fixture();
    for finding in &report.findings {
        let key = rto_graph::FindingKey::new("osv-scanner", &finding.identity)
            .expect("a five-component identity is a valid finding key with no schema change");
        assert!(key.render().starts_with("finding:osv-scanner:"));
    }
    // `RunnerKind` already names the backends this analyzer runs under, so the
    // CHECK constraint on `analysis_runs` needs no widening either.
    assert_eq!(rto_graph::RunnerKind::Subprocess.as_str(), "subprocess");
    assert_eq!(rto_graph::RunnerKind::Ingested.as_str(), "ingested");
}

/// The whole point of a normalized report: the adapter output round-trips
/// through the interchange format unchanged, so a CI capture and a local run are
/// the same artifact.
#[test]
fn the_normalized_report_round_trips_through_the_wire_format() {
    let report = normalize_fixture();
    let wire = serde_json::to_vec(&report).expect("serialize");
    let back: rto_exec::NormalizedReport = serde_json::from_slice(&wire).expect("deserialize");
    assert_eq!(report, back);
}

// ---------------------------------------------------------------------------
// Live tests: real binary, real database. Self-skipping and visibly so.
// ---------------------------------------------------------------------------

/// Whether an `osv-scanner` binary is on `PATH`, printing why not when it is
/// not.
fn osv_scanner_available() -> bool {
    match std::process::Command::new("osv-scanner")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "SKIP: `osv-scanner --version` exited {:?}",
                output.status.code()
            );
            false
        }
        Err(e) => {
            eprintln!("SKIP: no osv-scanner on PATH ({e}); the fixture-driven tests still ran");
            false
        }
    }
}

/// The pinned OSV database directory, if this machine has one provisioned.
///
/// `ROTEIRO_OSV_DB` overrides it, which is how a developer points the live tests
/// at a database without provisioning the whole asset cache.
fn pinned_database() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("ROTEIRO_OSV_DB") {
        return Some(std::path::PathBuf::from(dir));
    }
    let path = rto_exec::asset_root().join("osv-db").join("db");
    path.join("osv-scalibr").is_dir().then_some(path)
}

/// Re-run the real tool and check the committed fixture still describes what it
/// emits.
///
/// It asserts *shape*, never an advisory count: the databases are rebuilt daily,
/// so a live run finding more than the capture did is correct behaviour, and a
/// test that failed on it would be noise.
#[cfg(feature = "exec-subprocess")]
#[test]
fn runs_the_real_analyzer_when_one_is_installed() {
    if !osv_scanner_available() {
        return;
    }
    let Some(database) = pinned_database() else {
        eprintln!(
            "SKIP: no OSV database provisioned. Run `roteiro security prefetch \
             --analyzer osv-scanner --allow-download`, or set ROTEIRO_OSV_DB."
        );
        return;
    };

    let root = fixture::deps_root();
    let entries = [("osv-db", database)];
    let invocation =
        rto_exec::adapter::osv_scanner::OsvScanner.command(&rto_exec::AssetPaths::new(&entries));
    let output = std::process::Command::new(&invocation.program)
        .args(&invocation.args)
        .current_dir(&root)
        .output()
        .expect("run osv-scanner");
    assert!(
        invocation
            .success_statuses
            .contains(&output.status.code().unwrap_or(-1)),
        "osv-scanner exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let live = normalize_native("osv-scanner", &output.stdout, &ctx(Some(&root)))
        .expect("normalise live output");

    // Every ecosystem still reports, and every path still comes out relative.
    for (ecosystem, _) in fixture::REQUIRED_ECOSYSTEMS {
        assert!(
            live.findings
                .iter()
                .any(|f| f.meta["ecosystem"] == *ecosystem),
            "live run produced no {ecosystem} finding"
        );
    }
    for finding in &live.findings {
        if let Some(path) = &finding.path {
            assert!(!path.starts_with('/'), "live run produced absolute {path}");
        }
    }
    // The committed fixture must still be a subset of what the tool reports:
    // advisories are added over time, effectively never removed.
    let live_rules: std::collections::BTreeSet<&str> =
        live.findings.iter().map(|f| f.rule.as_str()).collect();
    let fixture_report = normalize_fixture();
    let missing: Vec<&str> = fixture_report
        .findings
        .iter()
        .map(|f| f.rule.as_str())
        .filter(|rule| !live_rules.contains(rule))
        .collect();
    assert!(
        missing.is_empty(),
        "the committed fixture claims advisories the live tool no longer reports \
         (re-capture it): {missing:?}"
    );
}

/// Trait import for the live test's `command` call.
#[cfg(feature = "exec-subprocess")]
use rto_exec::Adapter as _;

/// Extension used by the path-leak test above: the identity components rendered
/// for a message.
trait IdentityDebug {
    fn identity_debug(&self) -> String;
}

impl IdentityDebug for rto_exec::ReportFinding {
    fn identity_debug(&self) -> String {
        self.identity.join("|")
    }
}

// ---------------------------------------------------------------------------
// The Rust overlap, over real output from *both* tools (ADR-0018 v1.1).
// ---------------------------------------------------------------------------

/// Build the two analyzers' live layers from the committed captures.
///
/// The two fixtures deliberately describe the same resolved versions —
/// `time 0.1.44` and `chrono 0.4.19` — because that is the only way to
/// demonstrate the overlap with real data from both tools rather than with a
/// hand-written pair that assumed the answer.
fn both_layers() -> Vec<rto_graph::FindingsLayer> {
    let source = SourceIdentity {
        lockfile_blob: Some("fixture-lock".to_owned()),
        ..SourceIdentity::default()
    };
    let mut audit_ctx = ctx(None);
    audit_ctx.source = &source;
    audit_ctx.analyzer_version = Some("0.22.2".to_owned());

    let root = std::path::Path::new(fixture::CAPTURE_ROOT);
    [
        (
            "cargo-audit",
            normalize_native("cargo-audit", &fixture::cargo_audit_native(), &audit_ctx)
                .expect("normalise cargo-audit"),
        ),
        (
            "osv-scanner",
            normalize_native(
                "osv-scanner",
                &fixture::osv_scanner_native(),
                &ctx(Some(root)),
            )
            .expect("normalise osv-scanner"),
        ),
    ]
    .into_iter()
    .map(|(analyzer, report)| layer_from(analyzer, &report))
    .collect()
}

fn layer_from(analyzer: &str, report: &rto_exec::NormalizedReport) -> rto_graph::FindingsLayer {
    let findings = report
        .findings
        .iter()
        .map(|reported| rto_graph::Finding {
            key: rto_graph::FindingKey::new(analyzer, &reported.identity).expect("key"),
            rule: reported.rule.clone(),
            severity: reported.severity.clone(),
            title: reported.title.clone(),
            message: reported.message.clone(),
            path: reported.path.clone(),
            span: reported.span,
            meta: reported.meta.clone(),
        })
        .collect();
    rto_graph::FindingsLayer {
        run: rto_graph::AnalysisRun {
            layer: format!("security:{analyzer}:ab12cd34"),
            analyzer: analyzer.to_owned(),
            analyzer_version: report.analyzer_version.clone(),
            runner: rto_graph::RunnerKind::Ingested,
            isolation: rto_graph::Isolation::Ingested,
            image_digest: None,
            rules_digest: None,
            advisory_db: None,
            command_policy: rto_graph::CommandPolicy {
                network: rto_graph::NetworkPolicy::Deny,
                worktree: rto_graph::WorktreeAccess::ReadOnly,
                environment: rto_graph::EnvironmentPolicy::Scrubbed,
            },
            source: report.source.clone(),
            started_at: report.started_at.clone(),
            ended_at: report.ended_at.clone(),
            exit_status: report.exit_status,
            report_digest: "0".repeat(64),
        },
        findings,
    }
}

/// The decision, over real data: the same Rust advisory from both tools reads as
/// **one advisory confirmed by two analyzers**, with both finding keys still
/// addressable.
#[test]
fn the_rust_overlap_reads_as_one_advisory_confirmed_twice() {
    let layers = both_layers();
    let crossref = rto_exec::cross_reference(&layers);

    let time = crossref
        .iter()
        .find(|c| c.package == "time" && c.version == "0.1.44")
        .expect("both tools report time 0.1.44");
    assert_eq!(time.confirmed_by(), 2, "{:?}", time.analyzers());
    assert_eq!(time.analyzers(), vec!["cargo-audit", "osv-scanner"]);
    assert_eq!(time.advisory, "RUSTSEC-2020-0071");
    // Neither key is superseded: fixing the advisory must make both disappear,
    // and a reader cannot check that without being told both.
    assert_eq!(time.keys().len(), 2);
    assert!(
        time.keys()
            .iter()
            .any(|k| k.starts_with("finding:cargo-audit:"))
    );
    assert!(
        time.keys()
            .iter()
            .any(|k| k.starts_with("finding:osv-scanner:"))
    );
}

/// The over-merge this join must not commit, and it is not hypothetical: in the
/// real `cargo-audit` capture, `chrono`'s advisory lists `CVE-2020-26235` and
/// `RUSTSEC-2020-0071` under `related` — the very identifiers `time`'s advisory
/// is published under. Joining on identifiers alone would fuse two advisories
/// about two different crates into one row.
#[test]
fn a_cve_shared_between_two_crates_does_not_fuse_their_advisories() {
    let layers = both_layers();
    let crossref = rto_exec::cross_reference(&layers);

    let chrono = crossref
        .iter()
        .find(|c| c.package == "chrono")
        .expect("chrono is reported");
    let time = crossref
        .iter()
        .find(|c| c.package == "time")
        .expect("time is reported");
    assert_ne!(chrono.advisory, time.advisory);
    // chrono's own advisory names it, not the alias that happens to sort first.
    assert_eq!(chrono.advisory, "RUSTSEC-2020-0159");
    assert!(
        chrono.aliases.contains(&"RUSTSEC-2020-0071".to_owned()),
        "the fixture no longer exercises the shared-identifier case"
    );
}

/// "Present in one, absent in the other" is a real state with two real causes,
/// and both appear in the fixtures: `paste` is unmaintained per `cargo-audit`
/// alone, and `derivative` is unmaintained per `osv-scanner` alone.
#[test]
fn single_source_advisories_are_reported_as_a_normal_state() {
    let layers = both_layers();
    let crossref = rto_exec::cross_reference(&layers);

    let paste = crossref
        .iter()
        .find(|c| c.package == "paste")
        .expect("cargo-audit reports paste as unmaintained");
    assert_eq!(paste.confirmed_by(), 1);
    assert_eq!(paste.analyzers(), vec!["cargo-audit"]);

    let derivative = crossref
        .iter()
        .find(|c| c.package == "derivative")
        .expect("osv-scanner reports derivative as unmaintained");
    assert_eq!(derivative.confirmed_by(), 1);
    assert_eq!(derivative.analyzers(), vec!["osv-scanner"]);
}

/// The invariant ADR-0018 states in as many words, checked over real captures:
/// the cross-reference is a view, and nothing it does changes how many findings
/// there are.
#[test]
fn cross_referencing_real_captures_never_halves_a_count() {
    let layers = both_layers();
    let dependency_findings: usize = layers
        .iter()
        .flat_map(|l| &l.findings)
        .filter(|f| {
            f.meta
                .get("package")
                .is_some_and(serde_json::Value::is_string)
        })
        .count();
    let crossref = rto_exec::cross_reference(&layers);
    let reported: usize = crossref.iter().map(|c| c.reports.len()).sum();

    assert_eq!(
        reported, dependency_findings,
        "every dependency finding must appear in exactly one correspondence"
    );
    assert!(
        crossref.len() < dependency_findings,
        "the fixtures must contain at least one duplicate pair for this to mean anything"
    );
}
