//! A symlinked top-level `raw/` is walked by a worktree sync, keyed through the
//! link, unless the policy switches it off (ADR-0026, #812/#815).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{IngestConfig, ObjectCache, PathPolicy, Registry, Repo, Store, sync_worktree};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
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

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-rawlink-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// A repository whose `raw/` links to a share outside it. The share holds a
/// nested paper and a symlink of its own, which must not be followed.
fn fixture(name: &str) -> (PathBuf, Repo, ObjectCache) {
    let base = fresh_dir(name);
    let share = base.join("share");
    let outside = base.join("outside");
    std::fs::create_dir_all(share.join("papers")).expect("mkdir");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(share.join("papers/a.md"), "# Paper A\n").expect("write");
    std::fs::write(outside.join("secret.md"), "# not for the graph\n").expect("write");
    std::os::unix::fs::symlink(&outside, share.join("further")).expect("inner link");

    let repo_dir = base.join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir");
    git(&repo_dir, &["init", "-q"]);
    std::fs::write(repo_dir.join("src/lib.rs"), "pub struct Thing;\n").expect("write");
    git(&repo_dir, &["add", "."]);
    git(&repo_dir, &["commit", "-q", "-m", "init"]);
    std::os::unix::fs::symlink(&share, repo_dir.join("raw")).expect("raw link");

    let repo = Repo::discover(&repo_dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");
    (base, repo, cache)
}

fn file_keys(store: &Store) -> Vec<String> {
    store
        .all_nodes()
        .expect("nodes")
        .into_iter()
        .map(|n| n.key)
        .filter(|k| k.starts_with("file:raw/"))
        .collect()
}

#[test]
fn a_symlinked_raw_is_followed_by_default_and_keyed_through_the_link() {
    let (base, repo, cache) = fixture("default");
    let mut store = Store::open_in_memory().expect("store");
    sync_worktree(
        &mut store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default()),
    )
    .expect("sync");
    assert_eq!(
        file_keys(&store),
        ["file:raw/papers/a.md"],
        "inner symlink must not be followed"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn follow_raw_symlink_false_keeps_the_share_out_and_reconciles() {
    let (base, repo, cache) = fixture("off");
    let mut store = Store::open_in_memory().expect("store");
    sync_worktree(
        &mut store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default()),
    )
    .expect("sync");
    assert_eq!(file_keys(&store).len(), 1);
    // Switching it off afterwards must drop the nodes, not report "up to date".
    let off = PathPolicy::new(Vec::new(), Vec::new()).with_raw_symlink(false);
    sync_worktree(
        &mut store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default().with_paths(&off)),
    )
    .expect("sync");
    assert!(file_keys(&store).is_empty(), "{:?}", file_keys(&store));
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn an_excluded_raw_stays_excluded_behind_a_link() {
    let (base, repo, cache) = fixture("excluded");
    let mut store = Store::open_in_memory().expect("store");
    let policy = PathPolicy::new(vec!["raw/**".to_owned()], Vec::new());
    sync_worktree(
        &mut store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default().with_paths(&policy)),
    )
    .expect("sync");
    assert!(file_keys(&store).is_empty());
    std::fs::remove_dir_all(&base).ok();
}
