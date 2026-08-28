//! The one place `roteiro` turns a change into diff text (issue #649).
//!
//! Both review arms need the same thing — the hunks for a path over a range —
//! and before this module only [`review_llm`](crate::review_llm) had it, behind
//! a generation-backend feature gate. The graph arm is unconditional, so reusing
//! that copy was not possible and adding a second one was the obvious move;
//! this module exists so that it was not made. There is one definition of "the
//! diff for this path", and both arms call it.
//!
//! # Why this shells out rather than using gix
//!
//! [`rto_graph::git`] is pure gix and never spawns a process, which is worth
//! keeping: it is what lets the graph build without a `git` binary on `PATH`.
//! But gix gives blob-level tree diffs — paths and oids, as [`rto_graph::TreeDiff`]
//! shows — not rendered unified hunks, and rendering those is a diff
//! implementation rather than a call. So the shell-out stays, and it stays *here*
//! rather than in `rto-graph`, so the graph crate keeps its property.

use std::path::Path;
use std::process::Command;

/// Lines of context either side of a change, matching what the LLM arm has
/// always asked for. Named rather than repeated so the two arms cannot drift
/// into reviewing differently sized windows onto the same change.
const CONTEXT_LINES: &str = "-U3";

/// Run `git` in `repo` and return trimmed stdout, or `None` if it failed.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

/// The unified diff for one tracked `path` over `range`.
///
/// `range` is passed to `git diff` verbatim: empty means the working tree
/// against the index and `HEAD` as git itself defines it, `["HEAD"]` means the
/// working tree against `HEAD` (staged edits included), and `[base, "HEAD"]`
/// means a commit range.
///
/// Returns `None` when git fails and `Some("")` when there is genuinely nothing
/// to show — a mode-only change, say. Callers must not collapse those two: one
/// is "we could not look" and the other is "we looked and there is no text",
/// which is the same distinction [`review_llm`](crate::review_llm) draws when it
/// refuses to send a hunkless file to a model.
#[must_use]
pub fn unified(repo: &Path, range: &[&str], path: &str) -> Option<String> {
    let mut args: Vec<&str> = vec!["diff", CONTEXT_LINES];
    args.extend(range.iter().copied());
    args.extend(["--", path]);
    git(repo, &args)
}

