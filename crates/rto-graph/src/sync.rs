//! The incremental, content-addressed sync engine.
//!
//! `sync` brings a [`Store`] into agreement with the repository's `HEAD` tree.
//! Extraction is the expensive part and is content-addressed by blob id, so only
//! blobs whose content changed are re-extracted; the rest load from the
//! [`ObjectCache`]. If the tree id is unchanged since the last sync, it is a
//! no-op. The graph itself is reassembled from the (cached) per-blob fact sets
//! and rebuilt in a single transaction — a deliberately simple DB-write model
//! for this stage; incremental DB updates can come later.

use std::collections::{BTreeMap, BTreeSet};

use crate::cache::{CacheError, ObjectCache};
use crate::extract::Extractor;
use crate::git::{GitError, Repo};
use crate::store::StoreError;
use crate::{Edge, EdgeKind, FactSet, NodeKind, Store};

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
        // Extraction is a pure function of (path, blob bytes), not blob id
        // alone: node keys are path-scoped (e.g. `file:<path>`), so the same
        // blob content at two different paths yields different facts. Key the
        // cache by (path, oid) so duplicate-content files (e.g. empty files,
        // which git dedupes to one oid) never collide, while the same path+oid
        // in another branch/worktree still hits.
        let key = cache_key(&blob.path, &blob.oid);
        let facts = if let Some(facts) = cache.get(&key)? {
            cached += 1;
            facts
        } else {
            let bytes = repo.read_blob(&blob.oid)?;
            let facts = extractor.extract(&blob.path, &blob.oid, &bytes);
            cache.put(&key, &facts)?;
            extracted += 1;
            facts
        };
        let FactSet { nodes, edges } = facts;
        assembled.nodes.extend(nodes);
        assembled.edges.extend(edges);
    }

    resolve_calls(&mut assembled);
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

/// Resolve the per-function call records (`meta.calls`) accumulated during
/// extraction into `calls` edges, now that every file's symbols are present.
///
/// A callee simple-name is linked only when it resolves to **exactly one**
/// function in the whole tree; ambiguous names (multiple `fn foo`) and unknown
/// names (external/std calls) are left unresolved rather than guessed. This runs
/// at assembly time — not per blob — because a single blob cannot see the
/// definitions in other files.
fn resolve_calls(facts: &mut FactSet) {
    // Simple function name → the keys of functions with that name.
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for n in &facts.nodes {
        if n.kind == NodeKind::Fn {
            by_name
                .entry(n.name.as_str())
                .or_default()
                .push(n.key.as_str());
        }
    }

    // Collect (caller, callee) pairs; BTreeSet dedupes and orders them.
    let mut resolved: BTreeSet<(String, String)> = BTreeSet::new();
    for n in &facts.nodes {
        if n.kind != NodeKind::Fn {
            continue;
        }
        let Some(calls) = n.meta.get("calls").and_then(|v| v.as_array()) else {
            continue;
        };
        for callee in calls.iter().filter_map(|v| v.as_str()) {
            if let Some(targets) = by_name.get(callee)
                && targets.len() == 1
            {
                resolved.insert((n.key.clone(), targets[0].to_owned()));
            }
        }
    }

    for (src, dst) in resolved {
        facts.edges.push(Edge::derived(src, dst, EdgeKind::Calls));
    }
}

/// Content-addressed cache key for a blob at a given path: the blob oid (kept
/// as the leading, well-distributed shard) suffixed with a stable 64-bit hash
/// of the path. Sharing across branches/worktrees is preserved (same path+oid →
/// same key) while duplicate content at distinct paths stays distinct.
fn cache_key(path: &str, oid: &str) -> String {
    format!("{oid}-{:016x}", fnv1a64(path.as_bytes()))
}

/// FNV-1a (64-bit). Dependency-free and deterministic; used only to derive
/// cache filenames, so it needs no cryptographic properties.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{cache_key, resolve_calls};
    use crate::{EdgeKind, FactSet, Node, NodeKind};

    fn fn_node(key: &str, name: &str, calls: &[&str]) -> Node {
        let mut n = Node::new(key, NodeKind::Fn, name);
        if !calls.is_empty() {
            n.meta = serde_json::json!({ "calls": calls });
        }
        n
    }

    #[test]
    fn resolve_calls_links_unique_names_only() {
        let mut fs = FactSet::new()
            .with_node(fn_node(
                "sym:rust:a.rs#caller",
                "caller",
                &["target", "dup", "missing"],
            ))
            .with_node(fn_node("sym:rust:a.rs#target", "target", &[]))
            // Two functions named `dup` → ambiguous, must not be linked.
            .with_node(fn_node("sym:rust:a.rs#dup", "dup", &[]))
            .with_node(fn_node("sym:rust:b.rs#dup", "dup", &[]));

        resolve_calls(&mut fs);

        let calls: Vec<_> = fs
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "only the unambiguous, known callee is linked"
        );
        assert_eq!(calls[0].src, "sym:rust:a.rs#caller");
        assert_eq!(calls[0].dst, "sym:rust:a.rs#target");
    }

    #[test]
    fn cache_key_separates_paths_but_is_stable() {
        let oid = "abc123";
        // Same path + oid is stable across calls.
        assert_eq!(cache_key("src/a.rs", oid), cache_key("src/a.rs", oid));
        // Same blob content (oid) at two different paths must not collide.
        assert_ne!(cache_key("src/a.rs", oid), cache_key("src/b.rs", oid));
        // Different content at the same path differs too.
        assert_ne!(cache_key("src/a.rs", "aaa"), cache_key("src/a.rs", "bbb"));
        // Key stays sharded on the oid so the cache's 2-char shard is well spread.
        assert!(cache_key("src/a.rs", oid).starts_with("abc123-"));
    }
}
