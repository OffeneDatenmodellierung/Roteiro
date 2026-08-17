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

use crate::cache::{CacheError, ObjectCache, ObjectSweep};
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
    /// Source files reflected in the graph — one `File` node per extracted blob.
    /// Derived from the assembled graph (not the raw tree walk) so full and
    /// incremental syncs report the same total for the same tree.
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
    /// The working tree this graph previously described, when it was a
    /// **different** one and the sync therefore rebuilt from scratch rather than
    /// trusting the recorded state (issue #330).
    ///
    /// `None` on every ordinary sync. `Some(path)` is the loud half of the
    /// guarantee: the answer was corrected rather than served, and the caller can
    /// say *which* tree the store had been holding, so a stale store is never a
    /// silent wrong answer nor an unexplained slow one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuilt_from_foreign_worktree: Option<String>,
}

/// A stable identity for the working tree a graph is assembled from: the
/// working-tree root, or the git dir for a bare repository.
///
/// The *path* is used rather than an opaque id because its whole job is to appear
/// in a message naming the tree the store actually holds — an id the reader
/// cannot act on would defeat the point. Linked worktrees have distinct roots, so
/// this separates them; a plain branch switch within one tree does not change it,
/// which is correct (the tree is the same, its content moved).
#[must_use]
pub fn worktree_id(repo: &Repo) -> String {
    repo.workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_string_lossy()
        .into_owned()
}

/// Decide whether `store`'s recorded sync state may be trusted for *this* tree.
///
/// Returns `Some(previous)` when the store was last assembled from a **different**
/// working tree: its tree id, dirty-set hash and extraction env all describe
/// someone else's tree, so no fast path may consult them and the caller must
/// rebuild in full. `None` when the store belongs here or has never been stamped
/// (unknown is adopted, not rebuilt — see [`Store::synced_worktree`]).
///
/// Rebuilding is cheap relative to being wrong: the object cache is shared across
/// worktrees and already warm, so the re-extraction mostly hits it.
fn foreign_worktree(store: &Store, repo: &Repo) -> Result<Option<String>, SyncError> {
    let here = worktree_id(repo);
    Ok(store.synced_worktree()?.filter(|prior| *prior != here))
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

    // The extraction *identity*: the extractor code version (`EXTRACT_VERSION`,
    // bumped when extraction output changes) plus its environment (installed image
    // models + ingestion toggles). Both change what an unchanged file extracts to,
    // and both are folded into the content-cache key — so this mirrors that key.
    // Recorded with the tree so the next sync can tell whether reusing the stored
    // facts (the incremental path) is sound; a binary upgrade that bumps the
    // version, or a model change, invalidates it and forces a full re-extraction.
    let env = format!(
        "v{}-e{:016x}",
        crate::extract::EXTRACT_VERSION,
        extractor.env_tag()
    );

    // Nothing to do only when **both** the tree and the extraction identity are
    // unchanged.
    //
    // The identity half is load-bearing, and its absence was a real hole: an
    // `EXTRACT_VERSION` bump is supposed to guarantee that no user is served the
    // previous version's facts, but a store already synced at the current `HEAD`
    // returned `no_op` here before the identity was ever computed — so the new
    // binary's facts appeared only once `HEAD` next moved. Enabling a feature that
    // changes extraction output (`audio-metadata`, `pdf-text`, `image-ocr`) on a
    // quiet repository therefore looked like it had done nothing at all. Every
    // *other* consumer of the identity — the content-cache key, the incremental
    // path below — already agreed on it; this one had simply never been asked.
    //
    // A store with no recorded identity (`None`) does not match, which is the safe
    // direction: it re-extracts once and records one.
    // …and only when the recorded state describes *this* working tree. A store
    // assembled from another tree has a tree id, dirty hash and env that are all
    // someone else's, so neither the no-op below nor the incremental diff may
    // consult them: that is how a stale store reports "up to date" while holding
    // a graph nobody is looking at (issue #330).
    let foreign = foreign_worktree(store, repo)?;

    if foreign.is_none()
        && store.sync_state()?.as_deref() == Some(tree.as_str())
        && store.sync_env()?.as_deref() == Some(env.as_str())
    {
        return Ok(SyncReport {
            no_op: true,
            nodes: store.node_count()?,
            edges: store.edge_count()?,
            tree,
            ..SyncReport::default()
        });
    }

    // Fast path: if the last sync was a committed one at a known tree with the
    // same extraction identity, update only the paths that changed. Falls back to
    // a full re-extraction on any doubt (no prior tree, identity changed, an
    // unavailable diff, or a tree that is not ours).
    if foreign.is_none()
        && let Some(report) = try_incremental(store, repo, cache, extractor, &tree, &env)?
    {
        return Ok(report);
    }

    let committed = extract_committed(repo, cache, extractor)?;
    let mut assembled = flatten(committed.by_path);
    resolve_calls(&mut assembled);
    append_submodule_nodes(repo.submodules()?, &mut assembled);
    let total = file_count(&assembled);
    store.reconcile(&assembled, Some(&tree))?;
    store.set_sync_env(&env)?;
    store.set_synced_worktree(&worktree_id(repo))?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: committed.extracted,
        blobs_cached: committed.cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree,
        rebuilt_from_foreign_worktree: foreign,
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
    append_submodule_nodes(repo.submodules()?, &mut assembled);
    let total = file_count(&assembled);
    store.reconcile(&assembled, Some(head_tree))?;
    store.set_sync_env(env)?;
    // The caller only reaches here for a store that is ours, but it may predate
    // the stamp — record it, or this path would leave it unstamped forever.
    store.set_synced_worktree(&worktree_id(repo))?;

    Ok(Some(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: extracted,
        blobs_cached: cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree: head_tree.to_owned(),
        // Unreachable with a foreign store: the caller skips this path entirely.
        rebuilt_from_foreign_worktree: None,
    }))
}

