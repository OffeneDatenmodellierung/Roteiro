//! Guard: the publish job waits for CI on the commit it is about to publish
//! (issue #473).
//!
//! Merging the release PR is a push to `main`, and that push starts `ci.yml` and
//! `release-plz.yml` **in parallel, in different workflows, with no dependency
//! between them**. Without a gate, `cargo publish` runs regardless of what CI is
//! about to say — and crates.io permits a yank, never a replacement.
//!
//! # Why branch protection does not already cover this
//!
//! It proves the PR was green **when it was checked**. It cannot prove the tree
//! is green *now*, because a tree can turn red with no commit behind the change.
//! On 2026-08-27 `bisync` was yanked upstream and `main` went red on `cargo
//! deny` — every required check had passed on every PR, and `0db4d2d` (the merge
//! of #646) is a real commit on this repository's `main` where `checks`
//! concluded `failure`. A release merged in that window would have published
//! from a tree CI was about to reject.
//!
//! # Why this is read out of the file
//!
//! The same argument `ci_release_pr_parity` makes: the defect is a **missing**
//! dependency, and nothing that runs the pipeline can observe an absence. The
//! only run that would notice is the one that publishes something it should not
//! have, which is the run whose outcome cannot be undone.

use std::path::{Path, PathBuf};

/// The workflow file this guard reads.
const WORKFLOW: &str = ".github/workflows/release-plz.yml";

/// The job that runs `cargo publish`.
const PUBLISH_JOB: &str = "release-plz-release";

/// Every check branch protection requires on `main`. The gate must wait for all
/// of them, or it vouches for less than the merge did.
const REQUIRED: [&str; 4] = ["checks", "default-features", "msrv", "no-default-features"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// The workflow text, or `None` when the tests run outside a checkout (a
/// packaged crate carries no `.github`), which is a skip rather than a failure.
fn workflow() -> Option<String> {
    std::fs::read_to_string(repo_root().join(WORKFLOW)).ok()
}

/// The `release-plz-release:` job block, from its key to the next job key.
fn publish_job(text: &str) -> String {
    let start = text
        .find(&format!("\n  {PUBLISH_JOB}:"))
        .unwrap_or_else(|| panic!("`{PUBLISH_JOB}` must exist in {WORKFLOW}"));
    let rest = &text[start + 1..];
    // The next top-level job key is two spaces then a name then a colon.
    let end = rest
        .match_indices("\n  ")
        .find(|(i, _)| {
            let line = rest[i + 3..].split('\n').next().unwrap_or("");
            line.ends_with(':') && !line.starts_with('-') && !line.contains(' ')
        })
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].to_owned()
}

#[test]
fn the_publish_job_waits_for_ci_before_it_publishes() {
    let Some(text) = workflow() else {
        return;
    };
    let job = publish_job(&text);

    let publishes = job.contains("command: release");
    assert!(
        publishes,
        "this guard is anchored on the job that runs `release-plz release`; if the \
         publish moved, move the guard with it rather than deleting it"
    );

    // The wait must come BEFORE anything that could publish. A gate after the
    // fact is not a gate.
    let wait_at = job
        .find("Wait for CI to pass on this commit")
        .unwrap_or_else(|| {
            panic!(
                "`{PUBLISH_JOB}` no longer waits for CI. Merging the release PR starts CI \
             and this job in parallel with no dependency between them, so removing the \
             wait means a red `main` publishes — and a published version cannot be \
             replaced, only yanked."
            )
        });
    let release_at = job
        .find("command: release")
        .expect("checked immediately above");
    assert!(
        wait_at < release_at,
        "the CI wait must run before `release-plz release`, not after it"
    );

    for name in REQUIRED {
        assert!(
            job.contains(name),
            "the wait must cover `{name}`, which branch protection requires on `main`. \
             Waiting for fewer checks than the merge required vouches for less than the \
             merge did."
        );
    }
}

#[test]
fn the_wait_fails_closed() {
    let Some(text) = workflow() else {
        return;
    };
    let job = publish_job(&text);

    // A gate that treats "no answer" as "green" is worse than no gate, because it
    // reads as protection. Each of these is a distinct way to get no answer.
    assert!(
        job.contains("has not reported yet"),
        "a required check that never reports must keep the wait waiting, not pass it"
    );
    assert!(
        job.contains("refusing to publish"),
        "the failure path must refuse the publish and say so"
    );
    assert!(
        job.contains("set -euo pipefail"),
        "without `-e` a failed `gh api` call leaves the loop reading an empty result, \
         which every check would then fail to match — silently, as though CI were \
         merely slow"
    );
    assert!(
        job.contains("deadline"),
        "the wait must be bounded: a job that waits forever is a release that never \
         happens and nobody is told why"
    );
}
