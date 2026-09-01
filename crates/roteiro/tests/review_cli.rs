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

/// `git`, capturing stdout — for reading a sha back out of the fixture.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Give `main` an upstream at `refs/remotes/origin/main`, pointing at `at`.
///
/// A real remote-tracking ref and real `branch.main.*` settings, not a stub: the
/// resolver reads the fetch refspec to map `branch.main.merge` onto a tracking
/// ref, so a fixture that skipped either would exercise a path production never
/// takes and pass on an implementation that does nothing.
fn set_upstream(dir: &Path, at: &str) {
    git(dir, &["config", "remote.origin.url", "."]);
    git(
        dir,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(dir, &["config", "branch.main.remote", "origin"]);
    git(dir, &["config", "branch.main.merge", "refs/heads/main"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", at]);
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

/// A run document carrying one whole-change verdict (issue #649, part 2).
fn run_doc_with_verdict(sha: &str, stance: &str) -> String {
    format!(
        "{{\"schema\": \"roteiro.review-run/v1\", \"attempted_shas\": [{sha:?}], \
         \"findings\": [], \"verdicts\": [{{\"reviewed_sha\": {sha:?}, \
         \"stance\": {stance:?}, \"summary\": \"nothing to push back on\"}}]}}"
    )
}

/// **The verdict is scoreable through the shipped `--score` path** (issue #649,
/// part 2) — the whole reason it is expressible in `roteiro.review-run/v1` at
/// all. A summary nobody has scored is an opinion with a confident tone.
///
/// `SHA_308` carries an adjudicated **real** row, so a `clean` verdict over it is
/// contradicted by evidence. That is the one thing the corpus can say about a
/// whole-change judgement without a human, and it is the failure that matters:
/// a missed finding is silence, and this is a reader being told there is nothing
/// to look at.
#[test]
fn a_whole_change_verdict_is_scored_against_the_corpus() {
    let (out, err, ok) = score_run(
        "verdict-contradicted",
        &run_doc_with_verdict(SHA_308, "clean"),
        &[],
    );
    assert!(ok, "scoring should succeed: {out}{err}");
    assert!(
        out.contains("1 whole-change verdict(s)"),
        "the verdict is counted: {out}"
    );
    assert!(
        out.contains("1 declared a change CLEAN"),
        "and adjudicated against the corpus: {out}"
    );
    assert!(
        out.contains("a model's opinion, gating nothing"),
        "and labelled as an opinion where it is printed: {out}"
    );
    assert!(
        out.contains("DECLARED A CHANGE CLEAN"),
        "the caveat fires: {out}"
    );

    // `concerns` is matched against nothing: the corpus records what one reviewer
    // said about these trees, not every defect in them.
    let (out, err, ok) = score_run(
        "verdict-concerns",
        &run_doc_with_verdict(SHA_308, "concerns"),
        &[],
    );
    assert!(ok, "scoring should succeed: {out}{err}");
    assert!(
        out.contains("0 declared a change CLEAN"),
        "a `concerns` verdict is never contradicted: {out}"
    );
    assert!(
        out.contains("1 the corpus cannot judge"),
        "it is unadjudicated, which is a different thing: {out}"
    );

    // And a verdict changes no recall figure — it is not a finding.
    let (json, err, ok) = score_run(
        "verdict-json",
        &run_doc_with_verdict(SHA_308, "clean"),
        &["--json"],
    );
    assert!(ok, "scoring should succeed: {json}{err}");
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["verdicts"], 1);
    assert_eq!(doc["verdicts_contradicted"], 1);
    assert_eq!(
        doc["found"], 0,
        "a verdict is never counted as a defect the reviewer detected: {json}"
    );
    assert_eq!(
        doc["unadjudicated"], 0,
        "nor as an unadjudicated finding: {json}"
    );
}

/// A run document written before verdicts existed still scores, unchanged. The
/// field is additive within `roteiro.review-run/v1`.
#[test]
fn a_run_document_without_verdicts_scores_exactly_as_before() {
    let doc = run_doc(
        &[SHA_308],
        &[(SHA_308, "docs/adr/0005-image-ocr-vision-ingestion.md", 16)],
    );
    let (out, err, ok) = score_run("verdict-absent", &doc, &[]);
    assert!(ok, "scoring should succeed: {out}{err}");
    assert!(
        !out.contains("whole-change verdict"),
        "no verdicts means no verdict section, not a section of zeroes: {out}"
    );
    assert!(
        out.contains("recall by defect class"),
        "and everything else is untouched: {out}"
    );
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

/// `review` shows the change, not only what the graph knows about it (#649).
///
/// The complaint the diff answers is that a reviewer had to hold `git diff` in
/// another window and join it to this report by hand, and that an agent handed
/// the JSON received the context without the change it is context *for*. So the
/// assertions here are about the diff being present **by default** in both
/// surfaces — an opt-in would not have fixed either half.
#[test]
fn review_shows_the_diff_by_default_in_both_surfaces() {
    let dir = fresh_dir("diff");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() { helper(); }\nfn helper() {}\n",
    );

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "non-drift review exits 0: {text}");
    assert!(text.contains("@@"), "a hunk header is shown: {text}");
    assert!(
        text.contains("+fn helper() {}"),
        "the added line is shown: {text}"
    );
    // The graph context is not displaced by the diff — the point is both, in one
    // place. A change that showed the diff *instead* would have traded one
    // half-report for another.
    assert!(
        text.contains("calls: sym:rust:src/main.rs#helper"),
        "graph context survives alongside the diff: {text}"
    );

    let json = roteiro(&dir, &["review", "--json"]);
    let text = String::from_utf8_lossy(&json.stdout);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let diff = doc["files"][0]["diff"].as_str().expect("a diff field");
    assert!(
        diff.contains("+fn helper() {}"),
        "JSON carries the hunks: {diff}"
    );

    // `--no-diff` is the escape hatch, and must leave the rest untouched.
    let plain = roteiro(&dir, &["review", "--no-diff"]);
    let text = String::from_utf8_lossy(&plain.stdout);
    assert!(!text.contains("@@"), "--no-diff omits the hunks: {text}");
    assert!(
        text.contains("calls: sym:rust:src/main.rs#helper"),
        "--no-diff keeps the graph context: {text}"
    );
    let plain_json = roteiro(&dir, &["review", "--no-diff", "--json"]);
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&plain_json.stdout)).expect("valid JSON");
    assert!(
        doc["files"][0].get("diff").is_none(),
        "--no-diff omits the field entirely rather than sending null"
    );
}

