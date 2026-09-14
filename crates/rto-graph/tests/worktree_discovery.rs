//! A `roots` scan must not host a **linked git worktree** as an independent
//! project (issue #837).
//!
//! One repository present under a scanned root three times, at three revisions,
//! under three names, is not merely noisy: it triple-counts the same symbols in
//! every coupling, hotspot and debt figure, and a workspace-scoped retrieval can
//! return one file at three revisions as three independent sources.
//!
//! # These fixtures are real worktrees
//!
//! Every worktree here is made by `git worktree add`, never by writing a `.git`
//! file with a `gitdir:` line by hand. A hand-faked fixture tests the shape this
//! implementation happens to look for, which is circular: it would pass just as
//! happily against a name check, against a `.git`-is-a-file check that mistakes a
//! submodule for a worktree, and against any future rewrite that agrees with
//! today's guess and not with git. The cost is a `git` binary in the test
//! environment, which every other git-backed test here already requires.

use std::path::{Path, PathBuf};
use std::process::Command;

use rto_graph::{Worktrees, discover_repos_under, is_linked_worktree, scan_root};

/// Run `git` in `dir`, with the identity and signing settings a fixture needs and
/// a developer's global config cannot be trusted to leave alone.
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
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A git repository at `dir` with one commit, so that `git worktree add` has a
/// HEAD to branch from.
fn repo_with_a_commit(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("write");
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    dir.to_path_buf()
}

/// A private base directory for one test, named after the test so two running in
/// parallel cannot collide (the process id alone does not separate them).
fn base(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rto-wt-{}-{tag}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir base");
    dir
}

/// **The predicate is structural, and it is wrong in both directions if it is
/// not.**
///
/// The `<repo>-wt-<task>` naming that made issue #837 visible is one machine's
/// habit. This fixture is the two ways a name test fails: a real worktree called
/// `checkout`, which a name test would host, and an ordinary independent clone
/// called `decoy-wt-mirror`, which a name test would drop. Asserting only the
/// first would leave a skip that skips too much passing.
#[test]
fn a_worktree_is_recognised_by_its_layout_and_never_by_its_name() {
    let base = base("layout");
    let main = repo_with_a_commit(&base.join("plain"));
    git(
        &main,
        &["worktree", "add", "-q", "../checkout", "-b", "side"],
    );
    repo_with_a_commit(&base.join("decoy-wt-mirror"));

    assert!(
        is_linked_worktree(&base.join("checkout")),
        "a real `git worktree add` checkout, whose name says nothing, was not \
         recognised"
    );
    assert!(
        !is_linked_worktree(&base.join("decoy-wt-mirror")),
        "an ordinary repository was called a worktree because of its name"
    );
    assert!(
        !is_linked_worktree(&main),
        "the main checkout of a repository that HAS a worktree was itself called \
         a worktree"
    );
}

/// A `roots` scan walks past the worktree, keeps every ordinary repository, and
/// **says** what it walked past.
#[test]
fn a_roots_scan_skips_the_worktree_and_still_hosts_every_real_repo() {
    let base = base("scan");
    let main = repo_with_a_commit(&base.join("plain"));
    git(
        &main,
        &["worktree", "add", "-q", "../checkout", "-b", "side"],
    );
    repo_with_a_commit(&base.join("decoy-wt-mirror"));
    std::fs::create_dir_all(base.join("notarepo")).expect("mkdir");

    let scan = scan_root(&base, Worktrees::Skip).expect("scan");

    // The negative, first: a skip that skips too much would pass a test that only
    // checked the worktree case.
    assert_eq!(
        scan.repos,
        vec![base.join("decoy-wt-mirror"), base.join("plain")],
        "an ordinary repository stopped being hosted"
    );
    assert_eq!(scan.worktrees, vec![base.join("checkout")]);
    // The two skip lists are kept apart, because the two have different remedies.
    assert_eq!(scan.skipped, vec![base.join("notarepo")]);
    assert_eq!(
        discover_repos_under(&base, Worktrees::Skip).expect("discover"),
        scan.repos,
        "the membership answer and the diagnostic one disagree"
    );
}

/// The opt-in restores exactly the old behaviour, and reports nothing skipped —
/// a worktree that is hosted is not also a worktree that was walked past.
#[test]
fn the_opt_in_hosts_the_worktree_and_then_has_nothing_to_report() {
    let base = base("optin");
    let main = repo_with_a_commit(&base.join("plain"));
    git(
        &main,
        &["worktree", "add", "-q", "../checkout", "-b", "side"],
    );

    let scan = scan_root(&base, Worktrees::Include).expect("scan");
    assert_eq!(scan.repos, vec![base.join("checkout"), base.join("plain")]);
    assert!(
        scan.worktrees.is_empty(),
        "a hosted worktree was also reported as skipped: {:?}",
        scan.worktrees
    );
}

