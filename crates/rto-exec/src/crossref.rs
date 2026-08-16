//! Cross-referencing dependency findings across analyzers.
//!
//! `cargo-audit` and `osv-scanner` both read `Cargo.lock`, and OSV.dev ingests
//! the `RustSec` database — so the same Rust advisory arrives twice, under
//! `finding:cargo-audit:…` and `finding:osv-scanner:…`. [ADR-0018] v1.1 decides
//! what to do about it: **keep both findings and cross-reference them at the
//! reporting layer**. This module is that reporting layer's join.
//!
//! [ADR-0018]: https://github.com/OffeneDatenmodellierung/Roteiro/blob/main/docs/adr/0018-analyzer-coverage-matrix.md
//!
//! # Nothing here filters, merges, or renumbers
//!
//! A [`Correspondence`] is a *view over* findings, not a replacement for them.
//! Every finding stays in its own layer, keyed as its own analyzer named it, and
//! every layer is still replaced wholesale per analyzer. The count of findings is
//! unchanged by anything in this file, which is the specific failure ADR-0018
//! names: "never a merged super-finding, and never a count that silently halves".
//! A duplicate pair reads as *one advisory confirmed by two analyzers*, with
//! both [`Correspondence::keys`] still addressable — so a reader who fixes the
//! advisory watches both disappear.
//!
//! # The join needs no invention
//!
//! Both upstreams publish the identifiers already. OSV keys a `RustSec`-derived
//! record by *the RUSTSEC id itself* (`RUSTSEC-2020-0071` resolves, carrying
//! `aliases: ["CVE-2020-26235", "GHSA-wcg3-cvx6-7396"]`), and `cargo-audit`'s
//! adapter stores the advisory's `aliases` and `related` verbatim in `meta`. So
//! two findings correspond when their **identifier sets intersect** — the
//! RUSTSEC id where both name it, any shared CVE or GHSA id otherwise. That is a
//! deterministic join over published identifiers: no similarity matching, no
//! heuristic, and nothing that needs a confidence score.
//!
//! # Why the package must match too
//!
//! Identifier intersection alone over-merges. A single CVE is regularly assigned
//! to several packages, and joining on it alone would fuse advisories about
//! different crates into one row. Correspondence therefore also requires the
//! same package **at the same version**, which both adapters record in
//! `meta.package` and `meta.version`. A finding without those — every SAST
//! finding — is not on the dependency axis and does not take part at all.
//!
//! # "Present in one, absent in the other" is a real state
//!
//! The two analyzers pin their databases independently and are prefetched at
//! different times, so they will legitimately disagree for a window, and there
//! are advisory kinds only one of them can ever carry (`cargo-audit` learns
//! *yanked* from the registry index, which is not an advisory and is not in OSV
//! at all). A [`Correspondence`] reported by one analyzer is therefore a normal
//! result, not a defect: [`Correspondence::confirmed_by`] answers *how many* said
//! so, and the caller renders that rather than treating a single source as a
//! discrepancy.
//!
//! @rto:0012
//! @rto:0018

use std::collections::BTreeMap;

use rto_graph::{Finding, FindingsLayer, Severity};

/// One advisory, and every finding that reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correspondence {
    /// The advisory's canonical identifier: the RUSTSEC id where any member
    /// names one — that is the id ADR-0018 calls the join key and the one a Rust
    /// developer recognises — and otherwise the lowest identifier in the set.
    pub advisory: String,
    /// Every identifier this advisory is published under, across all members.
    pub aliases: Vec<String>,
    /// The package it is about.
    pub package: String,
    /// The version of that package that was resolved.
    pub version: String,
    /// One entry per reporting finding, ordered by analyzer then key.
    pub reports: Vec<Report>,
}

/// One analyzer's report of an advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The analyzer that reported it.
    pub analyzer: String,
    /// The finding's rendered [`rto_graph::FindingKey`] — still addressable, and
    /// still owned by its own layer.
    pub key: String,
    /// The rule or advisory id *this* analyzer fired, which is not always the
    /// canonical one: `osv-scanner` may name an advisory by its GHSA id where
    /// `cargo-audit` names it by its RUSTSEC id.
    pub rule: String,
    /// The severity that analyzer assigned.
    pub severity: Severity,
}

