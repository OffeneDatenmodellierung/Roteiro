//! Inspect the specification's own bundles, and hold the importer to the
//! independent implementation's reading of them.
//!
//! `okf_interop.rs` (#710) asserts what Roteiro's **importer** makes of Google's
//! published bundles, with the independent implementation's counts quoted in a
//! comment as corroboration. This file turns that comment into a test.
//!
//! The distinction matters. A number copied into a doc comment is true on the
//! day it is written and silently stops being true afterwards; nothing fails
//! when the two implementations drift. Here both readings are computed in the
//! same process and compared per concept, so a divergence is a red test naming
//! the concept that diverged.
//!
//! This is the oracle that caught the original defect. The reader hand-parsed a
//! YAML subset shaped like its own writer's output and read all nine
//! `acme_retail` concepts as unverified when eight carry a human sign-off — a
//! failure invisible to any test that only round-tripped Roteiro's own output.
//! Keeping the oracle wired in is what stops the next one being found the same
//! way.

use std::path::{Path, PathBuf};

use rto_graph::Provenance;
use rto_render::okf::inspect;
use rto_render::okf::read::{ReadOptions, Trust, read_bundle};

/// The vendored upstream bundles. See `tests/fixtures/okf-upstream/PROVENANCE.md`
/// for their commit, licence and the trimming applied.
fn fixture(bundle: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/okf-upstream")
        .join(bundle)
}

/// Walk a bundle into the `(bundle-relative path, content)` pairs the importer
/// takes, the way the CLI's `read_bundle_files` does.
fn walk(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let path = entry.expect("fixture dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                let rel = path
                    .strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let content = std::fs::read_to_string(&path).expect("read fixture file");
                out.push((format!("/{rel}"), content));
            }
        }
    }
    out.sort();
    out
}

/// The tier the *importer* assigned each concept, keyed by concept id.
fn importer_tiers(bundle: &str) -> Vec<(String, String)> {
    let root = fixture(bundle);
    let files = walk(&root);
    let opts = ReadOptions {
        trust: Trust::Trust,
        peer: bundle,
        extref_keys: &[],
    };
    let import =
        read_bundle(&root.to_string_lossy(), &files, &opts).expect("read the upstream bundle");
    let prefix = format!("okf:{bundle}/");
    let mut tiers: Vec<(String, String)> = import
        .facts
        .nodes
        .iter()
        .filter_map(|n| {
            let id = n.key.strip_prefix(&prefix)?.strip_suffix(".md")?;
            // The importer's `External*` provenance is ADR-0021's mapping table
            // read backwards. Naming the OKF tier here, rather than comparing
            // enum to enum, keeps the assertion in the specification's
            // vocabulary — which is the vocabulary the two implementations are
            // supposed to agree in.
            let tier = match n.provenance {
                Provenance::ExternalAuthored => "human-reviewed",
                Provenance::ExternalDerived => "machine-confirmed",
                Provenance::ExternalInferred => "unverified",
                other => panic!("`{}` imported as {other:?}, not an external tier", n.key),
            };
            Some((id.to_owned(), tier.to_owned()))
        })
        .collect();
    tiers.sort();
    tiers
}

/// The tier `okf-core` assigned each concept, keyed by concept id.
fn oracle_tiers(bundle: &str) -> Vec<(String, String)> {
    let summary = inspect::trust_summary(&fixture(bundle)).expect("load the upstream bundle");
    let mut tiers: Vec<(String, String)> = summary
        .concepts
        .into_iter()
        .map(|c| (c.id, c.tier.to_owned()))
        .collect();
    tiers.sort();
    tiers
}

/// Roteiro's importer and the independent implementation must read the
/// specification's own bundle identically, **concept by concept**.
///
/// Asserted as whole vectors rather than as totals: a reader that got eight
/// concepts right and swapped the tiers of two others would satisfy any
/// aggregate count while being wrong about both documents.
#[test]
fn the_importer_and_the_independent_implementation_agree_on_every_tier() {
    assert_eq!(
        importer_tiers("acme_retail"),
        vec![
            (
                "computations/gross-margin-period".to_owned(),
                "human-reviewed".to_owned()
            ),
            (
                "computations/revenue-ytd".to_owned(),
                "human-reviewed".to_owned()
            ),
            (
                "metrics/gross-margin".to_owned(),
                "human-reviewed".to_owned()
            ),
            (
                "metrics/gross-margin-legacy".to_owned(),
                "human-reviewed".to_owned()
            ),
            ("metrics/revenue".to_owned(), "human-reviewed".to_owned()),
            (
                "policies/margin-standard".to_owned(),
                "human-reviewed".to_owned()
            ),
            (
                "policies/revenue-recognition".to_owned(),
                "human-reviewed".to_owned()
            ),
            ("skills/run-on-bq".to_owned(), "unverified".to_owned()),
            ("tables/orders".to_owned(), "human-reviewed".to_owned()),
        ],
        "the importer reads acme_retail as eight human-reviewed concepts and one \
         unverified (`skills/run-on-bq`, which carries `generated` and no `verified`)"
    );
    assert_eq!(
        importer_tiers("acme_retail"),
        oracle_tiers("acme_retail"),
        "Roteiro's importer and okf-core must agree per concept, not merely in \
         aggregate: a swap of two tiers is invisible to a total"
    );
}

