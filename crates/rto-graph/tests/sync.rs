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
    DEFAULT_MEMORY_SCOPE, EdgeKind, FileNodeExtractor, MemoryKind, MemoryWrite, ObjectCache,
    Registry, Repo, Store, sync, sync_worktree,
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

    // (c) Change one file and commit: the incremental fast path touches *only*
    // the changed blob (tree-diff against the last-synced tree), carrying the two
    // unchanged files' facts forward from the store rather than re-reading them.
    write(&dir, "b.txt", "beta changed\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "edit b"]);

    let r3 = sync(&mut store, &repo, &cache, &ex).expect("incremental sync");
    assert!(!r3.no_op);
    assert_eq!(r3.blobs_total, 3, "the whole tree is still reported");
    assert_eq!(
        r3.blobs_extracted, 1,
        "only the changed blob is re-extracted"
    );
    assert_eq!(
        r3.blobs_cached, 0,
        "the incremental path processes only the changed blob; unchanged facts \
         are carried forward from the store, not re-read"
    );
    assert_eq!(
        r3.nodes, 3,
        "unchanged file nodes survive the incremental sync"
    );
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

    let r = sync(&mut store, &repo, &cache, &Registry::default()).expect("sync");
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
    let r2 = sync(&mut store, &repo, &cache, &Registry::default()).expect("resync");
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
    let r0 = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("clean");
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
    let r1 = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("dirty");
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
    let r2 = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("resync");
    assert!(r2.no_op);
    assert_eq!(r2.blobs_dirty, 1);

    // Deleting a tracked file in the working tree drops its symbols.
    std::fs::remove_file(dir.join("util.rs")).expect("rm");
    let r3 = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("deleted");
    assert!(!r3.no_op);
    assert!(
        store
            .get_node("sym:rust:util.rs#helper")
            .expect("h2")
            .is_none()
    );

    // A committed-only sync supersedes the overlay, restoring the HEAD view.
    let r4 = sync(&mut store, &repo, &cache, &Registry::default()).expect("committed");
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

#[test]
fn diff_trees_reports_added_modified_deleted_and_prunes_unchanged() {
    // The incremental-sync primitive: diffing two tree oids yields exactly the
    // changed paths (added/modified with new oid, deleted), and never mentions an
    // unchanged file — gix prunes equal subtrees, so cost tracks the change.
    let dir = fresh_dir("difftrees");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/a.rs", "fn a() {}\n");
    write(&dir, "src/b.rs", "fn b() {}\n");
    write(&dir, "vendor/keep.rs", "fn keep() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let t1 = Repo::discover(&dir)
        .expect("discover")
        .head_tree_id()
        .expect("t1");

    // Modify a, delete b, add c; leave the whole vendor/ subtree untouched.
    write(&dir, "src/a.rs", "fn a() { let _x = 1; }\n");
    std::fs::remove_file(dir.join("src/b.rs")).expect("rm b");
    write(&dir, "src/c.rs", "fn c() {}\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "changes"]);
    let repo = Repo::discover(&dir).expect("rediscover");
    let t2 = repo.head_tree_id().expect("t2");

    let diff = repo.diff_trees(&t1, &t2).expect("diff");
    let changed: Vec<&str> = diff.changed.iter().map(|b| b.path.as_str()).collect();
    assert_eq!(
        changed,
        ["src/a.rs", "src/c.rs"],
        "a modified + c added, sorted"
    );
    assert_eq!(diff.deleted, ["src/b.rs"], "b deleted");
    assert!(
        !changed.contains(&"vendor/keep.rs"),
        "the unchanged subtree is pruned, not reported"
    );
    // The reported oid for the added file matches its committed blob.
    let c_oid = &diff
        .changed
        .iter()
        .find(|b| b.path == "src/c.rs")
        .unwrap()
        .oid;
    let head = repo.walk_blobs().expect("walk");
    let c_head = head
        .iter()
        .find(|b| b.path == "src/c.rs")
        .expect("c in head");
    assert_eq!(&c_head.oid, c_oid, "diff oid matches the tree blob oid");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn incremental_sync_matches_a_full_rebuild() {
    // The incremental committed sync must produce byte-for-byte the same derived
    // graph as a full sync — including the hard case where a change in ONE file
    // flips cross-file call resolution in an UNCHANGED file (a name that was
    // unique becomes ambiguous, so a `calls` edge must disappear).
    let dir = fresh_dir("incr-equiv");
    git(&dir, &["init", "-q"]);
    write(&dir, "a.rs", "pub fn helper() -> u32 {\n    1\n}\n");
    write(&dir, "b.rs", "pub fn caller() {\n    helper();\n}\n"); // unique helper → edge
    write(&dir, "gone.rs", "pub fn gone() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let reg = Registry::default();

    // Cold sync (full) — records the tree + env so the next sync can go incremental.
    let mut incr = Store::open(&dir.join(".git/incr.db")).expect("open incr");
    sync(&mut incr, &repo, &cache, &reg).expect("cold sync");

    // Change set: delete gone.rs, add d.rs with a SECOND `helper` (ambiguity flip).
    // b.rs is left UNCHANGED — its caller→helper edge must still disappear.
    std::fs::remove_file(dir.join("gone.rs")).expect("rm");
    write(&dir, "d.rs", "pub fn helper() -> u32 {\n    2\n}\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "flip"]);
    let repo = Repo::discover(&dir).expect("rediscover");

    let r = sync(&mut incr, &repo, &cache, &reg).expect("incremental sync");
    // Proof the fast path ran: it touched only the changed blob (d.rs added),
    // not every file in the tree.
    assert!(
        r.blobs_extracted + r.blobs_cached <= 1,
        "incremental should process only changed blobs, saw {}",
        r.blobs_extracted + r.blobs_cached
    );

    // A fresh store at the same HEAD takes the full path (no prior sync state).
    let mut full = Store::open(&dir.join(".git/full.db")).expect("open full");
    sync(&mut full, &repo, &cache, &reg).expect("full sync");

    // Canonicalise and compare the two derived graphs.
    let canon = |fs: rto_graph::FactSet| -> (Vec<String>, Vec<String>) {
        let mut ns: Vec<String> = fs
            .nodes
            .iter()
            .map(|n| format!("{}|{}|{}", n.key, n.kind.as_str(), n.provenance.as_str()))
            .collect();
        let mut es: Vec<String> = fs
            .edges
            .iter()
            .map(|e| {
                format!(
                    "{}|{}|{}|{}",
                    e.kind.as_str(),
                    e.src,
                    e.dst,
                    e.provenance.as_str()
                )
            })
            .collect();
        ns.sort();
        es.sort();
        (ns, es)
    };
    let ci = canon(incr.export_factset().expect("export incr"));
    let cf = canon(full.export_factset().expect("export full"));
    assert_eq!(ci, cf, "incremental sync must equal a full rebuild");

    // Sanity: the now-ambiguous caller→helper edge is gone in both, gone.rs dropped.
    // Canonical edge form is "<kind>|<src>|<dst>|<prov>", so a `calls` edge starts
    // with "calls|" — constrain the check to that kind, not any edge mentioning both.
    assert!(
        !cf.1
            .iter()
            .any(|e| e.starts_with("calls|") && e.contains("#caller") && e.contains("#helper")),
        "the ambiguous calls edge must be dropped: {:?}",
        cf.1
    );
    assert!(
        !cf.0.iter().any(|n| n.contains("gone.rs")),
        "deleted file dropped"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn untracked_overlay_includes_new_files_respecting_gitignore() {
    // The working-tree sync overlays brand-new *untracked* files (not only edits
    // to tracked files) so `check`/`review` see new-but-unstaged work — while
    // honouring `.gitignore`.
    let dir = fresh_dir("untracked-overlay");
    git(&dir, &["init", "-q"]);
    write(&dir, "main.rs", "fn main() {}\n");
    write(&dir, ".gitignore", "ignored.rs\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "committed"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");

    // Baseline: clean tree, only the committed symbol.
    let r0 = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("clean");
    assert_eq!(r0.blobs_dirty, 0);

    // A brand-new untracked file (never `git add`ed) and a git-ignored one.
    write(&dir, "fresh.rs", "pub fn brand_new() {}\n");
    write(&dir, "ignored.rs", "pub fn hidden() {}\n");

    let r = sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("untracked");
    assert!(!r.no_op);
    assert_eq!(
        r.blobs_dirty, 1,
        "only the untracked (non-ignored) file is new"
    );
    assert!(
        store
            .get_node("sym:rust:fresh.rs#brand_new")
            .expect("q")
            .is_some(),
        "untracked new file should be overlaid into the graph",
    );
    assert!(
        store
            .get_node("sym:rust:ignored.rs#hidden")
            .expect("q2")
            .is_none(),
        "git-ignored file must not be ingested",
    );

    // The low-level API classifies correctly: the new file is untracked, the
    // ignored and tracked files are not.
    let untracked = repo.untracked_files().expect("untracked");
    assert!(untracked.contains(&"fresh.rs".to_owned()));
    assert!(
        !untracked.iter().any(|p| p == "ignored.rs"),
        "ignored excluded"
    );
    assert!(
        !untracked.iter().any(|p| p == "main.rs"),
        "tracked excluded"
    );

    // Removing the untracked file returns to the committed baseline.
    std::fs::remove_file(dir.join("fresh.rs")).expect("rm");
    sync_worktree(&mut store, &repo, &cache, &Registry::default()).expect("removed");
    assert!(
        store
            .get_node("sym:rust:fresh.rs#brand_new")
            .expect("q3")
            .is_none(),
        "removing the untracked file drops its symbols",
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn scope_aware_calls_disambiguate_ambiguous_names() {
    // Two functions named `render`: a free `a::render` and a method `S::render`.
    // The bare name is ambiguous, so simple-name-only resolution would link
    // *neither* caller. The qualifier (`a::render()`) and the `self` receiver
    // (`self.render()`) each pick out exactly one target.
    let dir = fresh_dir("scope-calls");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "src/lib.rs",
        "mod a {\n    pub fn render() {}\n}\n\
         struct S;\n\
         impl S {\n    \
         fn render(&self) {}\n    \
         fn go(&self) {\n        self.render();\n    }\n}\n\
         fn drive() {\n    a::render();\n}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "ambiguous render"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");
    sync(&mut store, &repo, &cache, &Registry::default()).expect("sync");

    let calls_from = |key: &str| -> Vec<String> {
        let mut v: Vec<String> = store
            .edges_from(key)
            .expect("edges")
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| e.dst)
            .collect();
        v.sort();
        v
    };

    // `drive` calls `a::render()` → the module function, not `S::render`.
    assert_eq!(
        calls_from("sym:rust:src/lib.rs#drive"),
        ["sym:rust:src/lib.rs#a::render"],
        "a::render() must bind to the module fn, not the method",
    );
    // `S::go` calls `self.render()` → the same impl's method, not `a::render`.
    assert_eq!(
        calls_from("sym:rust:src/lib.rs#S::go"),
        ["sym:rust:src/lib.rs#S::render"],
        "self.render() must bind to S::render, not the module fn",
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An extractor whose *environment* differs from [`FileNodeExtractor`]'s while
/// its output for a given blob does not — the shape of an `EXTRACT_VERSION` bump
/// or a newly-installed model, as `sync` sees it.
struct ShiftedEnv(u64);

impl rto_graph::Extractor for ShiftedEnv {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> rto_graph::FactSet {
        FileNodeExtractor.extract(path, blob_id, bytes)
    }

    fn env_tag(&self) -> u64 {
        self.0
    }
}

/// **A change to the extraction identity re-extracts even at an unchanged tree.**
///
/// The `no_op` short-circuit used to compare the tree alone, so a binary whose
/// `EXTRACT_VERSION` had moved — or one with a newly-installed model, or a
/// newly-enabled extraction feature — reported "up to date" and served the *old*
/// version's facts until `HEAD` happened to move. That silently defeats the one
/// guarantee the version bump exists to give.
///
/// Both halves are asserted: the identity change must re-extract, and a second
/// run under that same identity must go back to being free.
#[test]
fn a_changed_extraction_identity_re_extracts_at_an_unchanged_tree() {
    use rto_graph::Extractor as _;

    let dir = fresh_dir("env-shift");
    git(&dir, &["init", "-q"]);
    write(&dir, "a.txt", "alpha\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");

    let first = ShiftedEnv(1);
    let r1 = sync(&mut store, &repo, &cache, &first).expect("cold sync");
    assert!(!r1.no_op);
    assert_eq!(r1.blobs_extracted, 1);

    // Same identity, same tree: free.
    let r2 = sync(&mut store, &repo, &cache, &first).expect("resync");
    assert!(r2.no_op, "an unchanged tree and identity is a no-op");

    // A different identity at the *same* tree must not be a no-op.
    let second = ShiftedEnv(2);
    assert_ne!(first.env_tag(), second.env_tag());
    let r3 = sync(&mut store, &repo, &cache, &second).expect("shifted env");
    assert!(
        !r3.no_op,
        "a changed extraction identity must re-extract, not report `up to date`",
    );
    assert_eq!(r1.tree, r3.tree, "the tree did not move");
    assert_eq!(
        r3.blobs_extracted, 1,
        "the blob must be re-extracted under the new identity, not served from cache",
    );

    // …and the new identity is now the recorded one, so a repeat is free again.
    assert!(
        sync(&mut store, &repo, &cache, &second)
            .expect("resync")
            .no_op
    );
}

/// Every cache entry as `(path relative to the cache root, bytes)`, sorted — the
/// whole fact cache, in a form two snapshots can be compared on.
fn cache_entries(cache: &ObjectCache) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.push((rel, std::fs::read(&path).expect("read cache entry")));
            }
        }
    }
    let mut out = Vec::new();
    walk(cache.root(), cache.root(), &mut out);
    out.sort();
    out
}

/// A default memory write: unanchored, `lesson`, default scope.
fn lesson(body: &str) -> MemoryWrite<'_> {
    MemoryWrite {
        scope: DEFAULT_MEMORY_SCOPE,
        kind: MemoryKind::Lesson,
        anchor: None,
        body,
        confidence: None,
        supersedes: None,
    }
}

/// **Writing agent memory does not invalidate the fact cache** (ADR-0013).
///
/// Memory is not extraction output: it is not derived from `(path, blob id,
/// bytes)`, no extractor emits it, and no cached fact set can contain it. So the
/// extraction identity — `EXTRACT_VERSION` plus the extractor environment, the
/// pair folded into every cache key — must be untouched by any memory write, and
/// a `sync` that follows one must still be free.
///
/// **If this test fails, you have a bug, not a renumbering.** It says a memory
/// code path reached the extraction identity or the cached facts, which is the
/// thing memory's separate-store design exists to prevent. It does not move when
/// `EXTRACT_VERSION` is bumped for a real change to extraction output (that is
/// what a bump is *for*, and no test pins the constant's value — see the note on
/// its declaration in `src/extract.rs`); both snapshots here are taken from the
/// same binary, so a legitimate bump by unrelated work cannot trip it.
///
/// # Why no test pins `EXTRACT_VERSION`'s value
///
/// This replaces `agent_memory_does_not_bump_the_extraction_version` in
/// `src/memory.rs`, which asserted the constant's literal value, and the history
/// of that test is the argument for this one.
///
/// ADR-0016 (#316) bumped the base 10 → 11 and added the `audio-metadata` (+400)
/// namespace, legitimately: `audio_stream` nodes genuinely change extraction
/// output, which is exactly what the constant is for. ADR-0013 (#317) added the
/// guard, asserting the base it saw — 10, less the two namespaces that existed
/// when it was written. The two merged **57 seconds apart**, so #317's CI had
/// never seen #316. Each was green alone; the trunk was not, under *every*
/// feature combination — 11 ≠ 10 by default, 411 ≠ 10 under `--all-features`,
/// the second because the subtraction list had silently gone stale too.
///
/// Nobody did anything wrong, and that is the point: the failure was a property
/// of the *form* of the assertion. A literal value of a shared global constant
/// cannot express a claim about one module's share of it, so it fires on work
/// that module had no part in — and it fires at merge time, on whoever is
/// unlucky with ordering, who must then work out whether they have broken an
/// invariant or merely renumbered a constant. Re-pinning it to 11 would restore
/// green and re-arm exactly the same trap for the next parallel merge; Stage
/// 26's lenses are expected to bump the constant again.
///
/// So **no test pins this constant's value, deliberately** (see the note on its
/// declaration in `src/extract.rs`). Bumping it for a real change in extraction
/// output should never be a test failure — that is what a bump is *for*. Tests
/// assert what the version is *for* instead: that it is folded into the cache key,
/// that a changed identity re-extracts at an unchanged tree, and — here — that
/// work which is not extraction cannot perturb it. **If a test does fail when you
/// bump it, it is reporting a real coupling, not a literal that needs updating.**
///
/// The property is also strictly stronger than the number it replaces: pinning
/// the integer would still have passed if a memory write had cleared the recorded
/// extraction identity or retired a cached fact set.
#[test]
fn memory_writes_do_not_invalidate_the_fact_cache() {
    let dir = fresh_dir("memory-cache");
    git(&dir, &["init", "-q"]);
    write(&dir, "a.txt", "alpha\n");
    write(&dir, "src/c.rs", "fn main() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let mut store = Store::open_in_memory().expect("store");
    let ex = FileNodeExtractor;

    let cold = sync(&mut store, &repo, &cache, &ex).expect("cold sync");
    assert!(!cold.no_op);
    assert_eq!(cold.blobs_extracted, 2, "both blobs are extracted cold");

    let facts_before = cache_entries(&cache);
    assert!(
        !facts_before.is_empty(),
        "the cache must have something in it"
    );
    let identity_before = store.sync_env().expect("sync env");
    let tree_before = store.sync_state().expect("sync state");
    assert!(identity_before.is_some(), "a sync records an identity");

    // The same spread of writes the artifact-purity test uses: anchored,
    // unanchored, superseding, and a forget.
    let anchored = store
        .record_memory(&MemoryWrite {
            anchor: Some("file:src/c.rs"),
            kind: MemoryKind::Attempt,
            confidence: Some(0.9),
            ..lesson("The retry loop double-counted partial batches.")
        })
        .expect("anchored write");
    store
        .record_memory(&MemoryWrite {
            supersedes: Some(anchored),
            ..lesson("Superseded: the dedup key alone was not enough.")
        })
        .expect("superseding write");
    let doomed = store
        .record_memory(&lesson("A record that will be forgotten."))
        .expect("write");
    store.forget_memory(doomed).expect("forget");
    assert_eq!(
        store.memory_counts().expect("counts"),
        (1, 1),
        "the writes must actually have done work",
    );

    assert_eq!(
        store.sync_env().expect("sync env"),
        identity_before,
        "a memory write must not perturb the recorded extraction identity",
    );
    assert_eq!(
        store.sync_state().expect("sync state"),
        tree_before,
        "a memory write must not disturb the synced tree",
    );
    assert_eq!(
        cache_entries(&cache),
        facts_before,
        "no cached fact set may be added, rewritten or retired by a memory write",
    );

    // The consequence that costs real time if it is ever lost: the next sync is
    // still free, rather than re-extracting the repository.
    let after = sync(&mut store, &repo, &cache, &ex).expect("resync");
    assert!(
        after.no_op,
        "a sync after a memory write must still be a no-op",
    );
    assert_eq!(
        after.blobs_extracted, 0,
        "memory must not force a single blob to be re-extracted",
    );

    std::fs::remove_dir_all(&dir).ok();
}
