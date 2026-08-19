//! The **read-only `security list` / `security status` documents** the
//! model-facing tool surfaces return.
//!
//! `roteiro security list` and `roteiro security status` are the two `security`
//! subcommands that read and never write, so they are the two that may be
//! exposed to a model at all — the other three (`ingest`, `run`, `prefetch`) are
//! permanent refusals, and `rto_render::mcp`'s module documentation carries the
//! disposition table with each reason. What this module adds is the two things a
//! CLI does not need and a tool surface cannot do without.
//!
//! # 1. An empty listing must not read as a clean one
//!
//! `roteiro security list --json` is 36 bytes on a repository no analyzer has
//! ever run against: `{"layers": [], "findings": 0}`. **"Nothing has been
//! analyzed" and "an analyzer ran and found nothing" are opposite facts**, and
//! `findings: 0` reads as the second while meaning the first. A model that
//! reports "no security findings" from that document is confidently wrong, and it
//! is the single most likely misuse of these tools.
//!
//! The data does distinguish them — a clean run leaves a live layer whose
//! `findings` is empty, and no run leaves no layer — so this is a defect in the
//! *document*, not in the store. [`Coverage`] fixes it the way
//! [`rto_spec::tool_check`]'s `Gate` fixes the same hazard for `check`: a
//! discriminator that is **always** present, and the payload omitted entirely in
//! the case that has no answer. A consumer reaching for findings in a
//! [`Coverage::NoAnalyzerOnRecord`] document finds no `report` at all, rather
//! than finding nothing-wrong.
//!
//! [`rto_spec::tool_check`]: https://docs.rs/rto-spec
//!
//! # 2. `security status` is two halves with two different scopes
//!
//! The CLI's status output reads the machine-global asset cache
//! ([`crate::asset_root`], [`crate::status`]) *and* the current repository's
//! findings layers, and prints them as one screen. On a CLI that is invisible and
//! harmless: one process, one repository, one machine.
//!
//! Over a tool surface it is neither. A caller selects a *project* (ADR-0008), so
//! the layer half follows the selected project and **the asset half does not** —
//! those digests describe the machine the server runs on, whichever project was
//! asked about. A model handed one flat blob has no way to tell which half is
//! which, and "this repository's analyzers are not provisioned" is a claim the
//! asset half cannot support.
//!
//! So the split is in the *output*, not only in this comment:
//! [`ToolSecurityStatus`] has exactly two named sections, each carrying an
//! explicit `scope` field, and each scope's identifying value lives **inside** its
//! own section — the asset root under `machine`, the project name under
//! `repository`. Neither half can be quoted without its scope travelling with it.
//!
//! @rto:0012
//! @rto:0018

use std::path::Path;

use rto_graph::{AdvisoryDb, AnalysisRun, Finding, FindingsLayer, Isolation, RunnerKind, Severity};
use serde::Serialize;

use crate::adapter::ADAPTERS;
use crate::assets::{AssetStatus, resolve, status};
use crate::clock::age_in_days;
use crate::crossref::{Correspondence, cross_reference};

/// Schema tag for the tool-surface `security list` document.
pub const TOOL_SECURITY_LIST_SCHEMA: &str = "roteiro.security.list/v1";

/// Schema tag for the tool-surface `security status` document.
pub const TOOL_SECURITY_STATUS_SCHEMA: &str = "roteiro.security.status/v1";

/// Whether any analyzer result is on record, as a value rather than an absence.
///
/// This is the whole reason these documents exist rather than the CLI's `--json`
/// being served directly. A caller that only tested `findings == 0` would read a
/// repository nobody has analyzed as a clean one; making the absence of a result
/// its own value means that caller has to notice.
///
/// # Why the negative case is not called `never-run`
///
/// Because that is more than the store can support.
/// [`rto_graph::Store::delete_findings_layer`] exists, so "no live layer" means
/// *no analyzer result is on record* — which covers a repository nobody analyzed
/// and one whose layer was later deleted. Both are the same actionable fact and
/// neither is "clean", so they share a token; claiming the stronger "never ran"
/// would be a guess dressed as evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Coverage {
    /// At least one analyzer has a live findings layer. The `report` is present,
    /// and a layer whose findings are empty is a genuine clean result.
    Analyzed,
    /// No live findings layer — nothing has been analyzed (or a layer was
    /// deleted). **Not a clean result**: there is no `report` at all.
    NoAnalyzerOnRecord,
}