/// **A worktree whose main repository is outside every root is still skipped.**
///
/// Stated deliberately rather than discovered later: this is the one case where
/// the rule loses content that is graphed today, because nothing else under the
/// root holds it. It is skipped anyway. A scan cannot tell "the main checkout is
/// elsewhere in your config" from "the main checkout is on another disk", so
/// making the answer depend on the rest of the config would make one directory's
/// fate depend on an unrelated entry — and the remedy is the same either way and
/// is named in the startup note: `include_worktrees`, or `repos = [...]`.
#[test]
fn a_worktree_whose_main_repo_is_outside_the_root_is_skipped_too() {
    let base = base("orphan");
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let main = repo_with_a_commit(&outside.join("elsewhere"));

    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("mkdir");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            root.join("checkout").to_str().expect("utf-8"),
            "-b",
            "side",
        ],
    );

    let scan = scan_root(&root, Worktrees::Skip).expect("scan");
    assert!(scan.repos.is_empty(), "hosted something: {:?}", scan.repos);
    assert_eq!(scan.worktrees, vec![root.join("checkout")]);

    // And the opt-in is a real escape hatch for it, not a consolation.
    assert_eq!(
        scan_root(&root, Worktrees::Include).expect("scan").repos,
        vec![root.join("checkout")]
    );
}

/// **A root that IS a worktree is still scanned.** `worktrees` governs discovery,
/// and the root is not discovered — it is the path the operator wrote, which is
/// the same deliberate act as naming one in `repos`. Refusing it would leave such
/// a config hosting nothing and reporting no error.
#[test]
fn pointing_a_root_directly_at_a_worktree_still_hosts_it() {
    let base = base("rootitself");
    let main = repo_with_a_commit(&base.join("plain"));
    git(
        &main,
        &["worktree", "add", "-q", "../checkout", "-b", "side"],
    );

    let scan = scan_root(&base.join("checkout"), Worktrees::Skip).expect("scan");
    assert_eq!(scan.repos, vec![base.join("checkout")]);
    assert!(scan.worktrees.is_empty(), "{:?}", scan.worktrees);
}

/// A **submodule** is a different repository, not a second checkout of this one,
/// and stays hosted. The narrow predicate is deliberate: `.git`-is-a-file is true
/// of both, and skipping submodules would be a second, unrequested behaviour
/// change riding along with this one.
#[test]
fn a_submodule_is_not_a_worktree_and_stays_hosted() {
    let base = base("submodule");
    let inner = repo_with_a_commit(&base.join("inner"));
    let outer = repo_with_a_commit(&base.join("outer"));
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            inner.to_str().expect("utf-8"),
            "vendored",
        ])
        .current_dir(&outer)
        .status()
        .expect("run git submodule add");
    assert!(status.success(), "git submodule add failed");

    assert!(
        !is_linked_worktree(&outer.join("vendored")),
        "a submodule was classified as a linked worktree"
    );
    let scan = scan_root(&outer, Worktrees::Skip).expect("scan");
    assert_eq!(
        scan.repos,
        vec![outer.clone(), outer.join("vendored")],
        "a submodule stopped being hosted"
    );
    assert!(scan.worktrees.is_empty(), "{:?}", scan.worktrees);
}

