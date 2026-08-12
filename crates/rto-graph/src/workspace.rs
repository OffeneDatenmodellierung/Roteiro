//! A **workspace**: many per-repo graphs served by one process (ADR-0008).
//!
//! Each Roteiro graph is per-repo — a small `SQLite` store at
//! `<repo>/.git/roteiro/graph.db`. The expensive resource a server holds is the
//! *model*, not the graphs, so one process can hold the model once and answer
//! questions about **any** registered repo by opening that repo's store on
//! demand and caching it. A [`Workspace`] is that registry + on-demand,
//! cached store resolver; the tool surfaces (MCP and the `/v1` model server)
//! call [`Workspace::with_store`] with an optional `project` selector.
//!
//! Single-repo serving is just a workspace with one project (see
//! [`Workspace::single`]), so the default `serve` path is unchanged.
//!
//! The registry can be **reloaded** in place ([`Workspace::reload_from`]) so a
//! long-lived server can pick up added/removed repos without a restart (a SIGHUP
//! trigger); already-open stores for still-present projects keep their warm
//! connections, and dropped projects are evicted. An optional first-open hook
//! ([`Workspace::with_on_open`], `serve --sync-on-access`) (re)builds a project's
//! graph the first time it is queried.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::git::{GitError, Repo};
use crate::model::Node;
use crate::store::{Store, StoreError};

/// A failure resolving or opening a project's graph.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// A call named a project the workspace does not know.
    #[error("no project named `{name}` (known: {known})")]
    UnknownProject {
        /// The requested name.
        name: String,
        /// Comma-separated list of known project names.
        known: String,
    },
    /// A call omitted `project` but the workspace has no single default (it holds
    /// several projects), so the selection is ambiguous.
    #[error("this server hosts several projects ({known}); name one with `project`")]
    AmbiguousProject {
        /// Comma-separated list of known project names.
        known: String,
    },
    /// The workspace is registered but empty (no repos resolved).
    #[error("no projects registered")]
    Empty,
    /// A cross-repo target was not a project-qualified key (`<project>::<key>`).
    #[error("`{key}` is not a project-qualified key (expected `<project>::<key>`)")]
    Unqualified {
        /// The malformed key.
        key: String,
    },
    /// The project's graph store does not exist yet — its repo has not been
    /// synced (`roteiro sync`).
    #[error("project `{name}` has no graph yet — run `roteiro sync` in {}", .path.display())]
    NoGraph {
        /// The project name.
        name: String,
        /// The repo directory whose graph is missing.
        path: PathBuf,
    },
    /// The on-open hook (`serve --sync-on-access`) failed to prepare a project's
    /// graph before it was first served.
    #[error("failed to prepare project `{name}` on first access: {msg}")]
    Prepare {
        /// The project name.
        name: String,
        /// The hook's error message.
        msg: String,
    },
    /// A store lock was poisoned by a panic in another thread.
    #[error("store lock poisoned")]
    Poisoned,
    /// Discovering the repo for a registered path failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// Opening the project's store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Where a project's store comes from: a `graph.db` to open on demand, or an
/// already-open store (the single-repo default and tests).
#[derive(Clone)]
enum Source {
    /// Open this `graph.db` path on first use.
    Path(PathBuf),
    /// A pre-opened store, shared directly.
    Open(Arc<Mutex<Store>>),
}

/// The registry plus the open-store cache, behind one lock. Held only briefly —
/// to look up a source or (un)cache a handle — never across a graph query, which
/// runs on the returned per-store `Mutex` after this lock is released.
struct Inner {
    /// Project name → its store source, in stable name order.
    projects: BTreeMap<String, Source>,
    /// The project used when a call omits `project` (the sole project, if there
    /// is exactly one; otherwise `None` and a bare call is ambiguous).
    default: Option<String>,
    /// Opened stores, cached by project name, tagged with the [`Source`] they
    /// were opened from. `Store` is `!Sync` (it holds a rusqlite connection), so
    /// each is behind its own `Mutex`. The tag lets a reload keep a warm
    /// connection only when the project still maps to the *same* source, and
    /// never serve a handle for a repo the name no longer points at.
    cache: HashMap<String, (Source, Arc<Mutex<Store>>)>,
}