impl Coverage {
    /// The token this serialises as, for a caller that renders it as text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Analyzed => "analyzed",
            Self::NoAnalyzerOnRecord => "no-analyzer-on-record",
        }
    }
}

/// The tool-surface `security list` result.
///
/// # Why `report` is an `Option` and not an empty listing
///
/// The hazard this shape addresses is that a listing which had nothing to list
/// looks exactly like a clean repository once it is serialised. A listing has
/// `findings: usize`, and `0` is the *good* answer — so a
/// [`Coverage::NoAnalyzerOnRecord`] result must not produce a listing at all. It
/// does not: `report` is `None` and is skipped entirely in JSON, so a consumer
/// reaching for `findings` finds nothing rather than nothing-wrong. `coverage`
/// says the same thing in one word for a consumer that reads only that.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSecurityList {
    /// Stable schema tag ([`TOOL_SECURITY_LIST_SCHEMA`]).
    pub schema: &'static str,
    /// Whether any analyzer result is on record. Always present.
    pub coverage: Coverage,
    /// The listing. **Absent unless an analyzer result is on record.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<SecurityListReport>,
    /// Why there is nothing to list, and what to run. Present exactly when
    /// `coverage` is `no-analyzer-on-record`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_result_reason: Option<String>,
}

/// The listing itself, present only when an analyzer result is on record.
#[derive(Debug, Clone, Serialize)]
pub struct SecurityListReport {
    /// Every live layer, with its run evidence and a bounded page of findings.
    pub layers: Vec<ToolFindingsLayer>,
    /// Total findings across those layers — the **true** count, never reduced by
    /// the page bound. **Unchanged** by the cross-reference below, which is a
    /// view over these findings and not a replacement for them (ADR-0018 v1.1).
    pub findings: usize,
    /// How many findings this document actually carries. Below `findings`
    /// whenever any layer was truncated.
    pub returned: usize,
    /// True when `returned < findings` — i.e. this document is a page and not the
    /// whole listing. Each layer says which one of them was cut, and by how much.
    pub truncated: bool,
    /// Dependency advisories seen across analyzers, most-corroborated first, and
    /// bounded by the same page size. Empty unless more than one dependency
    /// analyzer has a live layer, because a table in which every row reads
    /// "confirmed by 1" is noise dressed as information.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cross_reference: Vec<CrossReference>,
    /// How many advisories the cross-reference found in total, before the page
    /// bound. Equal to `cross_reference.len()` when nothing was cut.
    pub cross_reference_total: usize,
}

/// One live layer: its run evidence, and a **bounded page** of its findings.
///
/// # Why the count and the page are separate fields
///
/// `findings` here is the layer's real size and `page` is what fits. A single
/// field would have to be one or the other, and a model reading a truncated
/// count as a total under-reports a security result — the failure mode the whole
/// module is written against, one level down. This is the vocabulary
/// `rto_graph::tool_context` already uses for the same reason: a bound that
/// reports what it bound.
#[derive(Debug, Clone, Serialize)]
pub struct ToolFindingsLayer {
    /// The run that owns this layer: analyzer, version, backend, isolation,
    /// advisory database, command policy, source identity and report digest.
    pub run: AnalysisRun,
    /// Every finding this layer owns — the **true** count, never reduced by the
    /// page bound.
    pub findings: usize,
    /// The findings actually included, **most severe first** (see
    /// [`security_list`] for why this order and not the store's).
    pub page: Vec<Finding>,
    /// True when `page` is shorter than `findings`.
    pub truncated: bool,
    /// How many of `findings` are missing from `page`.
    pub omitted: usize,
}

