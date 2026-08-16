//! The source text a finding points at, read from the analyzed worktree.
//!
//! # Why this exists rather than using the analyzer's own snippet
//!
//! ADR-0012's identity recipe for semgrep ends in a `<snippet-hash>`, so that a
//! finding whose rule and byte offset are unchanged but whose *code* changed is a
//! new finding rather than the old one silently carried forward.
//!
//! Semgrep cannot supply that snippet. Its JSON output carries `extra.lines` and
//! `extra.fingerprint`, and in the open-source CLI both are the literal string
//! `"requires login"` unless the caller is authenticated to Semgrep's hosted
//! platform — verified directly against semgrep 1.136.0 with `--json`, with and
//! without `--quiet`. Hashing that would make every finding's identity component
//! a constant, and worse, would make finding keys *change* the day somebody logs
//! in.
//!
//! So the snippet is read from the tree instead. That is strictly better: it is
//! a function of the source rather than of the analyzer's authentication state,
//! which is what an identity component ought to be, and it makes a subprocess run
//! and an ingest of the same report agree by construction — both read the same
//! checkout.
//!
//! @rto:0012

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::runner::check_reported_path;

/// Somewhere the bytes a finding points at can be read from.
pub trait SnippetSource {
    /// The text between `start` and `end` in `path`, or `None` when it cannot be
    /// read — no such file, offsets past the end, or bytes that are not UTF-8.
    ///
    /// `None` is a normal answer, not an error: a report can legitimately
    /// describe a tree the caller does not have.
    fn snippet(&self, path: &str, start: u32, end: u32) -> Option<String>;
}

/// A [`SnippetSource`] that never has anything — for callers with no checkout,
/// and for tests that do not care.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnippets;

impl SnippetSource for NoSnippets {
    fn snippet(&self, _path: &str, _start: u32, _end: u32) -> Option<String> {
        None
    }
}

/// Reads snippets out of a worktree, caching each file it touches.
///
/// A scan produces many findings in few files, so the cache turns a read per
/// finding into a read per file. It is bounded by
/// [`WorktreeSnippets::MAX_FILE_BYTES`] per file: a report naming a huge
/// generated file must not be able to make Roteiro read it into memory.
#[derive(Debug)]
pub struct WorktreeSnippets {
    root: PathBuf,
    cache: RefCell<HashMap<String, Option<Vec<u8>>>>,
}

impl WorktreeSnippets {
    /// Largest file a snippet will be read from. Beyond this the snippet is
    /// unavailable and the identity says so, which is a better outcome than
    /// buffering an arbitrary file because a report asked.
    pub const MAX_FILE_BYTES: u64 = 8 << 20;

    /// Read snippets relative to `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// The file's bytes, read once and remembered — including the fact that it
    /// could not be read, so a missing file is not re-stat'd per finding.
    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(hit) = self.cache.borrow().get(path) {
            return hit.clone();
        }
        let read = self.read(path);
        self.cache
            .borrow_mut()
            .insert(path.to_owned(), read.clone());
        read
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        // A report is untrusted input. The same check the ingest path applies to
        // a finding's path applies here, *before* the path is joined onto the
        // root — otherwise a report naming `../../.ssh/id_ed25519` would have
        // Roteiro read it and hash it into a stored record.
        check_reported_path(path).ok()?;
        let full = self.root.join(Path::new(path));
        let meta = std::fs::metadata(&full).ok()?;
        if !meta.is_file() || meta.len() > Self::MAX_FILE_BYTES {
            return None;
        }
        std::fs::read(&full).ok()
    }
}

impl SnippetSource for WorktreeSnippets {
    fn snippet(&self, path: &str, start: u32, end: u32) -> Option<String> {
        if end < start {
            return None;
        }
        let bytes = self.bytes(path)?;
        let (from, to) = (start as usize, end as usize);
        // An offset past the end means the report and the tree disagree about
        // the file. Returning `None` rather than a truncated slice keeps the
        // identity from being built out of the wrong bytes.
        if to > bytes.len() {
            return None;
        }
        String::from_utf8(bytes[from..to].to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{NoSnippets, SnippetSource, WorktreeSnippets};

    /// A throwaway directory that removes itself, so these tests need no
    /// dev-dependency on a temp-dir crate.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rto-exec-snippet-{name}"));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(dir.join("src")).expect("create");
            std::fs::write(dir.join("src/app.py"), b"import os\nos.system(cmd)\n").expect("write");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn reads_the_bytes_a_finding_points_at() {
        let scratch = Scratch::new("reads");
        let snippets = WorktreeSnippets::new(&scratch.0);
        assert_eq!(
            snippets.snippet("src/app.py", 10, 24).as_deref(),
            Some("os.system(cmd)")
        );
        // The second read is served from the cache and must agree with the first.
        assert_eq!(
            snippets.snippet("src/app.py", 10, 24).as_deref(),
            Some("os.system(cmd)")
        );
    }

    #[test]
    fn a_file_it_does_not_have_is_simply_unavailable() {
        let scratch = Scratch::new("missing");
        let snippets = WorktreeSnippets::new(&scratch.0);
        assert!(snippets.snippet("src/nope.py", 0, 4).is_none());
    }

    /// A report is untrusted input, so a path that climbs out of the worktree is
    /// refused here as well as on the ingest path. Reading it would put file
    /// contents from outside the tree into a stored identity.
    #[test]
    fn refuses_to_read_outside_the_worktree() {
        let scratch = Scratch::new("escape");
        let snippets = WorktreeSnippets::new(&scratch.0);
        for hostile in ["../../../etc/passwd", "/etc/passwd", ""] {
            assert!(snippets.snippet(hostile, 0, 4).is_none(), "{hostile:?}");
        }
    }

    #[test]
    fn offsets_past_the_end_yield_nothing_rather_than_a_truncated_slice() {
        let scratch = Scratch::new("bounds");
        let snippets = WorktreeSnippets::new(&scratch.0);
        assert!(snippets.snippet("src/app.py", 0, 9_999).is_none());
        assert!(snippets.snippet("src/app.py", 20, 5).is_none());
    }

    #[test]
    fn the_empty_source_answers_nothing() {
        assert!(NoSnippets.snippet("src/app.py", 0, 4).is_none());
    }
}
