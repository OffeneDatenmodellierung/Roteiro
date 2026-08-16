//! Linked worktrees must not answer for each other (issue #330).
//!
//! Three properties, each of which was a way to get a confident wrong answer:
//!
//! 1. **The graph store is per-worktree, the extraction cache is shared.** The
//!    two are stored differently on purpose. [`rto_graph::ObjectCache`] is
//!    content-addressed by blob id, so sharing it under the *common* git dir is
//!    correct and valuable — extraction done in one worktree is reusable in all.
//!    `graph.db` is an assembled view of ONE tree, so sharing *it* would mean
//!    last-writer-wins: `sync`/`check` would report on whichever tree synced most
//!    recently. This test pins the distinction so it cannot be collapsed silently.
//!
//!    Note what is deliberately NOT the fix here: giving each worktree its own
//!    *database*. `findings`, `media_content`, `agent_memory` and `imports` all
//!    live inside `graph.db`, and ADR-0013 v1.1 depends on that store being
//!    shared. Splitting it per worktree would silently reintroduce the
//!    branch-scoping that ADR rejected, and would need the ADR amended rather
//!    than extended.
//!
//! 2. **A store holding another tree is rebuilt, loudly, not reused.** Belt and
//!    braces for (1): if a `graph.db` ever comes to describe a different tree —
//!    restored from a backup, copied with a `.git` directory, or reached after a
//!    layout change — the sync engine notices and rebuilds instead of reporting
//!    "up to date" about a graph nobody is looking at.
//!
//! 3. **A new ADR on disk is not silently missing from `check`.** This is the
//!    *observed* symptom of #330 — "`check` said 17 ADRs while 18 files sat on
//!    disk" — and its cause is not worktrees at all: `sync_worktree` overlays
//!    untracked files into the derived layer, but the authored layer read only
//!    the `HEAD` tree, so the two disagreed about which tree they described.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
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
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
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

fn adr(id: &str, title: &str) -> String {
    format!("---\nadr-id: \"{id}\"\nstatus: Accepted\n---\n\n# {title}\n\n## Decision\n\nBody.\n")
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-wt-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// How many ADRs `check` says it checked.
fn checked_adrs(out: &std::process::Output) -> usize {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("checked "))
        .unwrap_or_else(|| panic!("no `checked …` line in: {stdout}"));
    line.trim_start_matches("checked ")
        .split(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("cannot parse count from: {line}"))
}

