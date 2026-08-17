//! Content-addressed on-disk cache of per-blob [`FactSet`]s.
//!
//! The cache is a simple content-addressed key→[`FactSet`] store; the caller
//! derives the key (see [`crate::sync`], which keys by blob oid **and** path,
//! because extraction is a pure function of both). The cache lives under the
//! repository's *common* git directory (e.g. `<common>/roteiro/objects/`), so
//! all worktrees and branches that share a key share its extracted facts.
//! Entries are JSON, sharded by the first two characters of the key (git-style)
//! to keep directories small.

use std::fs;
use std::path::{Path, PathBuf};

use crate::FactSet;

/// Errors raised by the object cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Filesystem failure.
    #[error("cache io error: {0}")]
    Io(#[from] std::io::Error),
    /// A cached entry could not be (de)serialized.
    #[error("cache json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// What one [`ObjectCache::sweep`] pass did.
///
/// `scanned` is exactly `retained + removed + raced + failed`, and everything
/// under the root that is not an entry lands in `skipped` instead — so a sweep
/// that had nothing to do and a sweep that could not do it do not print the same
/// line.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ObjectSweep {
    /// Cache entries examined — every `<shard>/<rest>.json` under the root.
    pub scanned: usize,
    /// Entries `retain` kept.
    pub retained: usize,
    /// Entries `retain` rejected and this pass deleted.
    pub removed: usize,
    /// Bytes still held by the `retained` entries.
    pub retained_bytes: u64,
    /// Bytes freed — the size of the `removed` entries before deletion.
    pub freed_bytes: u64,
    /// Rejected entries already gone when the delete ran: another process swept
    /// the same shared cache concurrently. Counted rather than raised, because
    /// two sweeps agreeing is the expected outcome, not a fault.
    pub raced: usize,
    /// Rejected entries that could not be deleted — a permission problem, or a
    /// platform that refuses to unlink a file another process holds open.
    /// Counted and reported rather than aborting: a sweep that stops at the first
    /// stuck file both reclaims less and says nothing about why.
    pub failed: usize,
    /// Files under the root that are **not** entries: a `.json.tmp.<pid>-<nanos>`
    /// from a [`ObjectCache::put`] still in flight, or anything a later format
    /// puts here. Never shown to `retain` and never deleted — a sweep that
    /// guesses at a name it does not recognise is a sweep that deletes another
    /// process's half-written work.
    pub skipped: usize,
}

/// A content-addressed store of fact sets on disk.
pub struct ObjectCache {
    root: PathBuf,
}

