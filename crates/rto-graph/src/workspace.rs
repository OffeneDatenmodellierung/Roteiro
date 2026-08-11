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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::git::{GitError, Repo};
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
    /// The project's graph store does not exist yet — its repo has not been
    /// synced (`roteiro sync`).
    #[error("project `{name}` has no graph yet — run `roteiro sync` in {}", .path.display())]
    NoGraph {
        /// The project name.
        name: String,
        /// The repo directory whose graph is missing.
        path: PathBuf,
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
enum Source {
    /// Open this `graph.db` path on first use.
    Path(PathBuf),
    /// A pre-opened store, shared directly.
    Open(Arc<Mutex<Store>>),
}

/// A named set of per-repo graphs, each opened on demand and cached. Cheap to
/// hold: the stores are small `SQLite` files opened lazily; the caller (a server)
/// holds the one expensive model.
pub struct Workspace {
    /// Project name → its store source, in stable name order.
    projects: BTreeMap<String, Source>,
    /// The project used when a call omits `project` (the sole project, if there
    /// is exactly one; otherwise `None` and a bare call is ambiguous).
    default: Option<String>,
    /// Opened stores, cached by project name. `Store` is `!Sync` (it holds a
    /// rusqlite connection), so each is behind its own `Mutex`.
    cache: Mutex<HashMap<String, Arc<Mutex<Store>>>>,
}

impl Workspace {
    /// A single-project workspace over an already-open `store`, named `name`.
    /// This is the single-repo `serve` default and the test constructor; a bare
    /// (no-`project`) call resolves to it.
    #[must_use]
    pub fn single(name: impl Into<String>, store: Store) -> Self {
        let name = name.into();
        let mut projects = BTreeMap::new();
        projects.insert(name.clone(), Source::Open(Arc::new(Mutex::new(store))));
        Self {
            projects,
            default: Some(name),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build a workspace from repo directories: each is `git`-discovered, named
    /// after its working-tree directory (collisions get a `-2`, `-3`, … suffix),
    /// and its `graph.db` opened lazily. With exactly one repo, that repo is the
    /// default project.
    ///
    /// # Errors
    /// [`WorkspaceError::Git`] if a path is not inside a git repository, or
    /// [`WorkspaceError::Empty`] if `paths` is empty.
    pub fn from_repo_paths<I, P>(paths: I) -> Result<Self, WorkspaceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut projects: BTreeMap<String, Source> = BTreeMap::new();
        let mut seen_dbs: Vec<PathBuf> = Vec::new();
        for path in paths {
            let repo = Repo::discover(path.as_ref())?;
            let db = repo.git_dir().join("roteiro").join("graph.db");
            // De-duplicate the same repo reached via different paths.
            if seen_dbs.contains(&db) {
                continue;
            }
            seen_dbs.push(db.clone());
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
        // Exactly one project ⇒ it is the default (a bare, no-`project` call
        // resolves to it, so single-repo serving needs no selector).
        let default = if projects.len() == 1 {
            projects.keys().next().cloned()
        } else {
            None
        };
        Ok(Self {
            projects,
            default,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The registered project names, in stable order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.projects.keys().cloned().collect()
    }

    /// Whether the workspace holds more than one project (so `project` selection
    /// is meaningful to expose to callers/tools).
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.projects.len() > 1
    }

    /// Resolve `project` (or the default) to a concrete project name.
    ///
    /// # Errors
    /// [`WorkspaceError::UnknownProject`] if named but absent,
    /// [`WorkspaceError::AmbiguousProject`] if omitted with several projects, or
    /// [`WorkspaceError::Empty`] if there are none.
    pub fn resolve(&self, project: Option<&str>) -> Result<String, WorkspaceError> {
        match project {
            Some(name) if self.projects.contains_key(name) => Ok(name.to_owned()),
            Some(name) => Err(WorkspaceError::UnknownProject {
                name: name.to_owned(),
                known: self.names().join(", "),
            }),
            None => self.default.clone().ok_or_else(|| {
                if self.projects.is_empty() {
                    WorkspaceError::Empty
                } else {
                    WorkspaceError::AmbiguousProject {
                        known: self.names().join(", "),
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

    /// Get (opening + caching on first use) the shared store handle for `name`.
    fn handle(&self, name: &str) -> Result<Arc<Mutex<Store>>, WorkspaceError> {
        let mut cache = self.cache.lock().map_err(|_| WorkspaceError::Poisoned)?;
        if let Some(handle) = cache.get(name) {
            return Ok(handle.clone());
        }
        let handle = match self.projects.get(name) {
            Some(Source::Open(handle)) => handle.clone(),
            Some(Source::Path(db)) => {
                if !db.exists() {
                    return Err(WorkspaceError::NoGraph {
                        name: name.to_owned(),
                        // The repo dir is the store's grandparent (`…/.git/roteiro`).
                        path: db
                            .parent()
                            .and_then(Path::parent)
                            .and_then(Path::parent)
                            .unwrap_or(db)
                            .to_path_buf(),
                    });
                }
                Arc::new(Mutex::new(Store::open(db)?))
            }
            None => {
                return Err(WorkspaceError::UnknownProject {
                    name: name.to_owned(),
                    known: self.names().join(", "),
                });
            }
        };
        cache.insert(name.to_owned(), handle.clone());
        Ok(handle)
    }
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
}
