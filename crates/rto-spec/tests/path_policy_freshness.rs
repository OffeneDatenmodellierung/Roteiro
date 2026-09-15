//! **The read-only freshness check must refuse a graph built under a different
//! `[paths]` declaration** (ADR-0007 `[paths]`, issue #840).
//!
//! The write paths are asserted in `rto-graph`'s `path_policy_freshness.rs`;
//! this is the fourth surface that can claim a graph is current, and it is here
//! rather than there because it needs both crates: the graph has to be *built*
//! to be stale, and `sync` lives one crate down.
//!
//! [`rto_spec::tool_check`] gated on `sync_state == HEAD` alone. With the path
//! policy now part of the extraction identity, a graph can sit at exactly the
//! right tree and still have been assembled under a declaration nobody asked
//! for — and answering a drift verdict from it is the confident wrong answer
//! `not-run` exists to refuse.

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{IngestConfig, ObjectCache, PathPolicy, Registry, Repo, Store, sync};
use rto_spec::Gate;

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
    let dir = std::env::temp_dir().join(format!("roteiro-specfresh-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn the_read_only_check_refuses_a_graph_built_under_another_policy() {
    let dir = fresh_dir("toolcheck");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(
        &dir,
        "manifest/papers.json",
        r#"{"papers": {"a": {"title": "A Paper"}}}"#,
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");
    let mut store = Store::open_in_memory().expect("store");
    sync(
        &mut store,
        &repo,
        &cache,
        &Registry::new(IngestConfig::default()),
    )
    .expect("cold sync");

    // Asked under the policy the graph was built with: current, so it runs. This
    // half is what stops the guard from being "always refuse", which would pass
    // the assertion below while making the tool useless.
    let current = rto_spec::tool_check(&store, Some(&dir), IngestConfig::default()).expect("check");
    assert_ne!(
        current.gate,
        Gate::NotRun,
        "the graph is at HEAD and was built under this policy: {:?}",
        current.not_run_reason
    );

    // Asked under a different one. The tree still matches exactly, so nothing
    // the old check looked at has moved.
    let policy = PathPolicy::new(Vec::new(), vec!["manifest/**".to_owned()]);
    let stale = rto_spec::tool_check(
        &store,
        Some(&dir),
        IngestConfig::default().with_paths(&policy),
    )
    .expect("check");
    assert_eq!(
        stale.gate,
        Gate::NotRun,
        "a graph built under another declaration must be refused, not answered from"
    );
    assert!(
        stale
            .not_run_reason
            .as_deref()
            .is_some_and(|w| w.contains("sync")),
        "and must say what to do about it: {:?}",
        stale.not_run_reason
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **A repository that disables an `[ingest]` toggle must still be checkable.**
///
/// `extraction_identity` folds the extraction toggles *and* the path policy, so
/// reconstructing the expected identity from `IngestConfig::default()` — all
/// toggles on — makes it disagree with any graph synced under `prose = false`,
/// permanently, on a perfectly fresh tree. That turns a freshness guard into a
/// blanket refusal for every repository using `[ingest]`, which is a worse defect
/// than the staleness it was added to catch: the guard is supposed to refuse
/// *wrong* answers, not all of them.
#[test]
fn a_disabled_ingest_toggle_does_not_permanently_refuse_the_check() {
    let dir = fresh_dir("toggles");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "README.md", "# prose\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let repo = Repo::discover(&dir).expect("discover");
    let cache = ObjectCache::open(repo.common_dir().join("roteiro/objects")).expect("cache");
    let mut store = Store::open_in_memory().expect("store");

    // Synced with prose off — an ordinary, supported configuration.
    let toggles = IngestConfig {
        prose: false,
        ..IngestConfig::default()
    };
    sync(&mut store, &repo, &cache, &Registry::new(toggles)).expect("cold sync");

    // Asked with the SAME configuration. The graph is at HEAD and was built under
    // exactly this ingest config, so there is nothing stale about it.
    let verdict = rto_spec::tool_check(&store, Some(&dir), toggles).expect("check");
    assert_ne!(
        verdict.gate,
        Gate::NotRun,
        "a fresh graph under a non-default `[ingest]` must still be checkable: {:?}",
        verdict.not_run_reason
    );

    std::fs::remove_dir_all(&dir).ok();
}
