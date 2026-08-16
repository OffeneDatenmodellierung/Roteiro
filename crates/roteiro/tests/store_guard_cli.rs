//! End-to-end test for the store-from-the-future write guard (issue #342).
//!
//! A `graph.db` written by a **newer** Roteiro opens silently in an older one.
//! Nothing then stops that older binary re-extracting the whole tree under its
//! own, older `EXTRACT_VERSION` and committing the result over the newer build's
//! graph: no error, no warning, and a graph that looks perfectly normal while
//! carrying strictly worse content.
//!
//! The two halves of the fix are asserted here against the real binary, because
//! the reason the bug existed is that nothing ever observed it:
//!
//! - a **write** (`sync`) refuses, naming both versions and the way out;
//! - a **read** (`search`) still answers, and leaves the store alone.
//!
//! Note what the reads are *not* asked to do. They must not be refused — an
//! older binary's reads are provably sound, because migrations are additive in
//! effect — so they answer from the graph the newer build left rather than
//! refreshing it.

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

fn graph_db(dir: &Path) -> std::path::PathBuf {
    dir.join(".git/roteiro/graph.db")
}

/// Stamp `dir`'s store as though a **newer** build had migrated it, and return
/// the pair of versions the guard should report: `(store, build)`.
///
/// The new version is `MAX(recorded) + 1` rather than a literal. A fresh store
/// carries exactly the migrations the binary under test knows, so the maximum
/// recorded version *is* this build's latest — which makes the stamp "one beyond
/// whatever this build knows" by construction, and keeps the test from having to
/// be edited (or, worse, from quietly stopping to test anything) every time a
/// real migration lands.
///
/// Only the record is written, not the schema such a migration would have added.
/// That is precisely the older binary's position: it can see the stamp and can
/// see nothing of the shape.
fn stamp_from_the_future(dir: &Path) -> (u32, u32) {
    let conn = rusqlite::Connection::open(graph_db(dir)).expect("open the store directly");
    let build: u32 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("a synced store records its migrations");
    let store = build + 1;
    conn.execute("INSERT INTO schema_migrations (version) VALUES (?1)", [
        store,
    ])
    .expect("stamp a version this build does not know");
    (store, build)
}

/// Every node key in `dir`'s store — the graph's content, read raw so the
/// assertions do not run through the very commands under test.
fn node_keys(dir: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(graph_db(dir)).expect("open the store directly");
    let mut stmt = conn
        .prepare("SELECT key FROM nodes ORDER BY key")
        .expect("prepare");
    let keys = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect");
    keys
}

#[test]
fn a_store_from_the_future_refuses_writes_and_still_serves_reads() {
    let dir = std::env::temp_dir().join(format!("roteiro-store-guard-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(
        dir.join("src/lib.rs"),
        "/// The symbol the store starts out knowing.\npub fn original() -> u32 { 1 }\n",
    )
    .expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // A graph for the first tree, written by a build that is level with the store.
    let out = roteiro(&dir, &["sync", "--committed"]);
    assert!(out.status.success(), "the first sync must succeed: {out:?}");

    // Now the tree moves. A sync *would* rewrite the graph to match — which is
    // exactly the rewrite that must not happen against a store from the future.
    std::fs::write(
        dir.join("src/later.rs"),
        "/// Only a sync of the second tree can put this in the graph.\n\
         pub fn added_after_the_stamp() -> u32 { 2 }\n",
    )
    .expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "second tree"]);

    let (store_version, build_version) = stamp_from_the_future(&dir);
    let before = node_keys(&dir);
    assert!(
        !before.iter().any(|k| k.contains("added_after_the_stamp")),
        "setup: the stamped store must predate the new symbol"
    );

    // --- the write path refuses -------------------------------------------
    let out = roteiro(&dir, &["sync", "--committed"]);
    assert!(
        !out.status.success(),
        "sync must refuse to rewrite a store from the future, not report success: {out:?}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // Both versions by number, and the way out. A generic failure would leave the
    // reader knowing only that something is wrong — and the plausible guesses
    // (delete the store, downgrade the file) are all worse than the truth, which
    // is that this binary is the old one.
    assert!(
        err.contains(&store_version.to_string()),
        "the refusal must name the store's version ({store_version}): {err}"
    );
    assert!(
        err.contains(&build_version.to_string()),
        "the refusal must name this build's version ({build_version}): {err}"
    );
    assert!(
        err.to_lowercase().contains("upgrade"),
        "the refusal must say to upgrade the binary: {err}"
    );
    assert_eq!(
        node_keys(&dir),
        before,
        "the refused sync must not have touched the graph"
    );

    // --- the gates refuse too ---------------------------------------------
    // `check` reaches the graph through a different chokepoint than `sync` does,
    // and it is a gate: serving it an unrefreshed graph would be a confident
    // wrong verdict, so it refuses rather than falling back to a read the way
    // `search` does below. Nineteen commands share that chokepoint; this is the
    // one that proves it is guarded.
    let out = roteiro(&dir, &["check", "--committed"]);
    assert!(
        !out.status.success(),
        "a gate must refuse to rebuild a store from the future: {out:?}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&store_version.to_string()) && err.to_lowercase().contains("upgrade"),
        "the gate's refusal must be as actionable as the sync's: {err}"
    );
    assert_eq!(
        node_keys(&dir),
        before,
        "the refused check must not have touched the graph"
    );

    // --- the read path still answers, and leaves the store alone ----------
    let out = roteiro(&dir, &["search", "original", "--json"]);
    assert!(
        out.status.success(),
        "reads against a store from the future must keep working: {out:?}"
    );
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert!(
        !hits.as_array().expect("array").is_empty(),
        "the read must return the newer build's graph, not an empty one: {hits:?}"
    );
    assert_eq!(
        node_keys(&dir),
        before,
        "a read must not rewrite the graph either — `search` rebuilds before it \
         reads, and that rebuild is the same silent downgrade"
    );

    // The control: with the stamp removed, this build is allowed to write again,
    // and the sync it was refusing is the one that would have replaced the
    // graph. Without this the refusal above could be any old failure.
    {
        let conn = rusqlite::Connection::open(graph_db(&dir)).expect("open the store directly");
        conn.execute("DELETE FROM schema_migrations WHERE version = ?1", [
            store_version,
        ])
        .expect("unstamp");
    }
    let out = roteiro(&dir, &["sync", "--committed"]);
    assert!(out.status.success(), "the unstamped sync must run: {out:?}");
    assert!(
        node_keys(&dir)
            .iter()
            .any(|k| k.contains("added_after_the_stamp")),
        "control: the refused sync really was a graph-changing one"
    );

    std::fs::remove_dir_all(&dir).ok();
}
