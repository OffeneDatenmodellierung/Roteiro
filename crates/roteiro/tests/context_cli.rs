//! End-to-end test for `roteiro context`: a node's context is cached, and when a
//! *dependency* changes, the dependent's cached context is invalidated (rebuilt)
//! — the codegraph-style dirty-propagation this stage delivers.

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

fn json(out: &std::process::Output) -> serde_json::Value {
    assert!(out.status.success(), "command failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("valid JSON")
}

#[test]
fn dependency_change_invalidates_dependent_context() {
    let dir = std::env::temp_dir().join(format!("roteiro-ctx-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // `caller` (in caller.rs) calls `callee` (in callee.rs): a cross-file
    // dependency, so editing callee.rs leaves caller.rs's blob unchanged.
    std::fs::write(dir.join("src/callee.rs"), "pub fn callee() -> u32 { 1 }\n").expect("write");
    std::fs::write(
        dir.join("src/caller.rs"),
        "use crate::callee::callee;\npub fn caller() -> u32 { callee() }\n",
    )
    .expect("write");
    std::fs::write(dir.join("src/lib.rs"), "pub mod callee;\npub mod caller;\n").expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let caller_key = "sym:rust:src/caller.rs#caller";

    // Fetch the caller's context (populates the cache) and record its fingerprint.
    let first = json(&roteiro(&dir, &["context", caller_key, "--json"]));
    let fp_before = first["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();
    // It depends on callee via an outgoing call edge.
    let calls_callee = first["outgoing"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| e["node"].as_str().is_some_and(|n| n.contains("#callee")));
    assert!(calls_callee, "caller should call callee: {first}");

    // A refresh now finds everything fresh (nothing changed).
    let clean = json(&roteiro(&dir, &["context", "--refresh", "--json"]));
    assert_eq!(clean["rebuilt"], 0, "nothing changed yet: {clean}");
    assert!(clean["reused"].as_u64().expect("reused") >= 1);

    // Change the *callee*'s body (new blob) and commit; caller.rs is untouched.
    std::fs::write(dir.join("src/callee.rs"), "pub fn callee() -> u32 { 42 }\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "change callee"]);

    // The caller's cached context is now stale (its dependency changed): a refresh
    // rebuilds it.
    let after = json(&roteiro(&dir, &["context", "--refresh", "--json"]));
    assert!(
        after["rebuilt"].as_u64().expect("rebuilt") >= 1,
        "the dependent's context must be rebuilt: {after}",
    );

    // And the caller's fingerprint has moved.
    let refetched = json(&roteiro(&dir, &["context", caller_key, "--json"]));
    assert_ne!(
        fp_before,
        refetched["fingerprint"].as_str().expect("fingerprint"),
        "dependent fingerprint must change when its dependency changes",
    );

    std::fs::remove_dir_all(&dir).ok();
}
