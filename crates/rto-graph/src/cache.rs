//! Content-addressed on-disk cache of per-blob [`FactSet`]s.
//!
//! Extraction is a pure function of a git blob, so its result is keyed purely by
//! the blob's git object id. The cache lives under the repository's *common* git
//! directory (e.g. `<common>/roteiro/objects/`), so all worktrees and branches
//! that share a blob share its extracted facts. Entries are JSON, sharded by the
//! first two hex characters of the id (git-style) to keep directories small.

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
}

#[cfg(test)]
mod tests {
    use super::ObjectCache;
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind};

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
    fn short_ids_do_not_panic_on_shard() {
        let dir = std::env::temp_dir().join(format!("roteiro-cache-short-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = ObjectCache::open(&dir).expect("open");
        cache.put("a", &FactSet::new()).expect("put short id");
        assert_eq!(cache.get("a").expect("get"), Some(FactSet::new()));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
