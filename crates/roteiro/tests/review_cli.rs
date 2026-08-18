//! End-to-end test for `roteiro review` (Stage 17): the CLI-first, graph-grounded
//! review of the working-tree change — it surfaces each touched symbol's
//! callers/callees and governing ADRs, and fails when the change introduces
//! authored-layer drift.

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
        // Isolate from any real user config ($ROTEIRO_HOME/config.toml), so the
        // `[debt] ignore` test below sees only the list the fixture writes.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("roteiro-review-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

const ADR: &str = "---\n\
                   adr-id: \"0001\"\n\
                   status: Accepted\n\
                   ---\n\
                   \n\
                   # ADR-0001\n\
                   \n\
                   ## Decision\n\
                   \n\
                   The design centres on [[src/main.rs#greet]].\n";

#[test]
fn review_shows_context_for_a_clean_change_and_fails_on_drift() {
    let dir = fresh_dir("context");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    write(&dir, "docs/adr/0001.md", ADR);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // No change yet → nothing to review.
    let empty = roteiro(&dir, &["review"]);
    assert!(empty.status.success());
    assert!(
        String::from_utf8_lossy(&empty.stdout).contains("no working-tree changes"),
        "clean tree reports nothing to review"
    );

    // A non-drift edit: `greet` gains a callee. Review passes and surfaces the
    // governing ADR and the caller/callee context.
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() { helper(); }\nfn helper() {}\n",
    );
    let clean = roteiro(&dir, &["review"]);
    let out = String::from_utf8_lossy(&clean.stdout);
    assert!(
        clean.status.success(),
        "non-drift review should exit 0: {out}"
    );
    assert!(
        out.contains("governed by: adr:0001#decision"),
        "shows the ADR: {out}"
    );
    assert!(
        out.contains("calls: sym:rust:src/main.rs#helper"),
        "shows callee: {out}"
    );
    assert!(
        out.contains("no authored-layer drift"),
        "no drift reported: {out}"
    );

    // A drift-introducing edit: rename `greet`, so the ADR's link dangles. Review
    // reports the drift and exits non-zero.
    write(
        &dir,
        "src/main.rs",
        "fn main() { hello(); }\nfn hello() {}\n",
    );
    let drift = roteiro(&dir, &["review"]);
    let out = String::from_utf8_lossy(&drift.stdout);
    assert!(
        !drift.status.success(),
        "drift review must exit non-zero: {out}"
    );
    assert!(
        out.contains("drift introduced by this change"),
        "reports drift: {out}"
    );
    assert!(
        out.contains("src/main.rs#greet"),
        "names the dangling link target: {out}"
    );

    // JSON carries the schema and the drift.
    let json = roteiro(&dir, &["review", "--json"]);
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(
        text.contains("\"schema\": \"roteiro.review/v1\""),
        "schema tag: {text}"
    );
    assert!(text.contains("\"drift\""), "drift field present: {text}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_detects_drift_from_editing_an_adr() {
    // Regression: a broken link introduced by editing the *ADR* file. Its
    // violation message leads with the ADR node key (`adr:0001#…`), not the ADR
    // path, so drift attribution must resolve that key's node path.
    let dir = fresh_dir("adr-edit");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    write(&dir, "docs/adr/0001.md", ADR);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Edit only the ADR: add a link to a symbol that does not exist.
    let edited = format!("{ADR}\nAnd also [[src/main.rs#missing]].\n");
    write(&dir, "docs/adr/0001.md", &edited);

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "editing an ADR to add a dangling link must be caught as drift: {text}"
    );
    assert!(
        text.contains("drift introduced by this change"),
        "reports the ADR-edit drift: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn range_review_covers_a_branch_vs_base() {
    // `review --base <ref>` reviews the commit range against the committed graph,
    // not the working tree.
    let dir = fresh_dir("range");
    write(&dir, "src/main.rs", "fn main() { a(); }\nfn a() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);

    // A feature branch adds a function and a call — committed, clean working tree.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); b(); }\nfn a() {}\nfn b() {}\n",
    );
    git(&dir, &["commit", "-q", "-am", "add b"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "sync");

    let out = roteiro(&dir, &["review", "--base", "main"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "range review should exit 0 (no drift): {text}"
    );
    assert!(
        text.contains("src/main.rs [modified]"),
        "reports the changed file: {text}"
    );
    assert!(
        text.contains("fn b") && text.contains("called by: sym:rust:src/main.rs#main"),
        "shows the new symbol's context from the committed graph: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_labels_untracked_files_as_added() {
    // A brand-new untracked file is overlaid into the working-tree review and
    // labelled `[added]` — not `[modified]`, which it isn't.
    let dir = fresh_dir("added");
    write(&dir, "src/main.rs", "fn main() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Written but never `git add`ed → untracked.
    write(&dir, "src/extra.rs", "pub fn brand_new() {}\n");

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "no drift → exit 0: {text}");
    assert!(
        text.contains("src/extra.rs [added]"),
        "an untracked new file must be labelled added, not modified: {text}"
    );
    assert!(
        text.contains("fn brand_new"),
        "the overlaid file's symbol is reviewed: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Fixtures for the `[debt] ignore` test: one marker in the repository's own
/// source, one in a vendored tree. Each literal carries a `roteiro:ignore`
/// directive because the detector scans *this* file too, and these are test data
/// rather than debt in this repository. They live at module scope, one literal
/// per line, so `cargo fmt` cannot reflow the call that used to hold them and
/// leave the directive on a different line from the marker — which silently
/// re-arms them.
const OWN: &str = "// TODO: wire this up\npub fn own() {}\n"; // roteiro:ignore
const VENDORED: &str = "// FIXME: upstream bug\npub fn dep() {}\n"; // roteiro:ignore
const OWN_EDITED: &str = "// TODO: wire this up\npub fn own() {}\npub fn own_two() {}\n"; // roteiro:ignore
const VENDORED_EDITED: &str = "// FIXME: upstream bug\npub fn dep() {}\npub fn dep_two() {}\n"; // roteiro:ignore

#[test]
fn review_applies_the_configured_debt_ignore() {
    // Issue #409: `review`'s per-file `debt` is `roteiro debt`'s inventory scoped
    // to the change, so it is governed by the same `[debt] ignore` (ADR-0007).
    // Before the fix `review::build` kept every `Marker` node in a changed file,
    // so a repository that excluded a vendored tree everywhere else still saw it
    // in `review` — the fifth surface to report a second debt figure for one
    // repository (after #321 and #372).
    let dir = fresh_dir("debt-ignore");
    // Two tracked files, one marker each: one in our own source, one in a
    // vendored tree. Tracked rather than untracked so `roteiro debt` — which
    // reads the repository, not the review's change set — sees the same two
    // files, and the cross-check at the end is a real comparison.
    write(&dir, "src/own.rs", OWN);
    write(&dir, "vendor/dep/lib.rs", VENDORED);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Edit both so the working-tree review covers them.
    write(&dir, "src/own.rs", OWN_EDITED);
    write(&dir, "vendor/dep/lib.rs", VENDORED_EDITED);

    // Debt per changed path, from the `--json` report.
    let debt_by_path = |dir: &Path| -> std::collections::BTreeMap<String, usize> {
        let out = roteiro(dir, &["review", "--json"]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "review --json failed: {text}");
        let report: serde_json::Value =
            serde_json::from_str(&text).expect("review --json is valid JSON");
        report["files"]
            .as_array()
            .expect("files array")
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().expect("path").to_owned(),
                    f["debt"].as_array().expect("debt array").len(),
                )
            })
            .collect()
    };

    // With no exclusions configured, both markers are in the review.
    let unfiltered = debt_by_path(&dir);
    assert_eq!(
        unfiltered.get("src/own.rs"),
        Some(&1),
        "our own marker is reviewed: {unfiltered:?}"
    );
    assert_eq!(
        unfiltered.get("vendor/dep/lib.rs"),
        Some(&1),
        "with no `[debt] ignore` the vendored marker is reviewed too: {unfiltered:?}"
    );

    // Exclude the vendored tree, as `roteiro debt` in this repository would.
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"vendor/**\"]\n");
    let filtered = debt_by_path(&dir);
    assert_eq!(
        filtered.get("vendor/dep/lib.rs"),
        Some(&0),
        "`review` must apply `[debt] ignore`: vendor/dep/lib.rs is excluded \
         everywhere else, so a marker reported here is a second debt figure for \
         one repository (issue #409): {filtered:?}"
    );
    assert_eq!(
        filtered.get("src/own.rs"),
        Some(&1),
        "the exclusion is scoped to the glob — src/own.rs keeps its marker: \
         {filtered:?}"
    );

    // The same list, on the same repository, through `roteiro debt`: the two
    // surfaces must agree about which markers exist, which is the property the
    // fix is for rather than `review` merely having *a* filter.
    let out = roteiro(&dir, &["debt", "--json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("debt --json is valid JSON");
    let paths: Vec<&str> = report["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert!(
        !paths.contains(&"vendor/dep/lib.rs"),
        "`roteiro debt` excludes the vendored tree: {paths:?}"
    );
    assert_eq!(
        paths.iter().filter(|p| **p == "src/own.rs").count(),
        1,
        "`roteiro debt` and `review` report the same marker set: {paths:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// `roteiro review --score` (Stage 35): scoring a candidate reviewer against the
// adjudicated corpus. Needs no graph, no model and no network — the corpus is
// embedded and the scoring is pure, which is what lets these numbers be
// recomputed anywhere, CI included.
// ---------------------------------------------------------------------------

/// Two real `reviewed_sha`s from the shipped corpus, and the rows anchored to
/// them. Written out rather than read from the fixture so that this test states
/// what it expects instead of agreeing with whatever the file happens to say.
const SHA_308: &str = "2b761ce79c44df5759ef69ef9e5f8476302d10cb";

/// Write a run document and score it, returning `(stdout, stderr, success)`.
fn score_run(name: &str, doc: &str, extra: &[&str]) -> (String, String, bool) {
    let dir = fresh_dir(name);
    let path = dir.join("run.json");
    std::fs::write(&path, doc).expect("write run");
    let mut args = vec!["review", "--score", path.to_str().expect("utf-8 path")];
    args.extend_from_slice(extra);
    let out = roteiro(&dir, &args);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A run document over `shas` with `findings` (each `(sha, path, line)`).
fn run_doc(shas: &[&str], findings: &[(&str, &str, u32)]) -> String {
    let shas: Vec<String> = shas.iter().map(|s| format!("{s:?}")).collect();
    let findings: Vec<String> = findings
        .iter()
        .map(|(sha, path, line)| {
            format!(
                "{{\"reviewed_sha\": {sha:?}, \"path\": {path:?}, \"line\": {line}, \
                 \"description\": \"a finding\"}}"
            )
        })
        .collect();
    format!(
        "{{\"schema\": \"roteiro.review-run/v1\", \"attempted_shas\": [{}], \
         \"findings\": [{}]}}",
        shas.join(", "),
        findings.join(", ")
    )
}

/// The embedded corpus scores a run end to end, and the report is **per class**.
/// A single averaged number is deliberately absent: which classes a reviewer can
/// see is the only thing an implementer can act on.
#[test]
fn score_reports_recall_per_defect_class() {
    // A row on #308: the ADR whose text contradicted the code it described.
    let doc = run_doc(
        &[SHA_308],
        &[(SHA_308, "docs/adr/0005-image-ocr-vision-ingestion.md", 16)],
    );
    let (out, err, ok) = score_run("score-per-class", &doc, &[]);
    assert!(ok, "scoring should succeed: {out}{err}");
    assert!(
        out.contains("recall by defect class"),
        "the table is the report: {out}"
    );
    assert!(
        out.contains("1/1   contract-drift") || out.contains("1/1  contract-drift"),
        "credits the contract-drift row: {out}"
    );
    // Only the attempted commit's rows are in scope, so this reads as a partial
    // run rather than as a poor one.
    assert!(
        out.contains("partial run"),
        "a one-commit run says it is partial: {out}"
    );
    assert!(
        !out.contains("overall recall") && !out.contains("average"),
        "no averaged headline: {out}"
    );
}

/// A finding matching no row is reported as **unadjudicated**, never as a false
/// positive — the corpus records what one reviewer said about these trees, not
/// every defect in them.
#[test]
fn an_unmatched_finding_is_reported_as_unadjudicated() {
    let doc = run_doc(
        &[SHA_308],
        &[(SHA_308, "crates/rto-graph/src/store.rs", 42)],
    );
    let (out, err, ok) = score_run("score-unadjudicated", &doc, &[]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("UNADJUDICATED"), "{out}");
    assert!(
        out.contains("not computable"),
        "no adjudicated finding means no precision figure, not 0%: {out}"
    );
}

/// **The most expensive mistake available here**, refused rather than scored: a
/// run against a merged PR head names a commit no row carries, and the error says
/// what the number would have meant.
#[test]
fn scoring_against_a_commit_outside_the_corpus_is_refused() {
    let head = "0123456789abcdef0123456789abcdef01234567";
    let (out, err, ok) = score_run("score-wrong-sha", &run_doc(&[head], &[]), &[]);
    assert!(!ok, "must not score: {out}");
    assert!(
        err.contains("reviewed_sha") && err.contains("silently reports zero"),
        "explains the PR-head trap: {err}"
    );
}

/// A document that is not a run says so, rather than failing as a missing field.
#[test]
fn a_document_with_the_wrong_schema_is_named_as_such() {
    let (_, err, ok) = score_run(
        "score-wrong-schema",
        "{\"schema\": \"roteiro.review/v1\", \"attempted_shas\": [], \"findings\": []}",
        &[],
    );
    assert!(!ok);
    assert!(
        err.contains("roteiro.review-run/v1"),
        "names the schema it scores: {err}"
    );
}

/// `--json` emits the versioned score document.
#[test]
fn score_json_carries_the_schema_tag() {
    let doc = run_doc(&[SHA_308], &[]);
    let (out, err, ok) = score_run("score-json", &doc, &["--json"]);
    assert!(ok, "{out}{err}");
    let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(value["schema"], "roteiro.review-score/v1");
    assert!(
        value["per_class"].as_array().is_some_and(|a| a.len() == 14),
        "every class is present, so two reports line up: {out}"
    );
}

/// `--score` and `--base` answer different questions and are refused together, so
/// a caller cannot believe it scored a branch review.
#[test]
fn score_and_base_are_mutually_exclusive() {
    let dir = fresh_dir("score-conflict");
    std::fs::write(dir.join("run.json"), run_doc(&[SHA_308], &[])).expect("write");
    let out = roteiro(&dir, &["review", "--score", "run.json", "--base", "main"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot be used with"),
        "clap refuses the pair: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
