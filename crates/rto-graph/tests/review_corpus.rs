//! Validation for the adjudicated review corpus — **through its consumer**.
//!
//! `tests/fixtures/review/review-corpus.jsonl` is a historical record of what an
//! automated reviewer said about specific commits, and what the maintainer decided
//! about each comment. It is the fixture any candidate reviewer is scored against,
//! so a malformed row silently corrupts every future score.
//!
//! These checks used to re-parse the file as untyped JSON and compare key sets by
//! hand. They now go through [`rto_graph::review_corpus::Corpus`] — the type the
//! scorer loads it with (Stage 35). That is deliberate and it is stronger in the
//! way that matters: the file is no longer validated against a *second* transcript
//! of its schema that could drift from the real one, but against the only reader
//! that has consumers. A row this test accepts is a row the scorer can score.
//! The per-field rules themselves are unit-tested in that module, against spoiled
//! rows this file has no need to construct.
//!
//! What remains here is what only the real file can answer: that the shipped
//! loader accepts it, that the README's class table describes it, and that its
//! `reviewed_sha`s are commits in this repository.
//!
//! Deliberately **offline**: the committed file and the local git history, nothing
//! else. Re-deriving the corpus from the GitHub API would make CI depend on
//! network availability and — worse — would let the fixture change when a comment
//! is edited or a thread resolved, which is precisely what a historical record must
//! not do.
//!
//! See `tests/fixtures/review/README.md` for what the fields mean and
//! `docs/REVIEW_CHECKLIST.md` for the adjudication rule that decides a verdict.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rto_graph::review_corpus::{COMPLETE_THROUGH_PR, Corpus, DefectClass, Verdict};

/// How many rows the corpus held when this test was written. It is an append-only
/// historical record, so it may grow but must not shrink.
const ROWS_AT_LEAST: usize = 26;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/review")
}

/// The corpus, loaded the way a scorer loads it. A parse failure fails here with
/// the loader's own message, which names the line and the field.
fn corpus() -> Corpus {
    let path = fixture_dir().join("review-corpus.jsonl");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    Corpus::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// **The anti-rot link.** The shipped loader accepts every row of the real file.
/// Before Stage 35 the corpus had no consumer; this is the check that it still has
/// one, and that the two have not drifted apart.
#[test]
fn the_shipped_loader_accepts_the_whole_corpus() {
    let corpus = corpus();
    assert!(
        corpus.len() >= ROWS_AT_LEAST,
        "the corpus has shrunk to {} rows; it is an append-only historical record, \
         so rows should not be removed",
        corpus.len()
    );
    // Every row's own invariants are enforced by `Corpus::parse`. What is worth
    // asserting over the real data is that it is not accidentally uniform: a
    // corpus of one verdict, or of one commit, would score nothing useful.
    assert!(
        corpus.with_verdict(Verdict::Real).count() > 0
            && corpus.with_verdict(Verdict::False).count() > 0,
        "the corpus needs both verdicts to measure both recall and precision"
    );
    assert!(
        corpus.reviewed_shas().len() > 1,
        "a corpus on one commit measures one tree"
    );
}

/// The complete, unfiltered subset is what a *ratio* may be computed over, and it
/// must be a real subset — if every row were inside it, the distinction the README
/// draws would be describing nothing.
#[test]
fn the_complete_subset_is_a_proper_and_non_empty_part_of_the_corpus() {
    let corpus = corpus();
    let complete = corpus.complete_subset();
    assert!(
        !complete.is_empty(),
        "no row is inside the complete subset (pr <= {COMPLETE_THROUGH_PR}), so no \
         ratio over this corpus would be meaningful"
    );
    assert!(
        complete.len() < corpus.len(),
        "every row is inside the complete subset, so the README's caveat about \
         selectively added rows no longer describes the file — either the caveat or \
         COMPLETE_THROUGH_PR is stale"
    );
}

/// **The result that gives the corpus its point, asserted against the data.**
/// Every false positive is a compile-failure claim and every compile-failure claim
/// is a false positive. `rto_graph::compile_claim` is licensed by exactly this, so
/// if a real defect ever joins the class, the suppression rule loses its licence
/// and this must fail rather than let the filter go on discarding true findings.
#[test]
fn the_compile_claim_class_is_still_the_only_false_one_and_wholly_false() {
    let corpus = corpus();
    let counts = corpus.class_counts();
    for (class, (real, fals)) in &counts {
        if *class == DefectClass::FalseCompileClaim {
            assert_eq!(
                *real, 0,
                "a REAL defect has joined `false-compile-claim`. \
                 `rto_graph::compile_claim` withholds this class on the measured \
                 grounds that it discards nothing true; that measurement no longer \
                 holds, so the filter must be revisited before this test is changed"
            );
            assert!(
                *fals > 0,
                "the class has no rows left to license the filter"
            );
        } else {
            assert_eq!(
                *fals, 0,
                "class `{class}` now has a false positive, so \
                 `false-compile-claim` is no longer the only class containing one. \
                 The suppression rule's premise was that compile claims are the \
                 whole of the false class — update docs/REVIEW_CHECKLIST.md and the \
                 filter's licence together"
            );
        }
    }
}

/// The README's class table must agree with the file it describes.
///
/// The corpus's own review caught the README and `docs/REVIEW_CHECKLIST.md`
/// disagreeing about the row count — a `contract-drift` defect in the change that
/// ships a catalogue of `contract-drift`. Counts stated in prose drift from the
/// data they describe; the cheapest fix is to make the drift fail a test.
///
/// Only the **table** is parsed, never the surrounding prose: its rows have a fixed
/// shape (`| `class` — description | n | real | false |`), so this reads structure
/// rather than English. Narrative totals elsewhere are deliberately not asserted —
/// a test that greps sentences is more fragile than the staleness it prevents,
/// which is why `docs/REVIEW_CHECKLIST.md` links here rather than restating them.
#[test]
fn the_readme_class_table_matches_the_corpus() {
    let readme = fixture_dir().join("README.md");
    let text = std::fs::read_to_string(&readme)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", readme.display()));

    // Counted from the data, by the same function a score uses — so the documented
    // counts and the scored counts cannot come from two readings of one file.
    let actual: BTreeMap<String, (usize, usize)> = corpus()
        .class_counts()
        .into_iter()
        .map(|(class, counts)| (class.as_str().to_owned(), counts))
        .collect();

    // Stated in the table. A row is `| `class` — … | n | real | false |`.
    let mut stated: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let [head, total, real, fals] = cells.as_slice() else {
            continue;
        };
        let Some(class) = head.trim_start_matches("| ").split('`').nth(1) else {
            continue;
        };
        let nums: Vec<usize> = [total, real, fals]
            .iter()
            .filter_map(|c| c.parse().ok())
            .collect();
        let [total, real, fals] = nums.as_slice() else {
            continue;
        };
        assert_eq!(
            total,
            &(real + fals),
            "README class table row for `{class}`: {total} does not equal {real} \
             real + {fals} false"
        );
        assert!(
            DefectClass::from_token(class).is_some(),
            "README class table names `{class}`, which is not a class the loader \
             knows — add the variant to `rto_graph::review_corpus::DefectClass` and \
             the table row together"
        );
        stated.insert(class.to_owned(), (*real, *fals));
    }

    assert!(
        !stated.is_empty(),
        "no class-table rows parsed out of {}; if the table was reformatted, update \
         this test with it",
        readme.display()
    );
    assert_eq!(
        stated, actual,
        "the README class table disagrees with review-corpus.jsonl (left = stated in \
         the README, right = counted from the data). Update the table — and any total \
         quoted in prose — to match the file"
    );
}

