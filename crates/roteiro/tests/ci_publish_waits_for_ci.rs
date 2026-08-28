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

mod common;

/// The workflow file this guard reads.
const WORKFLOW: &str = ".github/workflows/release-plz.yml";

/// The job that runs `cargo publish`.
const PUBLISH_JOB: &str = "release-plz-release";

/// Every check branch protection requires on `main`. The gate must wait for all
/// of them, or it vouches for less than the merge did.
const REQUIRED: [&str; 4] = ["checks", "default-features", "msrv", "no-default-features"];

/// The workflow text, or `None` when the tests run outside a checkout (a
/// packaged crate carries no `.github`), which is a skip rather than a failure.
///
/// [`common::repo_file`] rather than `read_to_string(..).ok()`: that spelling
/// skipped this guard on **any** IO error, so "could not read the workflow"
/// became a green — the same silent-skip defect the guard itself is about, one
/// level up. Only a missing file outside a checkout is a skip; the rest panic.
fn workflow() -> Option<String> {
    common::repo_file(WORKFLOW)
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
fn the_wait_is_allowed_to_read_the_checks_it_waits_for() {
    let Some(text) = workflow() else {
        return;
    };
    let job = publish_job(&text);

    // Only meaningful while the wait authenticates as `GITHUB_TOKEN`. An app
    // token carries the installation's own permissions and this stops applying.
    if !job.contains("GH_TOKEN: ${{ github.token }}") {
        return;
    }

    // A real mapping key, not a substring: `contains("checks: read")` is
    // satisfied by a commented-out line or by prose discussing the scope — and
    // this file's own workflow comment discusses it at length. The guard is about
    // the permission being *in effect*, so it has to read like YAML does.
    let granted = text.lines().any(|l| {
        let t = l.trim();
        !t.starts_with('#') && t.split('#').next().unwrap_or("").trim() == "checks: read"
    });
    assert!(
        granted,
        "the wait queries `.../commits/{{sha}}/check-runs`, which needs the `checks: \
         read` scope. This workflow sets an explicit `permissions:` block, and that \
         sets every scope it does not name to `none` — so dropping this line does not \
         make the gate permissive, it makes the publish step fail on its first API \
         call, every single time. A release that can never happen looks exactly like \
         a release nobody asked for."
    );
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

/// The wait reads the **newest** check run for each name, not whichever the API
/// listed first.
///
/// A commit can carry several check runs sharing one name: re-running a job adds
/// a run rather than replacing it, and re-running a red job is the most ordinary
/// thing to do on the tree this gate guards. Without a collapse, the loop's
/// first-match lookup reads an arbitrary one — so a stale success can authorise
/// a publish while the newest run is failing, which is the outcome the whole
/// step exists to prevent, reached by querying the right endpoint and believing
/// the wrong row.
///
/// Raised by Copilot on PR #652. Latent rather than observed: a scan of twelve
/// recent commits on `main` found no duplicate names, because nothing had been
/// re-run. "Not yet" is not a guarantee, and the failure would be silent.
#[test]
fn the_wait_reads_the_newest_run_for_each_name() {
    let Some(text) = workflow() else {
        return;
    };
    let job = publish_job(&text);

    assert!(
        job.contains("\\(.id)"),
        "the check-run rows must carry the id, or there is nothing to pick the \
         newest by"
    );
    assert!(
        job.contains("-k2,2nr"),
        "the rows must be collapsed to the newest run per name before the \
         per-name lookup. Without it the loop takes whichever row the API \
         happened to list first, and a re-run makes that a coin toss on the job \
         that decides whether to publish."
    );
    // The collapse must be a sort, not a `jq group_by`: `--paginate` applies
    // `--jq` per page, so a group across pages would silently see only the last
    // one — a wrong answer that looks like a working filter.
    //
    // Read the *commands*, not the comments. The first version of this assertion
    // was `!job.contains("group_by")` and failed on the correct workflow,
    // because the comment above the sort explains why a `group_by` is not used.
    // That is the same defect Copilot raised about `contains("checks: read")`,
    // arrived at from the other side: a substring test cannot tell code from
    // prose about the code.
    let commands: String = job
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !commands.contains("group_by"),
        "collapse with `sort`, not `jq group_by`: `--paginate` runs the jq filter \
         once per page, so a cross-page group silently sees only the last page"
    );
}
