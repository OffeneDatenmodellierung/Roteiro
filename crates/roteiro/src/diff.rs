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

/// The flags that make `git diff` answer for itself, in its own format.
///
/// `--no-ext-diff` refuses `diff.external` and `GIT_EXTERNAL_DIFF`. Git runs
/// those instead of diffing, and their output *replaces* the diff entirely, so
/// a configured helper turns this function into "run whatever that is and
/// return its stdout" — verified: with `diff.external` set, `git diff` printed
/// only the helper's output and no hunks at all.
///
/// Reachability, stated honestly rather than alarmingly: `git clone` does **not**
/// copy the remote's config, so this is not a clone-and-execute vector. It is
/// reachable through `GIT_EXTERNAL_DIFF` in the environment, and through the
/// `.git/config` of a repository directory obtained as an archive or a shared
/// checkout. Both are narrow, and the flag costs nothing.
///
/// `--no-color` is the same argument without the security half: `color.diff` can
/// be `always`, and ANSI escapes in a `--json` field are corruption of data
/// rather than decoration of a terminal.
const OWN_OUTPUT: [&str; 2] = ["--no-ext-diff", "--no-color"];

/// The trailing bytes a captured `git` stdout may end with and nobody wants.
///
/// **Only** `\r` and `\n` — not `trim_end()`, which strips *all* trailing
/// whitespace and so silently truncates a diff whose last line ends in
/// meaningful spaces. That is not a cosmetic loss: a change that *adds* trailing
/// whitespace renders as `-foo` / `+foo`, identical lines, in the one tool whose
/// job is to show what changed. Raised as a suppressed comment by Copilot on
/// PR #656.
const LINE_ENDINGS: [char; 2] = ['\r', '\n'];

/// Run `git` in `repo` and return its stdout with the trailing newline removed,
/// or `None` if it failed.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches(LINE_ENDINGS)
            .to_owned()
    })
}

