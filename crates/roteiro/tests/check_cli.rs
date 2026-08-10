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

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-check-cli-{}", std::process::id()));
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
    let dir = fresh_dir();
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
