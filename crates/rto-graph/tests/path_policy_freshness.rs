//! **The sync engine must honour `[paths]` on every path that assembles a
//! graph** — the committed sync, the worktree sync, the index sync, the
//! read-only freshness check, and the submodule nodes appended after extraction
//! (ADR-0007 `[paths]`, issue #840).
//!
//! # Why this file exists rather than one more case in the CLI test
//!
//! The end-to-end test in `roteiro` proves the property through `roteiro export`,
//! which builds from `GraphSource::Committed`. That is **one projection** of the
//! guarantee, and the other three were where the holes were: `sync_worktree` and
//! `sync_index` keyed their no-op on tree/dirty/index state alone, and
//! `rto_spec::tool_check` treated `sync_state == HEAD` as current. Each would
//! have reported "up to date" over a graph still holding everything the
//! declaration was written to remove — silently, which is the only way this
//! fails.
//!
//! The fourth — the read-only `tool_check` — is asserted in `rto-spec`'s own
//! `path_policy_freshness.rs`, because it lives in the crate that depends on
//! this one and the dependency cannot run the other way.
//!
//! A guard that samples one projection while a live hole sits beside it is a
//! shape this repository has paid for repeatedly, so the assertion is made
//! against **each** entry point by name rather than against whichever one is
//! convenient to reach.
//!
//! # The property, stated once
//!
//! A `[paths]` declaration changes what an **unchanged blob** extracts to. Every
//! surface that can answer "nothing to do" therefore has to compare the
//! extraction *identity*, not merely the tree — and every one of them must
//! reconcile when it differs.

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{
    IngestConfig, ObjectCache, PathPolicy, Registry, Repo, Store, sync, sync_index, sync_worktree,
};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-pathfresh-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Manifest-shaped JSON, so `config_key` mining fires on it without a
/// declaration — the fact whose disappearance each assertion below measures.
const MANIFEST: &str = r#"{"papers": {"a": {"title": "A Paper", "sha256": "0"}}}"#;

/// A repository holding one ordinary source file and one data manifest.
fn fixture(name: &str) -> (PathBuf, Repo, ObjectCache) {
    let dir = fresh_dir(name);
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "manifest/papers.json", MANIFEST);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let repo = Repo::discover(&dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");
    (dir, repo, cache)
}

/// The policy that declares the manifest opaque.
fn opaque_manifest() -> PathPolicy {
    PathPolicy::new(Vec::new(), vec!["manifest/**".to_owned()])
}

/// How many `config_key` nodes the store holds for the manifest — the mined
/// facts a declaration is written to remove.
fn mined(store: &Store) -> usize {
    store
        .all_nodes()
        .expect("nodes")
        .into_iter()
        .filter(|n| n.key.starts_with("cfgkey:manifest/"))
        .count()
}

/// `sync` — the committed path, which has always compared the identity. Asserted
/// anyway, as the control: the three below are only meaningful if this one holds.
#[test]
fn the_committed_sync_reconciles_when_the_policy_changes() {
    let (dir, repo, cache) = fixture("committed");
    let mut store = Store::open_in_memory().expect("store");

    let open = Registry::new(IngestConfig::default());
    let first = sync(&mut store, &repo, &cache, &open).expect("cold");
    assert!(!first.no_op);
    assert!(mined(&store) > 0, "the manifest is mined without a policy");

    // Same tree, same blobs — only the declaration differs.
    let policy = opaque_manifest();
    let declared = Registry::new(IngestConfig::default().with_paths(&policy));
    let second = sync(&mut store, &repo, &cache, &declared).expect("declared");
    assert!(!second.no_op, "a changed policy is not a no-op");
    assert_eq!(mined(&store), 0, "and the mined facts are gone");

    // Re-running under the *same* policy is a no-op again, so the identity is a
    // comparison rather than a switch that never settles.
    let third = sync(&mut store, &repo, &cache, &declared).expect("repeat");
    assert!(third.no_op, "an unchanged policy still no-ops");

    std::fs::remove_dir_all(&dir).ok();
}

