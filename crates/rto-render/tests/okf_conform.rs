//! Conformance and hygiene checking, against the specification's own bundles.
//!
//! # These fixtures are the point
//!
//! Every rule here was written against `okf-validator` run as a **differential
//! oracle** over the four bundles published in the OKF specification repository
//! at `ad30107`. Upstream reports 200 diagnostics across them; this
//! implementation reports 194, and the whole difference is the six code-syntax
//! warnings that `roteiro okf syntax` now owns. Nothing is reported here that
//! upstream does not.
//!
//! Four of this module's rules were **wrong** before that comparison, and each
//! failed in the same direction — inventing a requirement the specification does
//! not state:
//!
//! - a missing `okf_version` was warned about, and §8/§12 say a root `index.md`
//!   *MAY* carry one. It fired on all four published bundles;
//! - a heading followed by a deeper heading was called empty, when it is a
//!   container;
//! - a concept linking twice to one deprecated target was reported twice;
//! - concept-id portability was an `error` citing §4, and the specification
//!   states no portability rule at all — it is now hygiene under `R1`.
//!
//! So the fixtures are not decoration. A synthetic corpus would have passed all
//! four of those, which is exactly what it did before the real one was run.
//!
//! Only two of the four bundles are vendored here (see
//! `tests/fixtures/okf-upstream/PROVENANCE.md`), so the rules the other two
//! exercise are pinned with synthetic documents below.

use std::path::{Path, PathBuf};

use rto_render::okf::conform;

/// The vendored upstream bundles.
fn fixture(bundle: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/okf-upstream")
        .join(bundle)
}

/// A fresh directory for a synthetic bundle.
///
/// Keyed by process id and a monotonic counter, for the reason the sibling
/// helpers give: each caller clears its directory first, so uniqueness must not
/// depend on everyone remembering to pick a distinct name.
fn scratch(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "rto-okf-conform-{}-{seq}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// Write a bundle and return its root.
fn bundle(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(tag);
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dir");
        std::fs::write(&path, content).expect("write concept");
    }
    root
}

const ROOT_INDEX: &str = "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n";

