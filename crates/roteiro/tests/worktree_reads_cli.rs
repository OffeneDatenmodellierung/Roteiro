// roteiro:ignore-file — the fixtures below deliberately embed a `TODO` to
// exercise the marker detector; they are test data, not real debt in this repo.
//! Issue #599: a read command must report on the tree the developer is looking
//! at, and must not rewrite the store out from under the sync that built it.
//!
//! `roteiro sync` assembles the **working tree** and says so (`+N uncommitted`).
//! Every read command used to rebuild to `HEAD` before answering — and because
//! that rebuild is a write (`rto_graph::sync_worktree` stamps `sync_state` as
//! `{tree}:dirty:{hash}` precisely so a committed sync supersedes the overlay),
//! it did not merely read a different tree: it **deleted** the nodes `sync` had
//! just written, then reported on what was left. `debt` printed
//! `intent debt: none` for a marker that existed; `search TODO` printed
//! `no matches` after discarding the node it was about to match.
//!
//! These tests are written across the whole read family rather than over `debt`
//! alone, because the same shape has been fixed one surface at a time here
//! before (#409, five attempts). A fix that lands on the reported command and
//! leaves its four neighbours is the failure mode, so the neighbours are asserted.

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
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-wtread-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// A repository with one committed file and one **uncommitted** edit that adds a
/// marker and a second symbol — the state a developer is in just before they
/// commit, which is exactly when `debt` was answering about the wrong tree.
fn repo_with_uncommitted_marker(tag: &str) -> PathBuf {
    let dir = fresh_dir(tag);
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    write(
        &dir,
        "src/lib.rs",
        "pub struct Thing;\n// TODO: uncommitted work in progress\npub struct Pending;\n",
    );
    dir
}

