//! End-to-end test for `roteiro import --from codegraph`: compares the derived
//! Rust graph against a codegraph `SQLite` snapshot (validation oracle only — no
//! structural edges are imported).

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

/// Write a minimal codegraph-schema snapshot describing `src/lib.rs`.
fn write_snapshot(path: &Path) {
    let conn = rusqlite::Connection::open(path).expect("open snapshot");
    conn.execute_batch(
        "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT);
         CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, type TEXT,
             name TEXT, qualified_name TEXT);
         CREATE TABLE edges (id INTEGER PRIMARY KEY, source_id INTEGER,
             target_id INTEGER, relation TEXT);
         CREATE TABLE meta (key TEXT, value TEXT);
         INSERT INTO meta VALUES ('snapshot_source_commit', 'deadbeef');
         INSERT INTO files VALUES (1, 'src/lib.rs');
         -- Shared with the repo: Widget struct + Widget::go method.
         INSERT INTO nodes VALUES (1, 1, 'struct',   'Widget', 'Widget');
         INSERT INTO nodes VALUES (2, 1, 'function', 'go',     'Widget.go');
         -- codegraph-only: a symbol the repo does not have.
         INSERT INTO nodes VALUES (3, 1, 'function', 'phantom','phantom');",
    )
    .expect("seed snapshot");
}

#[test]
fn codegraph_oracle_reports_agreement_and_divergence() {
    let dir = std::env::temp_dir().join(format!("roteiro-cg-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub struct Widget;\nimpl Widget {\n    pub fn go() {}\n}\n",
    )
    .expect("write src");

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let db = dir.join("cg.db");
    write_snapshot(&db);

    let out = Command::new(BIN)
        .args(["import", "--from", "codegraph"])
        .arg(&db)
        .arg("--json")
        .current_dir(&dir)
        .output()
        .expect("run roteiro");
    assert!(out.status.success(), "oracle failed: {out:?}");

    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("oracle --json is valid JSON");
    assert_eq!(report["schema"], "roteiro.oracle/v1");
    assert_eq!(report["source_commit"], "deadbeef");
    // Widget and Widget::go are found by both; phantom is codegraph-only.
    assert_eq!(report["symbols_matched"], 2, "Widget + Widget::go");
    assert_eq!(report["codegraph_only"], 1, "phantom");
    assert!(
        report["codegraph_only_sample"]
            .as_array()
            .expect("sample")
            .iter()
            .any(|k| k == "sym:rust:src/lib.rs#phantom"),
        "phantom should be listed: {report}",
    );

    std::fs::remove_dir_all(&dir).ok();
}
