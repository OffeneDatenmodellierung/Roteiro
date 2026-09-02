//! End-to-end tests for `roteiro okf` — inspecting a bundle rather than
//! importing it (ADR-0021).
//!
//! These test the **CLI wiring**: that each action reaches the right library
//! entry point, that `--json` parses, and that `--check` gates while the bare
//! command does not. Every inspecting action ships in a stock build — `validate`
//! and `lint` were behind a feature earlier on this branch and are not any more,
//! so there is no unavailable-surface case left to test. (`view` is the one
//! exception, and it is a server rather than a report: it lives behind
//! `okf-viewer` and is tested in `crates/roteiro/src/okf_viewer.rs`.)
//! What the underlying checks *mean* is settled in `rto-render`'s
//! `okf_inspect.rs` against the specification's own published bundles; there is
//! no value in asserting it twice, and a fixture copied into two crates is a
//! fixture that will disagree with itself.
//!
//! The bundles here are written inline for that reason: they are the smallest
//! thing that exercises a code path, not a claim about what real OKF looks like.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn roteiro(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("run roteiro")
}

/// Write a bundle under a fresh temp directory and return its root.
///
/// Keyed by process id, a monotonic counter **and** `tag`. The counter is what
/// makes this safe rather than the tag: tests run in parallel and each begins by
/// clearing its directory, so two callers that happened to pass the same `tag`
/// would race on `remove_dir_all` and flake. Uniqueness must not depend on
/// everyone remembering to pick a fresh name — the tag is there to make a
/// failure legible, not to keep it correct.
fn bundle(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "roteiro-okf-inspect-{}-{seq}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create bundle dir");
        std::fs::write(&path, content).expect("write concept");
    }
    root
}

/// A concept verified by a human, and one verified by nobody.
fn two_tier_bundle(tag: &str) -> PathBuf {
    bundle(
        tag,
        &[
            // The root `index.md` declares `okf_version` and **nothing else**;
            // adding a `type` here is a conformance error. This bundle was
            // originally written that way and an OKF conformance check caught
            // it — worth keeping correct even though nothing here validates it
            // today, because the fixture is what a conformance check would run
            // against when one lands.
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/revenue.md",
                "---\ntype: Metric\ntitle: Revenue\nverified: { by: human:alice, at: 2026-08-01T10:00:00Z }\n---\n\n# Definition\n",
            ),
            (
                "metrics/cost.md",
                "---\ntype: Metric\ntitle: Cost\ngenerated: { by: agent/1.0, at: 2026-08-01T10:00:00Z }\n---\n\n# Definition\n",
            ),
        ],
    )
}