#[test]
fn debt_reports_the_marker_the_developer_is_looking_at() {
    let dir = repo_with_uncommitted_marker("debt");

    let out = roteiro(&dir, &["debt", "--json"]);
    assert!(out.status.success(), "debt failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("debt --json is valid JSON");
    assert_eq!(
        report["total"], 1,
        "an uncommitted marker is debt the moment it is written, not the moment \
         it is committed: {report}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// One `roteiro sync --json` report, as the fields this test reasons about.
fn sync_json(dir: &Path) -> serde_json::Value {
    let out = roteiro(dir, &["sync", "--json"]);
    assert!(out.status.success(), "sync failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("sync --json is valid JSON")
}

/// The half that makes this a data-loss bug rather than a reporting one: the
/// read must leave the store as `sync` built it.
///
/// Asserted on `sync --json`'s `no_op` boolean rather than on the human line
/// containing "up to date": the JSON field is the stable contract, and a test
/// that pins prose fails the next time someone improves the wording — a false
/// red that teaches people to edit the assertion rather than read it.
#[test]
fn a_read_does_not_discard_the_graph_sync_assembled() {
    let dir = repo_with_uncommitted_marker("survive");

    let first = sync_json(&dir);
    assert_eq!(
        first["blobs_dirty"], 1,
        "the fixture must actually be dirty: {first}"
    );

    for read in [
        vec!["debt"],
        vec!["debt-density"],
        vec!["coupling"],
        vec!["config-secrets"],
        vec!["search", "Pending"],
    ] {
        let out = roteiro(&dir, &read);
        assert!(out.status.success(), "{read:?} failed: {out:?}");

        let again = sync_json(&dir);
        assert_eq!(
            again["no_op"],
            serde_json::Value::Bool(true),
            "`roteiro {}` rewrote the store to a different tree, so the next sync \
             had to rebuild it: {again}",
            read.join(" "),
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// `search` is the surface where the old behaviour was self-defeating: it
/// deleted the node it was about to match, then reported no match.
#[test]
fn search_finds_a_symbol_that_is_not_committed_yet() {
    let dir = repo_with_uncommitted_marker("search");

    let out = roteiro(&dir, &["search", "Pending"]);
    assert!(out.status.success(), "search failed: {out:?}");

    // Asserted positively, on the hit list. The negative form — "stdout does not
    // say `no matches`" — passes for the wrong reason: a miss is reported on
    // **stderr**, so stdout is empty either way and the assertion never fires.
    // Verified by injection: with the reads pointed back at `HEAD` this test
    // stayed green while `search` printed `no matches for \`Pending\``.
    assert!(
        stdout(&out).contains("Pending"),
        "a symbol in the working tree is findable before it is committed \
         (stdout: {:?}, stderr: {:?})",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Reading the committed tree stays available — and, because it is a switch away
/// from what every neighbouring command reads, it names the tree it answered
/// about. A silent disagreement between two commands over one store is the
/// defect #599 filed; the flag is not.
#[test]
fn committed_stays_reachable_and_says_which_tree_it_answered_about() {
    let dir = repo_with_uncommitted_marker("committed");

    let out = roteiro(&dir, &["debt", "--committed", "--json"]);
    assert!(out.status.success(), "debt --committed failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("debt --json is valid JSON");
    assert_eq!(
        report["total"], 0,
        "--committed still means HEAD, uncommitted work excluded: {report}"
    );

    // The note must name the **flag the reader typed**, not just the internal
    // source token: someone who wrote `--staged` and reads "the index tree" is
    // left matching a word they never used, and anything grepping this output
    // has the flag name to hand and not `GraphSource`'s.
    let note = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        note.contains("--committed"),
        "the tree a report describes must be stated, by the flag that selected \
         it, when it is not the default: {note}"
    );
    assert!(
        note.contains("not the working tree"),
        "and must say what it is *not*, since that is the default it departs \
         from: {note}"
    );

    // The note goes to stderr, so a `--json` consumer's stdout stays exactly one
    // document — the reason it is not simply appended to the report.
    assert!(
        stdout(&out).starts_with('{'),
        "stdout must remain pure JSON: {}",
        stdout(&out)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Every report surface announces its tree, not just the one #599 was filed
/// against.
///
/// The announcement is one method called from nine places. That is better than
/// nine copies of it, but it moves the failure mode rather than removing it: a
/// surface can still lose its `source.announce()` line while the other eight
/// keep theirs, and a test that only ever looked at `debt` would not notice.
#[test]
fn every_report_surface_names_the_tree_it_answered_about() {
    let dir = repo_with_uncommitted_marker("announce");

    for cmd in [
        vec!["debt", "--committed"],
        vec!["debt-density", "--committed"],
        vec!["coupling", "--committed"],
        vec!["config-secrets", "--committed"],
        vec!["search", "--committed", "Pending"],
        vec!["path", "--committed", "file:src/lib.rs", "file:src/lib.rs"],
    ] {
        let out = roteiro(&dir, &cmd);
        let note = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            note.contains("--committed"),
            "`roteiro {}` answered about a different tree without saying so \
             (stderr: {note:?})",
            cmd.join(" "),
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// `--staged` is the index, which is neither of the other two: an edit on disk
/// but not staged is invisible to it, and staging the identical bytes makes it
/// appear without anything being committed.
#[test]
fn staged_reads_the_index_rather_than_the_disk() {
    let dir = repo_with_uncommitted_marker("staged");

    let unstaged = roteiro(&dir, &["debt", "--staged", "--json"]);
    assert!(
        unstaged.status.success(),
        "debt --staged failed: {unstaged:?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&unstaged.stdout).expect("debt --json is valid JSON");
    assert_eq!(
        report["total"], 0,
        "an unstaged edit is not what a commit would record: {report}"
    );

    git(&dir, &["add", "src/lib.rs"]);
    let staged = roteiro(&dir, &["debt", "--staged", "--json"]);
    assert!(staged.status.success(), "debt --staged failed: {staged:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&staged.stdout).expect("debt --json is valid JSON");
    assert_eq!(
        report["total"], 1,
        "staging the marker makes it what a commit would record: {report}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
