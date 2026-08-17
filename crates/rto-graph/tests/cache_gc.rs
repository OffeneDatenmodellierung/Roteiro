//! Integration tests for object-cache reclamation (issue #387) against a real
//! git fixture and a real cache — the keys are the ones [`rto_graph::sync`]
//! actually wrote, not hand-built strings.
//!
//! The unit tests in `sync.rs` pin the *policy* (which generations a sweep may
//! remove). These pin the thing the policy exists to protect, and the one way
//! this fix could do real damage: **a sweep must never cost a hit on a live
//! entry.** They assert that by measurement rather than by inspection — sync
//! after the sweep, through a store that has no memory of the first one, and
//! count the cache hits. A wrongly-deleted entry cannot hide from that; it shows
//! up as an extraction.

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{
    DEFAULT_KEEP_GENERATIONS, FactSet, FileNodeExtractor, ObjectCache, Repo, Store,
    sweep_superseded, sync,
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

/// A three-file repository at a single commit, in a temp dir unique to `name`.
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-cachegc-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "alpha\n").expect("write");
    std::fs::write(dir.join("b.txt"), "beta\n").expect("write");
    std::fs::write(dir.join("c.txt"), "gamma\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

fn cache_for(repo: &Repo) -> ObjectCache {
    ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("open cache")
}

/// Every `<shard>/<rest>.json` under the cache root, as reassembled keys.
fn keys(cache: &ObjectCache) -> Vec<String> {
    let mut out = Vec::new();
    let shards = std::fs::read_dir(cache.root()).expect("read cache root");
    for shard in shards {
        let shard = shard.expect("shard entry");
        if !shard.file_type().expect("shard type").is_dir() {
            continue;
        }
        let prefix = shard.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(shard.path()).expect("read shard") {
            let entry = entry.expect("cache entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_suffix(".json") {
                out.push(format!("{prefix}{rest}"));
            }
        }
    }
    out.sort();
    out
}

/// The **hard constraint**, measured: a sweep keeping nothing behind the current
/// generation must still leave every live entry in place.
///
/// `keep_generations: 0` is deliberate — it is the most aggressive setting the
/// API offers, so if the reachability rule were wrong in the dangerous direction
/// this is where it shows. The proof is not that the files are still there but
/// that they are still *reachable*: a second sync through a **fresh store** (no
/// recorded tree, so no no-op and no incremental fast path) has to consult the
/// cache for every blob, and reports three hits and no extractions.
#[test]
fn a_sweep_that_keeps_nothing_still_costs_the_live_set_no_hits() {
    let dir = fixture("live-set");
    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let ex = FileNodeExtractor;

    let mut store = Store::open_in_memory().expect("store");
    let cold = sync(&mut store, &repo, &cache, &ex).expect("cold sync");
    assert_eq!(cold.blobs_extracted, 3, "cold sync extracts every blob");
    let live = keys(&cache);
    assert_eq!(live.len(), 3, "one cache entry per blob: {live:?}");

    let swept = sweep_superseded(&cache, 0).expect("sweep");
    assert_eq!(
        (swept.removed, swept.failed, swept.raced),
        (0, 0, 0),
        "nothing at the current generation is superseded: {swept:?}",
    );
    assert_eq!(swept.retained, 3, "{swept:?}");
    assert!(swept.retained_bytes > 0, "{swept:?}");
    assert_eq!(
        keys(&cache),
        live,
        "the cache is byte-for-byte the same set"
    );

    let mut fresh = Store::open_in_memory().expect("second store");
    let warm = sync(&mut fresh, &repo, &cache, &ex).expect("post-sweep sync");
    assert_eq!(
        (warm.blobs_extracted, warm.blobs_cached),
        (0, 3),
        "every blob must still be served from the cache after the sweep",
    );
}

/// Split a real cache key into everything before its version tag, its version,
/// and its environment tag — so a test can vary the one field the sweep policy
/// reads and leave every other one identical to what `sync` wrote.
fn split_version(key: &str) -> (&str, u32, &str) {
    let (head, tail) = key.rsplit_once("-v").expect("a key carries a version tag");
    let (version, env) = tail.rsplit_once("-e").expect("…and an environment tag");
    (head, version.parse().expect("a numeric version"), env)
}

/// The reclaim itself, on a cache holding what an `EXTRACT_VERSION` bump actually
/// leaves behind — and, in the same cache, the thing that must not be mistaken
/// for it.
///
/// Both are built from the live keys by rewriting only the `-v<version>-` field,
/// so they differ from the live set in exactly the one field the policy may read:
///
/// - **Superseded** — an older generation, in two feature namespaces. Goes.
/// - **The current generation in another feature namespace** — what an
///   `--all-features` binary writes for these same blobs while a default build
///   writes the live set. Both are live *at once* (this repository's own default
///   and `--all-features` test runs are exactly that pair), and a sweep that
///   compared whole `EXTRACT_VERSION`s rather than generations would delete each
///   build's cache on sight of the other, for ever. Stays.
///
/// A namespace is any whole multiple of the stride, so adding 700 to a real key's
/// version names a sibling namespace at the same generation without the test
/// needing to know which namespace it started in.
#[test]
fn a_superseded_generation_is_reclaimed_and_neither_live_namespace_is() {
    let dir = fixture("two-generations");
    let repo = Repo::discover(&dir).expect("discover");
    let cache = cache_for(&repo);
    let ex = FileNodeExtractor;

    let mut store = Store::open_in_memory().expect("store");
    sync(&mut store, &repo, &cache, &ex).expect("cold sync");
    let live = keys(&cache);
    assert_eq!(live.len(), 3, "one cache entry per blob: {live:?}");

    let mut sibling = Vec::new();
    let mut superseded = Vec::new();
    for key in &live {
        let (head, version, env) = split_version(key);
        sibling.push(format!("{head}-v{}-e{env}", version + 700));
        for namespace in [0, 700] {
            // `1` is at least two generations back for every released version of
            // this crate, so `DEFAULT_KEEP_GENERATIONS` never retains it — and it
            // stays that way without this test tracking bumps.
            superseded.push(format!("{head}-v{}-e{env}", 1 + namespace));
        }
    }
    for key in sibling.iter().chain(&superseded) {
        cache.put(key, &FactSet::new()).expect("plant");
    }
    assert_eq!(keys(&cache).len(), 12, "3 live + 3 sibling + 6 superseded");

    let swept = sweep_superseded(&cache, DEFAULT_KEEP_GENERATIONS).expect("sweep");
    assert_eq!(
        (swept.scanned, swept.retained, swept.removed),
        (12, 6, 6),
        "exactly the superseded generation goes: {swept:?}",
    );
    let mut expected: Vec<String> = live.iter().chain(&sibling).cloned().collect();
    expected.sort();
    assert_eq!(
        keys(&cache),
        expected,
        "both live namespaces survive; nothing else does",
    );

    let mut fresh = Store::open_in_memory().expect("second store");
    let warm = sync(&mut fresh, &repo, &cache, &ex).expect("post-sweep sync");
    assert_eq!(
        (warm.blobs_extracted, warm.blobs_cached),
        (0, 3),
        "reclaiming the old generation cost the live one nothing",
    );
}
