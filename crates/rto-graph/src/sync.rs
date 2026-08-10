//! The incremental, content-addressed sync engine.
//!
//! `sync` brings a [`Store`] into agreement with the repository's `HEAD` tree.
//! Extraction is the expensive part and is content-addressed by blob id, so only
//! blobs whose content changed are re-extracted; the rest load from the
//! [`ObjectCache`]. If the tree id is unchanged since the last sync, it is a
//! no-op. The graph itself is reassembled from the (cached) per-blob fact sets
//! and rebuilt in a single transaction — a deliberately simple DB-write model
//! for this stage; incremental DB updates can come later.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::cache::{CacheError, ObjectCache};
use crate::extract::Extractor;
use crate::git::{GitError, Repo};
use crate::store::StoreError;
use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance, Store};

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
    /// Reading a working-tree file failed (dirty overlay).
    #[error("worktree io error: {0}")]
    Io(#[from] std::io::Error),
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
    /// Working-tree files whose uncommitted content overrode the committed blob
    /// (the dirty overlay); always zero for a committed-only [`sync`].
    pub blobs_dirty: usize,
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

    // The extractor environment (installed image models + ingestion toggles) is
    // part of extraction's identity. Recorded with the tree so the next sync can
    // tell whether reusing the previous facts (the incremental path) is sound.
    let env = format!("{:016x}", extractor.env_tag());

    // Fast path: if the last sync was a committed one at a known tree with the
    // same env, update only the paths that changed. Falls back to a full
    // re-extraction on any doubt (no prior tree, env changed, diff unavailable).
    if let Some(report) = try_incremental(store, repo, cache, extractor, &tree, &env)? {
        return Ok(report);
    }

    let committed = extract_committed(repo, cache, extractor)?;
    let total = committed.by_path.len();
    let mut assembled = flatten(committed.by_path);
    resolve_calls(&mut assembled);
    store.reconcile(&assembled, Some(&tree))?;
    store.set_sync_env(&env)?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: committed.extracted,
        blobs_cached: committed.cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree,
    })
}

/// Attempt an incremental committed sync from the last-synced tree to `head_tree`.
/// Returns `Ok(Some(report))` when it ran, `Ok(None)` when the fast path is not
/// eligible (the caller then does a full sync).
///
/// It is sound because it produces the exact same **derived-only** graph a full
/// sync would: it reconstructs the derived subgraph from the store (identified by
/// the `Derived` provenance tag — unchanged paths' facts are a deterministic
/// function of their unchanged blob content, so they equal a fresh extraction),
/// drops the changed/deleted paths, extracts only the changed blobs, re-resolves
/// cross-file `calls` globally, and feeds the result to the same [`Store::reconcile`]
/// the full path uses. `check`/`reapply_imports` re-layer the authored/import
/// facts afterward exactly as before — this only accelerates the derived layer.
fn try_incremental(
    store: &mut Store,
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
    head_tree: &str,
    env: &str,
) -> Result<Option<SyncReport>, SyncError> {
    // Eligibility: a prior committed tree (a plain oid — worktree/index states
    // carry a `:`-delimited marker), extracted under the same environment.
    let Some(prior_tree) = store.sync_state()? else {
        return Ok(None);
    };
    if prior_tree.contains(':') || store.sync_env()?.as_deref() != Some(env) {
        return Ok(None);
    }
    // The prior tree object may have been pruned (gc); on any diff failure, fall
    // back to the full path rather than guessing.
    let Ok(diff) = repo.diff_trees(&prior_tree, head_tree) else {
        return Ok(None);
    };

    // Reconstruct the derived subgraph from the store: every derived node, and
    // every derived edge except `calls` (globally re-derived below from the full
    // function set, since a changed file can flip name-resolution elsewhere).
    let mut nodes: Vec<Node> = store.nodes_by_provenance(Provenance::Derived)?;
    let mut edges: Vec<Edge> = store
        .edges_by_provenance(Provenance::Derived)?
        .into_iter()
        .filter(|e| e.kind != EdgeKind::Calls)
        .collect();

    // Drop the changed and deleted paths' derived facts (their nodes, and any edge
    // incident to them — per-blob derived edges are intra-file, so this is exact).
    let touched: BTreeSet<&str> = diff
        .changed
        .iter()
        .map(|b| b.path.as_str())
        .chain(diff.deleted.iter().map(String::as_str))
        .collect();
    let dropped: HashSet<String> = nodes
        .iter()
        .filter(|n| n.path.as_deref().is_some_and(|p| touched.contains(p)))
        .map(|n| n.key.clone())
        .collect();
    nodes.retain(|n| !dropped.contains(&n.key));
    edges.retain(|e| !dropped.contains(&e.src) && !dropped.contains(&e.dst));

    // Extract the changed blobs (cache-aware) and add their derived facts.
    let env_tag = extractor.env_tag();
    let mut extracted = 0usize;
    let mut cached = 0usize;
    for blob in &diff.changed {
        let key = cache_key(&blob.path, &blob.oid, env_tag);
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
        nodes.extend(facts.nodes);
        edges.extend(facts.edges);
    }

    // Prune orphaned import-target nodes — a path-less derived node (e.g.
    // `import:rust:foo`) that no surviving edge references. A full sync emits it
    // only while some file imports it, so dropping the now-unreferenced ones keeps
    // the two paths identical.
    let referenced: HashSet<&str> = edges
        .iter()
        .flat_map(|e| [e.src.as_str(), e.dst.as_str()])
        .collect();
    nodes.retain(|n| n.path.is_some() || referenced.contains(n.key.as_str()));

    // Global call resolution over the full (reconstructed + changed) function set,
    // then reconcile to derived-only — identical to what the full path produces.
    let mut assembled = FactSet { nodes, edges };
    resolve_calls(&mut assembled);
    let total = assembled
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .count();
    store.reconcile(&assembled, Some(head_tree))?;
    store.set_sync_env(env)?;

    Ok(Some(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: extracted,
        blobs_cached: cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree: head_tree.to_owned(),
    }))
}

