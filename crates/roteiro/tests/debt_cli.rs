// roteiro:ignore-file — the fixtures below deliberately embed `TODO`/`FIXME` to
// exercise the detector; they are test data, not real debt in this repo.
//! End-to-end test for `roteiro debt` with the `[debt] ignore` config (ADR-0007):
//! markers under an ignored path (e.g. a vendored tree) are excluded from the
//! report — both the totals and the item list — while others are kept.

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

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-debt-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn debt_ignore_excludes_vendored_markers() {
    let dir = fresh_dir();
    git(&dir, &["init", "-q"]);
    // One marker in our own source, one in a vendored tree.
    write(
        &dir,
        "src/lib.rs",
        "// TODO: wire this up\npub struct Thing;\n",
    );
    write(
        &dir,
        "vendor/dep/lib.rs",
        "// FIXME: upstream bug\npub struct Dep;\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Without any ignore config, both markers are reported.
    let all = roteiro(&dir, &["debt", "--json"]);
    assert!(all.status.success(), "debt failed: {all:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&all.stdout).expect("debt --json is valid JSON");
    assert_eq!(report["total"], 2, "both markers reported: {report}");

    // Ignore the vendored tree: only our own marker remains, and the totals and
    // per-category counts reflect the exclusion.
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"vendor/**\"]\n");
    let filtered = roteiro(&dir, &["debt", "--json"]);
    assert!(filtered.status.success(), "debt failed: {filtered:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&filtered.stdout).expect("debt --json is valid JSON");
    assert_eq!(report["total"], 1, "vendored marker excluded: {report}");
    assert_eq!(report["by_category"]["fixme"], serde_json::Value::Null);
    let items = report["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["path"], "src/lib.rs");

    std::fs::remove_dir_all(&dir).ok();
}
