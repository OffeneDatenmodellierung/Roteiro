//! Integration tests for the content-addressed sync engine against real git
//! fixture repositories built with the `git` CLI.
//!
//! Covers the Stage 2 Definition of Done: a cold sync populates the store; an
//! unchanged tree is a no-op that extracts nothing; a single-file change
//! re-extracts exactly one blob; and the content-addressed cache is reused
//! across independent stores (the property that makes it worktree/branch
//! correct).

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{
    EdgeKind, FileNodeExtractor, ObjectCache, Registry, Repo, Store, sync, sync_worktree,
};

/// Run `git` in `dir` with hermetic identity/signing settings, asserting success.
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
        .expect("failed to run git (is it installed?)");
    assert!(status.success(), "git {args:?} failed");
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write file");
}

/// Create a fresh temp directory unique to `name`, removing any stale copy.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-sync-{}-{}", std::process::id(), name));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Open a cache under the repository's common git dir (shared across worktrees).
fn cache_for(repo: &Repo) -> ObjectCache {
    ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("open cache")
}

#[test]
fn cold_then_noop_then_single_file_change() {
    let dir = fresh_dir("incremental");
    git(&dir, &["init", "-q"]);
    write(&dir, "a.txt", "alpha\n");
    write(&dir, "b.txt", "beta\n");
    write(&dir, "src/c.rs", "fn main() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);

    let repo = Repo::discover(&dir).expect("discover");
    assert!(repo.git_dir().exists(), "git dir should resolve");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");
    let ex = FileNodeExtractor;

    // (a) Cold sync populates the store; every blob is a cache miss.
    let r1 = sync(&mut store, &repo, &cache, &ex).expect("cold sync");
    assert!(!r1.no_op);
    assert_eq!(r1.blobs_total, 3);
    assert_eq!(r1.blobs_extracted, 3);
    assert_eq!(r1.blobs_cached, 0);
    assert_eq!(r1.nodes, 3);
    assert!(store.get_node("file:a.txt").expect("get").is_some());
    assert!(store.get_node("file:src/c.rs").expect("get").is_some());

    // (b) Re-sync with an unchanged tree is a no-op: zero blobs touched.
    let r2 = sync(&mut store, &repo, &cache, &ex).expect("noop sync");
    assert!(r2.no_op);
    assert_eq!(r2.blobs_extracted, 0);
    assert_eq!(r2.nodes, 3);
    assert_eq!(r1.tree, r2.tree);

    // (c) Change one file and commit: exactly one blob is re-extracted, the
    // other two are served from the content-addressed cache.
    write(&dir, "b.txt", "beta changed\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "edit b"]);

    let r3 = sync(&mut store, &repo, &cache, &ex).expect("incremental sync");
    assert!(!r3.no_op);
    assert_eq!(r3.blobs_total, 3);
    assert_eq!(
        r3.blobs_extracted, 1,
        "only the changed blob is re-extracted"
    );
    assert_eq!(r3.blobs_cached, 2, "unchanged blobs are cache hits");
    assert_ne!(r1.tree, r3.tree);

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn removed_file_drops_its_node() {
    let dir = fresh_dir("removal");
    git(&dir, &["init", "-q"]);
    write(&dir, "keep.txt", "keep\n");
    write(&dir, "gone.txt", "gone\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "two files"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");
    let ex = FileNodeExtractor;

    sync(&mut store, &repo, &cache, &ex).expect("sync");
    assert!(store.get_node("file:gone.txt").expect("get").is_some());

    git(&dir, &["rm", "-q", "gone.txt"]);
    git(&dir, &["commit", "-q", "-m", "remove gone"]);

    let r = sync(&mut store, &repo, &cache, &ex).expect("sync after rm");
    assert_eq!(r.nodes, 1);
    assert!(store.get_node("file:gone.txt").expect("get").is_none());
    assert!(store.get_node("file:keep.txt").expect("get").is_some());

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn duplicate_content_at_distinct_paths_stays_distinct() {
    // Two files with identical content share a single git blob oid (git dedupes
    // by content). Keying the cache by oid alone would collapse them into one
    // node; keying by (path, oid) keeps them separate.
    let dir = fresh_dir("dup-content");
    git(&dir, &["init", "-q"]);
    write(&dir, "a.txt", "same\n");
    write(&dir, "b.txt", "same\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "duplicate content"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");
    let ex = FileNodeExtractor;

    let r = sync(&mut store, &repo, &cache, &ex).expect("sync");
    // Both blobs are extracted despite sharing an oid, and both nodes exist.
    assert_eq!(r.blobs_total, 2);
    assert_eq!(
        r.blobs_extracted, 2,
        "identical-content files must not share a cache entry"
    );
    assert_eq!(r.nodes, 2);
    assert!(store.get_node("file:a.txt").expect("get a").is_some());
    assert!(store.get_node("file:b.txt").expect("get b").is_some());

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn rust_extraction_produces_derived_graph_with_cross_file_calls() {
    // A two-file Rust project: `main` in one file calls `helper` defined in the
    // other. Extraction is per-file, so this call can only be linked at assembly
    // time — exactly what the sync engine's call resolution does.
    let dir = fresh_dir("rust-derive");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "src/main.rs",
        "mod util;\nfn main() {\n    util::helper();\n}\n",
    );
    write(&dir, "src/util.rs", "pub fn helper() -> u32 {\n    42\n}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "two rust files"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");

    let r = sync(&mut store, &repo, &cache, &Registry).expect("sync");
    assert!(!r.no_op);

    // Symbol nodes are derived from both files.
    assert!(
        store
            .get_node("sym:rust:src/main.rs#main")
            .expect("m")
            .is_some()
    );
    assert!(
        store
            .get_node("sym:rust:src/util.rs#helper")
            .expect("h")
            .is_some()
    );

    // The file `defines` its top-level function.
    let defines = store.edges_from("file:src/main.rs").expect("defines");
    assert!(
        defines
            .iter()
            .any(|e| e.kind == EdgeKind::Defines && e.dst == "sym:rust:src/main.rs#main"),
    );

    // The cross-file call `main -> helper` is resolved to a derived `calls` edge.
    let calls = store
        .edges_from("sym:rust:src/main.rs#main")
        .expect("calls");
    assert!(
        calls
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst == "sym:rust:src/util.rs#helper"),
        "cross-file call main -> helper should resolve",
    );

    // A second sync over the unchanged tree is a cache-stable no-op.
    let r2 = sync(&mut store, &repo, &cache, &Registry).expect("resync");
    assert!(r2.no_op);
    assert_eq!(r2.nodes, r.nodes);
    assert_eq!(r2.edges, r.edges);

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dirty_overlay_previews_uncommitted_edits() {
    // The working-tree sync overlays uncommitted edits to tracked files on top
    // of the committed graph, so a symbol added but not yet committed is visible.
    let dir = fresh_dir("dirty-overlay");
    git(&dir, &["init", "-q"]);
    write(&dir, "main.rs", "fn main() {}\n");
    write(&dir, "util.rs", "pub fn helper() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "committed"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");

    // A clean working tree: no overlay, committed symbols present.
    let r0 = sync_worktree(&mut store, &repo, &cache, &Registry).expect("clean");
    assert_eq!(r0.blobs_dirty, 0);
    assert!(
        store
            .get_node("sym:rust:util.rs#helper")
            .expect("h")
            .is_some()
    );
    assert!(
        store
            .get_node("sym:rust:util.rs#added")
            .expect("a")
            .is_none()
    );

    // Edit a tracked file WITHOUT committing: the new symbol is previewed.
    write(&dir, "util.rs", "pub fn helper() {}\npub fn added() {}\n");
    let r1 = sync_worktree(&mut store, &repo, &cache, &Registry).expect("dirty");
    assert!(!r1.no_op);
    assert_eq!(r1.blobs_dirty, 1);
    assert_eq!(
        r1.blobs_extracted, 0,
        "committed blobs still come from cache"
    );
    assert!(
        store
            .get_node("sym:rust:util.rs#added")
            .expect("added")
            .is_some(),
        "uncommitted symbol should be previewed",
    );

    // Re-running with the same dirty state is a no-op.
    let r2 = sync_worktree(&mut store, &repo, &cache, &Registry).expect("resync");
    assert!(r2.no_op);
    assert_eq!(r2.blobs_dirty, 1);

    // Deleting a tracked file in the working tree drops its symbols.
    std::fs::remove_file(dir.join("util.rs")).expect("rm");
    let r3 = sync_worktree(&mut store, &repo, &cache, &Registry).expect("deleted");
    assert!(!r3.no_op);
    assert!(
        store
            .get_node("sym:rust:util.rs#helper")
            .expect("h2")
            .is_none()
    );

    // A committed-only sync supersedes the overlay, restoring the HEAD view.
    let r4 = sync(&mut store, &repo, &cache, &Registry).expect("committed");
    assert!(!r4.no_op);
    assert_eq!(r4.blobs_dirty, 0);
    assert!(
        store
            .get_node("sym:rust:util.rs#helper")
            .expect("h3")
            .is_some()
    );
    assert!(
        store
            .get_node("sym:rust:util.rs#added")
            .expect("a3")
            .is_none()
    );

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn cache_is_reused_across_independent_stores() {
    let dir = fresh_dir("cache-reuse");
    git(&dir, &["init", "-q"]);
    write(&dir, "one.txt", "1\n");
    write(&dir, "two.txt", "2\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let ex = FileNodeExtractor;

    // First store populates the shared cache.
    let mut store_a = Store::open_in_memory().expect("store a");
    let ra = sync(&mut store_a, &repo, &cache, &ex).expect("sync a");
    assert_eq!(ra.blobs_extracted, 2);
    assert_eq!(ra.blobs_cached, 0);

    // A second, independent store syncing the same tree extracts nothing — it
    // reuses the content-addressed cache. This is what makes the cache correct
    // across worktrees/branches that share blobs.
    let mut store_b = Store::open_in_memory().expect("store b");
    let rb = sync(&mut store_b, &repo, &cache, &ex).expect("sync b");
    assert_eq!(rb.blobs_extracted, 0, "shared blobs are cache hits");
    assert_eq!(rb.blobs_cached, 2);
    assert_eq!(rb.nodes, 2);

    std::fs::remove_dir_all(&dir).expect("cleanup");
}
