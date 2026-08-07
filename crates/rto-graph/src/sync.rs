//! The incremental, content-addressed sync engine.
//!
//! `sync` brings a [`Store`] into agreement with the repository's `HEAD` tree.
//! Extraction is the expensive part and is content-addressed by blob id, so only
//! blobs whose content changed are re-extracted; the rest load from the
//! [`ObjectCache`]. If the tree id is unchanged since the last sync, it is a
//! no-op. The graph itself is reassembled from the (cached) per-blob fact sets
//! and rebuilt in a single transaction — a deliberately simple DB-write model
//! for this stage; incremental DB updates can come later.

use crate::cache::{CacheError, ObjectCache};
use crate::extract::Extractor;
use crate::git::{GitError, Repo};
use crate::store::StoreError;
use crate::{FactSet, Store};

/// Errors raised while syncing.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A cache operation failed.
    #[error(transparent)]
    Cache(#[from] CacheError),
    /// A git operation failed.
    #[error(transparent)]
    Git(#[from] GitError),
}

/// A summary of the work a [`sync`] performed.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncReport {
    /// Hex id of the synced `HEAD` tree.
    pub tree: String,
    /// Whether the tree was unchanged and nothing was done.
    pub no_op: bool,
    /// Total blobs in the tree.
    pub blobs_total: usize,
    /// Blobs that were extracted (cache misses).
    pub blobs_extracted: usize,
    /// Blobs served from the cache (cache hits).
    pub blobs_cached: usize,
    /// Nodes in the store after syncing.
    pub nodes: u64,
    /// Edges in the store after syncing.
    pub edges: u64,
}

/// Sync `store` to the repository's `HEAD` tree, extracting changed blobs with
/// `extractor` and caching results in `cache`.
///
/// # Errors
/// Returns a [`SyncError`] if git access, extraction caching, or the store
/// rebuild fails.
pub fn sync(
    store: &mut Store,
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
) -> Result<SyncReport, SyncError> {
    let tree = repo.head_tree_id()?;

    if store.sync_state()?.as_deref() == Some(tree.as_str()) {
        return Ok(SyncReport {
            no_op: true,
            nodes: store.node_count()?,
            edges: store.edge_count()?,
            tree,
            ..SyncReport::default()
        });
    }

    let blobs = repo.walk_blobs()?;
    let mut assembled = FactSet::new();
    let mut extracted = 0usize;
    let mut cached = 0usize;

    for blob in &blobs {
        let facts = if let Some(facts) = cache.get(&blob.oid)? {
            cached += 1;
            facts
        } else {
            let bytes = repo.read_blob(&blob.oid)?;
            let facts = extractor.extract(&blob.path, &blob.oid, &bytes);
            cache.put(&blob.oid, &facts)?;
            extracted += 1;
            facts
        };
        assembled.nodes.extend(facts.nodes);
        assembled.edges.extend(facts.edges);
    }

    store.rebuild(&assembled, &tree)?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: blobs.len(),
        blobs_extracted: extracted,
        blobs_cached: cached,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree,
    })
}
