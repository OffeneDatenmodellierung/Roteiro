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
//! # 3. A readiness claim names what it has actually checked
//!
//! `roteiro security status` used to label one analyzer `ready` on the strength of
//! its *pinned assets* being provisioned. Running it needs a second thing — the
//! analyzer's own program on `PATH` — and that is the one Roteiro deliberately
//! **never installs** (ADR-0014). So on a host with the rules provisioned and
//! `semgrep` absent, the old report read `semgrep  ready` and the run then failed
//! with `analyzer binary not found on PATH`. Both statements were true about
//! different things and only one of them used the word *ready* (issue #464).
//!
//! `docs/REVIEW_CHECKLIST.md` has the rule this is a corollary of — *a refusal
//! names the way forward* — applied to a report rather than a refusal: **a
//! readiness claim names what it has actually checked.** And it is the same shape
//! as §1, one field over: a caller that cannot run `command -v` — which is every
//! caller on a tool surface — will read `ready` as *this will run*.
//!
//! [`Readiness`] is therefore three states rather than a `bool`, because **the
//! remedy differs**: `assets-not-provisioned` is fixed by `prefetch`, which
//! Roteiro performs; `binary-not-found` is fixed by an install, which it refuses
//! to perform; `ready` is both. Both underlying facts are reported alongside it,
//! so a host missing both is fully readable in one call rather than in two.
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
/// Every field here is a property of the host — its asset cache under
/// [`crate::asset_root`] and its `PATH` — and none of it is a property of any
/// repository. A `ready` analyzer means this machine *could* run it; it says
/// nothing at all about whether it has been run anywhere, which is the
/// `repository` half's question.
#[derive(Debug, Clone, Serialize)]
pub struct MachineScope {
    /// Always `"machine"`. Redundant with this section's name on purpose: a model
    /// that quotes the section alone still carries its scope with it.
    pub scope: &'static str,
    /// The pinned-asset cache these digests describe.
    pub asset_root: String,
    /// What each shipped analyzer covers, read off the adapters rather than off a
    /// document, and whether this machine can actually run it — **both** halves of
    /// that, since they have different remedies (see [`Readiness`]).
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

/// Whether one analyzer can actually be run **on this host**, as one word.
///
/// Three states rather than a `bool`, because the two things a host run needs have
/// different remedies and only one of them is Roteiro's to perform (issue #464):
///
/// | state | what is missing | the fix |
/// | --- | --- | --- |
/// | `ready` | nothing | — |
/// | `assets-not-provisioned` | a pinned asset, or its bytes no longer match | `roteiro security prefetch` |
/// | `binary-not-found` | the analyzer's own program, on `PATH` | an install; **Roteiro never does this** |
///
/// # Precedence, and why both facts are still reported
///
/// A host can be missing both. This names the asset side first, because that is
/// the step Roteiro can take and the one a caller should take first — but a
/// one-word verdict that names one blocker would send a caller round twice, so
/// [`AnalyzerCoverage`] carries `assets_provisioned` and `missing_programs`
/// alongside it. Both are always present; this is a summary of them, never a
/// substitute.
///
/// # What "on this host" excludes, and it is not a caveat on the word
///
/// The sandboxed backend runs the analyzer inside a digest-pinned OCI image
/// (ADR-0014/ADR-0019), which supplies the program — so `binary-not-found` does
/// **not** block a sandboxed run, and it is the only state where the two backends
/// disagree. This says nothing about sandbox readiness: it does not inspect the
/// local image store, and reporting a sandbox verdict it has not checked would be
/// issue #464 committed a second time. `security run` still refuses, naming what
/// is missing, when the sandbox cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Readiness {
    /// Every pinned asset is provisioned and verified, and every program this
    /// analyzer needs is on `PATH`.
    Ready,
    /// A pinned asset is absent, or its bytes no longer match the recorded digest.
    /// Fixed by `roteiro security prefetch`.
    AssetsNotProvisioned,
    /// The assets are fine and the analyzer's own program is not on `PATH`. Fixed
    /// by installing it — which Roteiro will not do. This is the same fact
    /// `SubprocessError::BinaryNotFound` reports, found before a run rather than
    /// during one.
    BinaryNotFound,
}

impl Readiness {
    /// The token this serialises as, for a caller that renders it as text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::AssetsNotProvisioned => "assets not provisioned",
            Self::BinaryNotFound => "binary not found",
        }
    }
}

/// The three-state verdict from the two facts it is built from.
///
/// A pure function so the precedence rule above is checkable without a
/// provisioned asset cache or a controlled `PATH` — neither of which a test can
/// arrange here, since `unsafe_code = "forbid"` rules out `std::env::set_var`.
#[must_use]
fn readiness(assets_provisioned: bool, missing_programs: &[&str]) -> Readiness {
    if !assets_provisioned {
        Readiness::AssetsNotProvisioned
    } else if missing_programs.is_empty() {
        Readiness::Ready
    } else {
        Readiness::BinaryNotFound
    }
}

