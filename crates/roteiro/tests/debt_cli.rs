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
        stderr.contains("matches nothing and changes no result"),
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
