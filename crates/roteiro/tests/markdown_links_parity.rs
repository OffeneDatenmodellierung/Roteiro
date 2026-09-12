//! [`rto_graph::wiki_link_targets`] returns exactly what `rto_spec`'s own
//! `[[…]]` scanner returned, over every line this repository actually has.
//!
//! # Why this test exists
//!
//! #801 phase 2 replaced five unshared implementations of "find a Markdown
//! link" with one. Four of them fed `roteiro check`'s `link(s) ok` count, so the
//! consolidation is only safe if the new scanner is the old one — not "better",
//! and not "the same on the cases somebody thought to write down". A unit test
//! proves the cases its author imagined. This proves the cases the repository
//! contains, which is the set that decides whether the gate's number moves.
//!
//! The hazard being guarded is the one #790 left recorded at
//! `docs_are_canonical.rs:23-29`: two Markdown walkers drifted apart because one
//! got a fix the other did not, and nothing failed. A consolidation is the same
//! risk taken deliberately.
//!
//! # The oracle is frozen on purpose
//!
//! [`legacy_scan_wiki_links`] below is `rto_spec::text::scan_wiki_links` and its
//! two code-span helpers, copied verbatim at commit `3be924e` — the state of
//! `main` this change was cut from. It is **not** a maintained second
//! implementation and must never be given a fix: the moment it is improved it
//! stops being evidence about what the gate used to count and becomes the sixth
//! implementation this work exists to remove. If the shared scanner is
//! deliberately changed, this test is deleted in the same commit that argues for
//! the change, not edited to agree with it.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// The frozen oracle — `rto_spec::text` @ 3be924e. Do not fix. See module docs.
// ---------------------------------------------------------------------------

/// `rto_spec::text::scan_wiki_links`, verbatim at `3be924e`.
fn legacy_scan_wiki_links(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let stripped = legacy_strip_code_spans(line);
    let mut rest = stripped.as_str();
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find("]]") {
            let inner = after[..close].trim();
            if !inner.is_empty() {
                out.push(inner.to_owned());
            }
            rest = &after[close + 2..];
        } else {
            break;
        }
    }
    out
}

/// `rto_spec::text::strip_code_spans`, verbatim at `3be924e`.
fn legacy_strip_code_spans(line: &str) -> String {
    let spans = legacy_code_spans(line);
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    for (start, end) in spans {
        out.push_str(&line[at..start]);
        at = end;
    }
    out.push_str(&line[at..]);
    out
}

/// `rto_spec::text::code_spans`, verbatim at `3be924e`.
fn legacy_code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let escaped = |at: usize| {
        bytes[..at]
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count()
            % 2
            == 1
    };
    while i < bytes.len() {
        if bytes[i] != b'`' || escaped(i) {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let run = i - run_start;
        let mut j = i;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let s = j;
                while j < bytes.len() && bytes[j] == b'`' {
                    j += 1;
                }
                if j - s == run {
                    close = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        if let Some(end) = close {
            out.push((run_start, end));
            i = end;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

/// The repository root — this crate's manifest directory, two levels up.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/roteiro sits two levels below the repository root")
        .to_path_buf()
}

/// Whether this is a checkout of the Roteiro repository, rather than a packaged
/// crate unpacked from the registry.
///
/// This file ships in the published `roteiro` package — `cargo package --list`
/// names it, along with 46 other integration tests. There
/// `CARGO_MANIFEST_DIR` is `…/registry/src/<index>/roteiro-<version>`, so
/// [`repo_root`]'s two ancestors are the registry's *source directory*: a tree
/// full of unrelated crates, with plenty of `.md` and `.rs` in it. The floors
/// below would happily clear on somebody else's files, and the parity claim
/// would be made about a repository this test never saw. Raised in review on
/// #806.
///
/// The marker is the **workspace** manifest, and the rule for reading it is
/// `ci_coverage_claims.rs`'s, which met this same problem first and wrote down
/// why the marker has to be as loud as the thing it guards: reading `false` on
/// an I/O error would turn "cannot read the repository" into "this is not a
/// repository" and skip in silence, which is the defect these two tests spent a
/// review round removing one level down. Only `NotFound` means absent.
fn is_repository_checkout() -> bool {
    let manifest = repo_root().join("Cargo.toml");
    match std::fs::read_to_string(&manifest) {
        Ok(text) => text.lines().any(|line| line.trim() == "[workspace]"),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). Without it this test cannot tell a \
             packaged crate from a repository checkout, and guessing would make \
             the corpus either vacuous or somebody else's.",
            manifest.display(),
            e.kind(),
        ),
    }
}

/// Every file under `dir` whose extension is in `exts`, recursively.
///
/// **Regular files only**, symlinks not followed — the same three-way
/// `file_type` test the production walker uses, and for the same reason
/// `docs_are_canonical.rs` gives: "not a directory" also admits a FIFO, and
/// `read_to_string` blocks on one forever.
///
/// Every I/O error **panics** rather than pruning the subtree quietly. The floor
/// assertions below are the only thing standing between this test and a vacuous
/// pass, and an unreadable directory costs coverage without costing enough of it
/// to trip them — so a skipped subtree would be a silently smaller corpus, which
/// is exactly what the floors exist to catch. Raised in review on #806.
fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "cannot read {} while building the corpus: {e}",
            dir.display()
        )
    });
    for entry in rd {
        let entry =
            entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()));
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            // Build output and the git object store are not authored sources,
            // and walking them costs minutes.
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("target" | ".git" | "node_modules" | ".worktrees")
            ) {
                continue;
            }
            walk(&path, exts, out);
        } else if kind.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// Every Markdown and Rust source file in the tree.