/// Sync `store` to the working tree: the committed `HEAD` state with uncommitted
/// edits to **tracked** files overlaid on top (a pre-commit preview).
///
/// Committed blobs come from the content-addressed cache as in [`sync`]; then
/// each tracked file whose working copy differs from its committed blob is
/// re-extracted in memory (never cached, since dirty content is not a git
/// object), and deleted files are dropped. New *untracked* files are not yet
/// included. The recorded sync state encodes the dirty set, so a later
/// committed [`sync`] correctly supersedes the overlay.
///
/// # Errors
/// Returns a [`SyncError`] if git access, extraction caching, working-tree I/O,
/// or the store rebuild fails.
pub fn sync_worktree(
    store: &mut Store,
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
) -> Result<SyncReport, SyncError> {
    let tree = repo.head_tree_id()?;
    let committed = extract_committed(repo, cache, extractor)?;
    let total = committed.by_path.len();
    let mut by_path = committed.by_path;

    // Overlay uncommitted edits to tracked files. A file is dirty when its
    // working-copy content hashes to a different git blob id than the committed
    // one; identical content hashes identically, so clean files are skipped.
    let mut dirty: BTreeSet<(String, String)> = BTreeSet::new();
    if let Some(workdir) = repo.workdir() {
        for blob in &committed.blobs {
            match std::fs::read(workdir.join(&blob.path)) {
                Ok(bytes) => {
                    let woid = repo.blob_oid(&bytes)?;
                    if woid != blob.oid {
                        by_path.insert(
                            blob.path.clone(),
                            extractor.extract(&blob.path, &woid, &bytes),
                        );
                        dirty.insert((blob.path.clone(), woid));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    by_path.remove(&blob.path);
                    dirty.insert((blob.path.clone(), "\0deleted".to_owned()));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    // Encode the dirty set into the sync state so repeated identical previews
    // no-op, but any committed change (which alters the plain tree id) does not.
    let state = if dirty.is_empty() {
        tree.clone()
    } else {
        let mut buf = String::new();
        for (path, marker) in &dirty {
            buf.push_str(path);
            buf.push('\0');
            buf.push_str(marker);
            buf.push('\n');
        }
        format!("{tree}:dirty:{:016x}", fnv1a64(buf.as_bytes()))
    };
    let dirty_count = dirty.len();

    if store.sync_state()?.as_deref() == Some(state.as_str()) {
        return Ok(SyncReport {
            no_op: true,
            blobs_total: total,
            blobs_dirty: dirty_count,
            nodes: store.node_count()?,
            edges: store.edge_count()?,
            tree,
            ..SyncReport::default()
        });
    }

    let mut assembled = flatten(by_path);
    resolve_calls(&mut assembled);
    store.reconcile(&assembled, Some(&state))?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: committed.extracted,
        blobs_cached: committed.cached,
        blobs_dirty: dirty_count,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree,
    })
}

/// Sync `store` to the **git index** — the staged tree that a commit would
/// record. Unlike [`sync_worktree`] (files on disk) this reads each staged blob
/// by its index object id, so it validates *exactly what is about to be
/// committed* (partially-staged changes and all). New staged files are included;
/// unstaged working-tree edits are not. Backs the index-aware pre-commit gate.
///
/// # Errors
/// Returns a [`SyncError`] if git access, extraction caching, or the store
/// reconcile fails.
pub fn sync_index(
    store: &mut Store,
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
) -> Result<SyncReport, SyncError> {
    let staged = repo.index_files()?;
    // A stable state id over the staged (path, oid) set, in its own `index:`
    // namespace so it never collides with a committed tree id or a worktree dirty
    // marker — repeated identical index syncs then no-op, while any staged change
    // does not.
    let mut buf = String::new();
    for blob in &staged {
        buf.push_str(&blob.path);
        buf.push('\0');
        buf.push_str(&blob.oid);
        buf.push('\n');
    }
    let state = format!("index:{:016x}", fnv1a64(buf.as_bytes()));

    if store.sync_state()?.as_deref() == Some(state.as_str()) {
        return Ok(SyncReport {
            no_op: true,
            blobs_total: staged.len(),
            nodes: store.node_count()?,
            edges: store.edge_count()?,
            tree: state,
            ..SyncReport::default()
        });
    }

    let extracted = extract_blobs(repo, cache, extractor, staged)?;
    let total = extracted.by_path.len();
    let mut assembled = flatten(extracted.by_path);
    resolve_calls(&mut assembled);
    store.reconcile(&assembled, Some(&state))?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: extracted.extracted,
        blobs_cached: extracted.cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree: state,
    })
}

/// The committed fact sets for the `HEAD` tree, one per path, plus the blob list
/// (for overlay comparison) and cache-hit/miss counts.
struct Committed {
    blobs: Vec<crate::BlobRef>,
    by_path: BTreeMap<String, FactSet>,
    extracted: usize,
    cached: usize,
}

/// Extract (or load from cache) the fact set for every blob in the `HEAD` tree.
fn extract_committed(
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
) -> Result<Committed, SyncError> {
    extract_blobs(repo, cache, extractor, repo.walk_blobs()?)
}

/// Extract (or load from cache) the fact set for each blob in `blobs` — the
/// shared core of [`extract_committed`] and [`sync_index`], differing only in
/// which tree the blob list comes from (`HEAD` vs the git index).
fn extract_blobs(
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
    blobs: Vec<crate::BlobRef>,
) -> Result<Committed, SyncError> {
    let mut by_path = BTreeMap::new();
    let mut extracted = 0usize;
    let mut cached = 0usize;

    // Extraction output depends on runtime state beyond (path, bytes): which
    // image models are installed, and the extractor's ingestion toggles. The
    // extractor folds both into a single tag for the cache key. Computed once
    // per sync.
    let env = extractor.env_tag();

    for blob in &blobs {
        // Extraction is a function of (path, blob bytes) and — with `image-ocr`
        // — the OCR model environment (`env`), never blob id alone: node keys are
        // path-scoped (e.g. `file:<path>`), so the same blob content at two
        // different paths yields different facts. Key the cache by (path, oid,
        // env) so duplicate-content files (e.g. empty files, which git dedupes to
        // one oid) never collide, the same path+oid in another branch/worktree
        // still hits, and installing/upgrading OCR models re-extracts images.
        let key = cache_key(&blob.path, &blob.oid, env);
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
        by_path.insert(blob.path.clone(), facts);
    }

    Ok(Committed {
        blobs,
        by_path,
        extracted,
        cached,
    })
}

/// Concatenate per-path fact sets into one assembled fact set.
fn flatten(by_path: BTreeMap<String, FactSet>) -> FactSet {
    let mut assembled = FactSet::new();
    for facts in by_path.into_values() {
        assembled.nodes.extend(facts.nodes);
        assembled.edges.extend(facts.edges);
    }
    assembled
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
/// as the leading, well-distributed shard) suffixed with a stable 64-bit hash of
/// the path, the [`crate::extract::EXTRACT_VERSION`], and the extractor
/// environment tag `env` (the installed image-model — OCR + vision — identity;
/// `0` when no image model is active — see [`crate::extract::image_env_tag`]).
/// Sharing across branches/worktrees is preserved (same path+oid+version+env →
/// same key) while duplicate content at distinct paths stays distinct; bumping
/// the extractor version *or* changing the installed image models retires old
/// entries so a re-extraction is forced.
fn cache_key(path: &str, oid: &str, env: u64) -> String {
    format!(
        "{oid}-{:016x}-v{}-e{env:016x}",
        fnv1a64(path.as_bytes()),
        crate::extract::EXTRACT_VERSION,
    )
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
        // Same path + oid + env is stable across calls.
        assert_eq!(cache_key("src/a.rs", oid, 0), cache_key("src/a.rs", oid, 0));
        // Same blob content (oid) at two different paths must not collide.
        assert_ne!(cache_key("src/a.rs", oid, 0), cache_key("src/b.rs", oid, 0));
        // Different content at the same path differs too.
        assert_ne!(
            cache_key("src/a.rs", "aaa", 0),
            cache_key("src/a.rs", "bbb", 0)
        );
        // A different extractor environment (e.g. OCR models installed) differs,
        // so image facts are re-extracted when the models change.
        assert_ne!(
            cache_key("src/a.rs", oid, 0),
            cache_key("src/a.rs", oid, 42)
        );
        // Key stays sharded on the oid so the cache's 2-char shard is well spread.
        assert!(cache_key("src/a.rs", oid, 0).starts_with("abc123-"));
        // The extractor version is folded in, so a bump retires old entries.
        assert!(
            cache_key("src/a.rs", oid, 0)
                .contains(&format!("-v{}", crate::extract::EXTRACT_VERSION))
        );
    }
}
