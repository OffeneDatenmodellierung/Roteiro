//! Inspect an OKF bundle **as a bundle**, without importing it.
//!
//! [`read`](super::read) answers "what would this add to the graph". This module
//! answers questions about the bundle itself — what it claims, whether it hangs
//! together, how it differs from another copy — and answers them with somebody
//! else's implementation of the specification.
//!
//! # Why an independent implementation is the whole value
//!
//! Roteiro both *writes* OKF (`render okf`) and *reads* it (`import --from
//! okf`). A reader of our own construction, run over our own output, would
//! agree with us about a format we also invent: it can only catch a mistake we
//! did not make twice. `okf-core` is an independent reading of the same
//! specification by an author who is not us, so its disagreement is
//! *information*.
//!
//! That is not hypothetical here. ADR-0021 records that deriving a concept's
//! path from its node key "guessed wrong for 43 links" in a real render, and the
//! reader's own YAML subset silently dropped every human sign-off in Google's
//! published bundles until an independent oracle was pointed at it. Both were
//! found by checking our output against something that did not share our
//! assumptions.
//!
//! # What is here, and what is not
//!
//! [`trust_summary`], [`link_report`] and [`diff_report`], all built on
//! `okf-core` — **one crate, zero transitive dependencies**.
//!
//! Conformance checking and hygiene linting are **not** here. They live
//! upstream in a second crate, `okf-validator`, whose dependencies are not
//! optional and which syntax-checks fenced code blocks in eight languages.
//! Taking it means taking `rustpython-parser`: 61 crates, `LGPL-3.0-only`
//! through the `malachite` tree, and six unmaintained advisories whose own text
//! says no safe upgrade exists. `cargo deny` refuses it on both counts, and
//! ADR-0017 §3 is explicit that a licence is not admitted merely to turn CI
//! green.
//!
//! That price bought two of the validator's thirty-four checks, both of them
//! about whether embedded *code* parses rather than whether the *bundle*
//! conforms. See `Cargo.toml` for the full measurement.
//!
//! # Subcommand names are upstream's
//!
//! `trust`, `links` and `diff` match the `okf` CLI's own names for the same
//! operations, so somebody who knows that tool already knows this one. The
//! library is called **in-process**; Roteiro is a self-contained offline binary
//! and requiring `okf` on `PATH` would reintroduce exactly the coupling the
//! vendored interop fixtures exist to avoid.

use std::path::Path;

use okf_core::{Bundle, TrustTier};
use serde::Serialize;

/// Why a bundle could not be inspected.
///
/// One variant today: every failure here is "the path is not a bundle we could
/// load". The underlying [`okf_core::BundleError`] is rendered into the message
/// rather than wrapped, so this type stays free of the dependency in its public
/// shape.
///
/// `#[non_exhaustive]` because that set is closed by nothing but current
/// implementation — unlike [`super::Actor`], whose three variants are closed by
/// §7 of the specification and which is deliberately exhaustive for that reason.
/// A second failure mode here (a bundle that loads but declares an OKF version
/// this crate cannot read, say) is an ordinary addition, and these crates are
/// published, so it must not be a breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InspectError {
    /// The path could not be loaded as an OKF bundle.
    #[error("`{path}` is not a readable OKF bundle: {detail}")]
    Unreadable {
        /// The path as the caller gave it.
        path: String,
        /// What `okf-core` said went wrong.
        detail: String,
    },
}

/// Load a bundle, naming the path in the error rather than only the cause.
fn load(root: &Path) -> Result<Bundle, InspectError> {
    Bundle::load(root).map_err(|e| InspectError::Unreadable {
        path: root.display().to_string(),
        detail: e.to_string(),
    })
}

/// A concept's trust claim, as the bundle states it.
#[derive(Debug, Clone, Serialize)]
pub struct ConceptTrust {
    /// The concept's path within the bundle, minus `.md`.
    pub id: String,
    /// §5.3's tier: `human-reviewed`, `machine-confirmed` or `unverified`.
    pub tier: &'static str,
    /// The lifecycle `status` §5.4 resolves for this concept.
    pub status: String,
    /// Every actor named in `verified`, in the order the document wrote them.
    ///
    /// Present even when the tier is `unverified`: an event with an unparseable
    /// timestamp does not count toward the tier but is still an attribution the
    /// bundle made, and dropping it would hide *why* the tier came out low.
    pub verified_by: Vec<String>,
}