///
/// Both, because both are scanned for `[[…]]` in production: Markdown by the
/// ADR, blueprint and site-page parsers, and Rust by `rto_spec::lat`'s `@lat:`
/// backlink reader, which takes the wiki-links after the marker on a comment
/// line. A corpus of only one of them would leave half the scanner's real input
/// unexercised.
fn corpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&repo_root(), &["md", "rs"], &mut out);
    out.sort();
    out
}

/// The text of one corpus file, or [`None`] if it is not text at all.
///
/// **Only `InvalidData` is skipped** — a file whose bytes are not UTF-8 is not a
/// document either scanner would read, so leaving it out is the corpus being
/// accurate rather than the corpus being incomplete. Every other error panics.
///
/// Both tests below used `let Ok(text) = … else { continue }`, which treats a
/// permission error, a vanished file or a failing disk exactly like a binary:
/// the file leaves the corpus and nothing says so. The floors are aggregate, so
/// they cannot name the file that went missing and a handful of dropped files
/// does not move them — which means the divergent line these tests exist to
/// catch could be in a file neither of them read, and both would pass. The
/// walker above already panics on a `read_dir` failure for this reason; this is
/// the same rule one level down. Raised in review on #806.
fn read_text(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::InvalidData => None,
        Err(e) => panic!(
            "cannot read {} while scanning the corpus: {e}. Every file the \
             walker listed must be read or the corpus is quietly smaller than \
             the floors below assume",
            path.display()
        ),
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The skip above is real, and here it must not happen.
///
/// Both corpus tests return early outside a repository checkout, which is right
/// in the published package and would be a silent hole in CI: a marker that read
/// `false` here would turn two guards over ~1,400 files into two instant passes
/// and nothing would say so. That is the same vacuity `read_text` was fixed for
/// one round earlier, moved up a level, so it gets the same treatment — the skip
/// is asserted *not* to fire where the files exist.
#[test]
fn the_corpus_guards_actually_run_in_a_checkout() {
    assert!(
        is_repository_checkout(),
        "no `[workspace]` manifest at {} — so both corpus tests below skipped. \
         In a checkout that is not a skip, it is two guards silently switched \
         off; fix the marker rather than this assertion.",
        repo_root().join("Cargo.toml").display()
    );
    // And the walker really reaches the tree it claims to, so "ran" is not
    // "ran over nothing" — the floors inside each test cover the rest.
    assert!(
        corpus().len() >= 200,
        "the corpus holds only {} file(s) in what claims to be a checkout",
        corpus().len()
    );
}

#[test]
fn the_shared_scanner_finds_exactly_what_the_old_one_did() {
    if !is_repository_checkout() {
        return;
    }
    let files = corpus();
    let root = repo_root();
    let mut lines = 0usize;
    let mut link_count = 0usize;
    let mut with_links = 0usize;
    let mut divergences = Vec::new();

    for file in &files {
        // A non-UTF-8 file is not a document either scanner would read; any
        // other read failure panics rather than shrinking the corpus silently.
        let Some(text) = read_text(file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            lines += 1;
            let old = legacy_scan_wiki_links(line);
            let new = rto_graph::wiki_link_targets(line);
            if !old.is_empty() {
                with_links += 1;
                link_count += old.len();
            }
            if old != new {
                let rel = file.strip_prefix(&root).unwrap_or(file);
                divergences.push(format!(
                    "{}:{}\n    was: {old:?}\n    now: {new:?}\n    line: {line}",
                    rel.display(),
                    n + 1
                ));
            }
        }
    }

    // The relation above is satisfiable by scanning nothing, so say how much was
    // actually compared. The floors are an order of magnitude below what the
    // tree holds today (≈1,400 files, ≈2,500 links), so ordinary editing does
    // not touch them and a corpus that collapsed to a handful of files does.
    assert!(
        files.len() >= 200 && lines >= 100_000,
        "compared only {} files / {lines} lines — the walker is not looking at \
         the repository, and a parity check over nothing passes",
        files.len()
    );
    assert!(
        with_links >= 200 && link_count >= 400,
        "the corpus holds only {link_count} wiki-link(s) on {with_links} line(s) — \
         parity over lines that contain no links proves nothing about links"
    );
    assert!(
        divergences.is_empty(),
        "the shared scanner disagrees with the one it replaced on {} line(s) of \
         this repository. Each of these is a link `roteiro check` counted \
         differently before and after, which is a regression even where the new \
         answer reads better:\n\n{}",
        divergences.len(),
        divergences.join("\n\n")
    );
}

/// The inline half has no predecessor to be compared against, so it is held to
/// the one thing that can be checked over the real corpus without a second
/// implementation: every destination it reports is really there, spelled the way
/// it reports it, at the range it reports it at.
///
/// This is what catches an off-by-one in the span map — the part of the new
/// scanner with no legacy answer to check against, and the part
/// `doc_anchor_fragments.rs` now splices on.
#[test]
fn every_reported_link_addresses_the_text_it_was_read_from() {
    let root = repo_root();
    if !is_repository_checkout() {
        return;
    }
    let mut checked = 0usize;
    let mut inline = 0usize;
    for file in corpus() {
        let Some(text) = read_text(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for link in rto_graph::markdown_links(line) {
                let rel = file.strip_prefix(&root).unwrap_or(&file);
                let where_ = format!("{}:{}", rel.display(), n + 1);
                assert!(
                    link.span.end <= line.len()
                        && line.is_char_boundary(link.span.start)
                        && line.is_char_boundary(link.span.end),
                    "{where_}: span {:?} is not inside the line it came from",
                    link.span
                );
                let source = &line[link.span.clone()];
                match link.kind {
                    rto_graph::LinkKind::Wiki => {
                        assert!(
                            source.starts_with("[[") && source.ends_with("]]"),
                            "{where_}: wiki span {source:?} is not a `[[…]]`"
                        );
                    }
                    rto_graph::LinkKind::Inline | rto_graph::LinkKind::Image => {
                        inline += 1;
                        let opens = if link.kind == rto_graph::LinkKind::Image {
                            "!["
                        } else {
                            "["
                        };
                        assert!(
                            source.starts_with(opens) && source.ends_with(')'),
                            "{where_}: inline span {source:?} does not open `{opens}` and close `)`"
                        );
                        assert!(
                            !link.target.is_empty(),
                            "{where_}: a link with no destination was reported"
                        );
                    }
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 1_000 && inline >= 500,
        "only {checked} link(s) ({inline} inline) found across the tree — the \
         corpus is not the repository and this proves nothing"
    );
}
