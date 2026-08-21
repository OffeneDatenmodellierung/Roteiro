//! Guard: the pre-#574 note-name spelling must not come back.
//!
//! [`rto_render::note_name`] builds a vault note's filename as a lowercased,
//! readable hint plus a hash of the whole key. Before issue #574 a workspace
//! member's note was named for its project and key joined by a hyphen — the
//! spelling this file searches for, assembled in [`dead_spelling`] — and
//! **three separate hand-written copies of it** were left behind when #574
//! changed the rule: the `--workspace-name` help, the `_Home.md` a workspace
//! render writes, and `website/pages/modes.md`. All three shipped in v2.0.0 and
//! were published, stating a naming rule the binary had already stopped
//! following.
//!
//! # Why a guard and not just a sweep
//!
//! Because this was the third round. #570 drew the key/name distinction and got
//! two review comments for describing it inconsistently; #574 changed the names
//! and got three more for descriptions it had left stale — one of them in a file
//! it edited, six lines from the line it added. Each round ended by rewriting the
//! copies, which resets the clock rather than stopping it. Nothing in the
//! repository could tell that a description of a filename disagreed with the
//! function that builds one, so the only thing standing between the code and the
//! prose was a reviewer reading both.
//!
//! The copy that could stop being hand-written did:
//! `render_workspace_home` now renders its example *through* `note_name`, so that
//! one cannot drift again (asserted by
//! `the_workspace_home_names_an_example_note_name_actually_produces`). The other
//! two cannot. A clap `///` is a compile-time literal and a website page is
//! markdown; neither can call a function, and neither is worth a build script to
//! make it able to. So they stay prose, and this guard covers the failure mode
//! prose actually had here: **the dead spelling was copy-pasted, not reinvented.**
//!
//! # What this does and does not catch
//!
//! It catches the exact string, which is now false wherever it appears: there is
//! no correct use of it. That is the whole claim. It does **not** understand
//! prose, so a *newly* wrong description — some future spelling that is also not
//! what `note_name` produces — passes this guard untouched. A cheap check against
//! the defect that actually recurred is worth more than an expensive one against
//! the defect that might; if a novel wrong spelling ever does appear, the honest
//! response is another literal here, not a prose parser.
//!
//! Not feature-gated: it reads files and links against nothing, so it runs on the
//! default feature set as well as under CI's `--all-features` job.

use std::path::{Path, PathBuf};

/// The dead spelling, assembled at run time.
///
/// Written in pieces so that **this file is not itself a hit**. The alternative
/// — spelling it out and skipping this path — would put a permanent hole in the
/// scan at exactly the file most likely to be copied from.
fn dead_spelling() -> String {
    format!("<{}>-<{}>", "project", "key")
}

/// Paths whose *whole point* is to record what used to be true.
///
/// Only changelogs. They are generated from commit subjects and are history by
/// construction, so a hit in one is a record rather than a claim. Everything else
/// — ADRs included — is checked: an ADR's version history is still published
/// prose, and one that genuinely needs to quote the dead form should say so and
/// extend this list in the same change, which is a review moment rather than a
/// silent pass.
fn is_historical_record(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "CHANGELOG.md")
}

/// The tree to scan: the repository root when this is a source checkout, else
/// this crate's own directory. As in `doc_anchor_fragments`, there is no "skip
/// when the tree is missing" branch — an empty scan is a green test that checked
/// nothing, and the assertion below refuses one.
fn scan_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/roteiro");
    if root.join("crates").is_dir() {
        root.to_path_buf()
    } else {
        manifest.to_path_buf()
    }
}

/// Every `.rs` and `.md` file under `dir`, recursively.
///
/// An unreadable directory is a failure, not an empty one: returning quietly
/// would drop a subtree while leaving `files` non-empty, so the "scanned
/// nothing" assertion would not notice.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {} ({:?}: {e})", dir.display(), e.kind()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .is_some_and(|n| n == "target" || n == ".git" || n == "dist");
            if !skip {
                sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs" || e == "md")
            && !is_historical_record(&path)
        {
            out.push(path);
        }
    }
}

#[test]
fn no_file_describes_a_note_name_as_the_pre_574_form() {
    let root = scan_root();
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();

    assert!(
        !files.is_empty(),
        "scanned no sources under {}: this guard would pass without checking \
         anything",
        root.display()
    );

    let needle = dead_spelling();
    let mut hits = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            // Not UTF-8: cannot contain the needle, and is not prose.
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.contains(&needle) {
                let rel = path.strip_prefix(&root).unwrap_or(path);
                hits.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "`{needle}` is the note-name form from before issue #574 and is false \
         wherever it appears: `note_name` builds a lowercased readable hint plus \
         a hash of the whole key, and a workspace member's *key* is \
         `<project>::{}` while its filename contains no `::` at all. Describe \
         the rule rather than a spelling, or — if this is a historical record — \
         extend `is_historical_record`.\n{}",
        "<key>",
        hits.join("\n")
    );
}
