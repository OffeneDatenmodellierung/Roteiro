//! `roteiro sync` must **say** when extraction failed on documents in the graph
//! (#907), and the graph must carry the per-document reason.
//!
//! # Why this exists separately from the library tests
//!
//! `rto-graph`'s `tests/pdf_extraction.rs` asserts `SyncReport` and node `meta`.
//! Neither reaches the CLI, so deleting the `report_content_failures` call — or
//! breaking the stderr emission — left the whole suite green while the *surfacing*
//! half of #907's acceptance criteria regressed. A count nobody is shown is the
//! same silence the issue is about, one layer up, and it needed its own guard.
//!
//! # Why the fixture is fifteen bytes
//!
//! `%PDF-1.4\ngarbage` is refused by the parser, which is the `unreadable`
//! outcome — an attempted-and-failed extraction, so it is counted exactly as a
//! crashed one is. Reproducing the *panic* needs a hand-built content stream and
//! is done where the distinction matters, in
//! `rto-graph/tests/pdf_extraction.rs`. Here the question is only whether the
//! count reaches the terminal, so the cheapest input that produces one is the
//! honest choice, and it keeps a fourth copy of a PDF builder out of the tree.
#![cfg(feature = "pdf-text")]

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
        // Isolate from any real user config: `[ingest] pdf` is a live ADR-0007
        // key, so a developer's own `~/.roteiro/config.toml` would otherwise be an
        // input to this test and could turn PDF ingestion off under it.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-pdfcli-{}-{}", std::process::id(), name));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// **A sync that failed to extract N documents says N, on stderr, in prose.**
///
/// Asserted on the *text*, not only on a JSON field, because the human-readable
/// path is the one a person running `roteiro sync` actually sees, and it is the
/// one that had no coverage at all.
#[test]
fn sync_warns_on_stderr_about_documents_it_could_not_extract() {
    let dir = fresh_dir("warn");
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("paper.pdf"), b"%PDF-1.4\ngarbage").expect("write pdf");
    std::fs::write(dir.join("notes.md"), "# Notes\n\nbody\n").expect("write md");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "corpus"]);

    let out = roteiro(&dir, &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("PDF text extraction failed on 1 document"),
        "the count must reach stderr in prose; got {stderr:?}",
    );
    assert!(
        stderr.contains("meta.extract"),
        "the warning must name where the per-document reason is; got {stderr:?}",
    );

    // The reason is in the graph, reachable by an ordinary read — the warning is
    // a pointer, so the thing it points at has to be there.
    let explain = roteiro(&dir, &["query", "file:paper.pdf", "--json"]);
    assert!(
        explain.status.success(),
        "query explain failed: {explain:?}"
    );
    let body = String::from_utf8_lossy(&explain.stdout);
    assert!(
        body.contains("unreadable"),
        "the file node must carry its outcome; got {body}",
    );

    // …and the machine-readable sync surface carries the count too, so a caller
    // that parses `--json` is not told less than a caller reading the terminal.
    let json = roteiro(&dir, &["sync", "--json"]);
    assert!(json.status.success(), "sync --json failed: {json:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("sync --json emits JSON");
    assert_eq!(
        report["blobs_content_failed"], 1,
        "the JSON report must carry the count; got {report}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **A clean repository says nothing.** The warning has to stay rare to stay
/// read, so its absence is as much a guarantee as its presence — and a warning
/// printed unconditionally would pass the test above while being useless.
#[test]
fn sync_is_silent_when_every_document_extracted() {
    let dir = fresh_dir("quiet");
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("notes.md"), "# Notes\n\nbody\n").expect("write md");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "prose only"]);

    let out = roteiro(&dir, &["sync"]);
    assert!(out.status.success(), "sync failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("PDF text extraction failed"),
        "nothing failed, so nothing may be warned about; got {stderr:?}",
    );

    std::fs::remove_dir_all(&dir).ok();
}