/// A brand-new file shows its contents (#649).
///
/// This is the case that fails **silently**. An untracked path is not part of
/// the comparison `git diff` makes, so it reports success and no text — which
/// is indistinguishable from a file that did not change. The review would list
/// the file, list its symbols, and omit the only thing new about it, while
/// looking entirely healthy.
#[test]
fn review_shows_the_contents_of_a_brand_new_file() {
    let dir = fresh_dir("diff-added");
    write(&dir, "src/main.rs", "fn main() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Never `git add`ed: untracked, which is how a new file exists for most of
    // the time anyone would want to review it.
    write(&dir, "src/added.rs", "fn brand_new() {}\n");

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("src/added.rs [added]"),
        "the new file is listed: {text}"
    );
    assert!(
        text.contains("+fn brand_new() {}"),
        "and its contents are shown, not just its name: {text}"
    );

    // Staging it must not change the answer: the same file, the same review.
    git(&dir, &["add", "src/added.rs"]);
    let staged = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&staged.stdout);
    assert!(
        text.contains("+fn brand_new() {}"),
        "a staged addition shows its contents too: {text}"
    );
}

/// A deletion shows what was removed (#649).
///
/// The graph cannot answer this one at all — the nodes are gone, so there is no
/// context to print and the file would otherwise appear as a bare path with a
/// `[deleted]` tag. The removed code is the only evidence of what the change did.
#[test]
fn review_shows_what_a_deletion_removed() {
    let dir = fresh_dir("diff-deleted");
    write(&dir, "src/main.rs", "fn main() {}\n");
    write(&dir, "src/gone.rs", "fn doomed() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    std::fs::remove_file(dir.join("src/gone.rs")).expect("remove");

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("src/gone.rs [deleted]"),
        "the deletion is listed: {text}"
    );
    assert!(
        text.contains("-fn doomed() {}"),
        "and the removed code is shown: {text}"
    );
}