/// Sync `store` to the working tree: the committed `HEAD` state with uncommitted
/// working-tree changes overlaid on top (a pre-commit preview).
///
/// Committed blobs come from the content-addressed cache as in [`sync`]; then
/// each tracked file whose working copy differs from its committed blob is
/// re-extracted in memory (never cached, since dirty content is not a git
/// object), deleted files are dropped, and brand-new **untracked** files (found
/// via a gitignore-aware dirwalk, [`Repo::untracked_files`]) are overlaid in.
/// The recorded sync state encodes the dirty set, so a later committed [`sync`]
/// correctly supersedes the overlay.
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

        // Overlay brand-new untracked files: not in `HEAD`, so absent from
        // `committed.blobs` above. A gitignore-aware walk finds them so the
        // working-tree `sync`/`check`/`review` see new work that isn't staged yet.
        // They count as dirty (so the preview re-runs when they change) and add to
        // the blob total (they are genuinely new blobs, not edits of existing ones).
        for path in repo.untracked_files()? {
            match std::fs::read(workdir.join(&path)) {
                Ok(bytes) => {
                    let woid = repo.blob_oid(&bytes)?;
                    by_path.insert(path.clone(), extractor.extract(&path, &woid, &bytes));
                    dirty.insert((path, woid));
                }
                // Raced away between the walk and the read — nothing to add.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    // The blob total is the file count of the *overlaid* graph — committed files,
    // minus working-tree deletions, plus untracked additions — not the committed
    // baseline, so it stays consistent whether files were added or removed.
    let total = by_path.len();

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

    // A dirty-set hash computed for another tree says nothing about this one, so
    // a foreign store may never no-op here (issue #330).
    let foreign = foreign_worktree(store, repo)?;
    if foreign.is_none() && store.sync_state()?.as_deref() == Some(state.as_str()) {
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
    append_submodule_nodes(repo.submodules()?, &mut assembled);
    store.reconcile(&assembled, Some(&state))?;
    store.set_synced_worktree(&worktree_id(repo))?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: committed.extracted,
        blobs_cached: committed.cached,
        blobs_dirty: dirty_count,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree,
        rebuilt_from_foreign_worktree: foreign,
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

    // An index hash from another tree describes another index (issue #330).
    let foreign = foreign_worktree(store, repo)?;
    if foreign.is_none() && store.sync_state()?.as_deref() == Some(state.as_str()) {
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
    // Index mode is "exactly what a commit would record", so submodule pins come
    // from the *staged* gitlinks, not `HEAD` — a staged bump is reflected.
    append_submodule_nodes(repo.index_submodules()?, &mut assembled);
    store.reconcile(&assembled, Some(&state))?;
    store.set_synced_worktree(&worktree_id(repo))?;

    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: extracted.extracted,
        blobs_cached: extracted.cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree: state,
        rebuilt_from_foreign_worktree: foreign,
    })
}

/// Extract a repo's **derived graph at an arbitrary commit/tree `rev`** into
/// `store`, replacing its contents — the same content-addressed extraction as
/// [`sync`], but for a historical point rather than `HEAD`. Because extraction is
/// keyed by `(path, blob oid, env)`, every blob unchanged versus another synced
/// point is a cache hit, so resolving an older version only re-does what differs.
///
/// This backs **version-pin resolution** (ADR-0009 step 8): to resolve a spoke's
/// cross-repo reference against the hub *version it deploys* (a submodule sha,
/// an image tag → commit), extract the hub at that `rev` into an ephemeral store
/// and resolve there. It populates the derived layer only (config keys, symbols,
/// calls); authored/import layers are not re-applied, since this is a read-only
/// resolution snapshot. No sync-state is recorded (`tree` carries `rev` for the
/// report only).
///
/// # Errors
/// Returns [`SyncError`] on git access, extraction caching, or store failure.
pub fn sync_tree(
    store: &mut Store,
    repo: &Repo,
    cache: &ObjectCache,
    extractor: &dyn Extractor,
    rev: &str,
) -> Result<SyncReport, SyncError> {
    let extracted = extract_blobs(repo, cache, extractor, repo.blobs_at(rev)?)?;
    let mut assembled = flatten(extracted.by_path);
    resolve_calls(&mut assembled);
    append_submodule_nodes(repo.submodules_at(rev)?, &mut assembled);
    let total = file_count(&assembled);
    store.rebuild(&assembled, None)?;
    Ok(SyncReport {
        no_op: false,
        blobs_total: total,
        blobs_extracted: extracted.extracted,
        blobs_cached: extracted.cached,
        blobs_dirty: 0,
        nodes: store.node_count()?,
        edges: store.edge_count()?,
        tree: rev.to_owned(),
        // A historical-rev store deliberately records no synced state at all
        // (`rebuild(.., None)` clears the row), so it is stamped with no tree
        // either — it is a scratch view of a commit, not of a working tree.
        rebuilt_from_foreign_worktree: None,
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

/// The `NodeKind::Other` token for a submodule-pin node (`submodule:<path>`).
pub(crate) const SUBMODULE_KIND: &str = "submodule";

/// Append the given submodule-pin nodes to `assembled`, replacing any already
/// present. `subs` is the caller's source-appropriate list — `repo.submodules()`
/// (the `HEAD` tree) for committed/worktree syncs, `repo.index_submodules()` (the
/// staged gitlinks) for the index-aware pre-commit gate. A submodule pin is a
/// **tree-level** derived fact (a gitlink + its `.gitmodules` URL, ADR-0009), not
/// a per-blob one, so it is recomputed on every sync rather than cached. Removing
/// any existing submodule nodes first makes the
/// incremental path — which reconstructs derived nodes from the store — produce
/// exactly the full sync's result: an unchanged pin re-adds identically, a bumped
/// pin's new sha wins, and a removed submodule leaves none behind. The nodes carry
/// `path = .gitmodules` (so a `.gitmodules` deletion drops them) and stand alone
/// (no edges — nothing in the graph is their guaranteed endpoint).
fn append_submodule_nodes(subs: Vec<crate::Submodule>, assembled: &mut FactSet) {
    let kind = NodeKind::Other(SUBMODULE_KIND.to_owned());
    assembled.nodes.retain(|n| n.kind != kind);
    for sm in subs {
        let key = format!("submodule:{}", sm.path);
        let mut node = Node::new(key, kind.clone(), sm.path.clone());
        node.path = Some(".gitmodules".to_owned());
        node.provenance = Provenance::Derived;
        node.meta = serde_json::json!({ "path": sm.path, "url": sm.url, "sha": sm.sha });
        assembled.nodes.push(node);
    }
}

/// The number of source files reflected in an assembled fact set (one `File`
/// node per extracted blob). Both the full and incremental sync paths derive
/// `SyncReport::blobs_total` from the *assembled graph* this way — not from the
/// raw blob list — so the two paths report the same total for the same tree (the
/// graphs are identical; see the equivalence test).
fn file_count(facts: &FactSet) -> usize {
    facts
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .count()
}

/// Resolve the per-function call records (`meta.calls`) accumulated during
/// extraction into `calls` edges, now that every file's symbols are present.
///
/// Resolution is deliberately conservative — it links a call only when the target
/// is **unambiguous** — but scope-aware: a callee descriptor may carry the
/// immediate qualifier the call site provided (`b::foo`, `Type::assoc`,
/// `Self::method`; see [`crate::extract`]). A call resolves when either
///
/// 1. its simple name is unique across the whole tree (the base case), or
/// 2. its name is ambiguous but a qualifier picks out **exactly one** matching
///    function — the one whose immediate scope segment equals that qualifier
///    (with `Self` bound to the caller's own impl type).
///
/// This never links a name it could not before (it is a strict superset), and it
/// still refuses to guess when a qualifier leaves more than one candidate. Runs at
/// assembly time — not per blob — since a single blob cannot see other files.
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
        // The caller's own type (for binding `Self::` calls) is the scope segment
        // immediately before its name in its key, if it is a method.
        let caller_self = self_type_of(&n.key);
        for descriptor in calls.iter().filter_map(|v| v.as_str()) {
            let (qualifier, name) = split_callee(descriptor);
            let Some(candidates) = by_name.get(name) else {
                continue;
            };
            let target = if candidates.len() == 1 {
                // Unambiguous by simple name — the base case (unchanged behaviour).
                Some(candidates[0])
            } else if let Some(q) = qualifier {
                // Ambiguous name; try the qualifier. `Self` binds to the caller's
                // impl type — a free function has none, so such a call stays open.
                let want = if q == "Self" { caller_self } else { Some(q) };
                want.and_then(|want| unique_in_scope(candidates, want, name))
            } else {
                None
            };
            if let Some(dst) = target {
                resolved.insert((n.key.clone(), dst.to_owned()));
            }
        }
    }

    for (src, dst) in resolved {
        facts.edges.push(Edge::derived(src, dst, EdgeKind::Calls));
    }
}

/// The qualified suffix of a symbol key (`sym:<lang>:<path>#<qualified>` →
/// `<qualified>`), i.e. the scope-segment path within its file.
fn qualified_suffix(key: &str) -> &str {
    key.rsplit_once('#').map_or(key, |(_, q)| q)
}

/// The caller's own type for binding a `Self::` call: the scope segment
/// immediately before the function's name in its key (`Type::method` → `Type`),
/// or `None` for a free function (no enclosing type).
fn self_type_of(key: &str) -> Option<&str> {
    let mut segs = qualified_suffix(key).rsplit("::");
    segs.next()?; // the function's own name
    segs.next() // the enclosing scope segment, if any
}

/// The single candidate whose immediate scope segment is `want` (so its key ends
/// with the `want::name` segment pair), or `None` when zero or several match —
/// segment-aware so `T::m` matches `a::T::m` but never `XT::m`.
fn unique_in_scope<'a>(candidates: &[&'a str], want: &str, name: &str) -> Option<&'a str> {
    let mut hit = None;
    for &key in candidates {
        let mut segs = qualified_suffix(key).rsplit("::");
        if segs.next() == Some(name) && segs.next() == Some(want) {
            if hit.is_some() {
                return None; // more than one match at this scope — refuse to guess
            }
            hit = Some(key);
        }
    }
    hit
}