/// Whether `program` resolves to an executable file in any of `dirs`.
///
/// A **read**, and that is load-bearing on a tool surface: it stats candidate
/// paths and never starts a process. Probing by running `<program> --version`
/// would be executing a third-party binary because a model asked a question, which
/// is the thing this whole surface refuses.
///
/// Split from [`on_path`] so the lookup is testable against a directory a test
/// owns, rather than against the process environment it cannot change.
#[must_use]
fn program_in(dirs: &[std::path::PathBuf], program: &str) -> bool {
    // A name containing a separator is a path rather than a `PATH` lookup — the
    // same rule `std::process::Command::new` follows, so this agrees with what a
    // run would actually do.
    if std::path::Path::new(program).components().count() > 1 {
        return is_executable_file(std::path::Path::new(program));
    }
    dirs.iter()
        .any(|dir| is_executable_file(&dir.join(program)))
}

/// Whether `program` resolves to an executable file on this process's `PATH`.
#[must_use]
fn on_path(program: &str) -> bool {
    let Some(var) = std::env::var_os("PATH") else {
        return false;
    };
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&var).collect();
    program_in(&dirs, program)
}

/// Whether `path` is a file this host would execute.
///
/// Follows symlinks, because a symlinked binary is exactly as runnable as a real
/// one and every package manager installs one.
#[cfg(unix)]
#[must_use]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether `path` is a file this host would execute.
///
/// There are no mode bits to consult, so being a file is the whole check, and the
/// `.exe` sibling is tried because that is what every program named by an adapter
/// here ships as off Unix. The full `PATHEXT` set is deliberately **not** walked:
/// none of these analyzers ships as a `.bat` or `.cmd`, and a probe that guessed
/// wider would report a readiness it had not established — which is the defect
/// [`Readiness`] exists to remove.
#[cfg(not(unix))]
#[must_use]
fn is_executable_file(path: &std::path::Path) -> bool {
    if path.is_file() {
        return true;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => path.with_file_name(format!("{name}.exe")).is_file(),
        None => false,
    }
}

/// What one shipped analyzer covers — the coverage matrix, read off the code
/// rather than off a document, so the two cannot drift apart unnoticed.
///
/// Every field is **machine-global**. Nothing here is a statement about any
/// repository: it asks what this host has provisioned and what it has installed,
/// and the answer is the same whichever project was selected.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzerCoverage {
    /// The analyzer id.
    pub analyzer: &'static str,
    /// One line on what it looks for.
    pub summary: &'static str,
    /// The languages it produces findings for (ADR-0018's matrix).
    pub languages: &'static [&'static str],
    /// Whether this host could run it, and if not, which remedy applies. A
    /// summary of the two fields below — see [`Readiness`].
    pub host_readiness: Readiness,
    /// Whether every pinned asset it needs is provisioned **on this machine** and
    /// still matches its digest. Fixed by `roteiro security prefetch`.
    pub assets_provisioned: bool,
    /// Every program it needs on `PATH` to run on this host
    /// ([`crate::Adapter::host_programs`]).
    pub host_programs: &'static [&'static str],
    /// Which of those are **not** on `PATH`. Empty exactly when all are present.
    /// Named individually because the name is the actionable part: Roteiro does not
    /// install these, so the reader has to know which one to go and get.
    pub missing_programs: Vec<&'static str>,
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

/// The coverage matrix for `analyzer` (or every shipped analyzer), with each one's
/// readiness resolved against the asset cache at `root` **and** this process's
/// `PATH`.
///
/// Shared by the CLI's `security status` and both tool surfaces, so the readiness
/// rule is one computation rather than three — the same reason
/// [`layer_staleness`] is shared, and the reason issue #464 was one fix rather
/// than three.
#[must_use]
pub fn coverage_matrix(root: &Path, analyzer: Option<&str>) -> Vec<AnalyzerCoverage> {
    coverage_matrix_with(root, analyzer, on_path)
}