/// The unified diff for one tracked `path` over `range`.
///
/// `range` is passed to `git diff` verbatim, and the three forms compare
/// **different pairs of trees** — a distinction worth stating because the empty
/// one is not the intuitive default:
///
/// - `[]` — the working tree against the **index**: unstaged edits only. A
///   staged change is invisible to it.
/// - `["HEAD"]` — the **working tree** against `HEAD`. Staged edits usually
///   appear too, but only because staging does not alter the worktree — this is
///   *not* "what a commit would contain". A commit records the **index**, and
///   where the two diverge (stage a file, then edit it again) this shows the
///   later worktree content while a commit would record the earlier staged
///   blob. It is nonetheless the right range for a working-tree review, because
///   it matches what the graph is built from: `GraphSource::Worktree` reads
///   content from disk, so the diff and the surrounding context describe the
///   same tree. `check --staged` is the surface that answers the committing
///   question.
/// - `[base, "HEAD"]` — a commit range, needing no working tree at all.
///
/// Returns `None` when git fails and `Some("")` when git ran and emitted
/// nothing. Callers must not collapse those two: one is "we could not look" and
/// the other is "we looked and there is no text", which is the same distinction
/// [`review_llm`](crate::review_llm) draws when it refuses to send a hunkless
/// file to a model.
///
/// The empty case is **not** a mode change, a rename, or a binary file, though
/// each reads like it should be: `git diff -U3` emits headers for all three, so
/// they come back as ordinary non-empty diffs (see
/// `a_mode_only_change_still_produces_a_diff`). What is left is a genuinely
/// empty file, or a path whose content already matches the other side of the
/// comparison.
#[must_use]
pub fn unified(repo: &Path, range: &[&str], path: &str) -> Option<String> {
    let mut args: Vec<&str> = vec!["diff", CONTEXT_LINES];
    args.extend(OWN_OUTPUT);
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
        .args(["diff", CONTEXT_LINES])
        .args(OWN_OUTPUT)
        .args(["--no-index", "--", "/dev/null", path])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(LINE_ENDINGS)
        .to_owned();
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

    /// The three `range` forms compare different pairs of trees.
    ///
    /// Pinned because the doc comment described `[]` as "the working tree
    /// against the index and `HEAD`", which conflates the two comparisons that
    /// actually differ — and a caller picking `[]` for a working-tree review
    /// would silently miss every staged edit. Raised by Copilot on PR #656.
    #[test]
    fn an_empty_range_sees_only_unstaged_edits() {
        let dir = repo("range");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("git")
        };
        fs::write(dir.join("kept.rs"), "fn a() {}\nfn staged() {}\n").expect("write");
        run(&["add", "kept.rs"]);
        fs::write(
            dir.join("kept.rs"),
            "fn a() {}\nfn staged() {}\nfn unstaged() {}\n",
        )
        .expect("write");

        let unstaged_only = unified(&dir, &[], "kept.rs").expect("diff");
        assert!(
            unstaged_only.contains("+fn unstaged() {}"),
            "an empty range shows the unstaged edit: {unstaged_only}"
        );
        assert!(
            !unstaged_only.contains("+fn staged() {}"),
            "and does NOT show the staged one — it compares against the index: \
             {unstaged_only}"
        );

        let vs_head = unified(&dir, &["HEAD"], "kept.rs").expect("diff");
        assert!(
            vs_head.contains("+fn staged() {}") && vs_head.contains("+fn unstaged() {}"),
            "`HEAD` shows both, which is why a working-tree review uses it: {vs_head}"
        );
    }

    /// Trailing whitespace on the last line of a diff survives.
    ///
    /// `trim_end()` strips all trailing whitespace, so a hunk ending in
    /// meaningful spaces came back truncated. The consequence is specific and
    /// bad: adding trailing whitespace to a file — a routine lint failure —
    /// produced `-fn a() {}` / `+fn a() {}`, two identical-looking lines, in the
    /// one tool whose job is to show what changed.
    ///
    /// Raised as a *suppressed* comment by Copilot on PR #656, which is worth
    /// noting: the suppressed fold is not visible in the review-comments API and
    /// had gone unread until it was pointed out.
    #[test]
    fn trailing_whitespace_on_the_last_diff_line_survives() {
        let dir = repo("trailing");
        // The edit *is* the trailing whitespace, so trimming it erases the change.
        fs::write(dir.join("kept.rs"), "fn a() {}   \n").expect("write");

        let d = unified(&dir, &[], "kept.rs").expect("diff");
        assert!(
            d.contains("+fn a() {}   "),
            "the added line keeps its trailing spaces: {d:?}"
        );
        assert!(
            !d.ends_with("+fn a() {}"),
            "and the diff does not end on a truncated version of it: {d:?}"
        );
    }

    /// A configured external differ does not get to answer for us.
    ///
    /// `diff.external` and `GIT_EXTERNAL_DIFF` make git run a command *instead
    /// of* diffing, and its stdout replaces the diff — so without `--no-ext-diff`
    /// this function becomes "run whatever that is and return its output". Raised
    /// by Copilot on PR #656, after the diff became default-on for the graph arm.
    ///
    /// Both vectors are covered here: repository config, and the environment.
    #[test]
    fn an_external_differ_is_refused() {
        let dir = repo("extdiff");
        let helper = dir.join("helper.sh");
        fs::write(&helper, "#!/bin/sh\necho EXTERNAL-DIFF-EXECUTED\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let configured = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["config", "diff.external"])
            .arg(&helper)
            .output()
            .expect("git")
            .status
            .success();
        assert!(configured, "set diff.external");
        fs::write(dir.join("kept.rs"), "fn a() {}\nfn b() {}\n").expect("write");

        let d = unified(&dir, &[], "kept.rs").expect("diff");
        assert!(
            !d.contains("EXTERNAL-DIFF-EXECUTED"),
            "the configured helper must not have run: {d}"
        );
        assert!(
            d.contains("+fn b() {}"),
            "and git's own diff must be what came back: {d}"
        );
    }

    /// `["HEAD"]` shows the **worktree**, not what a commit would record.
    ///
    /// The two are the same until the index and the worktree diverge, which is
    /// why "staged and unstaged" reads as "what you are about to commit" and is
    /// not. Stage one version, edit to another, and `git diff HEAD` shows the
    /// later one while the commit would record the earlier. Raised by Copilot on
    /// PR #656 against a doc comment that claimed the committing view.
    ///
    /// The behaviour is right for a working-tree review — it matches what
    /// `GraphSource::Worktree` reads from disk — so this pins the behaviour and
    /// its limit together, rather than leaving the justification to prose that
    /// has now been wrong three times in this file.
    #[test]
    fn head_shows_the_worktree_not_the_index() {
        let dir = repo("diverge");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("git")
        };
        fs::write(dir.join("kept.rs"), "fn staged_version() {}\n").expect("write");
        run(&["add", "kept.rs"]);
        fs::write(dir.join("kept.rs"), "fn worktree_version() {}\n").expect("write");

        let vs_head = unified(&dir, &["HEAD"], "kept.rs").expect("diff");
        assert!(
            vs_head.contains("+fn worktree_version() {}"),
            "`HEAD` shows the worktree content: {vs_head}"
        );
        assert!(
            !vs_head.contains("+fn staged_version() {}"),
            "and not the staged blob, which is what a commit would actually \
             record: {vs_head}"
        );

        let cached = unified(&dir, &["--cached"], "kept.rs").expect("diff");
        assert!(
            cached.contains("+fn staged_version() {}"),
            "`--cached` is the committing view: {cached}"
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