/// A file git calls untracked while `HEAD` still has it is not an addition.
///
/// `untracked_files` classifies against the **index**, so the two can be true at
/// once: `git rm --cached f` drops `f` from the index and leaves it on disk, and
/// git then reports it untracked while `HEAD` still carries it. Taking that set
/// wholesale labels a tracked file `[added]`.
///
/// The second-order effect is worse than the label. If the same file is also
/// edited, it arrives twice — `Modified` from `changed_files`, `Added` from the
/// untracked walk — and the `dedup_by(path)` that follows keeps whichever the
/// sort happened to place first, which is not a decision anyone made.
///
/// Raised by Copilot on PR #656.
#[test]
fn a_file_still_in_head_is_never_reported_as_added() {
    let dir = fresh_dir("rm-cached");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Out of the index, still on disk, still in HEAD, and **unedited** — git now
    // calls it untracked, and it is not.
    //
    // Unedited is what makes this discriminating. With an edit as well,
    // `changed_files` also emits the path, lands first, and the `dedup_by` keeps
    // the correct `modified` entry by luck of insertion order — so the buggy and
    // fixed versions agree and the test proves nothing. That version of this test
    // passed under fault injection, which is how the vacuity was found. Here the
    // content is identical to HEAD, so `changed_files` says nothing and the
    // untracked walk is the only voice: filtered, the file is absent from the
    // review entirely; unfiltered, it is reported as an addition of a file that
    // did not change.
    git(&dir, &["rm", "--cached", "-q", "src/main.rs"]);

    let unedited = roteiro(&dir, &["review", "--json"]);
    let text = String::from_utf8_lossy(&unedited.stdout);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert!(
        doc["files"]
            .as_array()
            .expect("files")
            .iter()
            .all(|f| f["path"] != "src/main.rs"),
        "an unchanged file HEAD still has must not be reported at all: {text}"
    );

    // Now edit it too: the path must arrive exactly once, and as a modification.
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() { helper(); }\nfn helper() {}\n",
    );

    let out = roteiro(&dir, &["review", "--json"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let files = doc["files"].as_array().expect("files");

    let entries: Vec<&serde_json::Value> = files
        .iter()
        .filter(|f| f["path"] == "src/main.rs")
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "the path must appear exactly once, not once per source: {text}"
    );
    assert_eq!(
        entries[0]["status"], "modified",
        "a file HEAD still has is modified, never added: {text}"
    );
}