impl Correspondence {
    /// How many distinct analyzers reported this advisory.
    ///
    /// Two or more is agreement between independent sources, which ADR-0018
    /// keeps as evidence rather than tidying away. One is a normal state, not a
    /// discrepancy — see the module docs.
    #[must_use]
    pub fn confirmed_by(&self) -> usize {
        let mut analyzers: Vec<&str> = self.reports.iter().map(|r| r.analyzer.as_str()).collect();
        analyzers.sort_unstable();
        analyzers.dedup();
        analyzers.len()
    }

    /// The distinct analyzers that reported it, sorted.
    #[must_use]
    pub fn analyzers(&self) -> Vec<&str> {
        let mut analyzers: Vec<&str> = self.reports.iter().map(|r| r.analyzer.as_str()).collect();
        analyzers.sort_unstable();
        analyzers.dedup();
        analyzers
    }

    /// Every finding key that reported it. Both halves of a duplicate pair stay
    /// addressable; neither is superseded by this view.
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.reports.iter().map(|r| r.key.as_str()).collect()
    }
}

/// Cross-reference the dependency findings across `layers`.
///
/// Returns one [`Correspondence`] per advisory-and-package, ordered by package,
/// then version, then advisory — a stable order, so two runs over the same store
/// render identically. Findings that are not on the dependency axis (no
/// `meta.package`) are absent, because there is nothing about a SAST finding for
/// a dependency scanner to agree with.
///
/// The findings themselves are neither modified nor consumed: this borrows them
/// and describes what it saw.
#[must_use]
pub fn cross_reference(layers: &[FindingsLayer]) -> Vec<Correspondence> {
    let candidates: Vec<Candidate<'_>> = layers
        .iter()
        .flat_map(|layer| {
            layer
                .findings
                .iter()
                .filter_map(|finding| Candidate::of(&layer.run.analyzer, finding))
        })
        .collect();

    // Bucket by package and version first. Identifier intersection alone
    // over-merges, because one CVE is regularly assigned to several packages.
    let mut buckets: BTreeMap<(&str, &str), Vec<&Candidate<'_>>> = BTreeMap::new();
    for candidate in &candidates {
        buckets
            .entry((candidate.package, candidate.version))
            .or_default()
            .push(candidate);
    }

    let mut out = Vec::new();
    for ((package, version), members) in buckets {
        for group in group_by_shared_identifier(&members) {
            out.push(assemble(package, version, &group));
        }
    }
    out.sort_by(|a, b| {
        (&a.package, &a.version, &a.advisory).cmp(&(&b.package, &b.version, &b.advisory))
    });
    out
}

/// Partition one package's findings into groups whose identifier sets overlap,
/// transitively.
///
/// Transitive closure is what makes the join work in the direction it has to:
/// `cargo-audit` may name an advisory `RUSTSEC-x` with alias `CVE-y`, and
/// `osv-scanner` may name it `GHSA-z` with alias `CVE-y`. Neither shares an id
/// with the other directly; both share one with the CVE.
fn group_by_shared_identifier<'a>(members: &[&'a Candidate<'a>]) -> Vec<Vec<&'a Candidate<'a>>> {
    let mut parent: Vec<usize> = (0..members.len()).collect();
    for (i, a) in members.iter().enumerate() {
        for (j, b) in members.iter().enumerate().skip(i + 1) {
            if a.shares_identifier(b) {
                union(&mut parent, i, j);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<&Candidate<'_>>> = BTreeMap::new();
    for (i, member) in members.iter().enumerate() {
        groups.entry(find(&mut parent, i)).or_default().push(member);
    }
    groups.into_values().collect()
}

fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        parent[b.max(a)] = b.min(a);
    }
}

/// Build the reported view of one group.
fn assemble(package: &str, version: &str, group: &[&Candidate<'_>]) -> Correspondence {
    let mut aliases: Vec<String> = group
        .iter()
        .flat_map(|c| c.identifiers.iter())
        .map(String::clone)
        .collect();
    aliases.sort();
    aliases.dedup();

    let mut reports: Vec<Report> = group
        .iter()
        .map(|c| Report {
            analyzer: c.analyzer.to_owned(),
            key: c.key.clone(),
            rule: c.rule.to_owned(),
            severity: c.severity.clone(),
        })
        .collect();
    reports.sort_by(|a, b| (&a.analyzer, &a.key).cmp(&(&b.analyzer, &b.key)));

    Correspondence {
        advisory: canonical(&aliases),
        aliases,
        package: package.to_owned(),
        version: version.to_owned(),
        reports,
    }
}

/// The identifier to name an advisory by: the RUSTSEC id where there is one,
/// otherwise the lowest.
///
/// Preferring RUSTSEC is not favouritism towards Rust — it is that ADR-0018
/// names it as *the* join key, and that where both analyzers report a Rust
/// advisory it is the one identifier both of them publish.
fn canonical(aliases: &[String]) -> String {
    aliases
        .iter()
        .find(|id| id.starts_with("RUSTSEC-"))
        .or_else(|| aliases.first())
        .cloned()
        .unwrap_or_default()
}

/// A dependency finding, reduced to what the join needs.
struct Candidate<'a> {
    analyzer: &'a str,
    key: String,
    rule: &'a str,
    severity: Severity,
    package: &'a str,
    version: &'a str,
    /// Every identifier this finding publishes for its advisory, including the
    /// rule id itself.
    identifiers: Vec<String>,
}