/// What a bundle claims about its own trustworthiness.
///
/// This is the answer to "should I trust this bundle", stated per concept and in
/// aggregate, and it is deliberately a **plain data type over a path**: it is
/// exactly the information a consent prompt wants at the moment it asks, and
/// nothing here needs the import machinery to have run first.
#[derive(Debug, Clone, Serialize)]
pub struct TrustSummary {
    /// The bundle root, as the caller named it.
    pub root: String,
    /// The `okf_version` the root `index.md` declares (§10), if any.
    pub okf_version: Option<String>,
    /// Concepts read, excluding the reserved `index.md` / `log.md` files.
    pub total: usize,
    /// Concepts carrying at least one valid `human:` verifier.
    pub human_reviewed: usize,
    /// Concepts verified only by non-`human:` actors.
    pub machine_confirmed: usize,
    /// Concepts with no valid `verified` event.
    pub unverified: usize,
    /// Every concept, in bundle order.
    pub concepts: Vec<ConceptTrust>,
}

/// Derive [`TrustSummary`] for the bundle at `root`.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if the path is not a loadable OKF bundle.
pub fn trust_summary(root: &Path) -> Result<TrustSummary, InspectError> {
    Ok(summarise_trust(&load(root)?, &root.display().to_string()))
}

/// The bundle-in-hand half of [`trust_summary`].
///
/// Split out so a caller that has already loaded a [`Bundle`] — to validate it,
/// or to ask a person whether to import it — pays for the directory walk once.
#[must_use]
pub fn summarise_trust(bundle: &Bundle, root: &str) -> TrustSummary {
    let mut summary = TrustSummary {
        root: root.to_owned(),
        okf_version: bundle.okf_version().map(ToOwned::to_owned),
        total: bundle.concepts().len(),
        human_reviewed: 0,
        machine_confirmed: 0,
        unverified: 0,
        concepts: Vec::with_capacity(bundle.concepts().len()),
    };
    for concept in bundle.concepts() {
        let tier = concept.trust_tier();
        match tier {
            TrustTier::HumanReviewed => summary.human_reviewed += 1,
            TrustTier::MachineConfirmed => summary.machine_confirmed += 1,
            TrustTier::Unverified => summary.unverified += 1,
        }
        summary.concepts.push(ConceptTrust {
            id: concept.id.to_string(),
            tier: tier.as_str(),
            status: concept.status().to_string(),
            verified_by: concept
                .document
                .frontmatter
                .verified()
                .into_iter()
                .filter_map(|v| v.by.map(|by| by.as_str().to_owned()))
                .collect(),
        });
    }
    summary
}

/// A markdown link that names a concept the bundle does not contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokenLink {
    /// The concept whose body carries the link.
    pub from: String,
    /// The link target, exactly as written.
    pub target: String,
}

/// Whether an emitted bundle's internal links resolve.
///
/// Roteiro's own link checking (`roteiro check`) covers the **graph** and the
/// **rendered site**. Neither looks at an emitted OKF bundle, which is a third
/// artefact produced by a third code path — the one ADR-0021 records guessing
/// wrong for 43 links.
#[derive(Debug, Clone, Serialize)]
pub struct LinkReport {
    /// The bundle root, as the caller named it.
    pub root: String,
    /// Concepts read.
    pub concepts: usize,
    /// Internal concept links found across every body.
    pub links: usize,
    /// Those that resolve to no concept in the bundle.
    pub broken: Vec<BrokenLink>,
}

impl LinkReport {
    /// `true` when every internal link resolves.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.broken.is_empty()
    }
}

/// Resolve every internal link in the bundle at `root`.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if the path is not a loadable OKF bundle.
pub fn link_report(root: &Path) -> Result<LinkReport, InspectError> {
    let bundle = load(root)?;
    let links = bundle
        .concepts()
        .iter()
        .map(|c| bundle.links_from(&c.id).len())
        .sum();
    Ok(LinkReport {
        root: root.display().to_string(),
        concepts: bundle.concepts().len(),
        links,
        broken: bundle
            .broken_links()
            .into_iter()
            .map(|(from, target)| BrokenLink {
                from: from.to_string(),
                target,
            })
            .collect(),
    })
}

/// A concept whose trust tier or lifecycle status moved between two bundles.
#[derive(Debug, Clone, Serialize)]
pub struct TrustMove {
    /// The concept that moved.
    pub id: String,
    /// `(before, after)` tiers, when the tier changed.
    pub tier: Option<(String, String)>,
    /// `(before, after)` statuses, when the status changed.
    pub status: Option<(String, String)>,
}

