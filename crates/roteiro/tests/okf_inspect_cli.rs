//! End-to-end tests for `roteiro okf` — inspecting a bundle rather than
//! importing it (ADR-0021).
//!
//! These test the **CLI wiring**: that each action reaches the right library
//! entry point, that `--json` parses, and that `--check` gates while the bare
//! command does not. All five actions ship in a stock build — `validate` and
//! `lint` were behind a feature earlier on this branch and are not any more, so
//! there is no unavailable-surface case left to test.
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
/// Keyed by `tag` and process id so two tests running in parallel — and two
/// `cargo test` runs on one machine — never share a directory.
fn bundle(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("roteiro-okf-inspect-{tag}-{}", std::process::id()));
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
            // adding a `type` here is a conformance error, which is exactly the
            // kind of thing this bundle was originally written wrong and
            // `okf validate` caught.
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

/// `validate` and `lint` are wired to *different* checks.
#[test]
fn validate_and_lint_are_wired_to_different_checks() {
    let root = two_tier_bundle("validate");
    let path = root.to_string_lossy().into_owned();

    let validate =
        String::from_utf8(roteiro(&["okf", "validate", &path, "--json"]).stdout).expect("utf-8");
    let lint = String::from_utf8(roteiro(&["okf", "lint", &path, "--json"]).stdout).expect("utf-8");

    let v: serde_json::Value = serde_json::from_str(&validate).expect("validate --json");
    let l: serde_json::Value = serde_json::from_str(&lint).expect("lint --json");
    assert_eq!(v["check"], "validate");
    assert_eq!(l["check"], "lint");
    assert_eq!(
        v["errors"], 0,
        "the inline bundle is conformant, so validate must find no errors: {validate}"
    );
    let _ = std::fs::remove_dir_all(&root);
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
