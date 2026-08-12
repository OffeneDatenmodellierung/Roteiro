//! `sync_tree` extracts a repo's graph at an arbitrary commit — the primitive
//! behind version-pin resolution (ADR-0009 step 8). A spoke deploys a *pinned*
//! version of the hub (a submodule sha / image tag → commit), but drift is
//! otherwise measured against HEAD. This proves we can materialise the hub's graph
//! **at any commit, in memory, with no checkout**, and that resolving a config
//! reference against the pinned version gives a different, correct answer than HEAD.

use std::path::Path;
use std::process::Command;

use rto_graph::{IngestConfig, ObjectCache, Registry, Repo, Store};

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed");
    out
}

fn rev(dir: &Path, r: &str) -> String {
    String::from_utf8(git(dir, &["rev-parse", r]).stdout)
        .unwrap()
        .trim()
        .to_owned()
}

/// The hub's config keys at commit `rev`, via the real `sync_tree` into an
/// ephemeral store, read back with the same `config_keys` API the matcher uses.
fn config_keys_at(repo: &Repo, cache: &ObjectCache, rev: &str) -> Vec<String> {
    let reg = Registry::new(IngestConfig::default());
    let mut store = Store::open_in_memory().expect("store");
    rto_graph::sync_tree(&mut store, repo, cache, &reg, rev).expect("sync_tree");
    let mut keys: Vec<String> = store
        .config_keys()
        .expect("config_keys")
        .into_iter()
        .map(|c| c.key)
        .collect();
    keys.sort();
    keys
}

#[test]
fn sync_tree_materialises_the_graph_at_a_pinned_commit() {
    let base = std::env::temp_dir().join(format!("roteiro-synctree-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(&base).expect("mkdir");

    git(&base, &["init", "-q"]);
    // v1: config defines `serve.tools`. A stable README rides along.
    std::fs::write(base.join("README.md"), "# hub\n").expect("write");
    std::fs::write(
        base.join("config.toml"),
        "[serve]\naddr = \"0.0.0.0\"\ntools = true\n",
    )
    .expect("write");
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "v1"]);
    let v1 = rev(&base, "HEAD");

    // The hub evolves: `serve.tools` renamed to `serve.features`. README unchanged.
    std::fs::write(
        base.join("config.toml"),
        "[serve]\naddr = \"0.0.0.0\"\nfeatures = true\n",
    )
    .expect("write");
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "v2"]);
    let head = rev(&base, "HEAD");

    let repo = Repo::discover(&base).expect("discover");
    let cache = ObjectCache::open(base.join(".git/roteiro/objects")).expect("cache");

    let at_pin = config_keys_at(&repo, &cache, &v1);
    let at_head = config_keys_at(&repo, &cache, &head);

    // The crux: a reference to `serve.tools` resolves at the pinned v1, but is drift
    // at HEAD — the skew version-pin resolution catches.
    assert!(
        at_pin.iter().any(|k| k == "serve.tools"),
        "v1 has it: {at_pin:?}"
    );
    assert!(
        !at_head.iter().any(|k| k == "serve.tools"),
        "HEAD renamed it: {at_head:?}"
    );
    assert!(
        at_head.iter().any(|k| k == "serve.features"),
        "HEAD has features: {at_head:?}"
    );

    // A commit sha peels to its tree, so `sync_tree` accepts either. `blobs_at` also
    // proves content-addressing: the unchanged README keeps its oid across versions
    // (⇒ a cache hit), so resolving an older hub only re-does what differs.
    let readme = |r: &str| {
        repo.blobs_at(r)
            .unwrap()
            .into_iter()
            .find(|b| b.path == "README.md")
            .unwrap()
            .oid
    };
    assert_eq!(
        readme(&v1),
        readme(&head),
        "unchanged blob keeps its oid → cache hit"
    );

    std::fs::remove_dir_all(&base).ok();
}