/// One advisory in the cross-reference (ADR-0018 v1.1), as a serialisable view.
///
/// A **view**, not a record: every finding it names is still in its own layer
/// under its own key, and [`SecurityListReport::findings`] still counts them all.
/// That is what makes a duplicate pair read as one advisory confirmed by two
/// analyzers rather than as a count that silently halved.
#[derive(Debug, Clone, Serialize)]
pub struct CrossReference {
    /// The advisory's canonical id — the RUSTSEC id where both sides publish one.
    pub advisory: String,
    /// Every identifier it is published under.
    pub aliases: Vec<String>,
    /// The package and resolved version it is about.
    pub package: String,
    /// That package's resolved version.
    pub version: String,
    /// How many distinct analyzers reported it. `1` is a normal state, not a
    /// discrepancy: the two databases are pinned independently, and `yanked` is
    /// not an advisory kind OSV can carry at all.
    pub confirmed_by: usize,
    /// Which analyzers, and the still-addressable finding key each one wrote.
    pub reports: Vec<CrossReferenceReport>,
}

/// One analyzer's report inside a [`CrossReference`].
#[derive(Debug, Clone, Serialize)]
pub struct CrossReferenceReport {
    /// The analyzer that reported it.
    pub analyzer: String,
    /// The finding key, unchanged and still addressable.
    pub key: String,
    /// The id *this* analyzer fired, which need not be the canonical one.
    pub rule: String,
    /// The severity that analyzer assigned.
    pub severity: Severity,
}

impl From<Correspondence> for CrossReference {
    fn from(c: Correspondence) -> Self {
        // `confirmed_by` comes from `Correspondence::confirmed_by`, never from a
        // second count written here: one concept reporting different numbers on
        // different surfaces is issue #321, and this is the same number the CLI
        // prints.
        let confirmed_by = c.confirmed_by();
        Self {
            advisory: c.advisory,
            aliases: c.aliases,
            package: c.package,
            version: c.version,
            confirmed_by,
            reports: c
                .reports
                .into_iter()
                .map(|r| CrossReferenceReport {
                    analyzer: r.analyzer,
                    key: r.key,
                    rule: r.rule,
                    severity: r.severity,
                })
                .collect(),
        }
    }
}

/// Build the tool-surface `security list` document from a project's live layers.
///
/// `limit` bounds the findings **per layer**, not across the document. That is the
/// deliberate choice: a document-wide bound spends its whole budget on the first
/// layer in key order and hands back `semgrep: 0 findings` for a layer it never
/// reached — which reads as "semgrep found nothing" and is the exact defect this
/// module exists to prevent, one level down. The worst case is therefore `limit ×
/// live layers`, and a live layer is one per analyzer per checkout, so it is
/// small and knowable rather than unbounded.
///
/// # Why the page is ordered by severity and the store's listing is not
///
/// [`rto_graph::Store::findings_layers`] returns findings ordered by key, which is
/// right for a full listing and wrong for a truncated one: it would drop findings
/// by alphabetical luck, and a critical whose advisory id sorts late would vanish
/// behind an informational one. The page is therefore sorted by severity,
/// descending, with the store's key order preserved within each level (the sort is
/// stable). One caveat, stated because it decides what gets dropped first:
/// [`Severity::Other`] — a level no shipped adapter emits, kept verbatim for a
/// future analyzer's vocabulary — orders *after* `info`, so an unrecognised
/// severity is truncated first. `truncated` and `omitted` are what keep that
/// visible instead of silent.
#[must_use]
pub fn security_list(layers: Vec<FindingsLayer>, limit: usize) -> ToolSecurityList {
    if layers.is_empty() {
        return ToolSecurityList {
            schema: TOOL_SECURITY_LIST_SCHEMA,
            coverage: Coverage::NoAnalyzerOnRecord,
            report: None,
            no_result_reason: Some(NO_RESULT_REASON.to_owned()),
        };
    }

    // The cross-reference is computed over the **full** layers, before any page
    // bound, so `confirmed_by` counts every analyzer that reported an advisory
    // rather than every analyzer whose page happened to include it. A bound
    // applied first would turn agreement between two sources into a single-source
    // row — inventing a disagreement out of a page size.
    let correspondences = cross_reference(&layers);
    let cross_reference_total = correspondences.len();
    let mut cross_reference = corroborated_first(correspondences);
    cross_reference.truncate(limit);

    let findings: usize = layers.iter().map(|l| l.findings.len()).sum();
    let layers: Vec<ToolFindingsLayer> = layers.into_iter().map(|l| page(l, limit)).collect();
    let returned: usize = layers.iter().map(|l| l.page.len()).sum();

    ToolSecurityList {
        schema: TOOL_SECURITY_LIST_SCHEMA,
        coverage: Coverage::Analyzed,
        report: Some(SecurityListReport {
            layers,
            findings,
            returned,
            truncated: returned < findings,
            cross_reference,
            cross_reference_total,
        }),
        no_result_reason: None,
    }
}