/// Split a `meta.calls` descriptor into its immediate qualifier and simple name:
/// `b::foo` → `(Some("b"), "foo")`, `foo` → `(None, "foo")`.
fn split_callee(descriptor: &str) -> (Option<&str>, &str) {
    match descriptor.rsplit_once("::") {
        Some((qualifier, name)) => (Some(qualifier), name),
        None => (None, descriptor),
    }
}

/// Content-addressed cache key for a blob at a given path: the blob oid (kept
/// as the leading, well-distributed shard) suffixed with a stable 64-bit hash of
/// the path, the [`crate::extract::EXTRACT_VERSION`], and the extractor
/// environment tag `env` (the installed media-model — OCR + vision + audio —
/// identity; `0` when no media model is active — see
/// [`crate::extract::media_env_tag`]). Sharing across branches/worktrees is
/// preserved (same path+oid+version+env → same key) while duplicate content at
/// distinct paths stays distinct; bumping the extractor version *or* changing the
/// installed media models retires old entries so a re-extraction is forced.
fn cache_key(path: &str, oid: &str, env: u64) -> String {
    format!(
        "{oid}-{:016x}-v{}-e{env:016x}",
        fnv1a64(path.as_bytes()),
        crate::extract::EXTRACT_VERSION,
    )
}

