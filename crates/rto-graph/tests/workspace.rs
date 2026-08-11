//! End-to-end test for [`rto_graph::Workspace`] (ADR-0008): one registry over
//! two real git repos, each with its own `.git/roteiro/graph.db`, routes a query
//! to the right project's graph and names projects after their directories.

use std::path::Path;
use std::process::Command;

use rto_graph::{FactSet, Node, NodeKind, Store, Workspace, WorkspaceError};

fn git_init(dir: &Path) {
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init", "-q"])
        .current_dir(dir)
        .status()
        .expect("run git init");
    assert!(status.success(), "git init failed in {}", dir.display());
}

/// Create a git repo at `dir` holding a graph with a single `fn` node named
/// `sym`, mimicking a synced repo's `.git/roteiro/graph.db`.
fn repo_with_node(dir: &Path, sym: &str) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_init(dir);
    let store_dir = dir.join(".git").join("roteiro");
    std::fs::create_dir_all(&store_dir).expect("mkdir store");
    let mut store = Store::open(&store_dir.join("graph.db")).expect("open store");
    let facts = FactSet::new().with_node(Node::new(sym, NodeKind::Fn, sym));
    store.apply_factset(&facts).expect("apply");
}

#[test]
fn workspace_routes_queries_to_the_right_project() {
    let base = std::env::temp_dir().join(format!("rto-ws-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    repo_with_node(&base.join("alpha"), "sym:rust:a.rs#only_in_alpha");
    repo_with_node(&base.join("beta"), "sym:rust:b.rs#only_in_beta");

    let ws = Workspace::from_repo_paths([base.join("alpha"), base.join("beta")])
        .expect("build workspace");

    // Projects are named after their directories, in stable order, and this is a
    // multi-project workspace (so tools should expose `project`).
    assert_eq!(ws.names(), vec!["alpha".to_owned(), "beta".to_owned()]);
    assert!(ws.is_multi());

    // Each project's own node is present only in its own graph.
    let alpha_has = |key: &str| {
        ws.with_store(Some("alpha"), |s| s.get_node(key).unwrap().is_some())
            .unwrap()
    };
    let beta_has = |key: &str| {
        ws.with_store(Some("beta"), |s| s.get_node(key).unwrap().is_some())
            .unwrap()
    };
    assert!(alpha_has("sym:rust:a.rs#only_in_alpha"));
    assert!(!alpha_has("sym:rust:b.rs#only_in_beta"));
    assert!(beta_has("sym:rust:b.rs#only_in_beta"));
    assert!(!beta_has("sym:rust:a.rs#only_in_alpha"));

    // A bare (no-project) call is ambiguous when several are hosted.
    let err = ws
        .with_store(None, |s| s.node_count().unwrap())
        .expect_err("bare call is ambiguous");
    assert!(matches!(err, WorkspaceError::AmbiguousProject { .. }));

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn reload_from_picks_up_added_and_dropped_repos() {
    let base = std::env::temp_dir().join(format!("rto-ws-reload-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    repo_with_node(&base.join("alpha"), "sym:rust:a.rs#in_alpha");
    repo_with_node(&base.join("beta"), "sym:rust:b.rs#in_beta");

    // Start hosting just alpha (single ⇒ default).
    let ws = Workspace::from_repo_paths([base.join("alpha")]).expect("build");
    assert_eq!(ws.names(), vec!["alpha".to_owned()]);
    // Warm its cache.
    assert!(
        ws.with_store(None, |s| s
            .get_node("sym:rust:a.rs#in_alpha")
            .unwrap()
            .is_some())
            .unwrap()
    );

    // Reload with beta added: both are now hosted (and it's multi-project).
    let names = ws
        .reload_from([base.join("alpha"), base.join("beta")])
        .expect("reload");
    assert_eq!(names, vec!["alpha".to_owned(), "beta".to_owned()]);
    assert!(ws.is_multi());
    assert!(
        ws.with_store(Some("beta"), |s| s
            .get_node("sym:rust:b.rs#in_beta")
            .unwrap()
            .is_some())
            .unwrap()
    );

    // Reload down to just beta: alpha is gone (and beta becomes the default).
    let names = ws.reload_from([base.join("beta")]).expect("reload");
    assert_eq!(names, vec!["beta".to_owned()]);
    assert!(matches!(
        ws.resolve(Some("alpha")).unwrap_err(),
        WorkspaceError::UnknownProject { .. }
    ));
    assert_eq!(ws.resolve(None).unwrap(), "beta");

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn a_registered_repo_without_a_graph_reports_no_graph() {
    let base = std::env::temp_dir().join(format!("rto-ws-nograph-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let dir = base.join("fresh");
    std::fs::create_dir_all(&dir).expect("mkdir");
    git_init(&dir); // a repo, but never synced — no graph.db

    let ws = Workspace::from_repo_paths([&dir]).expect("build workspace");
    // Single project ⇒ it's the default; querying it surfaces the missing graph.
    let err = ws
        .with_store(None, |s| s.node_count().unwrap())
        .expect_err("no graph yet");
    assert!(matches!(err, WorkspaceError::NoGraph { .. }), "{err:?}");
    assert!(err.to_string().contains("roteiro sync"));

    std::fs::remove_dir_all(&base).ok();
}