/// What changed between two bundles, semantically rather than by bytes.
///
/// ADR-0021 made `render okf` byte-deterministic specifically so "a consumer can
/// diff two downloads and learn something". This is that diff, and it is the
/// first thing in the workspace to exercise the determinism: `review --base`
/// diffs code, not bundles.
///
/// A **rename** is the interesting field. A textual diff of two bundles reports
/// a moved concept as one deletion and one unrelated addition; this reports it
/// as a rename, which is the difference between "we lost a concept" and "we
/// moved one".
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    /// The bundle taken as "before".
    pub before: String,
    /// The bundle taken as "after".
    pub after: String,
    /// Concepts present only in `after`.
    pub added: Vec<String>,
    /// Concepts present only in `before`.
    pub removed: Vec<String>,
    /// Concepts whose path changed, as `(from, to)`.
    pub renamed: Vec<(String, String)>,
    /// Concepts whose body changed.
    pub content_changed: Vec<String>,
    /// Concepts whose frontmatter keys changed.
    pub frontmatter_changed: Vec<String>,
    /// Concepts whose tier or status moved. The one to read first.
    pub trust_changed: Vec<TrustMove>,
    /// Links that broke between `before` and `after`, as `(concept, target)`.
    pub links_broken: Vec<(String, String)>,
    /// Links that were broken in `before` and resolve in `after`.
    pub links_mended: Vec<(String, String)>,
}

impl DiffReport {
    /// `true` when the two bundles are semantically identical.
    #[must_use]
    pub fn is_unchanged(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
            && self.content_changed.is_empty()
            && self.frontmatter_changed.is_empty()
            && self.trust_changed.is_empty()
            && self.links_broken.is_empty()
            && self.links_mended.is_empty()
    }
}

/// Compare two bundles semantically.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if either path is not a loadable OKF bundle.
pub fn diff_report(before: &Path, after: &Path) -> Result<DiffReport, InspectError> {
    let a = load(before)?;
    let b = load(after)?;
    let d = okf_core::bundle_diff(&a, &b);
    let ids = |v: Vec<okf_core::ConceptId>| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    let pairs = |v: Vec<(okf_core::ConceptId, String)>| {
        v.into_iter()
            .map(|(id, t)| (id.to_string(), t))
            .collect::<Vec<_>>()
    };
    Ok(DiffReport {
        before: before.display().to_string(),
        after: after.display().to_string(),
        added: ids(d.added),
        removed: ids(d.removed),
        renamed: d
            .renamed
            .into_iter()
            .map(|r| (r.from.to_string(), r.to.to_string()))
            .collect(),
        content_changed: ids(d.content),
        frontmatter_changed: d.frontmatter.iter().map(|c| c.id.to_string()).collect(),
        trust_changed: d
            .trust
            .into_iter()
            .map(|t| TrustMove {
                id: t.id.to_string(),
                tier: t
                    .tier
                    .map(|(a, b)| (a.as_str().to_owned(), b.as_str().to_owned())),
                status: t.status.map(|(a, b)| (a.to_string(), b.to_string())),
            })
            .collect(),
        links_broken: pairs(d.broken_links),
        links_mended: pairs(d.mended_links),
    })
}

/// One code block that did not parse.
#[derive(Debug, Clone, Serialize)]
pub struct SyntaxFinding {
    /// The concept the block belongs to.
    pub concept: String,
    /// The concept's file, relative to the bundle root.
    pub path: String,
    /// 1-indexed line of the block's opening fence within that file's body,
    /// when it could be determined.
    ///
    /// `None` for a computation whose code this crate could not locate in the
    /// body — an indented block with no `# Computation` heading to anchor it.
    /// Reporting a confident `1` there was worse than reporting nothing: it sent
    /// a reader to the frontmatter for a fault further down the file.
    pub line: Option<usize>,
    /// The language the block was tagged with, canonicalised.
    pub language: String,
    /// What the parser said.
    pub message: String,
}

/// The result of syntax-checking a bundle's code blocks.
///
/// `checked` and `skipped` are both reported, deliberately. A language with no
/// backend compiled in is *not checked* rather than *clean*, and a report that
/// conflated the two would be a check that passes by not looking.
#[derive(Debug, Clone, Serialize)]
pub struct SyntaxReport {
    /// The bundle root, as the caller named it.
    pub root: String,
    /// `computations` or `all-blocks` — what was looked at.
    pub scope: &'static str,
    /// Blocks a backend actually parsed.
    pub checked: usize,
    /// Blocks left alone, for any of three reasons: the block carried no
    /// language tag, this build has no backend for the language it carried, or
    /// the computation named a file rather than inlining its code.
    ///
    /// All three are "not looked at" rather than "looked at and clean", which is
    /// the distinction the whole report exists to keep.
    pub skipped: usize,
    /// The languages this build can check, so a reader can tell why.
    pub languages: Vec<String>,
    /// Findings, in bundle order.
    pub findings: Vec<SyntaxFinding>,
}

impl SyntaxReport {
    /// `true` when nothing failed to parse.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

/// The language an untagged computation block should be read as.
///
/// Only `bigquery` is mapped, and only because the corpus justifies it: every
/// `runtime:` in the four bundles published with the specification is
/// `bigquery`, and the spec's own Attested Computation example writes its query
/// as an *indented* block, which carries no info string. Without this the one
/// case that matters most would never be checked.
///
/// Deliberately not a general runtime→language table. Inventing a mapping for
/// runtimes nobody has written yet is how a reader ends up with a confident
/// diagnostic about a language the author never claimed.
fn language_for_runtime(runtime: Option<&str>) -> Option<&'static str> {
    // Case-insensitive, because every other tag here is: `Language::from_tag`
    // lowercases, so `runtime: BigQuery` reading differently from `bigquery`
    // would be an inconsistency inside one function's worth of code.
    match runtime.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
        Some("bigquery") => Some("sql"),
        _ => None,
    }
}