/// Whether two sources denote the same store: the same `graph.db` path, or the
/// very same pre-opened handle.
fn source_eq(a: &Source, b: &Source) -> bool {
    match (a, b) {
        (Source::Path(x), Source::Path(y)) => x == y,
        (Source::Open(x), Source::Open(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// A hook run against a project's `graph.db` path the first time it is opened —
/// used by `serve --sync-on-access` to (re)build a stale or missing graph before
/// it is served (ADR-0008). Returns a human-readable error on failure.
pub type OnOpen = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// A named set of per-repo graphs, each opened on demand and cached. Cheap to
/// hold: the stores are small `SQLite` files opened lazily; the caller (a server)
/// holds the one expensive model. The registry is reloadable in place.
pub struct Workspace {
    inner: Mutex<Inner>,
    /// Optional first-open hook (`serve --sync-on-access`): run against a
    /// project's `graph.db` path before it is opened, to sync it on demand.
    on_open: Option<OnOpen>,
}

impl Workspace {
    /// A single-project workspace over an already-open `store`, named `name`.
    /// This is the single-repo `serve` default and the test constructor; a bare
    /// (no-`project`) call resolves to it. Not reloadable (no repo paths).
    #[must_use]
    pub fn single(name: impl Into<String>, store: Store) -> Self {
        let name = name.into();
        let mut projects = BTreeMap::new();
        projects.insert(name.clone(), Source::Open(Arc::new(Mutex::new(store))));
        Self {
            inner: Mutex::new(Inner {
                projects,
                default: Some(name),
                cache: HashMap::new(),
            }),
            on_open: None,
        }
    }

    /// Build a workspace from repo directories: each is `git`-discovered, named
    /// after its working-tree directory (collisions get a `-2`, `-3`, … suffix),
    /// and its `graph.db` opened lazily. With exactly one repo, that repo is the
    /// default project.
    ///
    /// # Errors
    /// [`WorkspaceError::Git`] if a path is not inside a git repository, or
    /// [`WorkspaceError::Empty`] if `paths` resolves to no repos.
    pub fn from_repo_paths<I, P>(paths: I) -> Result<Self, WorkspaceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let (projects, default) = build_registry(paths)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                projects,
                default,
                cache: HashMap::new(),
            }),
            on_open: None,
        })
    }

    /// Set a first-open hook (`serve --sync-on-access`): before a project's store
    /// is opened for the first time, `hook` is run against its `graph.db` path to
    /// (re)build it. Applies to lazily-opened `Path` projects; a pre-opened
    /// `single` store is already loaded, so the hook does not fire for it.
    #[must_use]
    pub fn with_on_open(mut self, hook: OnOpen) -> Self {
        self.on_open = Some(hook);
        self
    }

    /// Rebuild the registry from a fresh set of repo `paths`: added repos become
    /// available, removed ones are dropped (and their cached store evicted), and
    /// still-present ones keep their warm connection. Returns the new project
    /// names. Use this to reload a running server (e.g. on SIGHUP) without a
    /// restart. A single-project pre-opened workspace ([`Workspace::single`]) has
    /// no repo paths, so reloading it simply replaces it with the given repos.
    ///
    /// # Errors
    /// As [`Workspace::from_repo_paths`].
    pub fn reload_from<I, P>(&self, paths: I) -> Result<Vec<String>, WorkspaceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        // Build the new registry outside the lock (discovery does git I/O).
        let (projects, default) = build_registry(paths)?;
        let names: Vec<String> = projects.keys().cloned().collect();
        let mut inner = self.lock()?;
        // Keep a warm connection only where the project still maps to the *same*
        // source; drop it if the name is gone or now points at a different
        // `graph.db` (or was a pre-opened `single` store), so a query never hits
        // the wrong repo.
        inner
            .cache
            .retain(|name, (src, _)| projects.get(name).is_some_and(|new| source_eq(new, src)));
        inner.projects = projects;
        inner.default = default;
        Ok(names)
    }

    /// The registered project names, in stable order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.lock()
            .map(|i| i.projects.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the workspace holds more than one project (so `project` selection
    /// is meaningful to expose to callers/tools).
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.lock().is_ok_and(|i| i.projects.len() > 1)
    }

    /// Resolve `project` (or the default) to a concrete project name.
    ///
    /// # Errors
    /// [`WorkspaceError::UnknownProject`] if named but absent,
    /// [`WorkspaceError::AmbiguousProject`] if omitted with several projects, or
    /// [`WorkspaceError::Empty`] if there are none.
    pub fn resolve(&self, project: Option<&str>) -> Result<String, WorkspaceError> {
        let inner = self.lock()?;
        match project {
            Some(name) if inner.projects.contains_key(name) => Ok(name.to_owned()),
            Some(name) => Err(WorkspaceError::UnknownProject {
                name: name.to_owned(),
                known: keys(&inner.projects),
            }),
            None => inner.default.clone().ok_or_else(|| {
                if inner.projects.is_empty() {
                    WorkspaceError::Empty
                } else {
                    WorkspaceError::AmbiguousProject {
                        known: keys(&inner.projects),
                    }
                }
            }),
        }
    }

    /// Run `f` with the resolved project's store (opened and cached on first
    /// use). The store lock is held only for `f`, never across an `.await`.
    ///
    /// # Errors
    /// As [`Workspace::resolve`], plus [`WorkspaceError::NoGraph`] if the store
    /// file is absent, [`WorkspaceError::Store`] on open failure, or
    /// [`WorkspaceError::Poisoned`] if a lock was poisoned.
    pub fn with_store<R>(
        &self,
        project: Option<&str>,
        f: impl FnOnce(&Store) -> R,
    ) -> Result<R, WorkspaceError> {
        let name = self.resolve(project)?;
        let handle = self.handle(&name)?;
        let store = handle.lock().map_err(|_| WorkspaceError::Poisoned)?;
        Ok(f(&store))
    }

    /// Resolve a **project-qualified** key `"<project>::<key>"` to its node across
    /// the workspace, opening the target project on demand (ADR-0009). `Ok(None)`
    /// means the key is well-formed and the project exists but the node does not —
    /// i.e. **cross-repo drift** (a removed or renamed target). Errors distinguish
    /// the other failure modes so a caller can report them precisely:
    /// [`WorkspaceError::Unqualified`] (not in `<project>::<key>` form),
    /// [`WorkspaceError::UnknownProject`] (target repo not in the workspace),
    /// [`WorkspaceError::NoGraph`] (target repo unsynced).
    ///
    /// # Errors
    /// As above, plus [`WorkspaceError::Store`] / [`WorkspaceError::Poisoned`].
    pub fn resolve_qualified(&self, qualified: &str) -> Result<Option<Node>, WorkspaceError> {
        let (project, key) =
            parse_qualified(qualified).ok_or_else(|| WorkspaceError::Unqualified {
                key: qualified.to_owned(),
            })?;
        let key = key.to_owned();
        self.with_store(Some(project), move |s| s.get_node(&key))?
            .map_err(WorkspaceError::from)
    }

    /// Follow an **external-ref** placeholder node to the real node it stands for,
    /// resolving its project-qualified target across the workspace (ADR-0009). An
    /// external-ref lives in a spoke's store as a local stand-in for a node in the
    /// hub's store (see [`crate::external_ref_node`]); this walks it through to the
    /// hub. `Ok(None)` means either `node` is not an external-ref, or its target no
    /// longer resolves — cross-repo drift (a removed or renamed hub key). Errors
    /// distinguish the other failure modes, as [`Workspace::resolve_qualified`].
    ///
    /// # Errors
    /// As [`Workspace::resolve_qualified`].
    pub fn follow_external_ref(&self, node: &Node) -> Result<Option<Node>, WorkspaceError> {
        match crate::external_ref_target(node) {
            Some(qualified) => self.resolve_qualified(&qualified),
            None => Ok(None),
        }
    }

    /// Lock the inner state, mapping a poisoned lock to [`WorkspaceError::Poisoned`].
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, WorkspaceError> {
        self.inner.lock().map_err(|_| WorkspaceError::Poisoned)
    }

    /// Get (opening + caching on first use) the shared store handle for `name`.
    /// Opens `graph.db` **outside** the registry lock so a first-touch open never
    /// blocks other projects' queries.
    fn handle(&self, name: &str) -> Result<Arc<Mutex<Store>>, WorkspaceError> {
        // Fast path and pre-opened sources resolve under a single short lock.
        let db = {
            let mut inner = self.lock()?;
            if let Some((_, handle)) = inner.cache.get(name) {
                return Ok(handle.clone());
            }
            match inner.projects.get(name) {
                Some(Source::Open(handle)) => {
                    let handle = handle.clone();
                    inner.cache.insert(
                        name.to_owned(),
                        (Source::Open(handle.clone()), handle.clone()),
                    );
                    return Ok(handle);
                }
                Some(Source::Path(db)) => db.clone(),
                None => {
                    return Err(WorkspaceError::UnknownProject {
                        name: name.to_owned(),
                        known: keys(&inner.projects),
                    });
                }
            }
        };
        // `serve --sync-on-access`: (re)build this project's graph before opening
        // it, so a stale or never-synced repo is prepared on first touch. Runs
        // outside the registry lock (it does extraction I/O).
        if let Some(on_open) = &self.on_open {
            on_open(&db).map_err(|msg| WorkspaceError::Prepare {
                name: name.to_owned(),
                msg,
            })?;
        }
        if !db.exists() {
            return Err(WorkspaceError::NoGraph {
                name: name.to_owned(),
                // The repo dir is the store's grandparent (`…/.git/roteiro`).
                path: db
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .unwrap_or(&db)
                    .to_path_buf(),
            });
        }
        let handle = Arc::new(Mutex::new(Store::open(&db)?));
        let opened = Source::Path(db.clone());
        let mut inner = self.lock()?;
        // Another thread may have opened it while we were; prefer the existing.
        if let Some((_, existing)) = inner.cache.get(name) {
            return Ok(existing.clone());
        }
        // Only cache if the registry still maps this name to the DB we opened —
        // a concurrent `reload_from` may have remapped or removed it. If so,
        // return the freshly-opened handle for this call (the caller resolved
        // before the reload) but do not cache a now-stale mapping.
        if inner
            .projects
            .get(name)
            .is_some_and(|current| source_eq(current, &opened))
        {
            inner
                .cache
                .insert(name.to_owned(), (opened, handle.clone()));
        }
        Ok(handle)
    }
}