/// **The stale-base defect, contained** (issue #649).
///
/// `--base main` binds to the *local* branch, so a `main` its upstream has moved
/// past measures a different question from the one asked — and answers it in
/// output textually identical to a correct run. The fixture therefore has to hold
/// the difference itself: `refs/heads/main` genuinely behind
/// `refs/remotes/origin/main`, with a real fetch refspec, because a same-commit
/// fixture would pass against an implementation that resolved nothing.
///
/// The three things asserted are the three the report could not previously say:
/// which ref the spec bound to, which commit that was, and that the upstream has
/// moved on.
#[test]
fn a_base_behind_its_upstream_is_reported_and_warned_about() {
    let dir = fresh_dir("stale-base");
    write(&dir, "src/main.rs", "fn main() { a(); }\nfn a() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);

    // Two commits the upstream has and the local ref does not — built on a scratch
    // branch and then pointed at by `refs/remotes/origin/main`, which is exactly
    // the state a fetch leaves behind when nobody fast-forwards `main`.
    git(&dir, &["checkout", "-q", "-b", "upstream-sim"]);
    write(&dir, "src/upstream_one.rs", "pub fn one() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "upstream one"]);
    write(&dir, "src/upstream_two.rs", "pub fn two() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "upstream two"]);
    let upstream_sha = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "-q", "main"]);
    set_upstream(&dir, &upstream_sha);

    // The change actually under review: one file, on a branch based on the
    // **upstream** — which is what rebasing onto `origin/main` leaves you with,
    // and the reason rebasing does not save you here. The branch has caught up;
    // the local `main` ref has not moved.
    git(&dir, &["checkout", "-q", "-b", "feature", &upstream_sha]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); b(); }\nfn a() {}\nfn b() {}\n",
    );
    git(&dir, &["commit", "-q", "-am", "add b"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "sync");

    let out = roteiro(&dir, &["review", "--base", "main", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    // 1. The report says which ref the spec bound to, and which commit.
    let base = &doc["base"];
    assert_eq!(base["spec"], "main", "the spec as typed is kept: {stdout}");
    assert_eq!(
        base["ref"], "refs/heads/main",
        "the LOCAL branch is what `main` binds to, and the report now says so: {stdout}"
    );
    let local_main = git_out(&dir, &["rev-parse", "main"]);
    assert_eq!(
        base["commit"].as_str(),
        Some(local_main.as_str()),
        "the reported commit is the one that was diffed: {stdout}"
    );

    // 2. It says the upstream has moved on, and by how much, in both directions.
    let up = &base["upstream"];
    assert_eq!(
        up["ref"], "refs/remotes/origin/main",
        "the upstream is named: {stdout}"
    );
    assert_eq!(up["behind"], 2, "two commits behind: {stdout}");
    assert_eq!(up["ahead"], 0, "and none ahead of it: {stdout}");
    assert_eq!(
        up["commit"].as_str(),
        Some(upstream_sha.as_str()),
        "the upstream commit is named too, so both sides can be checked: {stdout}"
    );

    // 3. The warning goes to stderr, so `--json` on stdout stays parseable — which
    //    the `from_str` above has already proved.
    assert!(
        stderr.contains("behind") && stderr.contains("refs/remotes/origin/main"),
        "stderr warns and names the upstream: {stderr}"
    );
    assert!(
        !stdout.contains("warning:"),
        "nothing about the warning reaches stdout: {stdout}"
    );

    // The defect itself: a stale base reports a SUPERSET. Asserted rather than
    // described, because it is the reason nothing ever failed — the review reads
    // as more thorough, not less.
    let files: Vec<&str> = doc["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(
        files.contains(&"src/upstream_one.rs"),
        "a stale base pulls in files this change never touched: {files:?}"
    );

    // And spelling it `origin/main` — the workaround, and the correct question —
    // gives the real footprint and reports no upstream to be stale against.
    let good = roteiro(&dir, &["review", "--base", "origin/main", "--json"]);
    let text = String::from_utf8_lossy(&good.stdout);
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(
        doc["base"]["ref"], "refs/remotes/origin/main",
        "`origin/main` binds to the tracking ref: {text}"
    );
    assert!(
        doc["base"].get("upstream").is_none(),
        "a tracking ref has no upstream of its own: {text}"
    );
    let files: Vec<&str> = doc["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert_eq!(
        files,
        vec!["src/main.rs"],
        "the real footprint is one file: {files:?}"
    );
    assert!(
        !String::from_utf8_lossy(&good.stderr).contains("behind"),
        "and there is nothing to warn about"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The text surface names its base too, in the summary line that gets quoted and
/// in the "nothing to review" sentence that reads as *you are done* (issue #649).
#[test]
fn the_text_review_names_the_base_it_resolved() {
    let dir = fresh_dir("base-line");
    write(&dir, "src/main.rs", "fn main() { a(); }\nfn a() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); b(); }\nfn a() {}\nfn b() {}\n",
    );
    git(&dir, &["commit", "-q", "-am", "add b"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "sync");
    let main_sha = git_out(&dir, &["rev-parse", "main"]);

    let out = roteiro(&dir, &["review", "--base", "main"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "no drift: {text}");
    assert!(
        text.contains(&format!(
            "base: main -> refs/heads/main @ {}",
            &main_sha[..12]
        )),
        "the header names the spec, the ref and the commit: {text}"
    );
    assert!(
        text.contains(&format!("against main @ {}", &main_sha[..12])),
        "and the quotable summary line carries its own basis: {text}"
    );

    // The empty case is the one most likely to be read as "you are done", so it is
    // the one that most needs to say what it compared against.
    let empty = roteiro(&dir, &["review", "--base", "feature"]);
    let text = String::from_utf8_lossy(&empty.stdout);
    assert!(
        text.contains("base: feature -> refs/heads/feature @"),
        "an empty range still reports its base: {text}"
    );
    assert!(
        text.contains("no changes in feature..HEAD to review"),
        "and still says there was nothing: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Divergence is warned about **differently**, because its consequence is
/// different: a behind-only base over-reports, while a diverged one can leave the
/// change out of the diff entirely (issue #649).
///
/// It is still a warning. `review`'s exit status means authored-layer drift and
/// nothing else; a second meaning folded into the same non-zero would make a hook
/// that reads it wrong in the other direction.
#[test]
fn a_diverged_base_is_warned_about_separately_and_still_exits_zero() {
    let dir = fresh_dir("diverged");
    write(&dir, "src/main.rs", "fn main() { a(); }\nfn a() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);

    // The upstream gains a commit the local ref will never see...
    git(&dir, &["checkout", "-q", "-b", "upstream-sim"]);
    write(&dir, "src/upstream_only.rs", "pub fn one() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "upstream only"]);
    let upstream_sha = git_out(&dir, &["rev-parse", "HEAD"]);

    // ...and the local `main` gains one the upstream has not. Now they have forked.
    git(&dir, &["checkout", "-q", "main"]);
    write(&dir, "src/local_only.rs", "pub fn two() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "local only"]);
    set_upstream(&dir, &upstream_sha);

    git(&dir, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); b(); }\nfn a() {}\nfn b() {}\n",
    );
    git(&dir, &["commit", "-q", "-am", "add b"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "sync");

    let out = roteiro(&dir, &["review", "--base", "main", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a diverged base warns, it does not gate: {stderr}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let up = &doc["base"]["upstream"];
    assert_eq!(up["behind"], 1, "one commit behind: {stdout}");
    assert_eq!(up["ahead"], 1, "and one ahead — forked: {stdout}");
    assert!(
        stderr.contains("DIVERGED"),
        "the diverged case says so in its own words: {stderr}"
    );
    assert!(
        stderr.contains("OUT of the diff"),
        "and names the consequence that makes it worse than staleness: {stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A base with no upstream configured — the ordinary case, and every case before
/// this change — resolves, reports its commit, and warns about nothing.
///
/// Guards the direction that would be worst to get wrong: a review that started
/// printing a staleness warning where there is no upstream to be stale against
/// would train people to ignore it.
#[test]
fn a_base_with_no_upstream_reports_its_commit_and_warns_about_nothing() {
    let dir = fresh_dir("no-upstream");
    write(&dir, "src/main.rs", "fn main() { a(); }\nfn a() {}\n");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let base_sha = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); b(); }\nfn a() {}\nfn b() {}\n",
    );
    git(&dir, &["commit", "-q", "-am", "add b"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "sync");

    let out = roteiro(&dir, &["review", "--base", "main", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(doc["base"]["ref"], "refs/heads/main");
    assert!(
        doc["base"].get("upstream").is_none(),
        "an unpushed branch has no upstream, and that is not an error: {stdout}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("behind"),
        "nothing to warn about: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A raw sha names no ref at all, which the report says rather than inventing
    // one.
    let out = roteiro(&dir, &["review", "--base", &base_sha, "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        doc["base"].get("ref").is_none(),
        "a sha binds to no ref: {stdout}"
    );
    assert_eq!(
        doc["base"]["commit"].as_str(),
        Some(base_sha.as_str()),
        "and still reports the commit it compared against: {stdout}"
    );

    // A working-tree review has no spec to have got wrong, so it carries no base
    // at all rather than a fabricated one.
    write(
        &dir,
        "src/main.rs",
        "fn main() { a(); }\nfn a() {}\nfn c() {}\n",
    );
    let out = roteiro(&dir, &["review", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        doc.get("base").is_none(),
        "a working-tree review compares against HEAD and says nothing about a base: {stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
