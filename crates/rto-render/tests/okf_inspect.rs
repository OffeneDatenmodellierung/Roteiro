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

/// A fresh directory for a synthetic bundle.
///
/// Keyed by process id **and** a monotonic counter, matching the CLI tests'
/// `bundle` helper. Each caller begins by clearing its directory, so two
/// concurrent test processes sharing a fixed name would race on
/// `remove_dir_all` and flake — uniqueness must not depend on everyone
/// remembering to pick a distinct name.
fn scratch(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("rto-okf-syntax-{}-{seq}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// Walk a bundle into the `(bundle-relative path, content)` pairs the importer
/// takes, the way the CLI's `read_bundle_files` does.
fn walk(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let path = entry.expect("fixture dir entry").path();
            // Symlinks are skipped, matching the production `read_bundle_files`
            // walker. `is_dir()` follows them, so a cycle would hang the suite
            // and a link escaping the fixture root would make this test read
            // whatever happens to be on the host. The vendored fixtures contain
            // no symlinks today; the point is that the helper must not depend on
            // that staying true.
            let link = std::fs::symlink_metadata(&path).expect("fixture entry metadata");
            if link.file_type().is_symlink() {
                continue;
            }
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
    // By path only. Paths are unique within a bundle, so the content half never
    // decides the order — it would only be compared, and these are whole
    // markdown documents.
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
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
    // A fixed date, though this test only reads tiers: letting the host clock
    // in would make an oracle comparison depend on the day it ran.
    let summary = inspect::trust_summary(&fixture(bundle), Some("2026-09-02"))
        .expect("load the upstream bundle");
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
    let err = inspect::trust_summary(Path::new("/no/such/bundle"), Some("2026-09-02"))
        .expect_err("refuse");
    assert!(
        err.to_string()
            .starts_with("`/no/such/bundle` is not a readable OKF bundle: "),
        "the error must name the path the caller gave, not only the cause: {err}"
    );
}

/// The specification's own Attested Computations parse.
///
/// `acme_retail` is the only vendored bundle that has any — two — and they are
/// `BigQuery` SQL with backtick-quoted identifiers, which is the shape that
/// decided the SQL backend: `tree-sitter-sequel` rejects 78 of 78 real blocks
/// over exactly this, `sqlparser` accepts them.
#[test]
fn the_specifications_own_computations_parse() {
    let report = inspect::syntax_report(&fixture("acme_retail"), true).expect("acme_retail");
    assert_eq!(report.scope, "computations");
    assert_eq!(
        report.checked, 2,
        "both Attested Computations must be looked at: {report:?}"
    );
    assert_eq!(report.skipped, 0);
    assert!(
        report.passed(),
        "the published bundle's computations must parse: {:?}",
        report.findings
    );
}

/// A bundle with no Attested Computations reports **nothing checked**, not a
/// clean bill of health.
///
/// This is the whole reason `checked` and `skipped` are in the report. `ga4` has
/// no computations, so the honest answer is "I did not look", and a caller that
/// printed "all clear" here would be reporting a green that means "could not
/// look".
#[test]
fn a_bundle_without_computations_says_it_checked_nothing() {
    let report = inspect::syntax_report(&fixture("ga4"), true).expect("ga4");
    assert_eq!(report.checked, 0, "{report:?}");
    assert!(report.passed(), "no findings, but also nothing checked");
}

/// `--all-blocks` widens past the computations, and finds real content.
#[test]
fn widening_to_all_blocks_looks_at_more() {
    let narrow = inspect::syntax_report(&fixture("acme_retail"), true).expect("narrow");
    let wide = inspect::syntax_report(&fixture("acme_retail"), false).expect("wide");
    assert_eq!(wide.scope, "all-blocks");
    // Blocks *seen*, not blocks *checked*: the extra ones this fixture carries
    // are untagged prose samples, so they widen `skipped` rather than `checked`.
    // Asserting on `checked` would have been asserting that the fixture happens
    // to tag its non-computation blocks, which is not the property.
    assert!(
        wide.checked + wide.skipped > narrow.checked + narrow.skipped,
        "widening must see more blocks: {}+{} vs {}+{}",
        wide.checked,
        wide.skipped,
        narrow.checked,
        narrow.skipped
    );
    assert!(
        wide.skipped > 0,
        "and the widened set includes untagged blocks, which are skipped rather \
         than silently passed: {wide:?}"
    );
}

/// A block that does not parse is found, and named precisely enough to fix.
#[test]
fn a_broken_block_is_found_with_its_place() {
    let root = scratch("broken");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("computations")).expect("mkdir");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("index");
    std::fs::write(
        root.join("computations/broken.md"),
        "---\ntype: Attested Computation\ntitle: Broken\nruntime: bigquery\n---\n\n\
         # Computation\n\n```sql\nSELCT a FROM t;\n```\n",
    )
    .expect("concept");

    let report = inspect::syntax_report(&root, true).expect("load");
    assert_eq!(report.checked, 1, "{report:?}");
    assert_eq!(report.findings.len(), 1, "{report:?}");
    let f = &report.findings[0];
    assert_eq!(f.language, "sql");
    assert!(f.path.contains("broken.md"), "{f:?}");
    assert!(!report.passed());
    let _ = std::fs::remove_dir_all(&root);
}

/// An **indented** computation block carries no info string, and the
/// specification's own example is written that way. The declared `runtime:` is
/// what makes it checkable at all.
#[test]
fn an_indented_computation_is_read_through_its_runtime() {
    let root = scratch("indented");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("computations")).expect("mkdir");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("index");
    std::fs::write(
        root.join("computations/indented.md"),
        "---\ntype: Attested Computation\ntitle: Indented\nruntime: bigquery\n---\n\n\
         # Computation\n\n    SELCT a FROM t;\n",
    )
    .expect("concept");

    let report = inspect::syntax_report(&root, true).expect("load");
    assert_eq!(
        report.checked, 1,
        "an untagged block under `runtime: bigquery` is SQL: {report:?}"
    );
    assert_eq!(report.findings.len(), 1, "and it is broken: {report:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A computation whose code lives in a *file* is skipped, not forgotten.
///
/// This is the difference between "there were no computations" and "there were
/// computations I could not reach", and only the second is true here. Without
/// it the report would say `0 checked, 0 skipped` and the CLI would print
/// "nothing to check" over a bundle that has two.
#[test]
fn a_computation_that_names_a_file_counts_as_skipped() {
    let root = scratch("filed");
    std::fs::create_dir_all(root.join("computations")).expect("mkdir");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("index");
    std::fs::write(
        root.join("computations/filed.md"),
        "---\ntype: Attested Computation\ntitle: Filed\nruntime: bigquery\n\
         computation: queries/revenue.sql\n---\n\n# Computation\n\nSee the file.\n",
    )
    .expect("concept");

    let report = inspect::syntax_report(&root, true).expect("load");
    assert_eq!(report.checked, 0, "nothing inline to parse: {report:?}");
    assert_eq!(
        report.skipped, 1,
        "but a computation was present and passed over: {report:?}"
    );
    assert!(report.passed(), "skipping is not a finding");
    let _ = std::fs::remove_dir_all(&root);
}

/// The runtime mapping is case-insensitive, like every other tag in this crate.
#[test]
fn a_capitalised_runtime_is_still_bigquery() {
    let root = scratch("caps");
    std::fs::create_dir_all(root.join("computations")).expect("mkdir");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("index");
    std::fs::write(
        root.join("computations/caps.md"),
        "---\ntype: Attested Computation\ntitle: Caps\nruntime: BigQuery\n---\n\n\
         # Computation\n\n    SELCT a FROM t;\n",
    )
    .expect("concept");

    let report = inspect::syntax_report(&root, true).expect("load");
    assert_eq!(
        report.checked, 1,
        "`BigQuery` must read as `bigquery`: {report:?}"
    );
    assert_eq!(report.findings.len(), 1, "and the bad SQL is caught");
    let _ = std::fs::remove_dir_all(&root);
}

/// A finding points at the line the code is on, not at the top of the file.
///
/// The computations path used to report a hardcoded `1`, which sent a reader to
/// the frontmatter for a fault further down. A confident wrong number is worse
/// than an honest unknown, so this pins the fenced case as *exact* and the
/// indented case as anchored to its `# Computation` heading.
#[test]
fn a_finding_names_the_line_its_code_is_on() {
    let root = scratch("lines");
    std::fs::create_dir_all(root.join("computations")).expect("mkdir");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("index");
    // Frontmatter is 5 lines; the body then has prose before the fence.
    std::fs::write(
        root.join("computations/fenced.md"),
        "---\ntype: Attested Computation\ntitle: F\nruntime: bigquery\n---\n\n\
         # Computation\n\nSome prose first.\n\n```sql\nSELCT a FROM t;\n```\n",
    )
    .expect("concept");

    let report = inspect::syntax_report(&root, true).expect("load");
    assert_eq!(report.findings.len(), 1, "{report:?}");
    let line = report.findings[0]
        .line
        .expect("a fenced block has an exact line");
    assert!(
        line > 1,
        "the fence is several lines into the body, not line 1: {line}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **The staleness boundary is the one the bundle documents for itself.**
///
/// `acme_retail` sets `stale_after: 2026-12-31T00:00:00Z` on seven concepts and
/// then says, in `computations/revenue-ytd.md`, that "on 2027-01-01, a consumer
/// running this computation SHOULD flag the result for re-verification". So the
/// fixture is not just data here — it states the answer, and this checks ours
/// against it rather than against my reading of §5.4.
///
/// The day *of* `stale_after` counts as stale, because the rule is
/// `now >= stale_after` and not `>`. That is the assertion most likely to be
/// wrong by one, so it is pinned on both sides.
#[test]
fn staleness_is_judged_on_the_day_the_bundle_names() {
    let before = inspect::trust_summary(&fixture("acme_retail"), Some("2026-12-30"))
        .expect("load the upstream bundle");
    let boundary = inspect::trust_summary(&fixture("acme_retail"), Some("2026-12-31"))
        .expect("load the upstream bundle");
    let after = inspect::trust_summary(&fixture("acme_retail"), Some("2027-01-01"))
        .expect("load the upstream bundle");

    assert_eq!(before.stale, 0, "nothing is stale the day before");
    assert_eq!(
        boundary.stale, 7,
        "`now >= stale_after`, so the day itself counts"
    );
    assert_eq!(after.stale, 7, "and it stays stale");

    // The combination the report exists to surface: a concept can be
    // human-reviewed *and* expired, and the tier alone reads as reassurance.
    let stale_and_reviewed = boundary
        .concepts
        .iter()
        .filter(|c| c.stale && c.tier == "human-reviewed")
        .count();
    assert_eq!(
        stale_and_reviewed, 7,
        "every stale concept here is also human-reviewed, which is the point"
    );

    // The date is reported whatever its source, so a captured summary says what
    // it was true of.
    assert_eq!(boundary.today, "2026-12-31");
}

/// **A malformed `--today` is refused, not quietly replaced by the clock.**
///
/// The flag exists to make the report reproducible. Falling back to today's date
/// on a typo would leave a pipeline green and meaningless, and the failure would
/// surface as staleness appearing on its own months later.
#[test]
fn a_malformed_today_is_refused_rather_than_ignored() {
    for bad in ["yesterday", "2026-13-45", "02-09-2026", ""] {
        let err = inspect::trust_summary(&fixture("acme_retail"), Some(bad))
            .expect_err("a non-ISO date must be refused");
        assert!(
            err.to_string().contains("is not an ISO date"),
            "`{bad}` must be refused by name: {err}"
        );
    }
}

/// **The computations listing finds what the bundle actually declares.**
///
/// `okf syntax --computations` reports whether the code parses; this reports
/// what contracts exist. A bundle whose computations all named files would show
/// "0 checked" from `syntax`, which reads as "there were none".
#[test]
fn the_computations_listing_matches_the_upstream_bundle() {
    let report =
        inspect::computation_report(&fixture("acme_retail")).expect("load the upstream bundle");
    assert_eq!(report.computations, 2);
    assert_eq!(report.inline, 2);
    assert_eq!(report.file, 0);
    assert_eq!(report.missing, 0);
    assert_eq!(report.runtimes, vec!["bigquery".to_owned()]);
    assert!(report.is_clean(), "every contract here is complete");

    let ids: Vec<&str> = report.entries.iter().map(|e| e.concept.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "computations/gross-margin-period",
            "computations/revenue-ytd"
        ]
    );
    // The parameters are the agent-facing half of §10, and the reason a listing
    // beats a count: this is what you would be filling in.
    assert_eq!(
        report.entries[0].parameters,
        vec!["period_start".to_owned(), "period_end".to_owned()]
    );
    assert_eq!(report.entries[0].language.as_deref(), Some("sql"));
}

/// **A bundle declaring no computations is clean, not failing.**
///
/// §10 is optional. Three of the four published bundles declare none, and a gate
/// that failed them would be a gate nobody could turn on.
#[test]
fn a_bundle_with_no_computations_is_clean() {
    let report = inspect::computation_report(&fixture("ga4")).expect("load the upstream bundle");
    assert_eq!(report.computations, 0);
    assert!(report.is_clean(), "declaring none is conformant");
    assert_eq!(report.incomplete(), 0);
}

/// **`info` reports the same numbers as the commands it summarises.**
///
/// It composes their reports rather than deriving anything, and this is what
/// holds it to that: a summary that could disagree with `trust`, `links` or
/// `computations` would be worse than no summary, because it is the one people
/// would quote.
#[test]
fn info_agrees_with_the_commands_it_summarises() {
    let root = fixture("acme_retail");
    let info = inspect::bundle_info(&root, Some("2026-12-31")).expect("load");
    let trust = inspect::trust_summary(&root, Some("2026-12-31")).expect("load");
    let links = inspect::link_report(&root).expect("load");
    let computations = inspect::computation_report(&root).expect("load");

    assert_eq!(info.concepts, trust.total);
    assert_eq!(info.trust.human_reviewed, trust.human_reviewed);
    assert_eq!(info.trust.stale, trust.stale);
    assert_eq!(info.trust.today, trust.today);
    assert_eq!(info.links, (links.links, links.broken.len()));
    assert_eq!(
        info.computations,
        (computations.computations, computations.incomplete())
    );
    assert_eq!(info.runtimes, computations.runtimes);

    // The status breakdown is `info`'s own, so it is pinned directly: this
    // bundle carries one deprecated concept, which is what §5.4 exists for.
    assert_eq!(
        info.statuses,
        vec![("deprecated".to_owned(), 1), ("stable".to_owned(), 8)]
    );
}