/// How many superseded extractor generations [`sweep_superseded`] keeps behind
/// the current one by default: **one**.
///
/// Not clutter, and not free — it is a trade against the one workflow this
/// project actually has. Roteiro is developed *inside* the repository it indexes,
/// so a branch that bumps [`crate::extract::EXTRACT_VERSION`] and the `main` it
/// will merge into share one `.git/roteiro` (the cache is under the **common**
/// git dir). With no retention, one maintenance pass on the branch deletes
/// `main`'s whole live set, and every switch back pays a full cold extraction;
/// keeping the previous generation makes that switch free. Rolling a release back
/// one version gets the same protection as a side effect.
///
/// It is bounded, which is the part that matters: the complaint being answered
/// (#387) is *unbounded* accumulation — four generations resident and counting —
/// and the steady state here is two, whatever happens next.
pub const DEFAULT_KEEP_GENERATIONS: u32 = 1;

/// Delete the object-cache entries left behind by **superseded** extractor
/// generations, keeping the current one and `keep_generations` behind it.
///
/// # Why a sweep and not a byte budget
///
/// Because a proof is available here and nowhere else. [`cache_key`] writes the
/// extractor generation into every key, and that generation only ever moves
/// forward, so an entry tagged with an older one *cannot be asked for* by any
/// binary at or beyond the current generation — no bookkeeping, no recency, no
/// guessing. A byte budget (the Stage 25 / `rto-llama` `ModelCache` precedent,
/// ported to disk by [`crate::Store::sweep_agent_cache`]) would have had to
/// invent an ordering over live entries and would then evict *reachable* ones by
/// design: on a cache shared by every worktree that means one worktree silently
/// paying for another's working set, and it would need a last-used column this
/// store has no clock to fill (ADR-0013 §3). It buys a bound this does not give —
/// the live set itself is unbounded, and a repository large enough for that to
/// hurt still needs one. That is a second policy on top of this one, not an
/// alternative to it, and nothing has yet measured a need for it.
///
/// # What "superseded" is allowed to mean
///
/// **Only the generation**, i.e. [`crate::extract::EXTRACT_BASE_VERSION`]. The
/// other two things folded into a key are deliberately *not* eligible:
///
/// - The **feature namespace** ([`crate::extract::FEATURE_NAMESPACE_STRIDE`] and
///   above). A default build and an `--all-features` build write different
///   `EXTRACT_VERSION`s at the *same* generation, and both are live at once —
///   `cargo test --workspace` and `cargo test --all-features` on one repository
///   are exactly that. Sweeping on the whole version number would have each build
///   delete the other's cache on sight, and the two would take turns
///   re-extracting for ever. So the namespace is masked off, and every namespace
///   at a kept generation is kept.
/// - The **environment tag** (`-e…`: the installed media-model and ingestion
///   identity). It is a hash — unordered, so no tag can be shown to supersede
///   another, and several are legitimately live at once (a build without
///   `image-ocr` tags `0`; a build with it and a model installed does not).
///   Reclaiming those would need the ordering the paragraph above rejected. They
///   are left alone, and the cost of that is stated rather than hidden: env churn
///   *within* one generation is not reclaimed by this pass.
///
/// # Why this is safe while other worktrees are live
///
/// The rule reads only the key, never the repository — so it does not need to
/// know what any other worktree has checked out, and cannot be wrong about it. A
/// reachability rule phrased over *blob ids* would need exactly that knowledge,
/// and would be the dangerous version of this function: an oid unreachable from
/// one worktree's `HEAD` is routinely live in another's. This one never asks.
///
/// Its only cross-worktree effect is on a worktree running an **older** binary,
/// which it can cost a re-extraction and nothing else — the cache is derived, so
/// a miss is slow, never wrong. The asymmetry runs one way: an entry from a
/// *newer* generation than the sweeper's is retained, because `generation >=
/// oldest_kept` holds for anything ahead. Two binaries of different ages can
/// therefore never take turns deleting each other's work.
///
/// # Errors
/// Returns [`CacheError`] if the cache cannot be listed. See
/// [`ObjectCache::sweep`] for what a failure to delete an individual entry does
/// (it is counted, not raised).
pub fn sweep_superseded(
    cache: &ObjectCache,
    keep_generations: u32,
) -> Result<ObjectSweep, CacheError> {
    let oldest_kept = crate::extract::EXTRACT_BASE_VERSION.saturating_sub(keep_generations);
    cache.sweep(&|key| match key_generation(key) {
        // Not a key this module writes — a foreign or future format. Unreadable
        // is not the same as unreachable, and only one of the two may be deleted.
        None => true,
        Some(generation) => generation >= oldest_kept,
    })
}

