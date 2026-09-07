// roteiro:ignore-file — the fixtures below deliberately embed `TODO`/`FIXME` to
// exercise the detector; they are test data, not real debt in this repo.
//! End-to-end test for `roteiro debt` with the `[debt] ignore` config (ADR-0007):
//! markers under an ignored path (e.g. a vendored tree) are excluded from the
//! report — both the totals and the item list — while others are kept.

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
        // Isolate from any real user config.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir() -> PathBuf {
    fresh_dir_tagged("main")
}

/// A fresh directory keyed by `tag` as well as the process, so two tests in this
/// file do not clear each other's fixture — every caller begins by removing it.
fn fresh_dir_tagged(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-debt-cli-{}-{tag}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn debt_ignore_excludes_vendored_markers() {
    let dir = fresh_dir();
    git(&dir, &["init", "-q"]);
    // One marker in our own source, one in a vendored tree.
    write(
        &dir,
        "src/lib.rs",
        "// TODO: wire this up\npub struct Thing;\n",
    );
    write(
        &dir,
        "vendor/dep/lib.rs",
        "// FIXME: upstream bug\npub struct Dep;\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Without any ignore config, both markers are reported.
    let all = roteiro(&dir, &["debt", "--json"]);
    assert!(all.status.success(), "debt failed: {all:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&all.stdout).expect("debt --json is valid JSON");
    assert_eq!(report["total"], 2, "both markers reported: {report}");

    // Ignore the vendored tree: only our own marker remains, and the totals and
    // per-category counts reflect the exclusion.
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"vendor/**\"]\n");
    let filtered = roteiro(&dir, &["debt", "--json"]);
    assert!(filtered.status.success(), "debt failed: {filtered:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&filtered.stdout).expect("debt --json is valid JSON");
    assert_eq!(report["total"], 1, "vendored marker excluded: {report}");
    assert_eq!(report["by_category"]["fixme"], serde_json::Value::Null);
    let items = report["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["path"], "src/lib.rs");

    std::fs::remove_dir_all(&dir).ok();
}

/// **A pattern the matcher cannot honour is reported by the command that applies
/// it** (issue #754).
///
/// The listing itself is silent about this by construction: an inert pattern
/// changes no result, so `roteiro debt` prints exactly what it would have printed
/// with the pattern absent. That is the whole defect — the over-ignoring
/// direction (`docs/**` alongside an inert `!docs/keep.md`) hides a file's debt
/// behind a config that appears to account for it, and **nothing in the numbers
/// distinguishes that from the file having no debt**.
///
/// Asserted on `roteiro debt` rather than `roteiro config`, because a pattern is
/// written once and read never again: a report only that command prints is one
/// nobody sees at the moment the number is wrong.
#[test]
fn an_inert_ignore_pattern_is_warned_about_where_it_is_applied() {
    let dir = fresh_dir_tagged("inert");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "src/lib.rs",
        "// TODO: wire this up\npub struct Thing;\n",
    );
    write(&dir, "docs/keep.md", "<!-- TODO: still outstanding -->\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // The reporter's own shape: exclude a tree, then try to re-include one file.
    write(
        &dir,
        "roteiro.toml",
        "[debt]\nignore = [\"docs/**\", \"!docs/keep.md\"]\n",
    );
    let out = roteiro(&dir, &["debt", "--json"]);
    assert!(
        out.status.success(),
        "an inert pattern must not fail the command: {out:?}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("!docs/keep.md") && stderr.contains("negation"),
        "the dead pattern is named, by construct: {stderr}"
    );
    assert!(
        stderr.contains("every other character is matched literally"),
        "and says the consequence, which is the half a reader acts on: {stderr}"
    );
    assert!(
        !stderr.contains("\"docs/**\":"),
        "the supported pattern beside it is not warned about: {stderr}"
    );

    // stdout stays parseable: a warning must never land in what a caller reads.
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("debt --json is valid JSON");
    assert_eq!(
        report["total"], 1,
        "and the negation really is inert — `docs/keep.md` is still excluded: {report}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **An unsupported construct is matched literally, not ignored** — which is why
/// the warning says "matched literally" and not "matches nothing".
///
/// The first draft of that message claimed the pattern changed no result. It is
/// false, and this is the input that shows it: a directory really named
/// `[v]endor` is excluded by `[v]endor/**`, because the class is never
/// interpreted and the characters are compared as themselves.
///
/// Pinned because the wrong claim is the tempting one — it is what the author of
/// such a pattern assumes, and it reads as a stronger warning. A warning that
/// overstates is one a reader catches out once and then stops believing.
#[test]
fn an_unsupported_construct_is_matched_literally() {
    let dir = fresh_dir_tagged("literal");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "[v]endor/a.rs",
        "// TODO: literally-named tree\npub struct A;\n",
    );
    write(&dir, "src/b.rs", "// TODO: ordinary\npub struct B;\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let total = |out: &std::process::Output| -> u64 {
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("debt --json is valid JSON")
            ["total"]
            .as_u64()
            .expect("total")
    };
    assert_eq!(total(&roteiro(&dir, &["debt", "--json"])), 2, "control");

    write(&dir, "roteiro.toml", "[debt]\nignore = [\"[v]endor/**\"]\n");
    let out = roteiro(&dir, &["debt", "--json"]);
    assert_eq!(
        total(&out),
        1,
        "the class is not interpreted, so the pattern matched the directory that \
         literally bears that name — the count moves, and any warning claiming \
         otherwise is false"
    );
    // Still reported: the pattern cannot express what its author meant, whether or
    // not some path happens to satisfy it literally.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("character classes are not interpreted"),
        "a pattern that happens to match is still not doing what was meant"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **`--json` selects a format and does not silence the warning.**
///
/// It did: the call sat inside `run_check`'s human arm, so `check --json` was the
/// one command a dead pattern could not reach — and a CI job is exactly where
/// `--json` is used and exactly where nobody re-reads the config. Same drift as
/// the `okf trust --check` gate that once lived past a `--json` early return; the
/// rule is this repository's own, and it is worth a test rather than a comment.
///
/// Asserted on `check` though this file is about `debt`, because the fixture
/// helpers are here and the defect is one line of `run_check`; splitting it into
/// `check_cli.rs` would separate the assertion from the `[debt] ignore` fixture
/// that produces it.
#[test]
fn check_json_still_warns_about_a_dead_pattern_and_keeps_stdout_parseable() {
    let dir = fresh_dir_tagged("check-json");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "src/lib.rs",
        "// TODO: wire this up\npub struct Thing;\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"!src/lib.rs\"]\n");

    let out = roteiro(&dir, &["check", "--json"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("negation (a leading `!`)"),
        "the warning must survive `--json`: {out:?}"
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("stdout stays a single JSON document — the warning goes to stderr");

    std::fs::remove_dir_all(&dir).ok();
}