/// Syntax-check the code blocks in a bundle.
///
/// With `computations_only`, just the bodies of Attested Computations — the
/// concepts that declare a `runtime:` and that an agent is expected to *run*, so
/// the ones where "does this parse" is a question about the bundle rather than
/// about its prose. Otherwise every fenced block in every document.
///
/// Findings are the checker's, not conformance: a bundle can be perfectly
/// conformant and contain a code sample that does not parse, which is why this
/// is its own command rather than part of validation.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if the path is not a loadable OKF bundle.
pub fn syntax_report(root: &Path, computations_only: bool) -> Result<SyntaxReport, InspectError> {
    let bundle = load(root)?;
    let languages = rto_okf_syntax::checkable_languages()
        .into_iter()
        .map(|l| l.as_str().to_owned())
        .collect();
    let mut report = SyntaxReport {
        root: root.display().to_string(),
        scope: if computations_only {
            "computations"
        } else {
            "all-blocks"
        },
        checked: 0,
        skipped: 0,
        languages,
        findings: Vec::new(),
    };

    for concept in bundle.concepts() {
        let rel = concept
            .path
            .strip_prefix(bundle.root())
            .unwrap_or(&concept.path)
            .display()
            .to_string();

        if computations_only {
            let Some(computation) = concept.attested_computation() else {
                continue;
            };
            let okf_core::ComputationSource::Inline(inline) = &computation.computation else {
                // A `computation:` file reference is checked by whatever owns
                // that file, and a `Missing` one has no code to check at all.
                // Counted as **skipped** rather than passed over silently: a
                // bundle whose computations all name files would otherwise
                // report "0 checked, 0 skipped" and print "nothing to check",
                // which reads as "there were none" when there were several.
                report.skipped += 1;
                continue;
            };
            // An indented block carries no info string, so fall back to the
            // declared runtime — see `language_for_runtime`.
            let tag = inline
                .language
                .as_deref()
                .or_else(|| language_for_runtime(computation.runtime.as_deref()))
                .unwrap_or("");
            let line = computation_line(&concept.document.body, &inline.code);
            record(
                &mut report,
                &concept.id.to_string(),
                &rel,
                line,
                tag,
                &inline.code,
            );
        } else {
            for block in rto_okf_syntax::extract_fenced_code_blocks(&concept.document.body) {
                let tag = block.language.as_deref().unwrap_or("");
                record(
                    &mut report,
                    &concept.id.to_string(),
                    &rel,
                    Some(block.start_line),
                    tag,
                    &block.code,
                );
            }
        }
    }

    Ok(report)
}

/// Where a computation's code starts in its document.
///
/// The fenced case is exact: the same extractor the all-blocks path uses finds
/// the block whose contents are the computation's, and reports its opening
/// fence. The indented case cannot be — `okf-core` dedents the code, so it no
/// longer matches the file byte for byte — and the `# Computation` heading is the
/// honest anchor there: it is where a reader should look, even though it is not
/// where the parser stopped.
///
/// `None` rather than a confident `1` when neither is found. Pointing a reader at
/// the frontmatter for a fault further down the file is worse than admitting the
/// line is unknown.
fn computation_line(body: &str, code: &str) -> Option<usize> {
    let wanted = code.trim();
    if let Some(block) = rto_okf_syntax::extract_fenced_code_blocks(body)
        .into_iter()
        .find(|b| b.code.trim() == wanted)
    {
        return Some(block.start_line);
    }
    body.lines().enumerate().find_map(|(i, l)| {
        l.trim_start()
            .strip_prefix('#')
            .is_some_and(|rest| rest.trim().eq_ignore_ascii_case("computation"))
            .then_some(i + 1)
    })
}

/// Check one block and fold the outcome into the report.
fn record(
    report: &mut SyntaxReport,
    concept: &str,
    path: &str,
    line: Option<usize>,
    tag: &str,
    code: &str,
) {
    let language = rto_okf_syntax::Language::from_tag(tag);
    if !rto_okf_syntax::is_checkable(language) {
        report.skipped += 1;
        return;
    }
    report.checked += 1;
    if let Err(err) = rto_okf_syntax::check_syntax(tag, code) {
        report.findings.push(SyntaxFinding {
            concept: concept.to_owned(),
            path: path.to_owned(),
            line,
            language: err.language.clone(),
            message: err.to_string(),
        });
    }
}