/// The unified diff for a path git does not track yet, rendered against
/// `/dev/null` so a brand-new file reads as an addition rather than as nothing.
///
/// A new file is the case where a reviewer most needs the text — there is no
/// prior version to infer it from — and it is exactly the case plain `git diff`
/// stays silent about, because an untracked path is not in the comparison at
/// all. The graph arm already counts untracked files as additions in its change
/// set, so leaving them without a diff would show a file, list its symbols, and
/// omit the only thing new about it.
///
/// `--no-index` exits **1** when the inputs differ, which here is every
/// successful call, so [`git`]'s `status.success()` test would discard the
/// output it just produced. This runs the command directly for that reason.
///
/// # Why the exit code alone is not enough
///
/// git returns **1 for both** "these differ" and "could not access that path":
///
/// ```text
/// $ git diff --no-index -- /dev/null absent.rs
/// error: Could not access 'absent.rs'
/// $ echo $?
/// 1
/// ```
///
/// So a rule that accepts every `1` reports a missing file as an empty diff —
/// the failure this whole module keeps trying not to make, where "we could not
/// look" is rendered as "there is nothing to see". What separates them is
/// **stderr**, which is silent on a real diff and carries the reason on a
/// failure.
#[must_use]
pub fn unified_untracked(repo: &Path, path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", CONTEXT_LINES, "--no-index", "--", "/dev/null", path])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    match out.status.code() {
        // 0 = the inputs are identical, which for a `/dev/null` comparison means
        // a genuinely empty new file. Nothing to show, and that is the answer.
        Some(0) => Some(stdout),
        // 1 = differs *or* failed; only a quiet stderr says which.
        Some(1) if out.stderr.is_empty() => Some(stdout),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A repository with one commit, so the diff paths have something to be a
    /// diff *against*.
    ///
    /// Built by hand rather than with `tempfile`, which is not in this crate's
    /// dependency tree: the integration tests alongside this one use the same
    /// `temp_dir` + pid + name recipe, and a helper for four unit tests is not
    /// worth a new crate in a workspace that argues about adding them.
    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-diff-{}-{name}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.as_path();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(p)
                .args(args)
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        fs::write(p.join("kept.rs"), "fn a() {}\n").expect("write");
        run(&["add", "kept.rs"]);
        // `commit.gpgsign` in a developer's *global* config would otherwise reach
        // in here and fail the commit, which has bitten this repository's tests
        // before.
        run(&["-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "x"]);
        dir
    }

    #[test]
    fn a_tracked_edit_produces_hunks() {
        let dir = repo("tracked");
        fs::write(dir.join("kept.rs"), "fn a() {}\nfn b() {}\n").expect("write");
        let d = unified(&dir, &[], "kept.rs").expect("diff");
        assert!(d.contains("@@"), "expected a hunk header, got: {d}");
        assert!(d.contains("+fn b() {}"), "expected the added line: {d}");
    }

    #[test]
    fn an_unchanged_file_is_empty_but_not_a_failure() {
        let dir = repo("unchanged");
        // The distinction the doc comment insists on: this must be `Some("")`,
        // never `None`, or "nothing changed" becomes indistinguishable from
        // "git would not answer".
        assert_eq!(unified(&dir, &[], "kept.rs").as_deref(), Some(""));
    }

    #[test]
    fn an_untracked_file_still_gets_a_diff() {
        let dir = repo("untracked");
        fs::write(dir.join("new.rs"), "fn c() {}\n").expect("write");
        // Plain `git diff` cannot see it — this is the gap the second entry
        // point exists to close, and the assertion that would have caught its
        // absence.
        assert_eq!(unified(&dir, &[], "new.rs").as_deref(), Some(""));

        let d = unified_untracked(&dir, "new.rs").expect("diff");
        assert!(
            d.contains("+fn c() {}"),
            "expected the new file's text: {d}"
        );
    }

    /// A mode-only change is **not** the empty case, though it reads like it
    /// should be.
    ///
    /// This assertion exists because the documentation got it wrong: `Some("")`
    /// was described as meaning "a mode-only change, say", and `git diff -U3`
    /// emits `old mode`/`new mode` headers for exactly that — 57 bytes on this
    /// fixture, not zero. Renames and binary files are the same story
    /// (`similarity index`/`rename from`, `Binary files … differ`). Raised by
    /// Copilot on PR #656.
    ///
    /// Pinned as a test rather than corrected in prose alone, because the wrong
    /// version was plausible enough to survive three separate write-ups of it.
    #[test]
    fn a_mode_only_change_still_produces_a_diff() {
        let dir = repo("mode");
        let mode_changed = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["update-index", "--chmod=+x", "kept.rs"])
            .output()
            .expect("git")
            .status
            .success();
        if !mode_changed {
            // Filesystems without an executable bit cannot stage this change, and
            // a skip is honest where a pass would not be.
            return;
        }
        let d = unified(&dir, &["--cached"], "kept.rs").expect("diff");
        assert!(
            !d.is_empty(),
            "a mode-only change emits headers, so it is an ordinary non-empty \
             diff — not the `Some(\"\")` case"
        );
        assert!(d.contains("mode"), "and those headers name the mode: {d}");
    }

    #[test]
    fn an_untracked_diff_of_a_missing_path_fails_rather_than_reads_empty() {
        let dir = repo("missing");
        // git exits 1 for "differs" *and* for "could not access", so the
        // exit-code test has to let 1 through — which is exactly how a missing
        // file gets reported as an empty diff. Only stderr separates them, and
        // this is the assertion that holds that apart. It failed when the rule
        // was `Some(0 | 1)`, which is how the ambiguity was found.
        assert!(unified_untracked(&dir, "absent.rs").is_none());
    }
}
