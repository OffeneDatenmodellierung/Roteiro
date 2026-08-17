// roteiro:ignore-file — the fixtures below deliberately embed `TODO`/`FIXME` to
// exercise the detector; they are test data, not real debt in this repo.
//! End-to-end test for `roteiro debt-density`.
//!
//! The unit tests in `rto_graph::query` build their `file` nodes by hand. This
//! one goes through **real extraction**, which is the only thing that can prove
//! the claim the lens rests on: that the denominator — a `file` node's
//! `meta.lines` — is already in the graph, so no new extraction metadata and no
//! `EXTRACT_VERSION` bump is needed to divide by it.
//!
//! It also covers the two behaviours that only appear against a real repository:
//! the shared `[debt] ignore` config (ADR-0007) governs density exactly as it
//! governs `roteiro debt`, and a raw marker count and a density genuinely rank
//! the same two files in opposite orders.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // Isolate from any real user config.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = roteiro(dir, args);
    assert!(out.status.success(), "roteiro {args:?} failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("--json is valid JSON")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-density-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// A file of `lines` lines with `markers` `TODO`s at the top. Every line is
/// newline-terminated, so the extractor's newline count is exactly `lines`.
fn source(markers: usize, lines: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for i in 0..markers {
        let _ = writeln!(s, "// TODO item {i}");
    }
    for i in markers..lines {
        let _ = writeln!(s, "pub const N{i}: u32 = {i};");
    }
    s
}

#[test]
fn density_divides_by_the_line_count_real_extraction_already_records() {
    let dir = fresh_dir();
    git(&dir, &["init", "-q"]);
    // The same marker count in two files an order of magnitude apart in length:
    // indistinguishable to `roteiro debt`, ten-fold apart under density.
    write(&dir, "src/big.rs", &source(4, 1000));
    write(&dir, "src/small.rs", &source(4, 100));
    // A vendored file, denser than either, to be excluded by config below.
    write(&dir, "vendor/dep.rs", &source(8, 100));
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let report = json(&dir, &["debt-density", "--json", "--min-lines", "0"]);

    // The denominator came out of the graph, not out of this test: extraction
    // recorded it, and the figures below are only meaningful because it did.
    let of = |path: &str| -> serde_json::Value {
        report["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|i| i["path"] == path)
            .unwrap_or_else(|| panic!("`{path}` missing from {report}"))
            .clone()
    };
    assert_eq!(of("src/big.rs")["lines"], 1000, "{report}");
    assert_eq!(of("src/small.rs")["lines"], 100);
    assert_eq!(
        report["unknown_length_files"], 0,
        "every file with a marker had a recorded length: {report}"
    );

    // Same count, ten-fold different density — the whole lens in one assertion.
    assert_eq!(of("src/big.rs")["markers"], of("src/small.rs")["markers"]);
    assert_eq!(of("src/big.rs")["per_kloc"], 4.0);
    assert_eq!(of("src/small.rs")["per_kloc"], 40.0);

    // And the ranking follows the density, not the count. `vendor/dep.rs` is 8 in
    // 100 = 80 per kloc, so it leads while it is still in scope.
    let ranked: Vec<&str> = report["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert_eq!(
        ranked,
        ["vendor/dep.rs", "src/small.rs", "src/big.rs"],
        "{report}"
    );

    // The `markers` order is the control: on the raw count `vendor/dep.rs` still
    // leads on 8, but the two four-marker files tie and break on path — so
    // `src/big.rs` comes *before* `src/small.rs`, the reverse of the density
    // ranking above.
    let by_count = json(
        &dir,
        &[
            "debt-density",
            "--json",
            "--min-lines",
            "0",
            "--order",
            "markers",
        ],
    );
    let ranked: Vec<&str> = by_count["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert_eq!(
        ranked,
        ["vendor/dep.rs", "src/big.rs", "src/small.rs"],
        "{by_count}"
    );

    // `[debt] ignore` is shared with `roteiro debt`, not a second vocabulary: the
    // vendored file leaves the population entirely, not merely the ranking.
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"vendor/**\"]\n");
    let filtered = json(&dir, &["debt-density", "--json", "--min-lines", "0"]);
    assert!(
        !filtered["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|i| i["path"] == "vendor/dep.rs"),
        "{filtered}"
    );
    assert_eq!(filtered["files_with_markers"], 2, "{filtered}");
    assert_eq!(filtered["total_markers"], 8, "{filtered}");
    // The baseline moves with the exclusion, so it cannot be read against a
    // population that is no longer being reported.
    assert_eq!(
        filtered["overall_per_kloc"], 7.27,
        "8 markers over 1100 lines: {filtered}"
    );

    // An unknown `--order` is refused rather than silently ranked by density.
    let bad = roteiro(&dir, &["debt-density", "--order", "count"]);
    assert!(!bad.status.success(), "{bad:?}");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("unknown --order `count`"), "was: {err}");

    // The human-readable output names what the denominator actually is, so a
    // figure read off the terminal is not mistaken for source lines of code.
    let text = roteiro(&dir, &["debt-density", "--min-lines", "0"]);
    assert!(text.status.success(), "{text:?}");
    let out = String::from_utf8_lossy(&text.stdout);
    assert!(
        out.contains("not source lines of code"),
        "the caveat travels with the figures: {out}"
    );
    assert!(out.contains("baseline:"), "{out}");

    std::fs::remove_dir_all(&dir).ok();
}
