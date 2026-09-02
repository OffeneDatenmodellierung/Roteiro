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
//! okf`). A checker of our own construction, run over our own output, would
//! agree with us about a format we also invent: it can only catch a mistake we
//! did not make twice. `okf-core` and `okf-validator` are an independent reading
//! of the same specification by an author who is not us, so their disagreement
//! is *information*.
//!
//! That is not hypothetical here. ADR-0021 records that deriving a concept's
//! path from its node key "guessed wrong for 43 links" in a real render, and the
//! reader's own YAML subset silently dropped every human sign-off in Google's
//! published bundles until an independent oracle was pointed at it. Both were
//! found by checking our output against something that did not share our
//! assumptions.
//!
//! # What is here, and what it costs
//!
//! The two crates behind this module differ in price by two orders of magnitude:
//!
//! | Surface | Crate | Cost |
//! | --- | --- | --- |
//! | [`trust_summary`], [`link_report`], [`diff_report`] | `okf-core` | **one crate**, zero transitive |
//! | [`validate_report`], [`lint_report`] | `okf-validator` | **73 crates** |
//!
//! Both are **unconditional**, which for the second was a decision rather than a
//! default. It was behind a feature first, and the gate was removed on the
//! reasoning that a conformance checker nobody enables checks nothing: the whole
//! value of an independent implementation is that it runs. See `Cargo.toml` for
//! the measurement and ADR-0017 for the policy the adoption was argued under.
//!
//! # Subcommand names are upstream's
//!
//! `trust`, `links`, `diff`, `validate` and `lint` match the `okf` CLI's own
//! names for the same operations, so somebody who knows that tool already knows
//! this one. The library is called **in-process**; Roteiro is a self-contained
//! offline binary and requiring `okf` on `PATH` would reintroduce exactly the
//! coupling the vendored interop fixtures exist to avoid.

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