impl ObjectCache {
    /// Open (creating if absent) a cache rooted at `root`.
    ///
    /// # Errors
    /// Returns [`CacheError::Io`] if the root directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CacheError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The directory this cache stores objects under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, blob_id: &str) -> PathBuf {
        // Shard by the first two characters, like git's `objects/ab/cdef…`.
        let (shard, rest) = blob_id.split_at(blob_id.len().min(2));
        self.root.join(shard).join(format!("{rest}.json"))
    }

    /// Whether a fact set is cached for `blob_id`.
    #[must_use]
    pub fn contains(&self, blob_id: &str) -> bool {
        self.path_for(blob_id).exists()
    }

    /// Load the cached fact set for `blob_id`, if present.
    ///
    /// # Errors
    /// Returns [`CacheError::Io`] on read failure or [`CacheError::Json`] if the
    /// entry cannot be decoded.
    pub fn get(&self, blob_id: &str) -> Result<Option<FactSet>, CacheError> {
        let path = self.path_for(blob_id);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store `facts` under `blob_id`, replacing any existing entry. The write is
    /// atomic (write-to-temp then rename) so a crash never leaves a torn entry.
    ///
    /// # Errors
    /// Returns [`CacheError::Io`] on write failure or [`CacheError::Json`] if
    /// `facts` cannot be encoded.
    pub fn put(&self, blob_id: &str, facts: &FactSet) -> Result<(), CacheError> {
        let path = self.path_for(blob_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Use a unique temp file name to avoid cross-process clobbering.
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let tmp = path.with_extension(format!("json.tmp.{unique}"));

        let bytes = serde_json::to_vec(facts)?;
        fs::write(&tmp, &bytes)?;

        match fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                fs::rename(&tmp, &path)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Delete every entry whose key `retain` rejects, returning what the pass did.
    ///
    /// **This module deliberately does not know what a key means.** It derives
    /// none and interprets none — the caller derives the key (see the module
    /// doc), so the caller is the only thing entitled to say which keys are still
    /// reachable. `retain` receives the *whole* key, reassembled from the shard
    /// directory and the file stem, so a policy that reads any part of it reads
    /// the same string [`Self::put`] was given.
    ///
    /// The pass is safe to run while other processes are using the same cache —
    /// which is not optional, because the root lives under the **common** git dir
    /// and every worktree shares it:
    ///
    /// - Entries are whole files written by atomic rename, and this deletes whole
    ///   files, so no reader can observe a torn one. A reader that had already
    ///   opened a deleted entry keeps reading it (POSIX); a reader that had not
    ///   gets [`Self::get`]'s ordinary `None`, which is a cache miss — and a miss
    ///   costs a re-extraction, never a wrong answer, because the cache is
    ///   derived. That is the whole reason a mistaken `retain` is survivable.
    /// - Nothing that is not an entry is touched, so a concurrent `put`'s temp
    ///   file survives to be renamed.
    /// - Shard directories are **not** removed, even when emptied. `put` does
    ///   `create_dir_all` and *then* writes; removing the directory in between
    ///   would fail an unrelated process's write to reclaim four kilobytes.
    ///
    /// # Errors
    /// Returns [`CacheError::Io`] if the root or a shard cannot be listed — an
    /// unreadable cache is reported, never silently swept as empty. Per-entry
    /// delete failures are counted in [`ObjectSweep::failed`] instead, so one
    /// stuck file does not abandon the rest.
    pub fn sweep(&self, retain: &dyn Fn(&str) -> bool) -> Result<ObjectSweep, CacheError> {
        let mut report = ObjectSweep::default();
        for shard in fs::read_dir(&self.root)? {
            let shard = shard?;
            // `file_type` on a `DirEntry` does not follow links, so a symlinked
            // directory is skipped rather than walked out of the cache.
            if !shard.file_type()?.is_dir() {
                report.skipped += 1;
                continue;
            }
            let Some(prefix) = shard.file_name().to_str().map(str::to_owned) else {
                // A shard name that is not UTF-8 cannot be half of a key this
                // cache wrote, so its contents are not ours to judge.
                report.skipped += 1;
                continue;
            };
            Self::sweep_shard(&shard.path(), &prefix, retain, &mut report)?;
        }
        Ok(report)
    }

    /// One shard directory of [`Self::sweep`].
    fn sweep_shard(
        dir: &Path,
        prefix: &str,
        retain: &dyn Fn(&str) -> bool,
        report: &mut ObjectSweep,
    ) -> Result<(), CacheError> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            // An entry is named exactly `<rest>.json`. A temp file is
            // `<rest>.json.tmp.<unique>` and so fails this test, which is the
            // point: it belongs to a `put` that has not finished.
            let Some(rest) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                report.skipped += 1;
                continue;
            };
            // `symlink_metadata` does not follow links, so a symlink is never
            // mistaken for an entry nor followed out of the cache — and it gives
            // the size in the same call, with one race to handle instead of two.
            let bytes = match fs::symlink_metadata(entry.path()) {
                Ok(meta) if meta.is_file() => meta.len(),
                Ok(_) => {
                    report.skipped += 1;
                    continue;
                }
                // Gone between listing and stat: another sweep of this shared
                // cache got there first. Nothing left to reclaim, nothing wrong.
                //
                // **Defensive, and not covered by a test.** Hitting it needs a
                // delete inside the window between `read_dir` yielding a name and
                // this stat, which nothing here can open deterministically —
                // whether a deleted name is still yielded depends on the
                // platform's directory buffering, so a test for it would be
                // flaky rather than a test. It is counted exactly as the same
                // race on `remove_file` below is (`sweep_counts_an_entry_a_
                // concurrent_sweep_removed_first`), which *is* covered; the
                // alternative — letting it propagate — would make one sweep of a
                // shared cache fail because another was doing its job.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    report.scanned += 1;
                    report.raced += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            report.scanned += 1;
            // The key as `put` received it: the shard is its first two characters,
            // not a hash of it, so concatenating recovers the original exactly.
            let key = format!("{prefix}{rest}");
            if retain(&key) {
                report.retained += 1;
                report.retained_bytes += bytes;
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => {
                    report.removed += 1;
                    report.freed_bytes += bytes;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.raced += 1,
                Err(_) => report.failed += 1,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectCache;
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind};

    /// A cache root unique to `name` and this process, with any stale copy gone.
    fn fresh(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-cache-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn sample() -> FactSet {
        FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls))
    }

    #[test]
    fn put_get_round_trip_and_miss() {
        let dir = std::env::temp_dir().join(format!("roteiro-cache-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = ObjectCache::open(&dir).expect("open");

        assert!(!cache.contains("deadbeef"));
        assert!(cache.get("deadbeef").expect("get").is_none());

        let facts = sample();
        cache.put("deadbeef", &facts).expect("put");
        assert!(cache.contains("deadbeef"));
        assert_eq!(cache.get("deadbeef").expect("get"), Some(facts));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn put_overwrites_existing_entry() {
        let dir =
            std::env::temp_dir().join(format!("roteiro-cache-overwrite-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = ObjectCache::open(&dir).expect("open");

        cache.put("beef", &sample()).expect("first put");
        // A second put for the same key must atomically replace the entry.
        let replacement = FactSet::new().with_node(Node::new("only", NodeKind::File, "only"));
        cache.put("beef", &replacement).expect("overwrite");
        assert_eq!(cache.get("beef").expect("get"), Some(replacement));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// The predicate decides, and it decides on the **whole** key — not the file
    /// stem, which is the key minus its shard. Keys differing only in their first
    /// character land in different shard directories with *identical* stems, so a
    /// sweep that judged the stem would give both the same verdict. Here one is
    /// kept and one removed, which only the reassembled key can distinguish.
    #[test]
    fn sweep_judges_the_whole_key_not_the_file_stem() {
        let dir = fresh("sweep-key");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("aakeep", &sample()).expect("put keep");
        cache.put("bakeep", &sample()).expect("put drop");

        let swept = cache
            .sweep(&|key| key.starts_with("aa"))
            .expect("sweep should read the cache");

        assert_eq!(swept.scanned, 2);
        assert_eq!(swept.retained, 1, "exactly one key starts with `aa`");
        assert_eq!(swept.removed, 1);
        assert!(cache.contains("aakeep"), "the retained entry must survive");
        assert!(!cache.contains("bakeep"), "the rejected entry must be gone");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// Bytes are accounted for on both sides. A sweep reporting only what it
    /// freed cannot be told apart from one that had nothing to free.
    #[test]
    fn sweep_accounts_for_both_freed_and_retained_bytes() {
        let dir = fresh("sweep-bytes");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("keeper", &sample()).expect("put");
        cache.put("goner", &sample()).expect("put");
        let each = std::fs::metadata(dir.join("ke").join("eper.json"))
            .expect("stat")
            .len();
        assert!(each > 0, "an entry with facts in it is not empty");

        let swept = cache.sweep(&|key| key == "keeper").expect("sweep");

        assert_eq!(swept.freed_bytes, each, "the removed entry's size, exactly");
        assert_eq!(swept.retained_bytes, each);
        assert_eq!((swept.raced, swept.failed, swept.skipped), (0, 0, 0));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A `put` in flight has written its temp file but not yet renamed it. The
    /// sweep must not see it as an entry, and must not delete it — the rename
    /// that follows would fail and take an unrelated sync down with it. The
    /// predicate is `|_| false`, so *everything* it is shown is deleted: only
    /// never being shown the temp file can save it.
    #[test]
    fn sweep_never_touches_a_put_in_flight() {
        let dir = fresh("sweep-tmp");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("deadbeef", &sample()).expect("put");
        let tmp = dir.join("de").join("adbeef.json.tmp.4242-1");
        std::fs::write(&tmp, b"half-written").expect("stage a temp file");

        let swept = cache.sweep(&|_| false).expect("sweep");

        assert_eq!(swept.scanned, 1, "the temp file is not a cache entry");
        assert_eq!(swept.removed, 1);
        assert_eq!(swept.skipped, 1, "and it is reported, not silently ignored");
        assert!(tmp.exists(), "a `put` in flight must survive the sweep");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// Emptying a shard must not remove the shard directory. `put` does
    /// `create_dir_all` and *then* writes into it; a sweep that removed the
    /// directory in between would fail that write to reclaim an empty inode.
    #[test]
    fn sweep_leaves_emptied_shard_directories_in_place() {
        let dir = fresh("sweep-shard");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("deadbeef", &sample()).expect("put");
        let shard = dir.join("de");

        let swept = cache.sweep(&|_| false).expect("sweep");

        assert_eq!(swept.removed, 1);
        assert!(shard.is_dir(), "the shard directory stays");
        // And the cache is still usable through it, which is the point.
        cache.put("deadbeef", &sample()).expect("put after sweep");
        assert!(cache.contains("deadbeef"));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A second sweep of the same shared cache that gets to an entry first is not
    /// a fault: the file is gone, which is what this pass wanted. It is counted
    /// as `raced` rather than `removed`, because this pass freed none of those
    /// bytes and reporting them would double-count the reclaim across the two.
    ///
    /// The race is made deterministic by having `retain` delete the entry itself
    /// before rejecting it — exactly the window a concurrent sweep opens.
    #[test]
    fn sweep_counts_an_entry_a_concurrent_sweep_removed_first() {
        let dir = fresh("sweep-race");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("deadbeef", &sample()).expect("put");

        let root = dir.clone();
        let swept = cache
            .sweep(&move |_| {
                std::fs::remove_file(root.join("de").join("adbeef.json")).expect("the other sweep");
                false
            })
            .expect("sweep");

        assert_eq!((swept.scanned, swept.raced), (1, 1), "{swept:?}");
        assert_eq!(
            (swept.removed, swept.freed_bytes),
            (0, 0),
            "this pass freed none of those bytes: {swept:?}",
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// A *directory* named `<something>.json` is not an entry, whatever it looks
    /// like. It is skipped rather than judged — the predicate is never shown a
    /// key that no `put` ever wrote.
    #[test]
    fn sweep_skips_a_directory_wearing_an_entry_name() {
        let dir = fresh("sweep-dir");
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("deadbeef", &sample()).expect("put");
        let impostor = dir.join("de").join("cafe.json");
        std::fs::create_dir_all(&impostor).expect("create the impostor");

        let swept = cache.sweep(&|_| false).expect("sweep");

        assert_eq!(swept.scanned, 1, "only the real entry: {swept:?}");
        assert_eq!(swept.skipped, 1, "{swept:?}");
        assert!(impostor.is_dir(), "the directory must be left alone");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// An unreadable cache is an error, never a silent "nothing to sweep". A
    /// sweep that swallowed the failure would report `removed: 0` — the same
    /// output a healthy, already-clean cache produces.
    #[test]
    fn sweep_of_a_missing_root_is_an_error_not_an_empty_pass() {
        let dir = fresh("sweep-missing");
        let cache = ObjectCache::open(&dir).expect("open");
        std::fs::remove_dir_all(&dir).expect("remove the root out from under it");

        let err = cache
            .sweep(&|_| true)
            .expect_err("a root that cannot be listed must be reported");
        assert!(matches!(err, super::CacheError::Io(_)), "got {err:?}");
    }

    #[test]
    fn short_ids_do_not_panic_on_shard() {
        let dir = std::env::temp_dir().join(format!("roteiro-cache-short-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("a", &FactSet::new()).expect("put short id");
        assert_eq!(cache.get("a").expect("get"), Some(FactSet::new()));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