/// Confirms each `reviewed_sha` names a real commit in this repository, which
/// catches a truncated or mistyped sha that the format check alone would pass.
///
/// Needs the git history, so it reports and passes in a shallow clone rather than
/// failing — the same shape as the model-gated tests in `audio_ingest.rs`.
#[test]
fn reviewed_shas_resolve_in_this_repository() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
    };

    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("SKIP: not a git work tree, cannot resolve review shas");
            return;
        }
    }

    // Only a shallow clone licenses skipping. In a full clone an unresolvable sha
    // is a corrupt row, not a missing object — so the two must be told apart here,
    // or a typo'd sha would skip its way to green.
    let shallow = git(&["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
    if shallow {
        eprintln!("SKIP: shallow clone, review shas are not all present");
        return;
    }

    for row in corpus().rows() {
        let out = git(&["cat-file", "-t", &row.reviewed_sha]).expect("git cat-file runs");
        assert!(
            out.status.success(),
            "row {} (line anchored at {}:{}): reviewed_sha {} does not resolve in \
             this repository. In a full clone that means the row is wrong, not that \
             history is missing — check it against the comment's \
             `original_commit_id`",
            row.id,
            row.path,
            row.line,
            row.reviewed_sha
        );
        let kind = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert_eq!(
            kind, "commit",
            "row {}: reviewed_sha {} resolves to a {kind}, not a commit",
            row.id, row.reviewed_sha
        );
    }
}

/// Each row's anchor must exist **in the tree the comment was made on** — the
/// check that a `reviewed_sha`/`path`/`line` triple is internally consistent.
///
/// This is what would catch a row silently rebuilt from a merged PR head: a path
/// added later, or a line number past the end of the file as it then stood, does
/// not resolve at the review commit even though it resolves at `HEAD`. Gated on
/// git history exactly like the test above.
#[test]
fn every_anchor_exists_in_the_tree_it_was_reviewed_on() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
    };
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("SKIP: not a git work tree, cannot read reviewed trees");
            return;
        }
    }
    if git(&["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
    {
        eprintln!("SKIP: shallow clone, reviewed trees are not all present");
        return;
    }

    for row in corpus().rows() {
        let spec = format!("{}:{}", row.reviewed_sha, row.path);
        let out = git(&["show", &spec]).expect("git show runs");
        assert!(
            out.status.success(),
            "row {}: {} does not exist at its reviewed_sha. A path that exists only \
             at a later commit is the signature of a row rebuilt from the merged PR \
             head instead of the comment's original_commit_id",
            row.id,
            row.path
        );
        let lines = String::from_utf8_lossy(&out.stdout).lines().count();
        assert!(
            row.line as usize <= lines,
            "row {}: line {} is past the end of {} at {} ({lines} lines). Either the \
             line or the commit is wrong",
            row.id,
            row.line,
            row.path,
            &row.reviewed_sha[..8]
        );
    }
}

/// **The recipe for reconstructing a reviewed diff, held to the data.**
///
/// The fixture README used to say `git diff $(git merge-base <base> <reviewed_sha>)
/// <reviewed_sha>`. That is wrong for almost every row here, and quietly so: these
/// PRs were merged with merge commits, so each `reviewed_sha` is an *ancestor* of
/// `main`, which makes `merge-base main <reviewed_sha>` the review commit itself
/// and the diff **empty**. A reviewer handed an empty diff finds nothing and scores
/// zero — the same silent-zero failure as scoring against the PR head, arrived at
/// from the other direction.
///
/// The correct base is where the PR branch forked: find the merge commit `M` that
/// brought the branch in (`reviewed_sha` is an ancestor of `M^2` but not of `M^1`),
/// and take `merge-base M^1 <reviewed_sha>`. Where no such merge exists — a branch
/// that was rebased or squashed away — `merge-base main <reviewed_sha>` is correct
/// after all, because the commit is no longer an ancestor.
///
/// This test is the recipe's executable form: for every row it reconstructs the
/// diff and requires it to be non-empty *and* to touch the row's own path. A
/// documented reconstruction that produces nothing is worse than none, because the
/// score it yields looks like a measurement.
#[test]
fn every_row_reconstructs_a_non_empty_reviewed_diff() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .expect("git runs")
    };
    let out = |args: &[&str]| String::from_utf8_lossy(&git(args).stdout).trim().to_owned();

    if !git(&["rev-parse", "--is-inside-work-tree"])
        .status
        .success()
    {
        eprintln!("SKIP: not a git work tree, cannot reconstruct reviewed diffs");
        return;
    }
    if out(&["rev-parse", "--is-shallow-repository"]) == "true" {
        eprintln!("SKIP: shallow clone, reviewed history is not all present");
        return;
    }
    // `origin/main` is not guaranteed to exist (a fresh clone of a fork, a worktree
    // with a different remote name), and its absence is a property of the checkout
    // rather than of the corpus.
    let main = ["origin/main", "main"].into_iter().find(|r| {
        git(&["rev-parse", "--verify", "--quiet", r])
            .status
            .success()
    });
    let Some(main) = main else {
        eprintln!("SKIP: neither origin/main nor main resolves here");
        return;
    };

    let is_ancestor =
        |a: &str, b: &str| git(&["merge-base", "--is-ancestor", a, b]).status.success();

    // Cached per commit, not per row: several comments share a review commit, and
    // walking the ancestry path is the expensive part.
    let mut fork_points: BTreeMap<String, String> = BTreeMap::new();
    for row in corpus().rows() {
        let sha = row.reviewed_sha.as_str();
        let fork = if let Some(cached) = fork_points.get(sha) {
            cached.clone()
        } else {
            // The merge that brought this branch into main, if there was one. Walked
            // oldest-first (`rev-list` prints newest-first) because that merge is the
            // *earliest* on the ancestry path — searching from the far end would test
            // every later merge in the repository's history first.
            let merges = out(&[
                "rev-list",
                "--merges",
                "--ancestry-path",
                &format!("{sha}..{main}"),
            ]);
            let found = merges
                .lines()
                .rev()
                .find_map(|m| {
                    let parents = out(&["rev-list", "--parents", "-n1", m]);
                    let mut it = parents.split_whitespace().skip(1);
                    let (p1, p2) = (it.next()?, it.next()?);
                    (is_ancestor(sha, p2) && !is_ancestor(sha, p1))
                        .then(|| out(&["merge-base", p1, sha]))
                })
                // Rebased or squashed away: no longer an ancestor, so the plain
                // merge-base is the fork point.
                .unwrap_or_else(|| out(&["merge-base", main, sha]));
            fork_points.insert(sha.to_owned(), found.clone());
            found
        };

        assert_ne!(
            fork,
            sha,
            "row {}: the reconstruction base for {} is the review commit itself, so \
             the diff is empty. That is the failure this test exists to catch — see \
             the doc comment for the rule that avoids it",
            row.id,
            &sha[..8]
        );
        let names = out(&["diff", "--name-only", &fork, sha]);
        assert!(
            !names.is_empty(),
            "row {}: the diff {}..{} is empty, so a reviewer scored on it would find \
             nothing and report zero",
            row.id,
            &fork[..8],
            &sha[..8]
        );
        assert!(
            names.lines().any(|p| p == row.path),
            "row {}: the reconstructed diff {}..{} does not touch {}, the file the \
             comment is anchored to. Either the base or the row is wrong; a diff \
             that omits the commented file cannot be what the reviewer saw. Touched: \
             {}",
            row.id,
            &fork[..8],
            &sha[..8],
            row.path,
            names.replace('\n', ", ")
        );
    }
}