/// Comma-separated project names (for error messages).
fn keys(projects: &BTreeMap<String, Source>) -> String {
    projects.keys().cloned().collect::<Vec<_>>().join(", ")
}

/// Split a **project-qualified** key `"<project>::<key>"` into `(project, key)`,
/// or `None` if it carries no `::` separator (a bare, within-repo key). A project
/// name never contains `::`; a bare key may itself contain single colons (e.g.
/// `sym:rust:…`), so only the **first** double-colon separates the project
/// (ADR-0009).
#[must_use]
pub fn parse_qualified(key: &str) -> Option<(&str, &str)> {
    key.split_once("::")
        .filter(|(project, bare)| !project.is_empty() && !bare.is_empty())
}

/// Discover repos at `paths` into a `(name → Source, default)` registry: each
/// path is git-discovered, named after its working-tree directory (deduped), and
/// mapped to a lazily-opened `graph.db`. Exactly one repo ⇒ it is the default.
type Registry = (BTreeMap<String, Source>, Option<String>);
fn build_registry<I, P>(paths: I) -> Result<Registry, WorkspaceError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut projects: BTreeMap<String, Source> = BTreeMap::new();
    let mut seen_dbs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for path in paths {
        let repo = Repo::discover(path.as_ref())?;
        let db = repo.git_dir().join("roteiro").join("graph.db");
        // De-duplicate the same repo reached via different paths (O(1) lookup, so
        // discovery stays linear even on a big workspace and every reload).
        if !seen_dbs.insert(db.clone()) {
            continue;
        }
        let base = repo
            .workdir()
            .and_then(Path::file_name)
            .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
        let name = dedupe_name(&projects, base);
        projects.insert(name, Source::Path(db));
    }
    if projects.is_empty() {
        return Err(WorkspaceError::Empty);
    }
    let default = if projects.len() == 1 {
        projects.keys().next().cloned()
    } else {
        None
    };
    Ok((projects, default))
}

