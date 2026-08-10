//! End-to-end test for `roteiro export` / `roteiro load`: assemble a graph in
//! one repo, export it to a portable JSON artifact, then load that artifact into
//! a *second* repo's store and confirm the graph is present without any
//! extraction — the CI-artifact story (a clone obtains a ready-made graph).

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
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-artifact-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn init_repo(name: &str) -> PathBuf {
    let dir = fresh_dir(name);
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "src/main.rs",
        "mod util;\nfn main() {\n    util::helper();\n}\n",
    );
    write(&dir, "src/util.rs", "pub fn helper() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn export_then_load_reproduces_graph_without_extraction() {
    // Source repo: assemble a graph and export it.
    let src = init_repo("src");
    let artifact = src.join("graph.json");
    let out = roteiro(&src, &["export", "--out", artifact.to_str().unwrap()]);
    assert!(out.status.success(), "export failed: {out:?}");
    assert!(artifact.exists(), "artifact should be written");

    let json = std::fs::read_to_string(&artifact).expect("read artifact");
    assert!(json.contains("roteiro.graph/v1"), "schema tag present");
    assert!(
        json.contains("sym:rust:src/util.rs#helper"),
        "symbols exported"
    );

    // Export is deterministic: a second export is byte-identical.
    let again = src.join("graph2.json");
    roteiro(&src, &["export", "--out", again.to_str().unwrap()]);
    assert_eq!(
        std::fs::read_to_string(&artifact).unwrap(),
        std::fs::read_to_string(&again).unwrap(),
        "export must be deterministic",
    );

    // Destination repo: a DIFFERENT checkout whose own code shares no symbols
    // with the source. Loading the artifact must populate the store from the
    // artifact, not from the destination's code — proving no extraction.
    let dst = fresh_dir("dst");
    git(&dst, &["init", "-q"]);
    write(&dst, "unrelated.rs", "fn only_here() {}\n");
    git(&dst, &["add", "."]);
    git(&dst, &["commit", "-q", "-m", "different"]);

    // `--force`: this destination's tree differs from the artifact's, which a
    // plain `load` refuses (see `load_refuses_a_mismatched_tree`); the bootstrap
    // case deliberately overrides.
    let loaded = roteiro(&dst, &["load", "--force", artifact.to_str().unwrap()]);
    assert!(loaded.status.success(), "load failed: {loaded:?}");
    let msg = String::from_utf8_lossy(&loaded.stdout);
    assert!(msg.contains("nodes"), "load reports counts: {msg}");

    // Read the destination's store directly (bypassing the CLI, which would
    // re-sync): it must hold the SOURCE graph verbatim — the source symbols are
    // present and the destination's own `only_here` symbol is absent.
    let db = dst.join(".git/roteiro/graph.db");
    let store = rto_graph::Store::open(&db).expect("open loaded store");
    assert!(
        store
            .get_node("sym:rust:src/util.rs#helper")
            .expect("get")
            .is_some(),
        "loaded store should contain the source symbol",
    );
    assert!(
        store
            .get_node("sym:rust:unrelated.rs#only_here")
            .expect("get")
            .is_none(),
        "loaded store must NOT contain the destination's own symbol (no extraction)",
    );
    // The recorded tree id is the source artifact's, so a `sync` at the matching
    // commit would short-circuit — the CI-artifact fast path.
    assert!(store.sync_state().expect("state").is_some());

    std::fs::remove_dir_all(&src).ok();
    std::fs::remove_dir_all(&dst).ok();
}

#[test]
fn load_rejects_unknown_schema() {
    let dir = init_repo("badschema");
    let bad = dir.join("bad.json");
    std::fs::write(
        &bad,
        r#"{"schema":"roteiro.graph/v999","tree":null,"facts":{"nodes":[],"edges":[]}}"#,
    )
    .expect("write");
    let out = roteiro(&dir, &["load", bad.to_str().unwrap()]);
    assert!(!out.status.success(), "loading an unknown schema must fail");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_refuses_a_mismatched_tree() {
    // A CI artifact is content-addressed by tree; loading one whose tree does not
    // match the checkout would install a wrong graph, so it is refused (the hook
    // then rebuilds). `--force` overrides for the fresh-clone bootstrap.
    let dir = fresh_dir("verify");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub fn a() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "one"]);

    let artifact = dir.join("g.json");
    assert!(roteiro(&dir, &["sync"]).status.success());
    assert!(
        roteiro(&dir, &["export", "--out", artifact.to_str().unwrap()])
            .status
            .success()
    );

    // Same tree → load succeeds.
    assert!(
        roteiro(&dir, &["load", artifact.to_str().unwrap()])
            .status
            .success(),
        "loading a matching artifact should succeed"
    );

    // Change the tree, then load the now-stale artifact → refused.
    write(&dir, "src/lib.rs", "pub fn a() {}\npub fn b() {}\n");
    git(&dir, &["commit", "-q", "-am", "two"]);
    let stale = roteiro(&dir, &["load", artifact.to_str().unwrap()]);
    assert!(
        !stale.status.success(),
        "a mismatched-tree load must be refused"
    );
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("does not match HEAD"),
        "explains the mismatch: {}",
        String::from_utf8_lossy(&stale.stderr)
    );

    // `--force` overrides.
    assert!(
        roteiro(&dir, &["load", "--force", artifact.to_str().unwrap()])
            .status
            .success(),
        "--force loads a mismatched artifact"
    );

    std::fs::remove_dir_all(&dir).ok();
}
