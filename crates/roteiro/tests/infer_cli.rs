//! End-to-end test for `roteiro infer`: on a fixture repo it produces
//! `inferred` similarity edges, and those edges surface in `query --json`
//! labelled with provenance and confidence — the Stage 8 definition of done.
//! Only built with the `inference` feature.
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
fn infer_produces_confidence_labelled_inferred_edges() {
    let dir = std::env::temp_dir().join(format!("roteiro-infer-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // Several sibling handlers with related names → strong similarity signal.
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn handle_open() {}\n\
         pub fn handle_close() {}\n\
         pub fn handle_read() {}\n\
         pub fn unrelated_zebra() {}\n",
    )
    .expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Run inference.
    let out = roteiro(&dir, &["infer", "--json"]);
    assert!(out.status.success(), "infer failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("infer --json is valid JSON");
    let count = report["inferred_edges"].as_u64().expect("edge count");
    assert!(count > 0, "should infer at least one edge, got {count}");

    // The inferred edges surface in `query`, labelled inferred + confidence.
    let q = roteiro(
        &dir,
        &["query", "sym:rust:src/lib.rs#handle_open", "--json"],
    );
    assert!(q.status.success(), "query failed: {q:?}");
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("query json");
    let edges = node["outgoing"]
        .as_array()
        .into_iter()
        .chain(node["incoming"].as_array())
        .flatten();
    let inferred: Vec<_> = edges
        .filter(|e| e["provenance"] == "inferred")
        .cloned()
        .collect();
    assert!(
        !inferred.is_empty(),
        "handle_open should have inferred edges: {node}",
    );
    for e in &inferred {
        assert_eq!(e["kind"], "related");
        let c = e["confidence"].as_f64().expect("confidence present");
        assert!((0.0..=1.0).contains(&c), "confidence in range: {c}");
    }
    // The related handlers are linked; the unrelated one is not.
    let targets: Vec<&str> = inferred.iter().filter_map(|e| e["node"].as_str()).collect();
    assert!(
        targets.iter().any(|t| t.contains("handle_")),
        "a sibling handler should be linked: {targets:?}",
    );
    assert!(
        !targets.iter().any(|t| t.contains("unrelated_zebra")),
        "the unrelated fn should not be linked: {targets:?}",
    );

    std::fs::remove_dir_all(&dir).ok();
}
