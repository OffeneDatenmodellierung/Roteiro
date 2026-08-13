//! End-to-end test for [`rto_graph::Workspace`] (ADR-0008): one registry over
//! two real git repos, each with its own `.git/roteiro/graph.db`, routes a query
//! to the right project's graph and names projects after their directories.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use rto_graph::{
    FactSet, Node, NodeKind, ResolvedWorkspace, Store, Workspace, WorkspaceError, WorkspaceSet,
};

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
fn reload_reroutes_when_a_name_maps_to_a_different_repo() {
    // Two repos share a directory name ("proj") under different roots, so both
    // resolve to the same project name but different `graph.db` files.
    let base = std::env::temp_dir().join(format!("rto-ws-reroute-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    repo_with_node(&base.join("a").join("proj"), "sym:rust:x.rs#from_a");
    repo_with_node(&base.join("b").join("proj"), "sym:rust:x.rs#from_b");

    let ws = Workspace::from_repo_paths([base.join("a").join("proj")]).expect("build");
    // Warm the cache against repo A.
    assert!(
        ws.with_store(None, |s| s
            .get_node("sym:rust:x.rs#from_a")
            .unwrap()
            .is_some())
            .unwrap()
    );

    // Reload the *same name* onto repo B: the cached handle for A must be
    // evicted, so queries now hit B's graph — not the stale A connection.
    let names = ws
        .reload_from([base.join("b").join("proj")])
        .expect("reload");
    assert_eq!(names, vec!["proj".to_owned()]);
    assert!(
        ws.with_store(None, |s| s
            .get_node("sym:rust:x.rs#from_b")
            .unwrap()
            .is_some())
            .unwrap(),
        "should now read repo B's graph"
    );
    assert!(
        !ws.with_store(None, |s| s
            .get_node("sym:rust:x.rs#from_a")
            .unwrap()
            .is_some())
            .unwrap(),
        "must not still serve repo A's graph from a stale cache entry"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn on_open_hook_prepares_the_graph_on_first_access() {
    // A repo with `.git` but no graph yet; `--sync-on-access` should build it on
    // first touch instead of erroring.
    let base = std::env::temp_dir().join(format!("rto-ws-onopen-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let dir = base.join("proj");
    std::fs::create_dir_all(&dir).expect("mkdir");
    git_init(&dir);
    let db = dir.join(".git").join("roteiro").join("graph.db");
    assert!(!db.exists(), "no graph before first access");

    let ws = Workspace::from_repo_paths([&dir])
        .expect("build")
        .with_on_open(Arc::new(|db: &Path| {
            // Stand in for `roteiro sync`: create the graph with one node.
            std::fs::create_dir_all(db.parent().unwrap()).map_err(|e| e.to_string())?;
            let mut store = Store::open(db).map_err(|e| e.to_string())?;
            let facts = FactSet::new().with_node(Node::new(
                "sym:rust:z.rs#prepared",
                NodeKind::Fn,
                "prepared",
            ));
            store.apply_factset(&facts).map_err(|e| e.to_string())?;
            Ok(())
        }));

    // First access runs the hook, which prepares the graph — so the query
    // succeeds rather than hitting `NoGraph`.
    assert!(
        ws.with_store(None, |s| s
            .get_node("sym:rust:z.rs#prepared")
            .unwrap()
            .is_some())
            .unwrap(),
        "the on-open hook should have built the graph"
    );
    assert!(db.exists(), "graph.db exists after first access");

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

#[test]
fn workspace_set_from_resolved_partitions_linked_and_standalone() {
    // A linked group of two repos (one multi-repo graph), plus a standalone repo
    // discovered under its own root (its own single-repo graph, no cross-repo
    // links). Mirrors a config's `[[workspaces]]` + `[standalone]` after
    // `Config::resolved_workspaces()`.
    let base = std::env::temp_dir().join(format!("rto-wsset-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    repo_with_node(&base.join("linked").join("api"), "sym:rust:a.rs#in_api");
    repo_with_node(&base.join("linked").join("web"), "sym:rust:b.rs#in_web");
    repo_with_node(&base.join("solo").join("tools"), "sym:rust:c.rs#in_tools");

    let resolved = vec![
        ResolvedWorkspace {
            name: "prod".to_owned(),
            roots: vec![base.join("linked").to_string_lossy().into_owned()],
            repos: Vec::new(),
            linked: true,
        },
        ResolvedWorkspace {
            name: "tools".to_owned(),
            roots: Vec::new(),
            repos: vec![
                base.join("solo")
                    .join("tools")
                    .to_string_lossy()
                    .into_owned(),
            ],
            linked: false,
        },
    ];
    let set = WorkspaceSet::from_resolved(resolved).expect("build set");

    // Both workspaces are present, with the right linked flags.
    assert_eq!(set.names(), vec!["prod".to_owned(), "tools".to_owned()]);
    assert_eq!(set.linked("prod"), Some(true));
    assert_eq!(set.linked("tools"), Some(false));

    // The linked workspace is a multi-repo graph (api + web); a named select routes
    // a query to the right member.
    let prod = set.select(Some("prod")).expect("select prod");
    assert_eq!(prod.names(), vec!["api".to_owned(), "web".to_owned()]);
    assert!(prod.is_multi());
    assert!(
        prod.with_store(Some("api"), |s| s
            .get_node("sym:rust:a.rs#in_api")
            .unwrap()
            .is_some())
            .unwrap()
    );

    // The standalone workspace is a one-repo graph (no cross-repo links possible):
    // a bare select resolves to its sole project.
    let tools = set.select(Some("tools")).expect("select tools");
    assert!(!tools.is_multi());
    assert_eq!(tools.names(), vec!["tools".to_owned()]);
    assert!(
        tools
            .with_store(None, |s| s
                .get_node("sym:rust:c.rs#in_tools")
                .unwrap()
                .is_some())
            .unwrap()
    );

    // `containing` maps a repo's own graph.db back to the workspace holding it.
    let api_db = base
        .join("linked")
        .join("api")
        .join(".git")
        .join("roteiro")
        .join("graph.db");
    let tools_db = base
        .join("solo")
        .join("tools")
        .join(".git")
        .join("roteiro")
        .join("graph.db");
    assert_eq!(set.containing(&api_db), Some("prod"));
    assert_eq!(set.containing(&tools_db), Some("tools"));
    assert_eq!(set.containing(Path::new("/nope/graph.db")), None);

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn workspace_set_from_resolved_skips_empty_groups() {
    // A group whose root holds no repos resolves to nothing and is simply absent —
    // it never aborts building the rest of the set.
    let base = std::env::temp_dir().join(format!("rto-wsset-empty-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(base.join("barren")).expect("mkdir empty root");
    repo_with_node(&base.join("real"), "sym:rust:a.rs#real");

    let resolved = vec![
        ResolvedWorkspace {
            name: "ghost".to_owned(),
            roots: vec![base.join("barren").to_string_lossy().into_owned()],
            repos: Vec::new(),
            linked: true,
        },
        ResolvedWorkspace {
            name: "real".to_owned(),
            roots: Vec::new(),
            repos: vec![base.join("real").to_string_lossy().into_owned()],
            linked: false,
        },
    ];
    let set = WorkspaceSet::from_resolved(resolved).expect("build set");

    // Only the non-empty group survives; the empty one is gone.
    assert_eq!(set.names(), vec!["real".to_owned()]);
    assert!(matches!(
        set.select(Some("ghost")),
        Err(WorkspaceError::UnknownWorkspace { .. })
    ));
    // Exactly one surviving workspace ⇒ it is the default (bare select works).
    assert!(set.select(None).is_ok());

    std::fs::remove_dir_all(&base).ok();
}
