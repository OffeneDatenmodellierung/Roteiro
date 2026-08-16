//! End-to-end test for the worktree-aware `roteiro check` (Stage 16): the
//! default validates the working tree so it can gate a commit before it is made,
//! while `--committed` validates only `HEAD`. Drives the real binary.

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
    let dir = std::env::temp_dir().join(format!("roteiro-check-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// An ADR whose `[[…]]` link points at a code symbol — the authored layer whose
/// drift `check` detects.
const ADR: &str = "---\n\
                   adr-id: \"0001\"\n\
                   status: Accepted\n\
                   ---\n\
                   \n\
                   # ADR-0001: Thing\n\
                   \n\
                   ## Decision\n\
                   \n\
                   The design centres on [[src/lib.rs#Thing]].\n";

#[test]
fn worktree_check_gates_a_drift_introducing_edit_that_committed_ignores() {
    let dir = fresh_dir("worktree");
    git(&dir, &["init", "-q"]);
    // A symbol the ADR links to, and the ADR itself.
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "docs/adr/0001-thing.md", ADR);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Clean tree: the link resolves, so both modes pass.
    assert!(
        roteiro(&dir, &["check"]).status.success(),
        "clean worktree check should pass"
    );
    assert!(
        roteiro(&dir, &["check", "--committed"]).status.success(),
        "clean committed check should pass"
    );

    // Introduce drift in the working tree only (do NOT commit): the linked symbol
    // `Thing` is gone.
    write(&dir, "src/lib.rs", "pub struct Other;\n");

    // Worktree-aware check (default) sees the pending change and fails on the now
    // dangling authored link…
    let worktree = roteiro(&dir, &["check"]);
    assert!(
        !worktree.status.success(),
        "worktree check must fail on the drift about to be committed: {}",
        String::from_utf8_lossy(&worktree.stderr)
    );

    // …while `--committed` validates HEAD (where `Thing` still exists) and passes.
    assert!(
        roteiro(&dir, &["check", "--committed"]).status.success(),
        "committed check should still pass — HEAD is unchanged"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn staged_check_validates_the_index_not_the_working_tree() {
    // `check --staged` gates exactly what a commit records. Stage a change that
    // dangles an authored link, then restore the file on disk (unstaged): the
    // working-tree `check` passes, but `--staged` fails on the staged drift.
    let dir = fresh_dir("staged");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "docs/adr/0001-thing.md", ADR);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Stage the removal of `Thing`, then put it back on disk (unstaged).
    write(&dir, "src/lib.rs", "pub struct Other;\n");
    git(&dir, &["add", "src/lib.rs"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");

    // Working tree still has Thing → the default (worktree) check passes.
    assert!(
        roteiro(&dir, &["check"]).status.success(),
        "worktree check should pass (Thing is present on disk)"
    );
    // The index dropped Thing → the staged check fails on the dangling link.
    let staged = roteiro(&dir, &["check", "--staged"]);
    assert!(
        !staged.status.success(),
        "staged check must fail on drift the commit would record: {}",
        String::from_utf8_lossy(&staged.stderr)
    );
    assert!(
        String::from_utf8_lossy(&staged.stderr).contains("does not resolve"),
        "reports the dangling staged link: {}",
        String::from_utf8_lossy(&staged.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_validates_blueprint_links_like_adrs() {
    // A house-style blueprint (no frontmatter, identified by its H1 marker) whose
    // `[[…]]` link points at a real symbol passes `check`; a dangling link fails,
    // exactly as ADR links do.
    let dir = fresh_dir("blueprint");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Widget;\n");
    write(
        &dir,
        "docs/plans/widget.md",
        "# Widget — Technical Implementation Plan\n\n\
         ## 1. Design\n\nThe core type is [[src/lib.rs#Widget]].\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // The link resolves to a real symbol → check passes and counts the blueprint.
    let ok = roteiro(&dir, &["check", "--committed"]);
    assert!(
        ok.status.success(),
        "blueprint with a resolvable link should pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(
        String::from_utf8_lossy(&ok.stdout).contains("1 blueprint(s)"),
        "check reports the blueprint: {}",
        String::from_utf8_lossy(&ok.stdout)
    );

    // Point the blueprint at a symbol that does not exist → drift, check fails.
    write(
        &dir,
        "docs/plans/widget.md",
        "# Widget — Technical Implementation Plan\n\n\
         ## 1. Design\n\nGone: [[src/lib.rs#Ghost]].\n",
    );
    git(&dir, &["commit", "-qam", "dangle"]);
    let bad = roteiro(&dir, &["check", "--committed"]);
    assert!(
        !bad.status.success(),
        "a blueprint link to a missing symbol must fail check"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("does not resolve"),
        "reports the dangling blueprint link: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_fails_when_two_adr_files_share_an_adr_id() {
    // Issue #324, reproduced with two *real* ADR files: two branches each author
    // ADR-0016. Git merges them cleanly (they share no line), both parse, and
    // both key on `adr:0016` — so before this check `check` reported 0
    // violations while one of the two decisions had silently vanished.
    let dir = fresh_dir("duplicate-adr-id");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(
        &dir,
        "docs/adr/0016-audio-metadata.md",
        "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n\
         # ADR-0016: Audio metadata extraction\n\n## Decision\n\nFormat reads are cheap.\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // One ADR, one id: clean.
    let ok = roteiro(&dir, &["check", "--committed"]);
    assert!(
        ok.status.success(),
        "a single ADR-0016 should pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    // The parallel branch's ADR-0016 lands: a different decision, the same id.
    write(
        &dir,
        "docs/adr/0016-speculative-decoding.md",
        "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n\
         # ADR-0016: MTP speculative decoding\n\n## Decision\n\nOpt-in only.\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "second 0016"]);

    let bad = roteiro(&dir, &["check", "--committed"]);
    let stderr = String::from_utf8_lossy(&bad.stderr).into_owned();
    assert!(
        !bad.status.success(),
        "two ADRs on one id must fail check; stdout: {}",
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(
        stderr.contains("duplicate-adr-id"),
        "labelled as a duplicate id: {stderr}"
    );
    // The violation must name BOTH files and the id, so the reader does not hunt.
    assert!(stderr.contains("0016"), "names the shared id: {stderr}");
    assert!(
        stderr.contains("docs/adr/0016-audio-metadata.md"),
        "names the first file: {stderr}"
    );
    assert!(
        stderr.contains("docs/adr/0016-speculative-decoding.md"),
        "names the second file: {stderr}"
    );

    // `--json` carries the same finding for machine consumers.
    let json = roteiro(&dir, &["check", "--committed", "--json"]);
    let stdout = String::from_utf8_lossy(&json.stdout).into_owned();
    assert!(
        stdout.contains("duplicate-adr-id"),
        "json report carries the kind: {stdout}"
    );

    // Renaming one decision onto a free id clears it — the check is about the
    // collision, not about having two files.
    std::fs::remove_file(dir.join("docs/adr/0016-speculative-decoding.md")).expect("rm");
    write(
        &dir,
        "docs/adr/0017-speculative-decoding.md",
        "---\nadr-id: \"0017\"\nstatus: Accepted\n---\n\n\
         # ADR-0017: MTP speculative decoding\n\n## Decision\n\nOpt-in only.\n",
    );
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "renumber"]);
    let fixed = roteiro(&dir, &["check", "--committed"]);
    assert!(
        fixed.status.success(),
        "distinct ids pass: {}",
        String::from_utf8_lossy(&fixed.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}