/// Messages of the findings carrying `code`, or all of them when `code` is
/// `None`.
fn messages(report: &conform::CheckReport, code: Option<&str>) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|f| code.is_none_or(|c| f.code == Some(c)))
        .map(|f| f.message.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// The published corpus
// ---------------------------------------------------------------------------

/// **The headline property: the specification's own bundles are conformant.**
///
/// Not one of the 200 diagnostics upstream reports over the four published
/// bundles is an error, and neither is one of ours. That is what makes
/// `validate` usable as a gate at all — a check that failed on warnings would
/// reject the corpus it was written against.
#[test]
fn the_published_bundles_carry_no_conformance_error() {
    for name in ["acme_retail", "ga4"] {
        let report = conform::validate_report(&fixture(name)).expect(name);
        assert!(
            report.passed(),
            "{name} must validate clean; errors: {:?}",
            report
                .findings
                .iter()
                .filter(|f| f.severity == "error")
                .collect::<Vec<_>>()
        );
        assert!(
            report.concepts > 0,
            "and it must have looked at something: {report:?}"
        );
    }
}

/// The counts agreed with upstream when this was written, and a drift in either
/// direction is worth failing over.
///
/// Pinned as exact numbers rather than "no errors", because the useful property
/// is *agreement with an independent implementation*, and a rule that quietly
/// stopped firing would keep "no errors" true.
#[test]
fn the_corpus_findings_match_what_upstream_reported() {
    let acme = conform::validate_report(&fixture("acme_retail")).expect("acme_retail");
    let found = messages(&acme, None);

    // Upstream reports exactly two over the *published* bundle, and so does this
    // implementation — verified against the full clone.
    assert_eq!(
        found.iter().filter(|m| m.contains("deprecated")).count(),
        2,
        "two concepts link to the retired `gross-margin-legacy`: {found:?}"
    );

    // The vendored copy is markdown-only, so `attesters/sql_equality.py` — which
    // `attester.resource` names and the published bundle does contain — is
    // absent here. Those two findings are the *fixture's* trim rather than a
    // disagreement with upstream, and they are asserted rather than tolerated
    // because they are also this suite's only evidence that the resource check
    // fires at all: no published bundle names a resource it lacks.
    assert_eq!(
        found
            .iter()
            .filter(|m| m.contains("sql_equality.py"))
            .count(),
        2,
        "the trimmed fixture is missing the attester script: {found:?}"
    );
    assert_eq!(found.len(), 4, "and nothing else: {found:?}");

    // `ga4` is clean over the published bundle. The vendored copy keeps only
    // `index.md`, `tables/index.md` and `tables/events_.md`, so the root index
    // lists two directories the trim removed — which makes this a *better*
    // assertion than "clean": the stale-index rule has to discriminate, and
    // `tables/index.md` is present and must not be reported.
    let ga4 = conform::validate_report(&fixture("ga4")).expect("ga4");
    assert!(ga4.passed(), "no errors: {:?}", messages(&ga4, None));
    let stale: Vec<String> = messages(&ga4, None)
        .into_iter()
        .filter(|m| m.contains("no longer exists"))
        .collect();
    assert_eq!(stale.len(), 2, "{stale:?}");
    assert!(
        stale.iter().any(|m| m.contains("datasets/index.md"))
            && stale.iter().any(|m| m.contains("references/index.md")),
        "{stale:?}"
    );
    assert!(
        !stale.iter().any(|m| m.contains("tables/index.md")),
        "`tables/index.md` is present, so reporting it would mean the rule \
         cannot tell a missing listing from a live one: {stale:?}"
    );
    // Everything else the trim causes is a broken link, which §6 tells a
    // consumer to tolerate — so `info`, never a warning.
    assert!(
        messages(&ga4, None)
            .iter()
            .filter(|m| m.contains("which the bundle does not contain"))
            .count()
            > 0
    );
    assert!(
        ga4.findings
            .iter()
            .filter(|f| f.message.contains("which the bundle does not contain"))
            .all(|f| f.severity == "info"),
        "§6: consumers MUST tolerate broken links: {:?}",
        ga4.findings
    );

    let lint = conform::lint_report(&fixture("acme_retail")).expect("lint acme_retail");
    assert_eq!(
        lint.findings.len(),
        26,
        "upstream reports 26 hygiene findings on acme_retail"
    );
    assert!(
        lint.passed(),
        "hygiene never gates: nothing here is an error"
    );
}

/// A concept that links twice to one deprecated target is one problem.
///
/// `metrics/gross-margin` in the specification's own `acme_retail` links to
/// `gross-margin-legacy` on two lines. An earlier draft reported it twice.
#[test]
fn one_deprecated_target_is_one_finding_however_many_links() {
    let report = conform::validate_report(&fixture("acme_retail")).expect("acme_retail");
    let from_gross_margin: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.concept.as_deref() == Some("metrics/gross-margin"))
        .collect();
    assert_eq!(
        from_gross_margin.len(),
        1,
        "two links, one retired target, one finding: {from_gross_margin:?}"
    );
}

/// A container heading is not an empty one.
///
/// `# Common query patterns` in `ga4` is followed immediately by `### 1. …`, so
/// its content lives under its subheadings.
#[test]
fn a_heading_with_subheadings_under_it_is_not_empty() {
    let report = conform::lint_report(&fixture("ga4")).expect("ga4");
    let empty = messages(&report, Some("L4"));
    assert!(
        empty.is_empty(),
        "no heading in ga4 is empty; these are containers: {empty:?}"
    );
}

/// `okf_version` is `MAY`, so its absence is not a finding — measured, because
/// **none** of the four published bundles declares one.
#[test]
fn a_bundle_without_okf_version_is_not_faulted_for_it() {
    for name in ["acme_retail", "ga4"] {
        let report = conform::validate_report(&fixture(name)).expect(name);
        assert!(
            !messages(&report, None)
                .iter()
                .any(|m| m.contains("okf_version")),
            "{name}: §8 and §12 both say MAY: {:?}",
            messages(&report, None)
        );
    }
}

// ---------------------------------------------------------------------------
// Rules the corpus does not exercise
// ---------------------------------------------------------------------------