/// The extractor **generation** encoded in a [`cache_key`] key, or `None` if the
/// key does not carry one in the exact shape `cache_key` writes.
///
/// The parse is strict on purpose: this is the predicate a delete hangs off, so
/// every doubt has to resolve to `None`, which retains. It therefore requires the
/// whole `-v<digits>-e<16 hex digits>` tail, rejects a sign that `u32::from_str`
/// would otherwise accept (`+12`), and rejects an environment tag of the wrong
/// width — anything merely *shaped like* a key is left alone.
fn key_generation(key: &str) -> Option<u32> {
    let (head, env) = key.rsplit_once("-e")?;
    if env.len() != 16 || !env.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (_, version) = head.rsplit_once("-v")?;
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Mask off the feature namespace; what remains is the generation. Sound while
    // the base stays below the stride, which `extract.rs` asserts at compile time.
    Some(version.parse::<u32>().ok()? % crate::extract::FEATURE_NAMESPACE_STRIDE)
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
    use super::{ObjectCache, cache_key, key_generation, resolve_calls};
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

    /// The sweep predicate's one input. The round trip is what makes the sweep
    /// safe: a key this module just wrote must decode to *this* generation, or a
    /// pass at the current version would delete its own live entries.
    #[test]
    fn key_generation_round_trips_the_key_this_module_writes() {
        let key = cache_key("src/a.rs", "abc123", 0);
        assert_eq!(
            key_generation(&key),
            Some(crate::extract::EXTRACT_BASE_VERSION),
            "a key written now decodes to the current generation: {key}",
        );
        // …and so does the same generation in another feature build's namespace,
        // which is the whole reason the namespace is masked off rather than
        // compared. Both are live at once on a machine that runs the default and
        // `--all-features` test suites over one repository.
        let base = crate::extract::EXTRACT_BASE_VERSION;
        for namespace in [100, 200, 300, 400, 500, 600, 700] {
            let other = format!(
                "abc123-0000000000000000-v{}-e0000000000000000",
                base + namespace
            );
            assert_eq!(
                key_generation(&other),
                Some(base),
                "namespace {namespace} is not a different generation",
            );
        }
    }

    /// Every doubt resolves to `None`, and `None` retains. These are the strings
    /// that must *not* be read as a generation — each one would otherwise put a
    /// file nobody can identify in reach of a delete.
    #[test]
    fn key_generation_refuses_anything_it_did_not_write() {
        for not_a_key in [
            "",
            "abc123",                                                 // no tail at all
            "abc123-0000000000000000-v12",                            // no env tag
            "abc123-0000000000000000-e0000000000000000",              // no version tag
            "abc123-0000000000000000-v12-e00000000000000",            // env too short
            "abc123-0000000000000000-v12-e00000000000000000",         // env too long
            "abc123-0000000000000000-v12-egggggggggggggggg",          // env not hex
            "abc123-0000000000000000-v+12-e0000000000000000",         // `+12` parses as 12
            "abc123-0000000000000000-v-e0000000000000000",            // empty version
            "abc123-0000000000000000-v1 2-e0000000000000000",         // not all digits
            "abc123-0000000000000000-v99999999999-e0000000000000000", // overflows u32
        ] {
            assert_eq!(
                key_generation(not_a_key),
                None,
                "`{not_a_key}` must not be read as a generation",
            );
        }
    }

    /// The sweep's contract, on a cache holding one entry per generation and
    /// namespace: the current generation survives in **every** namespace, the
    /// retained generations survive, older ones go, and a *newer* one — written
    /// by a binary ahead of this one sharing the same common git dir — is never
    /// touched, whatever the retention.
    #[test]
    fn sweep_superseded_keeps_current_future_and_kept_generations() {
        let base = crate::extract::EXTRACT_BASE_VERSION;
        let dir = std::env::temp_dir().join(format!("roteiro-gc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = ObjectCache::open(&dir).expect("open");

        let key = |version: u32| format!("abc123-0000000000000000-v{version}-e0000000000000000");
        let ancient = key(base - 2);
        let previous = key(base - 1);
        let current = key(base);
        let current_all_features = key(base + 700);
        let future = key(base + 1);
        let foreign = "not-a-roteiro-cache-key".to_owned();
        for k in [
            &ancient,
            &previous,
            &current,
            &current_all_features,
            &future,
            &foreign,
        ] {
            cache.put(k, &FactSet::new()).expect("put");
        }

        // Keeping one generation back: only `base - 2` is unreachable.
        let swept =
            super::sweep_superseded(&cache, super::DEFAULT_KEEP_GENERATIONS).expect("sweep");
        assert_eq!(swept.removed, 1, "{swept:?}");
        assert!(!cache.contains(&ancient));
        for k in [
            &previous,
            &current,
            &current_all_features,
            &future,
            &foreign,
        ] {
            assert!(cache.contains(k), "`{k}` must survive a keep-1 sweep");
        }

        // Keeping none: the previous generation goes too, and nothing else does.
        let swept = super::sweep_superseded(&cache, 0).expect("sweep");
        assert_eq!(swept.removed, 1, "{swept:?}");
        assert!(!cache.contains(&previous));
        for k in [&current, &current_all_features, &future, &foreign] {
            assert!(cache.contains(k), "`{k}` must survive a keep-0 sweep");
        }

        // A repeat pass is a no-op: nothing reachable is ever swept "eventually".
        let swept = super::sweep_superseded(&cache, 0).expect("sweep");
        assert_eq!(swept.removed, 0, "{swept:?}");
        assert_eq!(swept.retained, 4, "{swept:?}");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
