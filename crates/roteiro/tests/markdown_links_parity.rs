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

/// Every file under `dir` whose extension is in `exts`, recursively.
///
/// **Regular files only**, symlinks not followed — the same three-way
/// `file_type` test the production walker uses, and for the same reason
/// `docs_are_canonical.rs` gives: "not a directory" also admits a FIFO, and
/// `read_to_string` blocks on one forever.
fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
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

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[test]
fn the_shared_scanner_finds_exactly_what_the_old_one_did() {
    let files = corpus();
    let root = repo_root();
    let mut lines = 0usize;
    let mut link_count = 0usize;
    let mut with_links = 0usize;
    let mut divergences = Vec::new();

    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            // A non-UTF-8 file is not a document either scanner would read.
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
    let mut checked = 0usize;
    let mut inline = 0usize;
    for file in corpus() {
        let Ok(text) = std::fs::read_to_string(&file) else {
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
                    rto_graph::LinkKind::Inline => {
                        inline += 1;
                        assert!(
                            source.starts_with('[') && source.ends_with(')'),
                            "{where_}: inline span {source:?} is not a `[…](…)`"
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
