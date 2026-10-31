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

mod common;

use std::io::Write as _;
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

/// The committed clean-control fixture, read by path rather than rebuilt.
///
/// Cross-crate, and deliberately: the module note above refuses a fourth copy of
/// a PDF builder, and this needs only "a PDF whose text extracts". Borrowing the
/// artefact costs one path and keeps `rto-graph/tests/pdf_fixtures.rs` the
/// workspace's only PDF writer. If it is ever renamed this fails loudly with the
/// path, which is the outcome worth having.
///
/// `None` only in a packaged crate, which carries no sibling crates: there the
/// skip is said on stderr. In a checkout a missing fixture still panics.
fn clean_pdf() -> Option<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rto-graph/tests/fixtures/pdf/01-clean-control.pdf");
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !common::is_repository_checkout() => {
            // libtest discards a passing test's captured output; write past it.
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED: {} is absent and this is a packaged crate, not a checkout",
                path.display()
            );
            None
        }
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

/// **What a PDF `pass` does not cover reaches `--json`, not only the terminal.**
///
/// #813 requires the screening limits to be declared "in the ingest command's
/// own output at the point a document is accepted". `--json` *is* that output
/// for every caller that is not a person at a terminal, and it carried nothing:
/// a machine consumer reading a clean sync had no way to learn that
/// concealment-under-an-opaque-image was never checked. That is precisely the
/// "a `pass` on a PDF does not mean what a `pass` on markdown means" problem the
/// issue exists to close, surviving in the half of the surface nobody looked at.
///
/// Caught in review on PR #912, and it is the same defect this file was created
/// for one field over — which is why the guard lands here rather than in the
/// library: `SyncReport` having the field proves nothing about the command
/// emitting it.
#[test]
fn sync_json_carries_the_pdf_screening_declaration() {
    let dir = fresh_dir("screened-json");
    git(&dir, &["init", "-q"]);
    let Some(pdf) = clean_pdf() else { return };
    std::fs::write(dir.join("paper.pdf"), pdf).expect("write pdf");
    std::fs::write(dir.join("notes.md"), "# Notes\n\nbody\n").expect("write md");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "corpus"]);

    let json = roteiro(&dir, &["sync", "--json"]);
    assert!(json.status.success(), "sync --json failed: {json:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("sync --json emits JSON");
    assert_eq!(
        report["blobs_pdf_screened"], 1,
        "--json must say a PDF contributed screened text; got {report}",
    );
    // A count alone does not say what went unchecked (#813): the declaration
    // itself must be serialised, with the mechanisms and where they are written.
    let declared = &report["pdf_screening"];
    assert_eq!(declared["limits"], "docs/SCREENING.md", "got {report}");
    let not_covered = declared["not_covered"]
        .as_array()
        .unwrap_or_else(|| panic!("pdf_screening.not_covered must be a list; got {report}"));
    assert!(
        not_covered
            .iter()
            .any(|m| m.as_str().is_some_and(|m| m.contains("opaque image"))),
        "the opaque-image limit must be declared; got {report}",
    );

    // The human surface still says it too, in prose — the two halves are one
    // requirement, and a fix to either must not become a regression to the
    // other.
    let plain = roteiro(&fresh_synced(&dir), &["sync"]);
    let stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        stderr.contains("docs/SCREENING.md"),
        "the terminal must point at the limits; got {stderr:?}",
    );
}

/// A second checkout of the same corpus, so the prose assertion above runs
/// against a sync that actually did work rather than a cached no-op.
fn fresh_synced(source: &Path) -> PathBuf {
    let dir = fresh_dir("screened-text");
    git(&dir, &["init", "-q"]);
    std::fs::copy(source.join("paper.pdf"), dir.join("paper.pdf")).expect("copy pdf");
    std::fs::write(dir.join("notes.md"), "# Notes\n\nbody\n").expect("write md");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "corpus"]);
    dir
}
