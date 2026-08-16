//! Schema validation for the adjudicated review corpus.
//!
//! `tests/fixtures/review/review-corpus.jsonl` is a historical record of what an
//! automated reviewer said about specific commits, and what the maintainer
//! decided about each comment. It is the fixture any candidate reviewer is
//! scored against, so a malformed row silently corrupts every future score.
//!
//! These checks are deliberately **offline**: they read the committed file and
//! nothing else. Re-deriving the corpus from the GitHub API would make CI depend
//! on network availability and rate limits, and — worse — would let the fixture
//! change when a comment is edited or a thread resolved, which is precisely what
//! a historical record must not do.
//!
//! See `tests/fixtures/review/README.md` for what the fields mean and
//! `docs/REVIEW_CHECKLIST.md` for the adjudication rule that decides `verdict`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

/// The field set a row must have — exactly these, no more and no fewer.
const FIELDS: &[&str] = &[
    "id",
    "pr",
    "reviewer",
    "reviewed_sha",
    "path",
    "line",
    "verdict",
    "defect_class",
    "fix_commit",
    "description",
    "comment_url",
];

/// The permitted verdicts. Adding one means changing the adjudication rule.
const VERDICTS: &[&str] = &["real", "false"];

/// The defect classes documented in the fixture README's class table.
const CLASSES: &[&str] = &[
    "cleanup-gap",
    "contract-drift",
    "error-text-drift",
    "false-compile-claim",
    "lint-convention",
    "lossy-identity",
    "missing-event",
    "ordering-bug",
    "perf-contract",
    "permissive-constraint",
    "prose-clarity",
    "silent-truncation",
    "ux-diagnostic",
    "vacuous-test",
];

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/review/review-corpus.jsonl")
}

/// Every non-empty line, parsed, with its 1-based line number for messages.
fn rows() -> Vec<(usize, Value)> {
    let path = corpus_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let v: Value = serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("line {}: not valid JSON: {e}", i + 1));
            (i + 1, v)
        })
        .collect()
}

fn is_hex_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn the_corpus_is_not_empty_and_every_line_parses() {
    let rows = rows();
    assert!(
        rows.len() >= 26,
        "the corpus has shrunk to {} rows; it is an append-only historical \
         record, so rows should not be removed",
        rows.len()
    );
}

#[test]
fn every_row_carries_exactly_the_documented_fields() {
    for (n, row) in rows() {
        let obj = row
            .as_object()
            .unwrap_or_else(|| panic!("line {n}: row is not a JSON object"));
        let got: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let want: BTreeSet<&str> = FIELDS.iter().copied().collect();
        let missing: Vec<_> = want.difference(&got).collect();
        let extra: Vec<_> = got.difference(&want).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "line {n}: field set does not match the documented schema \
             (missing: {missing:?}, unexpected: {extra:?}); update \
             tests/fixtures/review/README.md and FIELDS together if the \
             schema is meant to change"
        );
    }
}

#[test]
fn verdict_and_defect_class_come_from_the_documented_vocabularies() {
    for (n, row) in rows() {
        let verdict = row["verdict"].as_str().expect("verdict is a string");
        assert!(
            VERDICTS.contains(&verdict),
            "line {n}: verdict {verdict:?} is not one of {VERDICTS:?}"
        );
        let class = row["defect_class"]
            .as_str()
            .expect("defect_class is a string");
        assert!(
            CLASSES.contains(&class),
            "line {n}: defect_class {class:?} is not a documented class; add it \
             to the README class table and to CLASSES together"
        );
    }
}

#[test]
fn identifiers_and_anchors_are_well_formed() {
    for (n, row) in rows() {
        let id = row["id"].as_u64();
        assert!(
            id.is_some_and(|v| v > 0),
            "line {n}: id must be a positive integer"
        );
        let pr = row["pr"].as_u64();
        assert!(
            pr.is_some_and(|v| v > 0),
            "line {n}: pr must be a positive integer"
        );
        let line = row["line"].as_u64();
        assert!(
            line.is_some_and(|v| v > 0),
            "line {n}: line must be a positive integer"
        );

        let sha = row["reviewed_sha"]
            .as_str()
            .expect("reviewed_sha is a string");
        assert!(
            is_hex_sha(sha),
            "line {n}: reviewed_sha {sha:?} is not a 40-character hex sha. It \
             must be the comment's `original_commit_id` — the tree the reviewer \
             saw — never the merged PR head, which contains the fix commits"
        );

        // `fix_commit` is optional (three rows legitimately have none), but when
        // present it must look like a sha rather than a note to the reader.
        let fix = row["fix_commit"].as_str().expect("fix_commit is a string");
        assert!(
            fix.is_empty() || (fix.len() >= 7 && fix.chars().all(|c| c.is_ascii_hexdigit())),
            "line {n}: fix_commit {fix:?} is neither empty nor a hex sha"
        );

        for field in ["reviewer", "path", "description", "comment_url"] {
            let v = row[field].as_str().expect("string field");
            assert!(!v.trim().is_empty(), "line {n}: {field} must not be blank");
        }
    }
}

#[test]
fn no_comment_id_appears_twice() {
    let mut seen = BTreeSet::new();
    for (n, row) in rows() {
        let id = row["id"].as_u64().expect("id is an integer");
        assert!(
            seen.insert(id),
            "line {n}: duplicate comment id {id}; the id is the corpus's \
             primary key, and a repeat would double-count that comment in \
             every score computed from the file"
        );
    }
}

/// Confirms each `reviewed_sha` names a real object in this repository, which
/// catches a truncated or mistyped sha that the format check alone would pass.
///
/// Needs the git history, so it reports and passes in a shallow clone rather
/// than failing — the same shape as the model-gated tests in `audio_ingest.rs`.
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

    // Only a shallow clone licenses skipping. In a full clone an unresolvable
    // sha is a corrupt row, not a missing object — so the two must be told
    // apart here, or a typo'd sha would skip its way to green.
    let shallow = git(&["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
    if shallow {
        eprintln!("SKIP: shallow clone, review shas are not all present");
        return;
    }

    for (n, row) in rows() {
        let sha = row["reviewed_sha"].as_str().expect("reviewed_sha");
        let out = git(&["cat-file", "-t", sha]).expect("git cat-file runs");
        assert!(
            out.status.success(),
            "line {n}: reviewed_sha {sha} does not resolve in this repository. \
             In a full clone that means the row is wrong, not that history is \
             missing — check it against the comment's `original_commit_id`"
        );
        let kind = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            kind, "commit",
            "line {n}: reviewed_sha {sha} resolves to a {kind}, not a commit"
        );
    }
}