/// What a `no-analyzer-on-record` listing says instead of listing nothing.
///
/// It names the fact and the remedy, and it says the thing a model must not
/// conclude — because the description of a tool is read once and the body of its
/// result is read every time.
const NO_RESULT_REASON: &str = "No analyzer has filed a findings layer here, so nothing has been \
                                analyzed. This is NOT a clean result and must not be reported as \
                                one: a clean run leaves a layer whose findings are empty, which \
                                would appear above with coverage `analyzed`. Run `roteiro \
                                security ingest <report.json>` (or `roteiro security run \
                                --analyzer <name>`) to produce a result.";

/// Sort a cross-reference so advisories more than one analyzer reported come
/// first, preserving [`cross_reference`]'s order within each group.
///
/// The page bound cuts from the end, so what it must never cut is the agreement
/// between independent sources — that is the evidence ADR-0018 v1.1 exists to
/// keep. Single-source rows are the ordinary state and are the right thing to
/// lose first; `cross_reference_total` is what says how many were lost.
fn corroborated_first(correspondences: Vec<Correspondence>) -> Vec<CrossReference> {
    let mut views: Vec<CrossReference> = correspondences.into_iter().map(Into::into).collect();
    // Stable, so `cross_reference`'s own ordering survives inside each group.
    views.sort_by_key(|c| std::cmp::Reverse(c.confirmed_by));
    views
}

/// One layer's bounded page, with the real count kept alongside it.
fn page(layer: FindingsLayer, limit: usize) -> ToolFindingsLayer {
    let FindingsLayer { run, mut findings } = layer;
    let total = findings.len();
    // Stable sort on severity alone: `Severity`'s `Ord` runs critical → info →
    // other, so ascending order is most-severe-first, and the store's key order
    // survives as the tie-break without needing `FindingKey: Ord`.
    findings.sort_by(|a, b| a.severity.cmp(&b.severity));
    findings.truncate(limit);
    ToolFindingsLayer {
        run,
        findings: total,
        omitted: total - findings.len(),
        truncated: findings.len() < total,
        page: findings,
    }
}

/// The tool-surface `security status` result: **two scopes, never one blob**.
///
/// See this module's documentation for why the split is in the document rather
/// than in a comment. In short: the asset half describes the machine the server
/// runs on and the layer half describes the selected project, so a reader who
/// cannot tell them apart will attribute one to the other.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSecurityStatus {
    /// Stable schema tag ([`TOOL_SECURITY_STATUS_SCHEMA`]).
    pub schema: &'static str,
    /// What this **machine** has provisioned. Identical for every project this
    /// server hosts.
    pub machine: MachineScope,
    /// What has been analyzed in the **selected project**. Different for each.
    pub repository: RepositoryScope,
}

