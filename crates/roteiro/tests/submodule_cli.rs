//! End-to-end test for git submodule pin extraction (ADR-0009 derived facts): a
//! repo with a submodule gitlink + `.gitmodules` syncs to a `submodule` graph
//! node recording the path, URL, and pinned commit sha.

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
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

#[test]
fn submodule_gitlink_becomes_a_pinned_submodule_node() {
    let base = std::env::temp_dir().join(format!("roteiro-submodule-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(&base).expect("mkdir");

    // A `.gitmodules` declaring the submodule's path + url, plus a plain file.
    std::fs::write(
        base.join(".gitmodules"),
        "[submodule \"vendor/app\"]\n\tpath = vendor/app\n\turl = https://github.com/acme/app.git\n",
    )
    .expect("write .gitmodules");
    std::fs::write(base.join("README.md"), "# deploy\n").expect("write");
    git(&base, &["init", "-q"]);
    git(&base, &["add", ".gitmodules", "README.md"]);

    // Fake the submodule gitlink in the index (mode 160000) pinned to a commit sha,
    // without needing a real second repo checked out.
    let sha = "1234567890123456789012345678901234567890";
    git(
        &base,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sha},vendor/app"),
        ],
    );
    git(&base, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["sync"])
        .current_dir(&base)
        .output()
        .expect("run sync");
    assert!(out.status.success(), "sync failed: {out:?}");

    let q = Command::new(BIN)
        .args(["query", "--kind", "submodule", "--json"])
        .current_dir(&base)
        .output()
        .expect("run query");
    assert!(q.status.success(), "query failed: {q:?}");
    let v: serde_json::Value = serde_json::from_slice(&q.stdout).expect("valid JSON");
    let nodes = v["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes.len(), 1, "one submodule node: {v}");
    let n = &nodes[0];
    assert_eq!(n["key"], "submodule:vendor/app");

    // The pin details (path, url, sha) are on the node — check via `query <key>`.
    let e = Command::new(BIN)
        .args(["query", "submodule:vendor/app", "--json"])
        .current_dir(&base)
        .output()
        .expect("run explain");
    assert!(e.status.success(), "query <key> failed: {e:?}");
    let ex: serde_json::Value = serde_json::from_slice(&e.stdout).expect("valid JSON");
    assert_eq!(ex["meta"]["sha"], sha, "pinned commit recorded: {ex}");
    assert_eq!(ex["meta"]["url"], "https://github.com/acme/app.git");
    assert_eq!(ex["meta"]["path"], "vendor/app");

    // Index-aware sync (the pre-commit gate) reads staged gitlinks — exercise that
    // path end-to-end: it must handle the gitlink in the index without error.
    let staged = Command::new(BIN)
        .args(["check", "--staged"])
        .current_dir(&base)
        .output()
        .expect("run check --staged");
    assert!(staged.status.success(), "check --staged failed: {staged:?}");

    std::fs::remove_dir_all(&base).ok();
}