/// The conformance errors, which the published corpus contains none of.
///
/// Without this the "no errors over the corpus" property above would be
/// satisfied by an implementation that can produce no error at all.
#[test]
fn the_error_severities_can_actually_fire() {
    let root = bundle(
        "errors",
        &[
            ("index.md", ROOT_INDEX),
            // No `type`, which §4.1 requires.
            (
                "metrics/untyped.md",
                "---\ntitle: Untyped\n---\n\n# Untyped\n",
            ),
            // Not parseable at all.
            (
                "metrics/broken.md",
                "---\ntype: [unclosed\n---\n\n# Broken\n",
            ),
        ],
    );
    let report = conform::validate_report(&root).expect("load");
    assert!(!report.passed(), "{report:?}");
    let errors: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.severity == "error")
        .map(|f| f.message.clone())
        .collect();
    assert!(
        errors.iter().any(|m| m.contains("`type` is missing")),
        "{errors:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A concept derived, through `sources`, from itself.
///
/// An error rather than a warning, and the only provenance finding that is: a
/// cycle means no reader can establish where the claim came from.
#[test]
fn circular_derivation_is_an_error_and_names_the_ring() {
    let root = bundle(
        "cycle",
        &[
            ("index.md", ROOT_INDEX),
            (
                "metrics/a.md",
                "---\ntype: Metric\ntitle: A\nsources:\n  - { id: b, resource: /metrics/b.md }\n---\n\n# A\n",
            ),
            (
                "metrics/b.md",
                "---\ntype: Metric\ntitle: B\nsources:\n  - { id: a, resource: /metrics/a.md }\n---\n\n# B\n",
            ),
        ],
    );
    let report = conform::validate_report(&root).expect("load");
    let cycles: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.message.contains("circular derivation"))
        .collect();
    assert_eq!(cycles.len(), 1, "one ring, reported once: {report:?}");
    assert_eq!(cycles[0].severity, "error");
    assert!(
        cycles[0].message.contains("metrics/a") && cycles[0].message.contains("metrics/b"),
        "the ring's members are named, because \"there is a cycle\" is not actionable: {}",
        cycles[0].message
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An index that lists something gone, and a concept no index lists: the two
/// directions of one question, and they must not contradict each other.
#[test]
fn a_stale_listing_and_an_unlisted_concept_are_both_found() {
    let root = bundle(
        "listings",
        &[
            ("index.md", ROOT_INDEX),
            (
                "metrics/index.md",
                "# Metrics\n\n* [Gone](gone.md) - a concept that was deleted.\n",
            ),
            (
                "metrics/here.md",
                "---\ntype: Metric\ntitle: Here\n---\n\n# Here\n",
            ),
        ],
    );
    let validate = conform::validate_report(&root).expect("validate");
    assert!(
        messages(&validate, None)
            .iter()
            .any(|m| m.contains("index lists `gone.md`")),
        "{:?}",
        messages(&validate, None)
    );

    let lint = conform::lint_report(&root).expect("lint");
    assert!(
        messages(&lint, Some("L9"))
            .iter()
            .any(|m| m.contains("no `index.md` lists this concept")),
        "`here.md` exists and nothing lists it: {:?}",
        messages(&lint, Some("L9"))
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `R1` is ours, and it says so.
///
/// The specification states no portability rule for path segments, so this
/// cannot be a conformance finding and must not borrow upstream's `L` numbering.
#[test]
fn portability_is_hygiene_under_our_own_code_not_conformance() {
    let root = bundle(
        "portability",
        &[
            ("index.md", ROOT_INDEX),
            (
                "metrics/Not Portable.md",
                "---\ntype: Metric\ntitle: NP\n---\n\n# NP\n",
            ),
        ],
    );
    let validate = conform::validate_report(&root).expect("validate");
    assert!(
        !messages(&validate, None)
            .iter()
            .any(|m| m.contains("portable")),
        "the specification requires no such thing, so validate must not claim it does: {:?}",
        messages(&validate, None)
    );

    let lint = conform::lint_report(&root).expect("lint");
    let ours = messages(&lint, Some("R1"));
    assert_eq!(ours.len(), 1, "{lint:?}");
    assert!(
        ours[0].contains("the specification does not forbid it"),
        "the finding must own up to being ours: {}",
        ours[0]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An Attested Computation with nothing to run and nothing to check it.
#[test]
fn an_incomplete_computation_contract_is_reported_part_by_part() {
    let root = bundle(
        "computation",
        &[
            ("index.md", ROOT_INDEX),
            (
                "computations/bare.md",
                "---\ntype: Attested Computation\ntitle: Bare\n---\n\n# Bare\n\nNo contract at all.\n",
            ),
        ],
    );
    let report = conform::validate_report(&root).expect("load");
    let found = messages(&report, None);
    for expected in [
        "`runtime` is missing",
        "missing `executor`",
        "missing `attester`",
    ] {
        assert!(
            found.iter().any(|m| m.contains(expected)),
            "expected `{expected}` among {found:?}"
        );
    }
    assert!(
        report.passed(),
        "each is a warning: an incomplete contract is still a readable document"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `stale_after` is checked for syntax and never against the clock.
///
/// The property is determinism: a bundle that validates today validates
/// tomorrow, which is what lets this be a gate.
#[test]
fn staleness_is_never_judged_against_the_clock() {
    let root = bundle(
        "stale",
        &[
            ("index.md", ROOT_INDEX),
            (
                "metrics/expired.md",
                "---\ntype: Metric\ntitle: Expired\nstale_after: 2000-01-01T00:00:00Z\n---\n\n# Expired\n",
            ),
            (
                "metrics/malformed.md",
                "---\ntype: Metric\ntitle: Malformed\nstale_after: yesterday\n---\n\n# Malformed\n",
            ),
        ],
    );
    let report = conform::validate_report(&root).expect("load");
    let found = messages(&report, None);
    assert!(
        !found.iter().any(|m| m.contains("stale since")),
        "a date long past is not a finding — only the clock could make it one: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|m| m.contains("`stale_after` is not an ISO-8601 datetime")),
        "but an unparseable one is, because that is a property of the text: {found:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle with no concepts reports that, rather than reporting nothing wrong.
#[test]
fn an_empty_bundle_says_it_examined_nothing() {
    let root = bundle("empty", &[("index.md", ROOT_INDEX)]);
    let report = conform::validate_report(&root).expect("load");
    assert_eq!(report.concepts, 0, "{report:?}");
    assert!(report.findings.is_empty());
    assert!(
        report.passed(),
        "nothing wrong, but `concepts` is what tells a reader nothing was looked at"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Findings come out errors first, then warnings, then info.
#[test]
fn findings_are_ordered_by_severity() {
    let root = bundle(
        "ordering",
        &[
            ("index.md", ROOT_INDEX),
            (
                "metrics/untyped.md",
                "---\ntitle: Untyped\n---\n\n# Untyped\n",
            ),
        ],
    );
    let report = conform::validate_report(&root).expect("load");
    let rank: Vec<u8> = report
        .findings
        .iter()
        .map(|f| match f.severity {
            "error" => 0,
            "warning" => 1,
            _ => 2,
        })
        .collect();
    assert!(
        rank.windows(2).all(|w| w[0] <= w[1]),
        "severity order is what a reader sees first: {:?}",
        report
            .findings
            .iter()
            .map(|f| (f.severity, &f.message))
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The two checks answer different questions and must not be wired to one.
#[test]
fn validate_and_lint_are_different_checks() {
    let acme = fixture("acme_retail");
    let validate = conform::validate_report(&acme).expect("validate");
    let lint = conform::lint_report(&acme).expect("lint");
    assert_eq!(validate.check, "validate");
    assert_eq!(lint.check, "lint");
    assert_ne!(
        validate.findings.len(),
        lint.findings.len(),
        "2 against 26 over this bundle; equal counts would suggest one entry point"
    );
    assert!(
        lint.findings.iter().all(|f| f.code.is_some()),
        "every hygiene finding carries its rule"
    );
    assert!(
        validate.findings.iter().all(|f| f.code.is_none()),
        "conformance findings carry none: they are the specification's rules, not ours"
    );
}

/// A path that is not a bundle is refused by name.
#[test]
fn a_path_that_is_not_a_bundle_is_refused() {
    let err = conform::validate_report(Path::new("/no/such/bundle")).expect_err("not a bundle");
    assert!(
        err.to_string().contains("/no/such/bundle"),
        "the refusal names the path the caller gave: {err}"
    );
}
