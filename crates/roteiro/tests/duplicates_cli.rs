//! End-to-end test for `roteiro duplicates`: on a fixture repo it surfaces both
//! *exact* duplicates (two files that are the same git blob) and *semantic*
//! near-duplicates (two docs with near-identical bodies), and never pairs an
//! unrelated node. Only built with the `inference` feature.
#![cfg(feature = "inference")]

use std::path::Path;
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
        .output()
        .expect("run roteiro")
}

#[test]
fn duplicates_reports_exact_and_semantic() {
    let dir = std::env::temp_dir().join(format!("roteiro-dup-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("copy")).expect("mkdir");

    // Two byte-identical files at distinct paths → one git blob → exact dupes.
    let code = "pub fn work() {}\n";
    std::fs::write(dir.join("lib.rs"), code).expect("write");
    std::fs::write(dir.join("copy/lib.rs"), code).expect("write");
    // Two docs with near-identical bodies but *distinct* blobs (they differ by a
    // word, so git stores two blobs) → a semantic, not exact, duplicate.
    let base = "# Guide\n\ntoken validation and oauth login flow session refresh handling";
    std::fs::write(dir.join("x.md"), format!("{base}\n")).expect("write");
    std::fs::write(dir.join("y.md"), format!("{base} extra\n")).expect("write");
    // An unrelated doc must never be paired.
    std::fs::write(
        dir.join("z.md"),
        "# Zebra\n\nquokkas graze on rottnest island\n",
    )
    .expect("write");

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = roteiro(&dir, &["duplicates", "--json"]);
    assert!(out.status.success(), "duplicates failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("duplicates --json is valid JSON");
    let pairs = report["pairs"].as_array().expect("pairs array");

    // The exact file pair is present and flagged exact.
    assert!(
        pairs.iter().any(|p| {
            p["exact"] == true
                && [p["a"].as_str(), p["b"].as_str()]
                    .iter()
                    .flatten()
                    .any(|s| s.contains("copy/lib.rs"))
        }),
        "expected an exact file duplicate: {pairs:?}",
    );
    // The semantic doc pair is present, not exact, with high similarity.
    assert!(
        pairs.iter().any(|p| {
            p["exact"] == false
                && p["similarity"].as_f64().is_some_and(|s| s >= 0.9)
                && [p["a"].as_str(), p["b"].as_str()]
                    .iter()
                    .flatten()
                    .any(|s| s.ends_with("x.md"))
        }),
        "expected a semantic doc duplicate: {pairs:?}",
    );
    // The unrelated doc is never paired.
    assert!(
        pairs.iter().all(|p| {
            ![p["a"].as_str(), p["b"].as_str()]
                .iter()
                .flatten()
                .any(|s| s.ends_with("z.md"))
        }),
        "unrelated doc must not be paired: {pairs:?}",
    );

    std::fs::remove_dir_all(&dir).ok();
}