/// The machine-global half of a status document.
///
/// Every field here is a property of the host, resolved from
/// [`crate::asset_root`], and none of it is a property of any repository. A
/// provisioned analyzer means this machine *could* run it; it says nothing at all
/// about whether it has been run anywhere.
#[derive(Debug, Clone, Serialize)]
pub struct MachineScope {
    /// Always `"machine"`. Redundant with this section's name on purpose: a model
    /// that quotes the section alone still carries its scope with it.
    pub scope: &'static str,
    /// The pinned-asset cache these digests describe.
    pub asset_root: String,
    /// What each shipped analyzer covers, read off the adapters rather than off a
    /// document, and whether this machine has its assets.
    pub analyzers: Vec<AnalyzerCoverage>,
    /// Every pinned asset, its digest, its age, and whether the bytes on disk
    /// still match what was recorded.
    pub assets: Vec<AssetStatus>,
}

/// The per-repository half of a status document.
///
/// Everything here is a property of one project's graph. It carries the same
/// [`Coverage`] discriminator as [`ToolSecurityList`], for the same reason: an
/// empty `layers` array would read as a clean repository.
#[derive(Debug, Clone, Serialize)]
pub struct RepositoryScope {
    /// Always `"repository"`. Redundant on purpose — see [`MachineScope::scope`].
    pub scope: &'static str,
    /// The project these layers belong to, as the workspace resolved it
    /// (ADR-0008). Named here rather than at the top level so it cannot be read
    /// as qualifying the machine half.
    pub project: String,
    /// Whether any analyzer result is on record for this project. Always present.
    pub coverage: Coverage,
    /// The live layers and how stale the advisory data behind each one is.
    /// **Absent unless an analyzer result is on record.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<LayerStaleness>>,
    /// Why there is nothing to report, and what to run. Present exactly when
    /// `coverage` is `no-analyzer-on-record`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_result_reason: Option<String>,
}

/// What one shipped analyzer covers — the coverage matrix, read off the code
/// rather than off a document, so the two cannot drift apart unnoticed.
///
/// `ready` is **machine-global**: it asks whether [`resolve`] finds every asset
/// this analyzer needs in this host's cache, still matching its recorded digest.
/// It is not a statement about any repository.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzerCoverage {
    /// The analyzer id.
    pub analyzer: &'static str,
    /// One line on what it looks for.
    pub summary: &'static str,
    /// The languages it produces findings for (ADR-0018's matrix).
    pub languages: &'static [&'static str],
    /// Whether every asset it needs is provisioned **on this machine** and still
    /// matches its digest.
    pub ready: bool,
}

/// The staleness of the advisory data behind one live findings layer.
///
/// Counts, never findings: this is the shape that lets a status document stay a
/// fixed size while a listing needs a page bound.
#[derive(Debug, Clone, Serialize)]
pub struct LayerStaleness {
    /// The layer key.
    pub layer: String,
    /// The analyzer that owns it.
    pub analyzer: String,
    /// How many findings it holds.
    pub findings: usize,
    /// Which backend produced it.
    pub runner: RunnerKind,
    /// The isolation boundary that run actually had.
    pub isolation: Isolation,
    /// The pinned advisory database it consulted, when it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory_db: Option<AdvisoryDb>,
    /// Days between the advisory database's publication and now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory_db_age_days: Option<i64>,
    /// `true` whenever an advisory database is involved at all. Never `false`
    /// meaning "current" — only "this result has no advisory-data axis".
    pub possibly_stale: bool,
}

/// The coverage matrix for `analyzer` (or every shipped analyzer), with each
/// one's readiness resolved against the asset cache at `root`.
///
/// Shared by the CLI's `security status` and both tool surfaces so `ready` is one
/// computation rather than three.
#[must_use]
pub fn coverage_matrix(root: &Path, analyzer: Option<&str>) -> Vec<AnalyzerCoverage> {
    ADAPTERS
        .iter()
        .filter(|a| analyzer.is_none_or(|name| a.analyzer() == name))
        .map(|adapter| AnalyzerCoverage {
            analyzer: adapter.analyzer(),
            summary: adapter.summary(),
            languages: adapter.languages(),
            ready: resolve(root, adapter.analyzer()).is_ok(),
        })
        .collect()
}