impl<'a> Candidate<'a> {
    /// A candidate, or `None` if the finding is not on the dependency axis.
    fn of(analyzer: &'a str, finding: &'a Finding) -> Option<Self> {
        let package = finding.meta.get("package")?.as_str()?;
        let version = finding.meta.get("version")?.as_str()?;
        if package.is_empty() || version.is_empty() {
            return None;
        }
        let mut identifiers = vec![finding.rule.clone()];
        // `aliases` is what both adapters call the set; `related` is where
        // `cargo-audit` puts a CVE that RustSec did not list as an alias, and
        // `ids` is `osv-scanner`'s group membership. All three are identifiers
        // an upstream published, so all three join.
        for field in ["aliases", "related", "ids"] {
            if let Some(values) = finding.meta.get(field).and_then(|v| v.as_array()) {
                identifiers.extend(values.iter().filter_map(|v| v.as_str()).map(str::to_owned));
            }
        }
        identifiers.retain(|id| !id.trim().is_empty());
        identifiers.sort();
        identifiers.dedup();
        Some(Self {
            analyzer,
            key: finding.key.render(),
            rule: &finding.rule,
            severity: finding.severity.clone(),
            package,
            version,
            identifiers,
        })
    }

    /// Whether two findings name at least one identifier in common.
    fn shares_identifier(&self, other: &Self) -> bool {
        self.identifiers
            .iter()
            .any(|id| other.identifiers.binary_search(id).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::{Correspondence, cross_reference};
    use rto_graph::{
        AnalysisRun, CommandPolicy, EnvironmentPolicy, Finding, FindingKey, FindingsLayer,
        Isolation, NetworkPolicy, RunnerKind, Severity, SourceIdentity, WorktreeAccess,
    };

    fn run(analyzer: &str) -> AnalysisRun {
        AnalysisRun {
            layer: format!("security:{analyzer}:ab12cd34"),
            analyzer: analyzer.to_owned(),
            analyzer_version: "1.0.0".to_owned(),
            runner: RunnerKind::Ingested,
            isolation: Isolation::Ingested,
            image_digest: None,
            rules_digest: None,
            advisory_db: None,
            command_policy: CommandPolicy {
                network: NetworkPolicy::Deny,
                worktree: WorktreeAccess::ReadOnly,
                environment: EnvironmentPolicy::Scrubbed,
            },
            source: SourceIdentity::default(),
            started_at: "2026-08-16T09:00:00Z".to_owned(),
            ended_at: "2026-08-16T09:00:01Z".to_owned(),
            exit_status: 1,
            report_digest: "0".repeat(64),
        }
    }

    fn finding(analyzer: &str, rule: &str, meta: serde_json::Value) -> Finding {
        Finding {
            key: FindingKey::new(analyzer, &[rule.to_owned()]).expect("key"),
            rule: rule.to_owned(),
            severity: Severity::High,
            title: format!("{rule} is a problem"),
            message: String::new(),
            path: None,
            span: None,
            meta,
        }
    }

    fn layer(analyzer: &str, findings: Vec<Finding>) -> FindingsLayer {
        FindingsLayer {
            run: run(analyzer),
            findings,
        }
    }

    /// The headline case: the same Rust advisory from both analyzers, named by
    /// different ids, joined on the RUSTSEC id both of them publish. One
    /// advisory, confirmed twice — and both keys still addressable.
    #[test]
    fn the_same_advisory_from_two_analyzers_is_one_confirmed_correspondence() {
        let layers = vec![
            layer(
                "cargo-audit",
                vec![finding(
                    "cargo-audit",
                    "RUSTSEC-2020-0071",
                    serde_json::json!({
                        "package": "time", "version": "0.2.22",
                        "aliases": ["CVE-2020-26235"], "related": []
                    }),
                )],
            ),
            layer(
                "osv-scanner",
                vec![finding(
                    "osv-scanner",
                    "GHSA-wcg3-cvx6-7396",
                    serde_json::json!({
                        "package": "time", "version": "0.2.22",
                        "aliases": ["CVE-2020-26235", "GHSA-wcg3-cvx6-7396", "RUSTSEC-2020-0071"],
                        "ids": ["GHSA-wcg3-cvx6-7396", "RUSTSEC-2020-0071"]
                    }),
                )],
            ),
        ];

        let crossref = cross_reference(&layers);
        assert_eq!(crossref.len(), 1, "one advisory, not two problems");
        let one = &crossref[0];
        assert_eq!(one.confirmed_by(), 2);
        assert_eq!(one.analyzers(), vec!["cargo-audit", "osv-scanner"]);
        // Named by the id ADR-0018 calls the join key.
        assert_eq!(one.advisory, "RUSTSEC-2020-0071");
        // Both keys survive: neither analyzer's finding is superseded here.
        assert_eq!(one.keys().len(), 2);
        assert!(one.keys().iter().any(|k| k.contains("cargo-audit")));
        assert!(one.keys().iter().any(|k| k.contains("osv-scanner")));
        // Each analyzer's own rule id is preserved, not rewritten to the
        // canonical one.
        let rules: Vec<&str> = one.reports.iter().map(|r| r.rule.as_str()).collect();
        assert!(rules.contains(&"RUSTSEC-2020-0071"));
        assert!(rules.contains(&"GHSA-wcg3-cvx6-7396"));
    }

    /// The transitive case, which is the one that actually happens: neither side
    /// names an id the other names directly, and both name the same CVE.
    #[test]
    fn two_findings_join_through_a_shared_cve_neither_names_directly() {
        let layers = vec![
            layer(
                "cargo-audit",
                vec![finding(
                    "cargo-audit",
                    "RUSTSEC-2021-0001",
                    serde_json::json!({
                        "package": "widget", "version": "1.0.0",
                        "aliases": [], "related": ["CVE-2021-9999"]
                    }),
                )],
            ),
            layer(
                "osv-scanner",
                vec![finding(
                    "osv-scanner",
                    "GHSA-aaaa-bbbb-cccc",
                    serde_json::json!({
                        "package": "widget", "version": "1.0.0",
                        "aliases": ["CVE-2021-9999"]
                    }),
                )],
            ),
        ];
        let crossref = cross_reference(&layers);
        assert_eq!(crossref.len(), 1);
        assert_eq!(crossref[0].confirmed_by(), 2);
    }

    /// The failure this join must not have. One CVE is regularly assigned to
    /// several packages; joining on the identifier alone would fuse advisories
    /// about different packages into one row.
    #[test]
    fn a_shared_identifier_on_different_packages_does_not_merge() {
        let layers = vec![layer(
            "osv-scanner",
            vec![
                finding(
                    "osv-scanner",
                    "GHSA-1",
                    serde_json::json!({
                        "package": "alpha", "version": "1.0.0", "aliases": ["CVE-2026-1"]
                    }),
                ),
                finding(
                    "osv-scanner",
                    "GHSA-2",
                    serde_json::json!({
                        "package": "beta", "version": "1.0.0", "aliases": ["CVE-2026-1"]
                    }),
                ),
            ],
        )];
        let crossref = cross_reference(&layers);
        assert_eq!(crossref.len(), 2, "different packages stay different rows");
    }

    /// The same package at two versions is two advisories to fix, not one.
    #[test]
    fn the_same_advisory_at_two_versions_does_not_merge() {
        let layers = vec![layer(
            "osv-scanner",
            vec![
                finding(
                    "osv-scanner",
                    "GHSA-1",
                    serde_json::json!({"package": "lodash", "version": "4.17.15"}),
                ),
                finding(
                    "osv-scanner",
                    "GHSA-1b",
                    serde_json::json!({"package": "lodash", "version": "4.17.20"}),
                ),
            ],
        )];
        assert_eq!(cross_reference(&layers).len(), 2);
    }

    /// "Present in one, absent in the other" is a real state, not a defect: the
    /// two analyzers pin their databases independently, and `yanked` is not an
    /// advisory kind OSV can ever carry.
    #[test]
    fn an_advisory_only_one_analyzer_reports_is_a_normal_single_source_row() {
        let layers = vec![
            layer(
                "cargo-audit",
                vec![finding(
                    "cargo-audit",
                    "yanked",
                    serde_json::json!({"package": "half-baked", "version": "0.3.1"}),
                )],
            ),
            layer(
                "osv-scanner",
                vec![finding(
                    "osv-scanner",
                    "GHSA-new",
                    serde_json::json!({"package": "fresh", "version": "1.0.0"}),
                )],
            ),
        ];
        let crossref = cross_reference(&layers);
        assert_eq!(crossref.len(), 2);
        assert!(crossref.iter().all(|c| c.confirmed_by() == 1));
        // Ordered by package: `fresh` before `half-baked`.
        assert_eq!(crossref[0].package, "fresh");
        assert_eq!(crossref[0].analyzers(), vec!["osv-scanner"]);
        assert_eq!(crossref[1].package, "half-baked");
        assert_eq!(crossref[1].analyzers(), vec!["cargo-audit"]);
    }

    /// The invariant ADR-0018 states in as many words: a cross-reference must
    /// never be a count that silently halves. Every finding is still accounted
    /// for after the join.
    #[test]
    fn no_finding_is_lost_or_double_counted_by_the_join() {
        let layers = vec![
            layer(
                "cargo-audit",
                vec![
                    finding(
                        "cargo-audit",
                        "RUSTSEC-2020-0071",
                        serde_json::json!({
                            "package": "time", "version": "0.2.22", "aliases": ["CVE-2020-26235"]
                        }),
                    ),
                    finding(
                        "cargo-audit",
                        "yanked",
                        serde_json::json!({"package": "half-baked", "version": "0.3.1"}),
                    ),
                ],
            ),
            layer(
                "osv-scanner",
                vec![finding(
                    "osv-scanner",
                    "RUSTSEC-2020-0071",
                    serde_json::json!({
                        "package": "time", "version": "0.2.22", "aliases": ["CVE-2020-26235"]
                    }),
                )],
            ),
        ];
        let total: usize = layers.iter().map(|l| l.findings.len()).sum();
        let crossref = cross_reference(&layers);
        let reported: usize = crossref.iter().map(|c| c.reports.len()).sum();
        assert_eq!(reported, total, "every finding appears exactly once");
        assert_eq!(total, 3);
        assert_eq!(crossref.len(), 2, "…across two advisories");
    }

    /// A SAST finding is not on the dependency axis, so there is nothing for a
    /// dependency scanner to agree with and it does not take part.
    #[test]
    fn sast_findings_are_not_cross_referenced() {
        let layers = vec![layer(
            "semgrep",
            vec![finding(
                "semgrep",
                "roteiro.python.eval-of-input",
                serde_json::json!({"engine": "python"}),
            )],
        )];
        assert!(cross_reference(&layers).is_empty());
    }

    #[test]
    fn nothing_ingested_cross_references_to_nothing() {
        assert!(cross_reference(&[]).is_empty());
    }

    /// A stable order, so two renderings of the same store are identical.
    #[test]
    fn the_order_is_stable_and_does_not_depend_on_layer_order() {
        let a = layer(
            "cargo-audit",
            vec![finding(
                "cargo-audit",
                "R-1",
                serde_json::json!({"package": "zeta", "version": "1.0.0"}),
            )],
        );
        let b = layer(
            "osv-scanner",
            vec![finding(
                "osv-scanner",
                "G-1",
                serde_json::json!({"package": "alpha", "version": "1.0.0"}),
            )],
        );
        let forwards = cross_reference(&[a.clone(), b.clone()]);
        let backwards = cross_reference(&[b, a]);
        assert_eq!(forwards, backwards);
        let packages: Vec<&str> = forwards.iter().map(|c| c.package.as_str()).collect();
        assert_eq!(packages, vec!["alpha", "zeta"]);
    }

    /// A correspondence with no identifiers at all still names itself, rather
    /// than rendering as a blank row.
    #[test]
    fn an_advisory_always_has_a_name() {
        let layers = vec![layer(
            "osv-scanner",
            vec![finding(
                "osv-scanner",
                "OSV-1",
                serde_json::json!({"package": "x", "version": "1.0.0"}),
            )],
        )];
        let crossref: Vec<Correspondence> = cross_reference(&layers);
        assert_eq!(crossref[0].advisory, "OSV-1");
    }
}
