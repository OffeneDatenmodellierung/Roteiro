//! The library contract, expressed as fixtures rather than as Rust.
//!
//! Each file in `tests/fixtures/` is one (findings, rendering) pair and the
//! verdict it must produce. The form matters: a rendering arrives as JSON from a
//! model, so a fixture is the real wire input rather than a hand-built value that
//! happens to resemble one, and adding a case is writing the input and the
//! expected output — no Rust, no harness change.
//!
//! Three of the fixtures assert a **clean** verdict on a rendering that is
//! nonetheless misleading (`cited-but-unrelated`, `rider-clause`,
//! `omission-is-not-a-defect`). They are not oversights. They are the crate's
//! limits pinned down where somebody adding a rule will meet them, so that
//! "faithful" keeps meaning *no claim was invented* and does not quietly drift
//! into meaning *the summary is true*.

use std::path::{Path, PathBuf};

use rto_faithful::{Rendering, check};
use rto_graph::FindingKey;
use serde::Deserialize;

/// One fixture file.
///
/// `deny_unknown_fields` because a mistyped key in a fixture would otherwise
/// deserialize to a default and pass: a fixture whose `expect` is silently empty
/// asserts nothing while looking like it asserts something.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    /// Why this case exists. Read by `every_fixture_says_why` — a case nobody
    /// can explain is a case nobody can correctly delete.
    why: String,
    /// The finding set the deterministic tools produced.
    findings: Vec<FindingKey>,
    /// What the renderer returned.
    rendering: Rendering,
    /// The expected defects, in order, in `Defect`'s serialized form.
    expect: Vec<serde_json::Value>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every `*.json` under `tests/fixtures`, sorted by name so a failure names the
/// same file on every machine.
fn fixtures() -> Vec<(String, Fixture)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
        .expect("read tests/fixtures")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
            let fixture = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{name} is not a well-formed fixture: {e}"));
            (name, fixture)
        })
        .collect()
}

#[test]
fn every_fixture_produces_its_recorded_verdict() {
    let cases = fixtures();
    assert!(
        cases.len() >= 11,
        "only {} fixtures loaded — the scan stopped matching, which would let this test pass by \
         checking nothing",
        cases.len()
    );
    for (name, fixture) in cases {
        let verdict = check(&fixture.findings, &fixture.rendering);
        let got = serde_json::to_value(&verdict.defects).expect("serialize defects");
        let want = serde_json::Value::Array(fixture.expect);
        assert_eq!(
            got, want,
            "{name} produced a different verdict than it records.\n  why: {}",
            fixture.why
        );
        assert_eq!(
            verdict.is_faithful(),
            verdict.defects.is_empty(),
            "{name}: `is_faithful` disagreed with the defect list"
        );
    }
}

/// A fixture that cannot say why it exists is a fixture nobody can correctly
/// change or delete — it will be edited into agreement with whatever the code
/// happens to do the first time it fails.
#[test]
fn every_fixture_says_why() {
    for (name, fixture) in fixtures() {
        assert!(
            fixture.why.split_whitespace().count() >= 8,
            "{name} does not explain itself. Say what the case is for, in a sentence."
        );
    }
}

/// Every defect this crate can report is exercised by at least one fixture.
///
/// The label set is read out of `Defect::label` in the source rather than
/// written down here, so a new variant cannot be added without a fixture: the
/// variant needs a `label` arm, the arm is picked up by the scan, and the scan
/// then demands a case. A hand-maintained list would just go stale, which is the
/// same failure as having no check.
#[test]
fn every_defect_is_exercised_by_a_fixture() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rto-faithful/src/lib.rs"),
    )
    .expect("read rto-faithful/src/lib.rs");

    let labels: Vec<String> = source
        .lines()
        .skip_while(|l| !l.contains("pub fn label(&self)"))
        .take_while(|l| !l.contains("pub fn segment(&self)"))
        .filter_map(|l| l.split_once("=> \""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .map(|(label, _)| label.to_owned())
        .collect();
    assert!(
        labels.len() >= 4,
        "found only {} defect labels in `Defect::label` — the scan broke, and a broken scan \
         demands nothing",
        labels.len()
    );

    let exercised: Vec<String> = fixtures()
        .into_iter()
        .flat_map(|(_, f)| f.expect)
        .filter_map(|d| d.get("defect").and_then(|v| v.as_str().map(str::to_owned)))
        .collect();
    for label in labels {
        assert!(
            exercised.contains(&label),
            "no fixture produces `{label}`. Add one to tests/fixtures/ — a defect nothing \
             exercises is a defect nothing protects."
        );
    }
}

/// At least one fixture must record a *clean* verdict on a rendering that
/// misleads.
///
/// This crate bounds fabrication and not distortion, and that boundary is only
/// honest while somebody keeps meeting it. If every fixture asserting a clean
/// verdict were deleted as redundant, the remaining suite would read as though a
/// green result meant an accurate report.
#[test]
fn the_limits_are_pinned_by_fixtures() {
    let clean_but_misleading = ["cited-but-unrelated.json", "rider-clause.json"];
    let names: Vec<String> = fixtures().into_iter().map(|(name, _)| name).collect();
    for wanted in clean_but_misleading {
        assert!(
            names.iter().any(|n| n == wanted),
            "{wanted} is gone. It records a rendering that passes this check and still misleads \
             — the limit is the point, not a gap to be tidied away."
        );
    }
    for (name, fixture) in fixtures() {
        if clean_but_misleading.contains(&name.as_str()) {
            assert!(
                fixture.expect.is_empty()
                    && check(&fixture.findings, &fixture.rendering).is_faithful(),
                "{name} no longer records a clean verdict, so it no longer demonstrates the limit"
            );
        }
    }
}