/// [`coverage_matrix`] with the `PATH` probe supplied by the caller.
///
/// The probe is an argument for the reason provisioning takes its fetcher as one:
/// it keeps the decision testable without the ambient state it would otherwise
/// depend on. A test cannot change this process's `PATH` — `unsafe_code =
/// "forbid"` rules out `std::env::set_var` — so without this seam two of the three
/// [`Readiness`] states would be unreachable from a test, on a machine where
/// whether they are reachable at all depends on what happens to be installed.
#[must_use]
pub fn coverage_matrix_with(
    root: &Path,
    analyzer: Option<&str>,
    on_path: impl Fn(&str) -> bool,
) -> Vec<AnalyzerCoverage> {
    ADAPTERS
        .iter()
        .filter(|a| analyzer.is_none_or(|name| a.analyzer() == name))
        .map(|adapter| {
            let host_programs = adapter.host_programs();
            let missing_programs: Vec<&'static str> = host_programs
                .iter()
                .copied()
                .filter(|program| !on_path(program))
                .collect();
            let assets_provisioned = resolve(root, adapter.analyzer()).is_ok();
            AnalyzerCoverage {
                analyzer: adapter.analyzer(),
                summary: adapter.summary(),
                languages: adapter.languages(),
                host_readiness: readiness(assets_provisioned, &missing_programs),
                assets_provisioned,
                host_programs,
                missing_programs,
            }
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
        Coverage, Readiness, TOOL_SECURITY_LIST_SCHEMA, TOOL_SECURITY_STATUS_SCHEMA,
        coverage_matrix_with, layer_staleness, program_in, readiness, security_list,
        security_status,
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

    /// The three states, and the precedence between them (issue #464).
    ///
    /// The table is exhaustive over the two facts on purpose: the defect being
    /// fixed is one `bool` standing in for two, so the test that matters is the one
    /// that walks all four combinations and shows that three distinct answers come
    /// out — and that the fourth, both-missing, is not silently the same as
    /// "binary missing".
    #[test]
    fn readiness_names_the_remedy_that_applies() {
        assert_eq!(readiness(true, &[]), Readiness::Ready);
        assert_eq!(
            readiness(false, &[]),
            Readiness::AssetsNotProvisioned,
            "assets missing, binary present"
        );
        assert_eq!(
            readiness(true, &["semgrep"]),
            Readiness::BinaryNotFound,
            "the state the old `ready: bool` could not express"
        );
        // Both missing names the asset side, because `prefetch` is the step Roteiro
        // itself performs and the one to take first. The other fact is not lost —
        // `AnalyzerCoverage` carries `missing_programs` alongside this verdict, which
        // `coverage_matrix_reports_both_facts_not_just_the_verdict` is about.
        assert_eq!(
            readiness(false, &["semgrep"]),
            Readiness::AssetsNotProvisioned,
            "both missing must not read as a binary-only problem"
        );
    }

    /// `ready` must mean both things, so a provisioned host with the binary absent
    /// is `binary-not-found` and not `ready`.
    ///
    /// This is issue #464's actual defect, and it is **not reproducible on the
    /// machine most likely to look for it**: a developer working on Roteiro has the
    /// analyzers installed, so the old `ready` was accidentally true there. The
    /// `PATH` probe is therefore injected rather than read from the environment —
    /// `unsafe_code = "forbid"` rules out `std::env::set_var`, so a test cannot
    /// arrange the absence any other way, and a test that depended on what happens
    /// to be installed would pass or fail for reasons that have nothing to do with
    /// this code.
    #[test]
    fn a_provisioned_analyzer_with_no_binary_is_not_ready() {
        // An asset root that cannot resolve, so the asset axis is fixed and the only
        // thing varying is the probe.
        let root = std::path::Path::new("/nonexistent-asset-root");

        // Every program present: the asset axis is what is left, and it decides.
        let all_present = coverage_matrix_with(root, Some("semgrep"), |_| true);
        assert_eq!(
            all_present[0].host_readiness,
            Readiness::AssetsNotProvisioned
        );
        assert!(all_present[0].missing_programs.is_empty());

        // Nothing present: same asset state, and the verdict still names the asset
        // remedy first — but the missing program is reported rather than hidden.
        let none_present = coverage_matrix_with(root, Some("semgrep"), |_| false);
        assert_eq!(
            none_present[0].host_readiness,
            Readiness::AssetsNotProvisioned
        );
        assert_eq!(none_present[0].missing_programs, vec!["semgrep"]);
    }

    /// All three states through the **public wiring**, on a genuinely provisioned
    /// asset cache — which is the only way `ready` and `binary-not-found` are
    /// reachable at all.
    ///
    /// Without this, every `coverage_matrix_with` test would run against an
    /// unprovisioned root, so `host_readiness` would be `assets-not-provisioned`
    /// whatever the probe said — and a `coverage_matrix_with` that ignored
    /// `missing_programs` entirely would pass the lot. That is a guard sampling the
    /// cheap projection instead of the claim.
    ///
    /// `semgrep-rules` is a *vendored* asset, so [`provision`] installs and digests
    /// it from bytes already compiled in: no network, no fetcher, and the same
    /// function `prefetch` calls, so what is provisioned here is what `resolve`
    /// accepts in earnest.
    #[test]
    fn all_three_states_are_reachable_on_a_provisioned_cache() {
        use crate::assets::{assets_for, provision};

        let root = std::env::temp_dir().join(format!(
            "rto-exec-readiness-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::remove_dir_all(&root).ok();
        for spec in assets_for("semgrep") {
            provision(&root, spec).expect("vendored asset provisions with no fetcher");
        }

        // Assets provisioned, program present: `ready` now means both, which is the
        // whole of issue #464.
        let ready = coverage_matrix_with(&root, Some("semgrep"), |_| true);
        assert_eq!(ready[0].host_readiness, Readiness::Ready);
        assert!(ready[0].assets_provisioned);
        assert!(ready[0].missing_programs.is_empty());

        // Same cache, program absent. This is the case the old `ready: bool`
        // reported as `ready`, and the run then failed with `analyzer binary not
        // found on PATH`.
        let no_binary = coverage_matrix_with(&root, Some("semgrep"), |_| false);
        assert_eq!(
            no_binary[0].host_readiness,
            Readiness::BinaryNotFound,
            "provisioned assets alone must not earn the word `ready`"
        );
        assert!(
            no_binary[0].assets_provisioned,
            "the asset half is still true, and still reported"
        );
        assert_eq!(no_binary[0].missing_programs, vec!["semgrep"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The verdict is a summary of two published facts, never a replacement for
    /// them: a caller told only "not ready" would have to guess which remedy applies.
    #[test]
    fn coverage_matrix_reports_both_facts_not_just_the_verdict() {
        let root = std::path::Path::new("/nonexistent-asset-root");
        let rows = coverage_matrix_with(root, None, |_| false);
        assert_eq!(rows.len(), 3, "one row per shipped analyzer");
        for row in &rows {
            let json = serde_json::to_value(row).expect("serialise");
            assert_eq!(json["assets_provisioned"], false, "{json}");
            assert!(json["host_programs"].is_array(), "{json}");
            assert!(json["missing_programs"].is_array(), "{json}");
            assert_eq!(json["host_readiness"], "assets-not-provisioned", "{json}");
            // The boolean the old shape published is gone, not renamed alongside:
            // a consumer reading `ready` was reading a claim about running computed
            // from provisioning, and leaving it in place would keep that available.
            assert!(json.get("ready").is_none(), "{json}");
        }
    }

    /// `cargo-audit` declares **both** `cargo` and `cargo-audit`, and the second is
    /// the one that decides.
    ///
    /// A probe built from `Invocation::program` would look for `cargo` alone, find it
    /// on any Rust developer's machine, and report `ready` in exactly the commonest
    /// failure — `cargo` installed, `cargo-audit` not. That is issue #464
    /// reintroduced one level down, which is why `Adapter::host_programs` is declared
    /// rather than derived.
    #[test]
    fn cargo_audit_is_not_ready_on_cargo_alone() {
        let root = std::path::Path::new("/nonexistent-asset-root");
        let rows = coverage_matrix_with(root, Some("cargo-audit"), |program| program == "cargo");
        assert_eq!(rows[0].host_programs, &["cargo", "cargo-audit"]);
        assert_eq!(
            rows[0].missing_programs,
            vec!["cargo-audit"],
            "`cargo` being present must not stand in for the subcommand binary"
        );
    }

    /// The `PATH` lookup itself: an executable file resolves, a non-executable one
    /// does not, and an absent one does not.
    ///
    /// Against a directory the test owns, because it cannot change this process's
    /// `PATH`. The middle case is the point — a readable file with no execute bit is
    /// not something the host will run, and treating it as one would be a readiness
    /// claim that had not been established.
    #[test]
    fn the_path_probe_requires_an_executable_file() {
        let dir = std::env::temp_dir().join(format!(
            "rto-exec-path-probe-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let exec = dir.join("runnable");
        std::fs::write(&exec, b"#!/bin/sh\ntrue\n").expect("write");
        let plain = dir.join("not-runnable");
        std::fs::write(&plain, b"data").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }

        let dirs = vec![dir.clone()];
        assert!(program_in(&dirs, "runnable"), "an executable file resolves");
        assert!(!program_in(&dirs, "absent"), "a name with no file does not");
        #[cfg(unix)]
        assert!(
            !program_in(&dirs, "not-runnable"),
            "a file with no execute bit is not something this host runs"
        );
        // A name with a separator is a path rather than a lookup, matching what
        // `Command::new` would do with it.
        assert!(program_in(&[], exec.to_str().expect("utf-8")));
        assert!(!program_in(&[], "/nonexistent/runnable"));

        std::fs::remove_dir_all(&dir).ok();
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