/// Make `base` unique against the names already in `projects`, appending
/// `-2`, `-3`, … on collision.
fn dedupe_name(projects: &BTreeMap<String, Source>, base: String) -> String {
    if !projects.contains_key(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !projects.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    #[test]
    fn single_project_is_the_default_and_resolves_bare() {
        let ws = Workspace::single("myrepo", store());
        assert_eq!(ws.names(), vec!["myrepo".to_owned()]);
        assert!(!ws.is_multi());
        // A bare call resolves to the sole project.
        assert_eq!(ws.resolve(None).unwrap(), "myrepo");
        // Naming it explicitly works too.
        assert_eq!(ws.resolve(Some("myrepo")).unwrap(), "myrepo");
        // with_store hands over the store.
        let n = ws.with_store(None, |s| s.node_count().unwrap()).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unknown_project_is_an_error_naming_the_known_ones() {
        let ws = Workspace::single("a", store());
        let err = ws.resolve(Some("b")).unwrap_err();
        assert!(matches!(err, WorkspaceError::UnknownProject { .. }));
        assert!(err.to_string().contains("known: a"));
    }

    #[test]
    fn cached_store_handle_is_reused() {
        let ws = Workspace::single("a", store());
        // Two accesses return the same underlying handle (cache hit).
        ws.with_store(None, |s| s.node_count().unwrap()).unwrap();
        let again = ws.handle("a").unwrap();
        // The handle is held by both the cache and this local, so ≥ 2.
        assert!(Arc::strong_count(&again) >= 2);
    }

    #[test]
    fn parse_qualified_splits_on_the_first_double_colon_only() {
        // Bare keys carry single colons; only `::` separates the project.
        assert_eq!(
            parse_qualified("app::sym:rust:a.rs#B"),
            Some(("app", "sym:rust:a.rs#B"))
        );
        assert_eq!(parse_qualified("app::file:x"), Some(("app", "file:x")));
        // Not qualified / malformed.
        assert_eq!(parse_qualified("sym:rust:a.rs#B"), None);
        assert_eq!(parse_qualified("::x"), None);
        assert_eq!(parse_qualified("app::"), None);
    }

    #[test]
    fn resolve_qualified_finds_drift_and_bad_targets() {
        use crate::model::{Node, NodeKind};
        let mut s = store();
        s.apply_factset(&crate::model::FactSet::new().with_node(Node::new(
            "file:cfg.rs",
            NodeKind::File,
            "cfg.rs",
        )))
        .unwrap();
        let ws = Workspace::single("app", s);

        // Resolves an existing node in the named project.
        let hit = ws.resolve_qualified("app::file:cfg.rs").unwrap();
        assert_eq!(hit.map(|n| n.key), Some("file:cfg.rs".to_owned()));
        // Well-formed but absent → drift (Ok(None)).
        assert!(ws.resolve_qualified("app::file:gone.rs").unwrap().is_none());
        // Unknown target project → an error the caller reports as drift.
        assert!(matches!(
            ws.resolve_qualified("ghost::file:x").unwrap_err(),
            WorkspaceError::UnknownProject { .. }
        ));
        // Not project-qualified at all.
        assert!(matches!(
            ws.resolve_qualified("file:cfg.rs").unwrap_err(),
            WorkspaceError::Unqualified { .. }
        ));
    }

    #[test]
    fn follow_external_ref_walks_a_placeholder_to_its_target() {
        use crate::links::external_ref_node;
        use crate::model::{Node, NodeKind};
        let mut s = store();
        // A real target node, plus a placeholder standing in for it (as it would
        // live in a spoke store pointing back at this project).
        s.apply_factset(&crate::model::FactSet::new().with_node(Node::new(
            "file:cfg.rs",
            NodeKind::File,
            "cfg.rs",
        )))
        .unwrap();
        let ws = Workspace::single("app", s);

        // Following the placeholder resolves the qualified target to the real node.
        let placeholder = external_ref_node("app::file:cfg.rs");
        let hit = ws.follow_external_ref(&placeholder).unwrap();
        assert_eq!(hit.map(|n| n.key), Some("file:cfg.rs".to_owned()));

        // A placeholder for a removed target is drift (Ok(None)), not an error.
        let gone = external_ref_node("app::file:gone.rs");
        assert!(ws.follow_external_ref(&gone).unwrap().is_none());

        // A plain (non-external-ref) node is simply not followed.
        let plain = Node::new("file:cfg.rs", NodeKind::File, "cfg.rs");
        assert!(ws.follow_external_ref(&plain).unwrap().is_none());
    }
}