#[test]
fn linked_worktrees_keep_separate_graphs_but_share_the_extraction_cache() {
    let base = fresh_dir("isolation");
    let main = base.join("main");
    let side = base.join("side");
    std::fs::create_dir_all(&main).expect("mkdir main");
    git(&main, &["init", "-q"]);
    write(&main, "src/lib.rs", "pub struct Thing;\n");
    write(&main, "docs/adr/0001-one.md", &adr("0001", "One"));
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);

    // A linked worktree on its own branch, with an ADR the main tree lacks.
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            side.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    write(&side, "docs/adr/0002-two.md", &adr("0002", "Two"));
    git(&side, &["add", "."]);
    git(&side, &["commit", "-q", "-m", "second adr"]);

    // Sync both, in an order that would let a shared store hide the difference:
    // the side tree syncs LAST, so a last-writer-wins store would make the main
    // tree report the side tree's two ADRs.
    assert!(roteiro(&main, &["sync"]).status.success(), "main sync");
    assert!(roteiro(&side, &["sync"]).status.success(), "side sync");

    // Each tree answers for itself.
    let main_check = roteiro(&main, &["check", "--committed"]);
    let side_check = roteiro(&side, &["check", "--committed"]);
    assert!(main_check.status.success(), "main check");
    assert!(side_check.status.success(), "side check");
    assert_eq!(
        checked_adrs(&main_check),
        1,
        "the main tree has one ADR and must report one, whoever synced last: {}",
        String::from_utf8_lossy(&main_check.stdout)
    );
    assert_eq!(
        checked_adrs(&side_check),
        2,
        "the side tree has two ADRs: {}",
        String::from_utf8_lossy(&side_check.stdout)
    );

    // The stores are distinct files, under each worktree's OWN git dir…
    let main_db = main.join(".git/roteiro/graph.db");
    let side_db = main.join(".git/worktrees/side/roteiro/graph.db");
    assert!(main_db.is_file(), "main graph.db at {}", main_db.display());
    assert!(side_db.is_file(), "side graph.db at {}", side_db.display());

    // …while the extraction cache is SHARED under the common git dir, because it
    // is content-addressed and reuse across worktrees is the point.
    assert!(
        main.join(".git/roteiro/objects").is_dir(),
        "shared object cache under the common git dir"
    );
    assert!(
        !main.join(".git/worktrees/side/roteiro/objects").exists(),
        "the object cache must NOT be duplicated per worktree — it is \
         content-addressed, so extraction done in one tree is reusable in all"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn a_store_holding_another_worktree_is_rebuilt_and_says_so() {
    // Belt and braces: simulate the failure the per-worktree layout prevents, by
    // copying one tree's store over another's. Before the stamp, the copied state
    // id would be trusted and `sync` would answer "up to date" for a graph
    // assembled somewhere else entirely.
    let base = fresh_dir("foreign");
    let main = base.join("main");
    let side = base.join("side");
    std::fs::create_dir_all(&main).expect("mkdir main");
    git(&main, &["init", "-q"]);
    write(&main, "src/lib.rs", "pub struct Thing;\n");
    write(&main, "docs/adr/0001-one.md", &adr("0001", "One"));
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            side.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    write(&side, "docs/adr/0002-two.md", &adr("0002", "Two"));
    write(&side, "docs/adr/0003-three.md", &adr("0003", "Three"));
    git(&side, &["add", "."]);
    git(&side, &["commit", "-q", "-m", "more adrs"]);

    assert!(roteiro(&main, &["sync"]).status.success(), "main sync");
    assert!(roteiro(&side, &["sync"]).status.success(), "side sync");

    // Plant the side tree's graph in the main tree's slot.
    std::fs::copy(
        main.join(".git/worktrees/side/roteiro/graph.db"),
        main.join(".git/roteiro/graph.db"),
    )
    .expect("plant foreign store");

    // The main tree must NOT report "up to date" over a graph built elsewhere.
    let out = roteiro(&main, &["sync"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "sync should recover, not fail: {stderr}"
    );
    assert!(
        !stdout.contains("up to date"),
        "a store from another working tree must never short-circuit as \
         `up to date`: {stdout}"
    );
    // …and the correction is visible, naming the tree the store actually held.
    assert!(
        stderr.contains("different working tree"),
        "the rebuild must be announced: {stderr}"
    );
    assert!(
        stderr.contains(side.to_string_lossy().as_ref()),
        "the message must name the tree the store was holding ({}): {stderr}",
        side.display()
    );

    // Having rebuilt, the main tree reports its own single ADR again.
    let check = roteiro(&main, &["check", "--committed"]);
    assert_eq!(
        checked_adrs(&check),
        1,
        "the rebuilt graph describes the main tree: {}",
        String::from_utf8_lossy(&check.stdout)
    );

    // And the next sync is an ordinary no-op with no further noise.
    let again = roteiro(&main, &["sync"]);
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("up to date"),
        "once adopted, the store is ours: {}",
        String::from_utf8_lossy(&again.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&again.stderr).contains("different working tree"),
        "the notice must not repeat once the store has been adopted"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn a_new_untracked_adr_is_checked_not_silently_skipped() {
    // The reported symptom: "`check` said 17 ADRs while 18 files sat on disk".
    // `sync_worktree` overlays untracked files into the derived layer on purpose,
    // but the authored layer read only the `HEAD` tree — so a brand-new ADR had
    // its symbols extracted while the file was never parsed as an ADR, and
    // nothing said so.
    let dir = fresh_dir("untracked-adr");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "docs/adr/0001-one.md", &adr("0001", "One"));
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let committed = roteiro(&dir, &["check"]);
    assert_eq!(checked_adrs(&committed), 1, "baseline: one ADR");

    // A brand-new ADR, on disk, never added to git — the shape a decision takes
    // while it is being drafted.
    write(&dir, "docs/adr/0002-two.md", &adr("0002", "Two"));
    let out = roteiro(&dir, &["check"]);
    assert!(out.status.success(), "check should still pass");
    assert_eq!(
        checked_adrs(&out),
        2,
        "the working-tree check must see the ADR that is on disk — reporting 1 \
         while 2 exist is the silent wrong answer of #330: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // (Not asserted via `roteiro query`: that command rebuilds the graph from the
    // committed tree by design, so it would never show an uncommitted draft. The
    // duplicate-id case below proves the file is genuinely parsed as an ADR
    // rather than merely counted.)

    // `--committed` is unchanged: it validates `HEAD`, where the ADR is absent.
    assert_eq!(
        checked_adrs(&roteiro(&dir, &["check", "--committed"])),
        1,
        "--committed still describes HEAD only"
    );

    // And it is checked as an ADR, so a *duplicate* id in an untracked draft is
    // caught too — the exact case that put two ADR-0016s in this repository.
    write(&dir, "docs/adr/0002b-dupe.md", &adr("0002", "Two again"));
    let dupe = roteiro(&dir, &["check"]);
    assert!(
        !dupe.status.success(),
        "a duplicate id in an untracked draft must fail: {}",
        String::from_utf8_lossy(&dupe.stdout)
    );
    assert!(
        String::from_utf8_lossy(&dupe.stderr).contains("duplicate-adr-id"),
        "reported as a duplicate id: {}",
        String::from_utf8_lossy(&dupe.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}