/// `okf trust` reports §5.3's tier per concept, and the aggregate a person
/// deciding whether to `--trust` an import actually needs.
#[test]
fn trust_reports_the_tier_of_every_concept() {
    let root = two_tier_bundle("trust");
    let out = roteiro(&["okf", "trust", &root.to_string_lossy()]);
    assert!(out.status.success(), "okf trust should succeed");
    let stdout = String::from_utf8(out.stdout).expect("utf-8");

    assert!(
        stdout.contains("  human-reviewed 1, machine-confirmed 0, unverified 1\n"),
        "the aggregate line must state all three tiers, so a bundle with none of \
         one is legible as zero rather than absent; got:\n{stdout}"
    );
    assert!(
        stdout.contains("human-reviewed     metrics/revenue — verified by human:alice\n"),
        "a human-verified concept must name its verifier: who signed off is the \
         load-bearing half of the claim; got:\n{stdout}"
    );
    assert!(
        stdout.contains("unverified         metrics/cost\n"),
        "a concept with `generated` and no `verified` is unverified, not unknown; \
         got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `--json` emits the same summary as a machine-readable document.
#[test]
fn trust_emits_json_on_request() {
    let root = two_tier_bundle("trust-json");
    let out = roteiro(&["okf", "trust", &root.to_string_lossy(), "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json must emit parseable JSON");

    assert_eq!(v["total"], 2);
    assert_eq!(v["human_reviewed"], 1);
    assert_eq!(v["unverified"], 1);
    assert_eq!(
        v["okf_version"], "0.2",
        "the root index's declared version (§10) belongs in the summary"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A broken link is reported, and **only `--check` makes it a gate**.
///
/// Both halves matter. Reporting without gating is what you want when reading
/// somebody else's bundle; gating is what you want in CI over your own. A
/// command that only did one of them would be wrong half the time.
#[test]
fn a_broken_link_is_reported_and_gates_only_under_check() {
    let root = bundle(
        "links",
        &[
            (
                "a.md",
                "---\ntype: Metric\n---\n\n# A\n\nSee [B](./b.md) and [gone](./gone.md).\n",
            ),
            ("b.md", "---\ntype: Metric\n---\n\n# B\n"),
        ],
    );
    let path = root.to_string_lossy().into_owned();

    let plain = roteiro(&["okf", "links", &path]);
    let stdout = String::from_utf8(plain.stdout).expect("utf-8");
    assert!(
        plain.status.success(),
        "without --check a broken link is reported, not gated: inspecting a \
         peer's bundle must not fail the command"
    );
    assert!(
        stdout.contains("  broken: a -> ./gone.md\n"),
        "the broken link must name both the concept and the target as written; \
         got:\n{stdout}"
    );

    let gated = roteiro(&["okf", "links", &path, "--check"]);
    assert_eq!(
        gated.status.code(),
        Some(1),
        "--check must exit 1 on a broken link, or it is not a gate"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle compared with itself reports no change; two different ones do.
///
/// The negative half is the one that matters: without it, a `diff` that always
/// printed "no semantic change" would pass.
#[test]
fn diff_separates_an_unchanged_bundle_from_a_changed_one() {
    let a = two_tier_bundle("diff-a");
    let b = bundle(
        "diff-b",
        &[(
            "metrics/revenue.md",
            "---\ntype: Metric\ntitle: Revenue\nverified: { by: human:alice, at: 2026-08-01T10:00:00Z }\n---\n\n# Definition\n",
        )],
    );

    let same = roteiro(&["okf", "diff", &a.to_string_lossy(), &a.to_string_lossy()]);
    let stdout = String::from_utf8(same.stdout).expect("utf-8");
    assert!(
        stdout.contains("  no semantic change\n"),
        "a bundle must be semantically identical to itself; got:\n{stdout}"
    );

    let changed = roteiro(&["okf", "diff", &a.to_string_lossy(), &b.to_string_lossy()]);
    let stdout = String::from_utf8(changed.stdout).expect("utf-8");
    assert!(
        stdout.contains("  removed  metrics/cost\n"),
        "a concept present in `before` and absent from `after` is a removal; \
         got:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// A path that is not a directory is refused in Roteiro's own words.
#[test]
fn a_path_that_is_not_a_bundle_is_refused_by_name() {
    let out = roteiro(&["okf", "trust", "/no/such/bundle"]);
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(
        stderr,
        "Error: OKF bundle not found: /no/such/bundle (expected a directory of concept documents)\n",
        "the refusal must name the path and say what was expected"
    );
}

/// The vendored upstream fixtures are `rto-render`'s, and this test does not
/// reach for them — a deliberate boundary recorded so the next person does not
/// "fix" it by duplicating them here.
#[test]
fn the_upstream_fixtures_stay_in_one_crate() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/okf-upstream");
    assert!(
        !here.exists(),
        "the specification's published bundles are vendored once, under \
         crates/rto-render/tests/fixtures/okf-upstream, with their provenance and \
         licence recorded beside them. A second copy would drift from the first"
    );
}

/// `okf syntax` gates on a block that does not parse, and says where.
#[test]
fn a_computation_that_does_not_parse_fails_the_command() {
    let root = bundle(
        "syntax-broken",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "computations/revenue.md",
                "---\ntype: Attested Computation\ntitle: Revenue\nruntime: bigquery\n---\n\n\
                 # Computation\n\n```sql\nSELCT total FROM `p.d.orders`;\n```\n",
            ),
        ],
    );
    let path = root.to_string_lossy().into_owned();
    let out = roteiro(&["okf", "syntax", &path]);
    assert!(
        !out.status.success(),
        "a computation that does not parse must fail the command"
    );
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(text.contains("revenue.md"), "names the file: {text}");
    assert!(text.contains("sql"), "names the language: {text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle with nothing to check says so, rather than reporting success.
///
/// The distinction this pins is the reason the report separates *checked* from
/// *skipped*. "0 findings" over 0 blocks is not a pass, and a command that
/// printed one would be the green that means "could not look".
#[test]
fn nothing_to_check_is_reported_as_nothing_rather_than_as_clean() {
    let root = bundle(
        "syntax-empty",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/prose.md",
                "---\ntype: Metric\ntitle: Prose\n---\n\n# Definition\n\nNo code here.\n",
            ),
        ],
    );
    let path = root.to_string_lossy().into_owned();
    let out = roteiro(&["okf", "syntax", &path]);
    assert!(out.status.success(), "nothing wrong, so it must not gate");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        text.contains("0 block(s) checked"),
        "the count is stated: {text}"
    );
    assert!(
        text.contains("nothing to check"),
        "and it is not dressed up as a pass: {text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `okf validate` gates on a conformance error, and names the concept.
#[test]
fn a_conformance_error_fails_the_validate_command() {
    let root = bundle(
        "validate-error",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            // §4.1 requires `type` on every concept.
            (
                "metrics/untyped.md",
                "---\ntitle: Untyped\n---\n\n# Untyped\n\nProse.\n",
            ),
        ],
    );
    let path = root.to_string_lossy().into_owned();
    let out = roteiro(&["okf", "validate", &path]);
    assert!(!out.status.success(), "a conformance error must gate");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(text.contains("metrics/untyped"), "names it: {text}");
    assert!(text.contains("`type` is missing"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// `okf lint` reports and never gates.
///
/// The two commands differ in kind and not only in outcome: hygiene has no error
/// severity at all, so a lint run that found plenty still exits zero.
#[test]
fn lint_reports_without_gating() {
    let root = bundle(
        "lint-noisy",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/messy.md",
                "---\ntitle: Messy\ntype: Metric\nstatus: draft\n---\n\nNo heading at all.   \n",
            ),
        ],
    );
    let path = root.to_string_lossy().into_owned();
    let out = roteiro(&["okf", "lint", &path]);
    assert!(
        out.status.success(),
        "hygiene never gates, however much it finds"
    );
    let text = String::from_utf8(out.stdout).expect("utf-8");
    for code in ["[L1]", "[L12]"] {
        assert!(text.contains(code), "expected {code} in: {text}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The two are wired to different checks, and say which they are.
#[test]
fn validate_and_lint_are_not_the_same_command() {
    let root = two_tier_bundle("conform-json");
    let path = root.to_string_lossy().into_owned();

    let v: serde_json::Value =
        serde_json::from_slice(&roteiro(&["okf", "validate", &path, "--json"]).stdout)
            .expect("validate --json");
    let l: serde_json::Value =
        serde_json::from_slice(&roteiro(&["okf", "lint", &path, "--json"]).stdout)
            .expect("lint --json");

    assert_eq!(v["check"], "validate");
    assert_eq!(l["check"], "lint");
    // `concepts` is what tells a reader the check looked at something — without
    // it, "no findings" over an empty bundle reads as a clean bill of health.
    assert_eq!(v["concepts"], 2, "{v}");
    assert_eq!(l["concepts"], 2, "{l}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle with **no concepts** still gates when something in it is an error.
///
/// The report is right and the *printer* was wrong: an early return on
/// `concepts == 0` skipped both the findings and the gate, so `okf validate`
/// exited 0 on a bundle whose only document does not parse. A vacuous green in
/// the command whose whole job is to refuse one.
#[test]
fn no_concepts_does_not_mean_nothing_to_gate_on() {
    let root = bundle(
        "no-concepts-error",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            // Does not parse, so it is a conformance error *and* contributes no
            // concept — the exact pair the early return hid.
            (
                "metrics/broken.md",
                "---\ntype: [unclosed\n---\n\n# Broken\n",
            ),
        ],
    );
    let path = root.to_string_lossy().into_owned();
    let out = roteiro(&["okf", "validate", &path]);
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        !out.status.success(),
        "an unreadable document is an error however few concepts survived it: {text}"
    );
    assert!(
        text.contains("no concepts examined"),
        "and it still says nothing was examined: {text}"
    );
    assert!(text.contains("broken.md"), "and names the file: {text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle whose one concept expires on a date this test names.
///
/// Written inline with an absolute `stale_after` rather than one computed from
/// the clock: a fixture that expires relative to "now" would make this test pass
/// or fail depending on the day it ran, which is the exact failure `--today`
/// exists to prevent. Testing determinism with a non-deterministic fixture would
/// be a joke at our own expense.
fn expiring_bundle(tag: &str) -> PathBuf {
    bundle(
        tag,
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/revenue.md",
                "---\ntype: Metric\ntitle: Revenue\nstale_after: 2026-12-31T00:00:00Z\n\
                 verified:\n  - by: human:alice\n    at: 2026-01-01T00:00:00Z\n---\n\n\
                 # Revenue\n\nTotal revenue.\n",
            ),
        ],
    )
}

/// `--today` makes staleness a function of the bundle, and `--check` gates on it.
///
/// The three dates are the boundary and its two sides, because `now >=
/// stale_after` is the assertion most likely to be wrong by one.
#[test]
fn trust_judges_staleness_against_the_given_day_and_gates_on_it() {
    let root = expiring_bundle("stale");
    let path = root.to_string_lossy().into_owned();

    let before = roteiro(&["okf", "trust", &path, "--today", "2026-12-30"]);
    assert!(before.status.success());
    assert!(
        String::from_utf8_lossy(&before.stdout).contains("stale 0 (as of 2026-12-30)"),
        "nothing is stale the day before"
    );

    let on = roteiro(&["okf", "trust", &path, "--today", "2026-12-31"]);
    let on_stdout = String::from_utf8_lossy(&on.stdout).into_owned();
    assert!(
        on_stdout.contains("stale 1 (as of 2026-12-31)"),
        "the day itself counts, because the rule is `now >= stale_after`; got:\n{on_stdout}"
    );
    assert!(
        on_stdout.contains("[STALE since 2026-12-31T00:00:00Z]"),
        "the concept line must say *since when*, not merely that it expired; got:\n{on_stdout}"
    );

    // Reporting without gating is what you want reading somebody else's bundle.
    assert!(
        on.status.success(),
        "the bare command reports and does not gate"
    );
    assert!(
        !roteiro(&["okf", "trust", &path, "--today", "2026-12-31", "--check"])
            .status
            .success(),
        "--check must gate on staleness"
    );
    assert!(
        roteiro(&["okf", "trust", &path, "--today", "2026-12-30", "--check"])
            .status
            .success(),
        "--check must not gate when nothing is stale"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `--json` selects a format and must not change the verdict.
///
/// The JSON path returns early, which is exactly where a gate gets forgotten —
/// and it was, on the first draft of this command.
#[test]
fn the_stale_gate_is_the_same_with_and_without_json() {
    let root = expiring_bundle("stale-json");
    let path = root.to_string_lossy().into_owned();
    let text = roteiro(&["okf", "trust", &path, "--today", "2027-01-01", "--check"]);
    let json = roteiro(&[
        "okf",
        "trust",
        &path,
        "--today",
        "2027-01-01",
        "--check",
        "--json",
    ]);
    assert_eq!(
        text.status.success(),
        json.status.success(),
        "--json must not change whether the command gates"
    );
    assert!(!json.status.success(), "and both must fail here");

    let v: serde_json::Value = serde_json::from_slice(&json.stdout).expect("parseable JSON");
    assert_eq!(v["stale"], 1);
    assert_eq!(v["today"], "2027-01-01");
    assert_eq!(v["concepts"][0]["stale"], true);
    assert_eq!(v["concepts"][0]["stale_after"], "2026-12-31T00:00:00Z");
    let _ = std::fs::remove_dir_all(&root);
}

/// A `--today` that is not an ISO date is refused rather than ignored.
#[test]
fn a_malformed_today_fails_the_command() {
    let root = expiring_bundle("stale-bad-date");
    let out = roteiro(&[
        "okf",
        "trust",
        &root.to_string_lossy(),
        "--today",
        "yesterday",
    ]);
    assert!(
        !out.status.success(),
        "a non-ISO date must fail the command"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not an ISO date"),
        "and must say so by name"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `okf computations` lists the contracts, and `--check` gates on an incomplete
/// one — while a bundle declaring none passes.
#[test]
fn computations_are_listed_and_only_incomplete_ones_gate() {
    let complete = bundle(
        "computations-ok",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "c/total.md",
                "---\ntype: Attested Computation\ntitle: Total\nruntime: bigquery\n---\n\n\
                 # Computation\n\n```sql\nSELECT 1\n```\n",
            ),
        ],
    );
    let out = roteiro(&["okf", "computations", &complete.to_string_lossy()]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success());
    assert!(
        stdout.contains("c/total — bigquery, inline sql, 1 line(s)"),
        "the listing must say the runtime and where the code is; got:\n{stdout}"
    );
    assert!(
        roteiro(&[
            "okf",
            "computations",
            &complete.to_string_lossy(),
            "--check"
        ])
        .status
        .success(),
        "a complete contract must not gate"
    );

    // No runtime: §10 makes it REQUIRED, so the contract cannot be run.
    let broken = bundle(
        "computations-no-runtime",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "c/total.md",
                "---\ntype: Attested Computation\ntitle: Total\n---\n\n\
                 # Computation\n\n```sql\nSELECT 1\n```\n",
            ),
        ],
    );
    assert!(
        !roteiro(&["okf", "computations", &broken.to_string_lossy(), "--check"])
            .status
            .success(),
        "a contract with no runtime must gate under --check"
    );
    assert!(
        roteiro(&["okf", "computations", &broken.to_string_lossy()])
            .status
            .success(),
        "and must not gate without it"
    );

    // §10 is optional, so declaring none is conformant and says so.
    let none = bundle(
        "computations-none",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/revenue.md",
                "---\ntype: Metric\ntitle: Revenue\n---\n\n# Revenue\n\nTotal.\n",
            ),
        ],
    );
    let out = roteiro(&["okf", "computations", &none.to_string_lossy(), "--check"]);
    assert!(out.status.success(), "declaring none is conformant");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("declares no attested computations"),
        "and must say so, rather than printing an empty listing that reads as a failure"
    );

    for root in [complete, broken, none] {
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `okf info` answers "what is this" without gating on any of it.
#[test]
fn info_summarises_without_gating() {
    let root = expiring_bundle("info");
    let out = roteiro(&[
        "okf",
        "info",
        &root.to_string_lossy(),
        "--today",
        "2027-01-01",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "info reports; the other commands gate — that is the division of labour"
    );
    for expected in [
        "okf_version: 0.2",
        "concepts: 1",
        "stale: 1 (as of 2027-01-01)",
        "internal links: 0, broken 0",
        "computations: 0, incomplete 0",
    ] {
        assert!(
            stdout.contains(expected),
            "info must report `{expected}`; got:\n{stdout}"
        );
    }

    let v: serde_json::Value = serde_json::from_slice(
        &roteiro(&[
            "okf",
            "info",
            &root.to_string_lossy(),
            "--today",
            "2027-01-01",
            "--json",
        ])
        .stdout,
    )
    .expect("parseable JSON");
    assert_eq!(v["trust"]["stale"], 1);
    assert_eq!(v["concepts"], 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// **The gates `okf info` names are the commands that actually gate.**
///
/// Not a string comparison against the line it prints — that would pin the claim
/// without checking it. This runs each command over one bundle that is *both*
/// link-broken and stale, and asserts the exit status.
///
/// It exists because the first draft of that line listed `lint` as a gate, which
/// it has never been, and omitted `trust --check`, which is the gate this PR
/// added — contradicting `docs/OKF_BUNDLE.md`'s table inside the same change. A
/// hand-maintained list of which commands gate drifts from the commands; this is
/// what stops the next edit doing it again.
#[test]
fn the_named_gates_are_the_commands_that_actually_gate() {
    let root = bundle(
        "gate-matrix",
        &[
            ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
            (
                "metrics/revenue.md",
                "---\ntype: Metric\ntitle: Revenue\nstale_after: 2026-12-31T00:00:00Z\n\
                 verified:\n  - by: human:alice\n    at: 2026-01-01T00:00:00Z\n---\n\n\
                 # Revenue\n\nSee [missing](absent.md).\n",
            ),
        ],
    );
    let p = root.to_string_lossy().into_owned();
    let ok = |args: &[&str]| roteiro(args).status.success();

    // Reports only — these must succeed on a bundle with faults in it, because
    // reporting is what you want when reading somebody else's.
    assert!(ok(&["okf", "lint", &p]), "`lint` never gates");
    assert!(ok(&["okf", "links", &p]), "bare `links` reports");
    assert!(
        ok(&["okf", "trust", &p, "--today", "2027-01-01"]),
        "bare `trust` reports"
    );
    assert!(
        ok(&["okf", "info", &p, "--today", "2027-01-01"]),
        "`info` never gates"
    );

    // Gates — the same bundle, with `--check`.
    assert!(
        !ok(&["okf", "links", &p, "--check"]),
        "`links --check` gates on a broken link"
    );
    assert!(
        !ok(&["okf", "trust", &p, "--today", "2027-01-01", "--check"]),
        "`trust --check` gates on staleness — the gate this omitted"
    );

    // And the printed line agrees with all of the above.
    let stdout =
        String::from_utf8_lossy(&roteiro(&["okf", "info", &p, "--today", "2027-01-01"]).stdout)
            .into_owned();
    let gates = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("gates:"))
        .expect("info names its gates");
    let reports = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("reports only:"))
        .expect("info names what only reports");
    assert!(
        gates.contains("trust"),
        "the stale gate must be named: {gates}"
    );
    assert!(
        !gates.contains("lint"),
        "`lint` must not be named as a gate: {gates}"
    );
    assert!(
        reports.contains("lint"),
        "`lint` belongs with the reporters: {reports}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