/// The advisory-staleness rows for `layers`, aged against `now` (an RFC 3339
/// timestamp, as [`crate::rfc3339_utc`] renders one).
///
/// Shared by the CLI's `security status` and both tool surfaces. `possibly_stale`
/// in particular is a judgement about evidence rather than a field to be copied:
/// three implementations of it would be three chances for one to say "current".
#[must_use]
pub fn layer_staleness(layers: &[FindingsLayer], now: &str) -> Vec<LayerStaleness> {
    layers
        .iter()
        .map(|layer| {
            // Staleness comes from the *run*, because the advisory database's
            // publication date is something the analyzer reported, not something
            // provisioning could know.
            let age = layer
                .run
                .advisory_db
                .as_ref()
                .and_then(|db| db.published_at.as_deref())
                .and_then(|published| age_in_days(published, now));
            LayerStaleness {
                layer: layer.run.layer.clone(),
                analyzer: layer.run.analyzer.clone(),
                findings: layer.findings.len(),
                runner: layer.run.runner,
                isolation: layer.run.isolation,
                advisory_db: layer.run.advisory_db.clone(),
                advisory_db_age_days: age,
                possibly_stale: layer.run.advisory_db.is_some(),
            }
        })
        .collect()
}

/// Build the tool-surface `security status` document.
///
/// `root` and `analyzer` govern the machine half; `project` and `layers` govern
/// the repository half. They are separate arguments because they are separate
/// facts, and the caller has to supply them from separate places — the asset root
/// from the host, the layers from the resolved project's store.
#[must_use]
pub fn security_status(
    root: &Path,
    analyzer: Option<&str>,
    project: &str,
    layers: &[FindingsLayer],
    now: &str,
) -> ToolSecurityStatus {
    let staleness = layer_staleness(layers, now);
    let (coverage, layers, reason) = if staleness.is_empty() {
        (
            Coverage::NoAnalyzerOnRecord,
            None,
            Some(NO_RESULT_REASON.to_owned()),
        )
    } else {
        (Coverage::Analyzed, Some(staleness), None)
    };

    ToolSecurityStatus {
        schema: TOOL_SECURITY_STATUS_SCHEMA,
        machine: MachineScope {
            scope: "machine",
            asset_root: root.display().to_string(),
            analyzers: coverage_matrix(root, analyzer),
            assets: status(root, analyzer),
        },
        repository: RepositoryScope {
            scope: "repository",
            project: project.to_owned(),
            coverage,
            layers,
            no_result_reason: reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Coverage, TOOL_SECURITY_LIST_SCHEMA, TOOL_SECURITY_STATUS_SCHEMA, layer_staleness,
        security_list, security_status,
    };
    use rto_graph::{
        AdvisoryDb, AnalysisRun, CommandPolicy, Finding, FindingKey, FindingsLayer, Isolation,
        RunnerKind, Severity, SourceIdentity,
    };

    fn run(analyzer: &str, advisory_db: Option<AdvisoryDb>) -> AnalysisRun {
        AnalysisRun {
            layer: format!("security:{analyzer}:wt"),
            analyzer: analyzer.to_owned(),
            analyzer_version: "1.0.0".to_owned(),
            runner: RunnerKind::Ingested,
            isolation: Isolation::Ingested,
            image_digest: None,
            rules_digest: None,
            advisory_db,
            command_policy: CommandPolicy::default(),
            source: SourceIdentity::default(),
            started_at: "2026-08-01T00:00:00Z".to_owned(),
            ended_at: "2026-08-01T00:00:01Z".to_owned(),
            exit_status: 0,
            report_digest: "deadbeef".to_owned(),
        }
    }

    fn finding(analyzer: &str, rule: &str, severity: Severity) -> Finding {
        Finding {
            key: FindingKey::new(analyzer, &[rule, crate::NO_SNIPPET]).expect("key"),
            rule: rule.to_owned(),
            severity,
            title: format!("{rule} title"),
            message: format!("{rule} message"),
            path: None,
            span: None,
            meta: serde_json::Value::Null,
        }
    }

    /// The trap the whole module is written against: an empty listing must not be
    /// a document a reader can mistake for a clean one.
    ///
    /// The assertion is deliberately about the **serialised** document rather than
    /// the struct: `findings` being `None` in Rust is worth nothing if serde still
    /// emits `"findings": 0`, and it is the JSON a model reads.
    #[test]
    fn nothing_analyzed_carries_no_findings_field_at_all() {
        let doc = security_list(Vec::new(), 20);
        assert_eq!(doc.coverage, Coverage::NoAnalyzerOnRecord);
        let json = serde_json::to_value(&doc).expect("serialise");
        assert_eq!(json["schema"], TOOL_SECURITY_LIST_SCHEMA);
        assert_eq!(json["coverage"], "no-analyzer-on-record");
        assert!(
            json.get("report").is_none(),
            "a listing with nothing to list must carry no report: {json}"
        );
        // The two fields a caller would reach for are absent, not zero. `0` is the
        // *good* answer for both, which is exactly why neither may appear here.
        assert!(json.get("findings").is_none(), "{json}");
        assert!(json.get("layers").is_none(), "{json}");
        let reason = json["no_result_reason"].as_str().expect("reason");
        assert!(reason.contains("NOT a clean result"), "{reason}");
    }

    /// The other half of the same trap, and the half that makes the first half
    /// mean something: a run that found nothing is `analyzed` with `findings: 0`.
    /// If both cases produced the same document the discriminator would be inert.
    #[test]
    fn a_clean_run_is_analyzed_with_zero_findings() {
        let layers = vec![FindingsLayer {
            run: run("semgrep", None),
            findings: Vec::new(),
        }];
        let doc = security_list(layers, 20);
        assert_eq!(doc.coverage, Coverage::Analyzed);
        let json = serde_json::to_value(&doc).expect("serialise");
        assert_eq!(json["coverage"], "analyzed");
        assert_eq!(json["report"]["findings"], 0);
        assert_eq!(json["report"]["layers"][0]["findings"], 0);
        assert!(json.get("no_result_reason").is_none(), "{json}");
    }

    /// The page bound is per layer, and every layer keeps its true count.
    ///
    /// The second layer is what this is really about: a document-wide bound would
    /// spend its budget on the first layer and report the second as empty, which
    /// reads as "that analyzer found nothing".
    #[test]
    fn the_page_bound_is_per_layer_and_never_hides_a_layer() {
        let layers = vec![
            FindingsLayer {
                run: run("cargo-audit", None),
                findings: (0..5)
                    .map(|i| finding("cargo-audit", &format!("RUSTSEC-{i}"), Severity::High))
                    .collect(),
            },
            FindingsLayer {
                run: run("semgrep", None),
                findings: (0..5)
                    .map(|i| finding("semgrep", &format!("rule-{i}"), Severity::Medium))
                    .collect(),
            },
        ];
        let doc = security_list(layers, 2);
        let report = doc.report.expect("analyzed");
        assert_eq!(report.findings, 10, "the true total survives the bound");
        assert_eq!(report.returned, 4, "two per layer, both layers reached");
        assert!(report.truncated);
        for layer in &report.layers {
            assert_eq!(layer.findings, 5, "true count per layer");
            assert_eq!(layer.page.len(), 2);
            assert_eq!(layer.omitted, 3);
            assert!(layer.truncated);
        }
    }

    /// A truncated page keeps the worst findings, not the alphabetically luckiest.
    ///
    /// The rule ids are ordered so that key order and severity order disagree:
    /// under the store's key ordering the critical would be cut and the
    /// informational kept.
    #[test]
    fn a_truncated_page_keeps_the_most_severe() {
        let layers = vec![FindingsLayer {
            run: run("semgrep", None),
            findings: vec![
                finding("semgrep", "aaa-info", Severity::Info),
                finding("semgrep", "bbb-low", Severity::Low),
                finding("semgrep", "zzz-critical", Severity::Critical),
            ],
        }];
        let doc = security_list(layers, 1);
        let report = doc.report.expect("analyzed");
        assert_eq!(report.layers[0].page.len(), 1);
        assert_eq!(report.layers[0].page[0].rule, "zzz-critical");
        assert_eq!(report.layers[0].omitted, 2);
    }

    /// An unbounded page is not a special case: `returned == findings` and nothing
    /// claims to be truncated.
    #[test]
    fn an_untruncated_listing_says_so() {
        let layers = vec![FindingsLayer {
            run: run("semgrep", None),
            findings: vec![finding("semgrep", "rule-1", Severity::High)],
        }];
        let report = security_list(layers, 20).report.expect("analyzed");
        assert_eq!(report.findings, 1);
        assert_eq!(report.returned, 1);
        assert!(!report.truncated);
        assert!(!report.layers[0].truncated);
        assert_eq!(report.layers[0].omitted, 0);
    }

    /// The two halves of a status document are separately labelled, and each
    /// scope's identifying value sits inside its own half.
    ///
    /// This is the property the issue was filed for: on a CLI the asymmetry is
    /// invisible and harmless, and over a tool surface a model must be able to
    /// tell "these digests are this machine's" from "this staleness is that
    /// repository's".
    #[test]
    fn status_labels_its_two_scopes_in_the_document() {
        let root = std::path::Path::new("/nonexistent-asset-root");
        let doc = security_status(root, None, "spoke", &[], "2026-08-19T00:00:00Z");
        let json = serde_json::to_value(&doc).expect("serialise");
        assert_eq!(json["schema"], TOOL_SECURITY_STATUS_SCHEMA);
        assert_eq!(json["machine"]["scope"], "machine");
        assert_eq!(json["repository"]["scope"], "repository");
        // The asset root is inside `machine` and the project inside `repository`,
        // so neither half can be quoted without the scope it belongs to.
        assert!(json["machine"]["asset_root"].is_string(), "{json}");
        assert_eq!(json["repository"]["project"], "spoke");
        assert!(json["machine"].get("project").is_none(), "{json}");
        assert!(json["repository"].get("asset_root").is_none(), "{json}");
    }

    /// The status document's repository half carries the same discriminator as the
    /// listing, so an unanalyzed project cannot read as a clean one there either.
    #[test]
    fn status_repository_half_distinguishes_unanalyzed_from_clean() {
        let root = std::path::Path::new("/nonexistent-asset-root");
        let empty = security_status(root, None, "p", &[], "2026-08-19T00:00:00Z");
        let json = serde_json::to_value(&empty).expect("serialise");
        assert_eq!(json["repository"]["coverage"], "no-analyzer-on-record");
        assert!(json["repository"].get("layers").is_none(), "{json}");
        assert!(
            json["repository"]["no_result_reason"]
                .as_str()
                .expect("reason")
                .contains("NOT a clean result")
        );

        let layers = vec![FindingsLayer {
            run: run("semgrep", None),
            findings: Vec::new(),
        }];
        let clean = security_status(root, None, "p", &layers, "2026-08-19T00:00:00Z");
        let json = serde_json::to_value(&clean).expect("serialise");
        assert_eq!(json["repository"]["coverage"], "analyzed");
        assert_eq!(json["repository"]["layers"][0]["findings"], 0);
    }

    /// `possibly_stale` is true whenever an advisory database is involved and
    /// false only when the result has no advisory-data axis at all — never
    /// "current". One computation, shared by the CLI and both tool surfaces.
    #[test]
    fn possibly_stale_tracks_the_presence_of_an_advisory_database() {
        let with_db = FindingsLayer {
            run: run(
                "cargo-audit",
                Some(AdvisoryDb {
                    digest: "abc".to_owned(),
                    published_at: Some("2026-08-09T00:00:00Z".to_owned()),
                }),
            ),
            findings: Vec::new(),
        };
        let without = FindingsLayer {
            run: run("semgrep", None),
            findings: Vec::new(),
        };
        let rows = layer_staleness(&[with_db, without], "2026-08-19T00:00:00Z");
        assert!(rows[0].possibly_stale);
        assert_eq!(rows[0].advisory_db_age_days, Some(10));
        assert!(!rows[1].possibly_stale);
        assert!(rows[1].advisory_db_age_days.is_none());
    }
}