/// **Verifying, rather than assuming, that a worktree and its main checkout
/// cannot contaminate each other** — the open case the #837 amendment names.
///
/// Two mechanisms are shared between a worktree and its main checkout and two
/// are not, and the amendment is right that "the paths differ, so it is probably
/// fine" is a reason to check:
///
/// * The **graph store** is per-worktree: `<git dir>/roteiro/graph.db`, and a
///   worktree's git dir is `<main>/.git/worktrees/<name>`. Two stores, asserted
///   below by path.
/// * The **object cache** is deliberately shared, under the *common* git dir, and
///   keyed on `(blob oid, path, extractor version, env)` (`sync::cache_key`). A
///   collision would need two different extraction results at one `(path, oid)`
///   — impossible, because the oid *is* the content. Sharing is the benefit, so
///   this asserts the hit rather than merely tolerating it.
///
/// The fixture diverges the two branches so leakage would be visible: each side
/// carries a file the other does not. A test using identical trees could not tell
/// "correctly separate" from "one graph serving both".
#[test]
fn a_worktree_and_its_main_checkout_share_a_cache_without_sharing_a_graph() {
    use rto_graph::{FileNodeExtractor, ObjectCache, Repo, Store, sync};

    let base = base("cachesep");
    let main = repo_with_a_commit(&base.join("plain"));
    std::fs::write(main.join("shared.txt"), "same on both\n").expect("write");
    std::fs::write(main.join("only_on_main.txt"), "main\n").expect("write");
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "main files"]);

    let checkout = base.join("checkout");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            checkout.to_str().expect("utf-8"),
            "-b",
            "side",
        ],
    );
    std::fs::remove_file(checkout.join("only_on_main.txt")).expect("rm");
    std::fs::write(checkout.join("only_on_side.txt"), "side\n").expect("write");
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "side files"]);

    let main_repo = Repo::discover(&main).expect("discover main");
    let wt_repo = Repo::discover(&checkout).expect("discover worktree");

    // The identities this all rests on, stated rather than implied.
    assert_eq!(main_repo.git_dir(), main_repo.common_dir());
    assert_ne!(
        wt_repo.git_dir(),
        wt_repo.common_dir(),
        "a linked worktree's git dir must differ from its common dir"
    );
    // Canonicalised: `gix` reports a worktree's common dir as its `commondir`
    // file records it — relative and unresolved (`…/worktrees/<name>/../..`) —
    // so comparing the raw strings would fail on a path that is in fact the same
    // directory. That unresolved form is exactly why `linked_worktree_of` goes
    // through `main_repo()` rather than through this path.
    let canon = |p: &Path| std::fs::canonicalize(p).expect("canonicalize");
    assert_eq!(canon(wt_repo.common_dir()), canon(main_repo.git_dir()));
    assert_eq!(
        wt_repo.linked_worktree_of().map(|p| canon(&p)),
        Some(canon(&main)),
        "the main checkout must be named as a path that exists, with no `..` \
         components left in it"
    );
    assert!(
        !wt_repo
            .linked_worktree_of()
            .expect("worktree")
            .to_string_lossy()
            .contains(".."),
        "the reported main checkout still carries unresolved `..` components, \
         which would be shown to a user as the repository's location"
    );
    assert_eq!(wt_repo.head_branch().as_deref(), Some("side"));
    assert_eq!(main_repo.head_branch().as_deref(), Some("main"));
    assert_eq!(main_repo.linked_worktree_of(), None);

    // Separate graph stores, by path — this is what `open_graph_at` derives.
    let main_db = main_repo.git_dir().join("roteiro").join("graph.db");
    let wt_db = wt_repo.git_dir().join("roteiro").join("graph.db");
    assert_ne!(main_db, wt_db, "the two checkouts share a graph store");

    // One cache, under the common git dir, for both.
    let cache_root = main_repo.common_dir().join("roteiro").join("objects");
    assert_eq!(
        canon(wt_repo.common_dir()).join("roteiro").join("objects"),
        canon(main_repo.common_dir())
            .join("roteiro")
            .join("objects"),
    );
    let cache = ObjectCache::open(&cache_root).expect("cache");
    let ex = FileNodeExtractor;

    let mut main_store = Store::open_in_memory().expect("main store");
    let main_report = sync(&mut main_store, &main_repo, &cache, &ex).expect("sync main");
    let mut wt_store = Store::open_in_memory().expect("worktree store");
    let wt_report = sync(&mut wt_store, &wt_repo, &cache, &ex).expect("sync worktree");

    // The benefit: the file both branches share is a cache **hit** on the second
    // sync, so the shared cache is doing its job across the two checkouts.
    assert!(
        wt_report.blobs_cached > 0,
        "nothing was reused from the shared cache, so the two checkouts are \
         paying twice for identical blobs: {wt_report:?} after {main_report:?}"
    );

    // The risk: and yet nothing leaked. Each graph holds its own branch's file
    // and not the other's.
    let has = |store: &Store, name: &str| {
        store
            .get_node(&format!("file:{name}"))
            .expect("get_node")
            .is_some()
    };
    assert!(has(&main_store, "only_on_main.txt"));
    assert!(
        !has(&main_store, "only_on_side.txt"),
        "the main checkout's graph picked up the worktree's branch"
    );
    assert!(has(&wt_store, "only_on_side.txt"));
    assert!(
        !has(&wt_store, "only_on_main.txt"),
        "the worktree's graph picked up the main checkout's branch — a shared \
         cache must not become a shared graph"
    );
    // Both hold the shared file, so the two `!has` assertions above were made
    // against populated graphs rather than empty ones.
    assert!(has(&main_store, "shared.txt") && has(&wt_store, "shared.txt"));
}