/// The same agreement over the second bundle, whose frontmatter is
/// machine-serialised by `PyYAML` rather than hand-written.
#[test]
fn the_two_implementations_agree_on_a_machine_serialised_bundle() {
    assert_eq!(
        importer_tiers("ga4"),
        oracle_tiers("ga4"),
        "the agreement must not depend on frontmatter being hand-written in the \
         flow style the specification's examples use"
    );
}

/// A published, conformant bundle's internal links all resolve.
///
/// This checks an **emitted bundle**, which is a different artefact from the
/// graph and the rendered site that `roteiro check` covers, and one nothing else
/// in this workspace checks. ADR-0021 records that deriving a concept's path
/// from its node key guessed wrong for 43 links in a real render.
#[test]
fn a_published_bundle_has_no_broken_internal_links() {
    let report = inspect::link_report(&fixture("acme_retail")).expect("load acme_retail");
    assert_eq!(
        report.broken,
        Vec::new(),
        "a bundle published by the specification's own authors must not link to \
         concepts it does not contain"
    );
    assert!(
        report.links > 0,
        "acme_retail cross-links its concepts, so a report of zero links means \
         the scanner found nothing rather than that nothing was broken"
    );
    assert!(report.is_clean());
}

/// A bundle diffed against itself reports no change.
///
/// The floor for [`inspect::diff_report`], and the property ADR-0021 built
/// byte-determinism for: if comparing a bundle with itself invented changes, no
/// comparison of two downloads would mean anything.
#[test]
fn a_bundle_diffed_against_itself_is_unchanged() {
    let root = fixture("acme_retail");
    let diff = inspect::diff_report(&root, &root).expect("load acme_retail twice");
    assert!(
        diff.is_unchanged(),
        "a bundle must be semantically identical to itself, but the diff \
         reported: {diff:?}"
    );
}

/// Two *different* bundles must not diff as unchanged.
///
/// Without this, [`a_bundle_diffed_against_itself_is_unchanged`] would be
/// satisfied by a comparison that always returned "no change".
#[test]
fn two_different_bundles_do_not_diff_as_unchanged() {
    let diff =
        inspect::diff_report(&fixture("acme_retail"), &fixture("ga4")).expect("load both bundles");
    assert!(
        !diff.is_unchanged(),
        "acme_retail and ga4 share no concepts, so the diff must report changes"
    );
    assert!(
        !diff.removed.is_empty() && !diff.added.is_empty(),
        "every acme_retail concept is absent from ga4 and vice versa"
    );
}

/// A path that is not a bundle is refused by name.
#[test]
fn a_path_that_is_not_a_bundle_is_refused_by_name() {
    let err = inspect::trust_summary(Path::new("/no/such/bundle")).expect_err("refuse");
    assert!(
        err.to_string()
            .starts_with("`/no/such/bundle` is not a readable OKF bundle: "),
        "the error must name the path the caller gave, not only the cause: {err}"
    );
}

/// The specification's own bundle is conformant, and Roteiro can say so without
/// shelling out to another tool.
#[test]
fn a_published_bundle_validates_as_conformant() {
    let report = inspect::validate_report(&fixture("acme_retail")).expect("load acme_retail");
    assert_eq!(
        report.errors,
        0,
        "a bundle published by the specification's authors must carry no \
         conformance errors, but it reported: {:?}",
        report
            .findings
            .iter()
            .filter(|f| f.severity == "error")
            .collect::<Vec<_>>()
    );
    assert!(report.passed());
    assert_eq!(report.check, "validate");
}

/// Linting is a *different* question from conformance, and must be wired to the
/// different entry point.
///
/// Asserted by the label rather than by the findings: a bundle may legitimately
/// carry hygiene warnings while being perfectly conformant, so pinning a count
/// here would pin upstream's rule set rather than our wiring.
#[test]
fn linting_is_reported_separately_from_conformance() {
    let root = fixture("acme_retail");
    let lint = inspect::lint_report(&root).expect("lint acme_retail");
    assert_eq!(lint.check, "lint");
    assert_eq!(
        inspect::validate_report(&root)
            .expect("validate acme_retail")
            .check,
        "validate",
        "the two checks must not be wired to the same upstream entry point"
    );
}
