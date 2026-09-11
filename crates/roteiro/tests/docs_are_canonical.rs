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

/// Every `.md` under `docs/`, symlinks not followed.
fn docs() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let rd = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("cannot read {} while scanning docs: {e}", dir.display()));
        for entry in rd {
            let entry =
                entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));
            if entry
                .file_type()
                .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()))
                .is_symlink()
            {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "md") {
                out.push(p);
            }
        }
    }
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
