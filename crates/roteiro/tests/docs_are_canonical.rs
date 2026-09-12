//! Every authored document under `docs/` is in the canonical form
//! `roteiro docs fmt` writes.
//!
//! # Why a test rather than a CI step
//!
//! `roteiro docs fmt` without `--write` already exits non-zero on a document
//! that is not canonical, so a CI step would work. This is a test because the
//! thing most likely to break it is a change to `rto_spec::fmt` itself — a new
//! rule, or a fix to an old one — and a test fails in the same run that
//! introduced it rather than a job later.
//!
//! It also pins **idempotence** over the real tree, which is the property a
//! reordering pass is most likely to lose and which no fixture can properly
//! test: the documents contain shapes nobody thought to write down, and one of
//! them (`` `<|im_start|>` `` in an ADR-0006 history row) already caught a
//! defect that every hand-written fixture missed.

use std::path::{Path, PathBuf};

/// Every `.md` under `dir`, recursively; **regular files only**, symlinks not
/// followed.
///
/// The same three-way `file_type` test the production walker in
/// `roteiro::markdown_files` uses, and for the same reason: "not a directory"
/// also admits a FIFO, a socket and a device node, and the callers below hand
/// every queued path to `read_to_string`, which blocks forever on a FIFO. The
/// production walker was given this treatment on #790 and this one was not, so
/// an untracked `docs/x.md` named pipe hung the test suite rather than the
/// command. Raised on #790 round eighteen.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {} while scanning docs: {e}", dir.display()));
    for entry in rd {
        let entry =
            entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()));
        if kind.is_symlink() {
            continue;
        }
        let p = entry.path();
        if kind.is_dir() {
            walk(&p, out);
        } else if kind.is_file() && p.extension().is_some_and(|e| e == "md") {
            out.push(p);
        }
    }
}

/// Every `.md` under `docs/`, symlinks not followed.
fn docs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("docs");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out.sort();
    assert!(
        out.len() > 20,
        "found only {} documents under docs/ — the scan is broken, not the tree",
        out.len()
    );
    out
}

/// Every document is already canonical.
#[test]
fn every_authored_document_is_canonical() {
    let mut drifted = Vec::new();
    for f in docs() {
        let before = std::fs::read_to_string(&f).expect("read");
        if rto_spec::fmt::canonical(&before) != before {
            drifted.push(f.display().to_string());
        }
    }
    assert!(
        drifted.is_empty(),
        "these documents are not canonical — run `roteiro docs fmt --write`:\n  {}",
        drifted.join("\n  ")
    );
}

/// Canonicalising a canonical document changes nothing, on the real tree.
#[test]
fn the_canonical_form_is_a_fixed_point_over_docs() {
    for f in docs() {
        let before = std::fs::read_to_string(&f).expect("read");
        let once = rto_spec::fmt::canonical(&before);
        assert_eq!(
            rto_spec::fmt::canonical(&once),
            once,
            "not idempotent: {}",
            f.display()
        );
    }
}

#[cfg(unix)]
mod eighteenth_round {
    use super::walk;

    /// A named pipe called `x.md` is not queued as a document.
    ///
    /// The walk admitted anything that was not a directory, so a FIFO — or a
    /// socket, or a device node — was queued alongside the real documents and
    /// reached the `read_to_string` in every test below, which blocks forever
    /// on one. The production walker was fixed on #790; this one, which is a
    /// second copy of the same walk, was left behind, so an untracked
    /// `docs/*.md` named pipe hung the whole test binary instead. Raised on
    /// #790 round eighteen.
    ///
    /// Asserted on what the walk *queues* rather than by running it over a real
    /// FIFO to completion: a regression here must fail, and a test that proves
    /// the point by hanging never reports at all.
    #[test]
    fn a_fifo_is_not_queued_as_a_document() {
        let root = std::env::temp_dir().join(format!("r18-fifo-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("REAL.md"), "# real\n").expect("write");
        std::fs::write(root.join("sub/ALSO.md"), "# also\n").expect("write");
        let fifo = root.join("PIPE.md");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo failed");

        let mut out = Vec::new();
        walk(&root, &mut out);
        out.sort();

        assert!(
            !out.contains(&fifo),
            "a FIFO was queued as a document — read_to_string on it would block forever: {out:?}"
        );
        // And the real documents are still found, so the fix is not a walk that
        // queues nothing.
        assert_eq!(
            out,
            vec![root.join("REAL.md"), root.join("sub/ALSO.md")],
            "the regular documents either side of the FIFO were lost"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