/// **`sync_worktree` — the hole.** Its no-op keyed on tree and dirty state only,
/// so a declaration over an unchanged worktree returned "up to date" while the
/// graph kept every node the declaration removed. This is also the path
/// `roteiro sync`, `check` and `review` take by default, so it is the one a user
/// actually hits.
#[test]
fn the_worktree_sync_reconciles_when_the_policy_changes() {
    let (dir, repo, cache) = fixture("worktree");
    let mut store = Store::open_in_memory().expect("store");

    let open = Registry::new(IngestConfig::default());
    assert!(
        !sync_worktree(&mut store, &repo, &cache, &open)
            .expect("cold")
            .no_op
    );
    assert!(mined(&store) > 0, "mined without a policy");

    // Nothing about the worktree changes — not one byte, not one dirty file.
    let policy = opaque_manifest();
    let declared = Registry::new(IngestConfig::default().with_paths(&policy));
    let second = sync_worktree(&mut store, &repo, &cache, &declared).expect("declared");
    assert!(
        !second.no_op,
        "a changed policy over an unchanged worktree must not report `up to date`"
    );
    assert_eq!(mined(&store), 0, "and must actually drop the mined facts");

    assert!(
        sync_worktree(&mut store, &repo, &cache, &declared)
            .expect("repeat")
            .no_op,
        "an unchanged policy still no-ops"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **`sync_index` — the same hole, and a second claim.** Its no-op branch also
/// reported `blobs_total` from the *staged* set rather than from the graph, so
/// on exactly the runs that did no work it claimed excluded files were in the
/// graph. Both are asserted: reconciling, and then counting the way every
/// reconciling path counts.
#[test]
fn the_index_sync_reconciles_and_counts_the_graph_not_the_index() {
    let (dir, repo, cache) = fixture("index");
    let mut store = Store::open_in_memory().expect("store");

    let open = Registry::new(IngestConfig::default());
    assert!(
        !sync_index(&mut store, &repo, &cache, &open)
            .expect("cold")
            .no_op
    );
    assert!(mined(&store) > 0);

    let policy = PathPolicy::new(vec!["manifest/**".to_owned()], Vec::new());
    let declared = Registry::new(IngestConfig::default().with_paths(&policy));
    let second = sync_index(&mut store, &repo, &cache, &declared).expect("declared");
    assert!(!second.no_op, "a changed policy over an unchanged index");
    assert_eq!(mined(&store), 0);
    let reconciled_total = second.blobs_total;

    // The no-op run must report the same quantity the reconciling run did. It
    // used to answer with the staged-file count, which still includes the
    // excluded manifest — a wrong number visible only on the quiet runs.
    let third = sync_index(&mut store, &repo, &cache, &declared).expect("repeat");
    assert!(third.no_op);
    assert_eq!(
        third.blobs_total, reconciled_total,
        "a no-op must not count files the graph deliberately does not hold"
    );
    assert_eq!(
        reconciled_total, 1,
        "one admitted file — `src/lib.rs`; the excluded manifest is not in the graph"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **The reader that hides.** Submodule nodes are assembled from `.gitmodules`
/// *after* `flatten`, so they never pass through `Extractor::extract` and the
/// filter in `extract_blobs` cannot see them — a repository excluding
/// `vendor/**` still got a `submodule:vendor/thing` node naming the path it
/// declared out.
///
/// This is the case that shows the limit of the "omitting the policy is a
/// compile error" property the rest of this mechanism leans on: that property
/// holds for a reader which *takes* the policy, and this one derived nodes from
/// a source no reader had been threaded through at all. A type cannot make you
/// consult a rule in code that never asked for it.
///
/// The gitlink is fabricated with `update-index --cacheinfo` rather than a real
/// `git submodule add`, which would need a second repository and a network-free
/// clone; the tree entry is what `submodules_in_tree` reads and it is identical
/// either way.
#[test]
fn a_submodule_under_a_declared_path_is_not_a_node() {
    let dir = fresh_dir("submodule");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(
        &dir,
        ".gitmodules",
        "[submodule \"vendor/dep\"]\n\tpath = vendor/dep\n\turl = https://example.invalid/dep.git\n\
         [submodule \"tools/ours\"]\n\tpath = tools/ours\n\turl = https://example.invalid/ours.git\n",
    );
    git(&dir, &["add", "."]);
    // Two gitlinks: one under a path the policy will name, one outside it, so
    // the assertion measures a filter rather than an empty list.
    let fake = "0000000000000000000000000000000000000001";
    git(
        &dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{fake},vendor/dep"),
        ],
    );
    git(
        &dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{fake},tools/ours"),
        ],
    );
    git(&dir, &["commit", "-q", "-m", "submodules"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");

    let submodule_keys = |store: &Store| -> Vec<String> {
        let mut keys: Vec<String> = store
            .all_nodes()
            .expect("nodes")
            .into_iter()
            .filter(|n| n.key.starts_with("submodule:"))
            .map(|n| n.key)
            .collect();
        keys.sort();
        keys
    };

    // Without a declaration, both are nodes — or this fixture proves nothing.
    let mut open_store = Store::open_in_memory().expect("store");
    sync(
        &mut open_store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default()),
    )
    .expect("cold");
    assert_eq!(
        submodule_keys(&open_store),
        vec![
            "submodule:tools/ours".to_owned(),
            "submodule:vendor/dep".to_owned()
        ],
        "both submodules are nodes without a declaration"
    );

    // Declared out: the excluded one is gone, the other untouched.
    let policy = PathPolicy::new(vec!["vendor/**".to_owned()], Vec::new());
    let mut declared_store = Store::open_in_memory().expect("store");
    sync(
        &mut declared_store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default().with_paths(&policy)),
    )
    .expect("declared");
    assert_eq!(
        submodule_keys(&declared_store),
        vec!["submodule:tools/ours".to_owned()],
        "an excluded path contributes no submodule node, and an admitted one still does"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **The declaring file is a second key, and it means something different.**
///
/// `.gitmodules` supplies the URLs; the facts are about `vendor/dep`. Two
/// repo-relative paths, so a repository may declare either, and gating one and
/// not the other was a live defect — the only production reader in the workspace
/// where the file supplying the facts is not the file they are about.
///
/// Asserted as **two separate halves**, because a test covering only `exclude`
/// passes over a live `opaque` hole: the two classes differ precisely here, and
/// `opaque` is the subtler one.
#[test]
fn declaring_gitmodules_itself_gates_the_source_not_only_the_subject() {
    let dir = fresh_dir("gitmodules-source");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(
        &dir,
        ".gitmodules",
        "[submodule \"vendor/dep\"]\n\tpath = vendor/dep\n\turl = https://example.invalid/dep.git\n",
    );
    git(&dir, &["add", "."]);
    let fake = "0000000000000000000000000000000000000001";
    git(
        &dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{fake},vendor/dep"),
        ],
    );
    git(&dir, &["commit", "-q", "-m", "submodule"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");

    let submodule_nodes = |store: &Store| -> Vec<rto_graph::Node> {
        store
            .all_nodes()
            .expect("nodes")
            .into_iter()
            .filter(|n| n.key.starts_with("submodule:"))
            .collect()
    };

    // Undeclared: a node exists and carries the URL parsed out of `.gitmodules`.
    // Without this the two halves below would be measuring an empty tree.
    let mut open_store = Store::open_in_memory().expect("store");
    sync(
        &mut open_store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default()),
    )
    .expect("cold");
    let open = submodule_nodes(&open_store);
    assert_eq!(open.len(), 1, "one submodule node without a declaration");
    assert_eq!(
        open[0].meta["url"], "https://example.invalid/dep.git",
        "and its URL is derived from `.gitmodules`' contents"
    );

    // Half one — `exclude = [".gitmodules"]`. Every submodule node is attributed
    // to that file (`node.path`), so an excluded source contributes none.
    let excluded = PathPolicy::new(vec![".gitmodules".to_owned()], Vec::new());
    let mut ex_store = Store::open_in_memory().expect("store");
    sync(
        &mut ex_store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default().with_paths(&excluded)),
    )
    .expect("excluded");
    assert!(
        submodule_nodes(&ex_store).is_empty(),
        "an excluded `.gitmodules` contributes no node: {:?}",
        submodule_nodes(&ex_store)
            .iter()
            .map(|n| &n.key)
            .collect::<Vec<_>>()
    );

    // Half two — `opaque = [".gitmodules"]`. Identity survives; the URL does not,
    // because it is the one field derived from what the bytes *say*. Path and sha
    // are gitlink facts read out of the tree, so they stay.
    let opaque = PathPolicy::new(Vec::new(), vec![".gitmodules".to_owned()]);
    let mut op_store = Store::open_in_memory().expect("store");
    sync(
        &mut op_store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default().with_paths(&opaque)),
    )
    .expect("opaque");
    let op = submodule_nodes(&op_store);
    assert_eq!(op.len(), 1, "the gitlink is still a fact about the tree");
    assert!(
        op[0].meta["url"].is_null(),
        "but no URL may be derived from an opaque file's contents: {}",
        op[0].meta
    );
    assert_eq!(
        op[0].meta["path"], "vendor/dep",
        "while the gitlink's own facts survive"
    );

    std::fs::remove_dir_all(&dir).ok();
}
