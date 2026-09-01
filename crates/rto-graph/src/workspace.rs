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
//! connections, and dropped projects are evicted. The outer
//! [`WorkspaceSet`] reloads the same way ([`WorkspaceSet::reload_from_resolved`])
//! — it must, because a `serve` process holds *both*, and reloading only the
//! inner one left the read-only graph API and the served UI reporting a stale
//! repo list beside a log line announcing a fresh one. Each reload splits into a
//! `plan_reload` that does all the git discovery and an `apply_reload` that only
//! takes a lock, so a caller holding both registries can swap them back to back
//! rather than interleaved with a filesystem walk. An optional first-open hook
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
    /// A selector named a workspace the [`WorkspaceSet`] does not know.
    #[error("no workspace named `{name}` (known: {known})")]
    UnknownWorkspace {
        /// The requested workspace name.
        name: String,
        /// Comma-separated list of known workspace names.
        known: String,
    },
    /// A selection omitted a name but the [`WorkspaceSet`] holds several
    /// workspaces, so the choice is ambiguous.
    #[error("several workspaces configured ({known}); select one with `--workspace-name`")]
    AmbiguousWorkspace {
        /// Comma-separated list of known workspace names.
        known: String,
    },
    /// Reading a workspace root directory during repo discovery failed.
    #[error("reading workspace root `{}`: {msg}", .root.display())]
    Discover {
        /// The root directory that could not be read.
        root: PathBuf,
        /// The underlying I/O error message.
        msg: String,
    },
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
    /// Open this `graph.db` path on first use, for the repository whose working
    /// tree is rooted at `root`.
    ///
    /// `root` is *carried* rather than derived from `db`, because a
    /// repository's own configuration governs how it is scanned, whoever is
    /// asking ([`Workspace::project_root`]) — and the "repo dir is the store's
    /// grandparent" shortcut is wrong for a **linked worktree**, whose git dir
    /// is `<main>/.git/worktrees/<name>`, not `<repo>/.git`. [`build_registry`]
    /// already holds the true working-tree root, so it is recorded here instead
    /// of guessed later. `None` where the caller supplied only a `graph.db`
    /// path ([`Workspace::from_named_dbs`]).
    Path {
        /// The `graph.db` to open.
        db: PathBuf,
        /// The repository's working-tree root, when known.
        root: Option<PathBuf>,
    },
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
/// very same pre-opened handle. The `graph.db` path *is* the store's identity,
/// so the recorded working-tree root does not enter the comparison.
fn source_eq(a: &Source, b: &Source) -> bool {
    match (a, b) {
        (Source::Path { db: x, .. }, Source::Path { db: y, .. }) => x == y,
        (Source::Open(x), Source::Open(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// A hook run against a project's `graph.db` path the first time it is opened —
/// used by `serve --sync-on-access` to (re)build a stale or missing graph before
/// it is served (ADR-0008). Returns a human-readable error on failure.
pub type OnOpen = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// A fully-discovered registry, ready to be swapped into a live [`Workspace`].
///
/// Opaque on purpose: it exists so that the **I/O half** of a reload (git
/// discovery, [`Workspace::plan_reload`]) can be separated from the **swap half**
/// ([`Workspace::apply_reload`]), which takes one lock and does no I/O. A server
/// that must reload several registries coherently plans them all first and then
/// applies them back to back, so the window in which two surfaces could report
/// different repo sets is a pair of adjacent lock acquisitions rather than a
/// filesystem walk.
pub struct ReloadPlan {
    /// Project name → its store source, in stable name order.
    projects: BTreeMap<String, Source>,
    /// The project a bare (no-`project`) call resolves to, if unambiguous.
    default: Option<String>,
}

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

    /// A workspace over several already-open stores, one per named project — the
    /// in-memory counterpart of [`Workspace::from_repo_paths`] (which opens each
    /// project's `graph.db` from disk lazily). Used for multi-repo serving of
    /// pre-built stores and for tests. With exactly one project it becomes the
    /// default (as [`Workspace::single`]); with several, a bare (no-`project`)
    /// call is ambiguous. Not reloadable (no repo paths).
    #[must_use]
    pub fn from_stores<I, S>(stores: I) -> Self
    where
        I: IntoIterator<Item = (S, Store)>,
        S: Into<String>,
    {
        let mut projects = BTreeMap::new();
        for (name, store) in stores {
            // Dedupe like `from_repo_paths` (`-2`, `-3`, …) so two stores sharing a
            // base name both survive instead of the second silently overwriting the
            // first (which would drop a project).
            let name = dedupe_name(&projects, name.into());
            projects.insert(name, Source::Open(Arc::new(Mutex::new(store))));
        }
        // Mirror `from_repo_paths`: a lone project is the default; several are
        // ambiguous until a call names one.
        let default = if projects.len() == 1 {
            projects.keys().next().cloned()
        } else {
            None
        };
        Self {
            inner: Mutex::new(Inner {
                projects,
                default,
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

    /// Build a workspace from explicit `(project name, graph.db path)` pairs,
    /// **without** git discovery — used where the names and store locations are
    /// already known ([`WorkspaceSet`] construction re-uses the CLI's discovery
    /// upstream, and tests build synthetic registries). Names are taken verbatim
    /// (deduplicate before calling if a collision is possible); with exactly one
    /// pair, that project is the default.
    #[must_use]
    pub fn from_named_dbs<I>(dbs: I) -> Self
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let projects: BTreeMap<String, Source> = dbs
            .into_iter()
            .map(|(n, db)| (n, Source::Path { db, root: None }))
            .collect();
        let default = (projects.len() == 1)
            .then(|| projects.keys().next().cloned())
            .flatten();
        Self {
            inner: Mutex::new(Inner {
                projects,
                default,
                cache: HashMap::new(),
            }),
            on_open: None,
        }
    }

    /// The `graph.db` paths of the workspace's lazily-opened (`Path`) projects, in
    /// stable name order. Pre-opened (`single`) projects carry no path and are
    /// omitted. Used by [`WorkspaceSet::containing`] to find which workspace holds
    /// a given repo.
    #[must_use]
    pub fn member_dbs(&self) -> Vec<PathBuf> {
        self.lock()
            .map(|i| {
                i.projects
                    .values()
                    .filter_map(|s| match s {
                        Source::Path { db, .. } => Some(db.clone()),
                        Source::Open(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The **working-tree root** of `project`'s repository, resolving `project`
    /// the same way [`Workspace::with_store`] does (so `None` means the default
    /// project).
    ///
    /// This exists so a caller can read *that repository's own* configuration
    /// rather than the invoking process's. The rule, following ADR-0009's
    /// per-repo `[[links]]` resolution: **a repository's own config governs how
    /// it is scanned, whoever is asking.** Without it, a server started in repo
    /// A answers questions about repo B using A's settings — and B's own
    /// `[debt] ignore` never applies, so the API and B's CLI disagree about B.
    ///
    /// Returns `Ok(None)` when the project's store was handed over pre-opened
    /// ([`Workspace::single`] / [`Workspace::from_stores`]) or registered by
    /// `graph.db` path alone ([`Workspace::from_named_dbs`]): there is no
    /// repository on disk to consult, and the caller falls back to its own
    /// configuration.
    ///
    /// # Errors
    /// [`WorkspaceError::UnknownProject`] / [`WorkspaceError::AmbiguousProject`]
    /// as [`Workspace::resolve`], or [`WorkspaceError::Poisoned`].
    pub fn project_root(&self, project: Option<&str>) -> Result<Option<PathBuf>, WorkspaceError> {
        let name = self.resolve(project)?;
        let inner = self.lock()?;
        Ok(match inner.projects.get(&name) {
            Some(Source::Path { root, .. }) => root.clone(),
            _ => None,
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
    /// This is [`Workspace::plan_reload`] followed immediately by
    /// [`Workspace::apply_reload`]; use the two halves separately when several
    /// registries must be swapped together (see [`WorkspaceSet::plan_reload`]).
    ///
    /// # Errors
    /// As [`Workspace::from_repo_paths`].
    pub fn reload_from<I, P>(&self, paths: I) -> Result<Vec<String>, WorkspaceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.apply_reload(Self::plan_reload(paths)?)
    }

    /// Discover `paths` into the registry a reload would install, **without
    /// touching the live workspace**. All of a reload's I/O (git discovery)
    /// happens here, so [`Workspace::apply_reload`] is a lock-and-swap with no
    /// I/O in it — which is what lets a caller holding several registries swap
    /// them all back to back rather than interleaved with discovery.
    ///
    /// # Errors
    /// As [`Workspace::from_repo_paths`].
    pub fn plan_reload<I, P>(paths: I) -> Result<ReloadPlan, WorkspaceError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let (projects, default) = build_registry(paths)?;
        Ok(ReloadPlan { projects, default })
    }

    /// Install a [`ReloadPlan`] built by [`Workspace::plan_reload`], returning the
    /// new project names. Takes the registry lock once and does no I/O under it.
    ///
    /// # Errors
    /// [`WorkspaceError::Poisoned`] if the registry lock was poisoned.
    pub fn apply_reload(&self, plan: ReloadPlan) -> Result<Vec<String>, WorkspaceError> {
        let ReloadPlan { projects, default } = plan;
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

    /// Like [`Workspace::with_store`], but hands `f` a **mutable** store so it can
    /// persist into the graph (e.g. [`Store::apply_import_layer`]). The store lock
    /// is held only for `f`, never across an `.await`. Backs the explorer's
    /// `links/write` endpoint, which materialises the inferred cross-repo links into
    /// a spoke's graph as a durable import layer.
    ///
    /// # Errors
    /// As [`Workspace::with_store`].
    pub fn with_store_mut<R>(
        &self,
        project: Option<&str>,
        f: impl FnOnce(&mut Store) -> R,
    ) -> Result<R, WorkspaceError> {
        let name = self.resolve(project)?;
        let handle = self.handle(&name)?;
        let mut store = handle.lock().map_err(|_| WorkspaceError::Poisoned)?;
        Ok(f(&mut store))
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

    /// Follow a **project-qualified** cross-repo target to the most specific
    /// *definition* it names — the follow-the-link hop that turns a click on a
    /// spoke's app-key target into a jump to the hub node that defines it.
    ///
    /// [`Workspace::resolve_qualified`] lands on the raw hub node a spoke points
    /// at, which for a config override is the hub's `config_key` node (e.g.
    /// `cfgkey:config.toml#serve.addr`), *not* the Rust struct that declares the
    /// setting. This method adds the net-new **`config_key` → struct bridge**: when
    /// the resolved node is a config key whose dotted path maps — with confidence —
    /// to exactly one hub struct and one of its named fields, it returns that
    /// struct as the jump target ([`Follow::StructField`], carrying the matched
    /// field name). Otherwise it returns the resolved node unchanged
    /// ([`Follow::Node`]) — a config key we could not bridge, or any non-config
    /// target (e.g. an authored `[[links]]` that already points at a symbol). A
    /// well-formed target whose node is gone is [`Follow::Drift`].
    ///
    /// The bridge is deliberately conservative (see [`bridge_config_key`]): it
    /// fires only on a *unique* match of both an independent section→struct-name
    /// signal and a field-presence signal, so it never jumps to a **wrong** node —
    /// an ambiguous or unmatched key falls back to the config-key node.
    ///
    /// # Errors
    /// As [`Workspace::resolve_qualified`] (a well-formed but unhosted / unsynced
    /// target project still errors; a resolved-but-missing node is `Drift`).
    pub fn follow_definition(&self, qualified: &str) -> Result<Follow, WorkspaceError> {
        let (project, key) =
            parse_qualified(qualified).ok_or_else(|| WorkspaceError::Unqualified {
                key: qualified.to_owned(),
            })?;
        let key = key.to_owned();
        self.with_store(Some(project), move |store| -> Result<Follow, StoreError> {
            let Some(node) = store.get_node(&key)? else {
                return Ok(Follow::Drift);
            };
            // Only a config-key node needs bridging; anything else the spoke points
            // at is already a definition-level target. Compare against the stable
            // token via `as_str()` — no allocation to build a throwaway `NodeKind`.
            if node.kind.as_str() == crate::config_keys::KIND {
                match bridge_config_key(store, &node)? {
                    Some((target, field)) => Ok(Follow::StructField {
                        node: target,
                        field,
                    }),
                    None => Ok(Follow::Node { node }),
                }
            } else {
                Ok(Follow::Node { node })
            }
        })?
        .map_err(WorkspaceError::from)
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
        let (db, root) = {
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
                Some(Source::Path { db, root }) => (db.clone(), root.clone()),
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
        let opened = Source::Path {
            db: db.clone(),
            root,
        };
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
fn keys<V>(entries: &BTreeMap<String, V>) -> String {
    entries.keys().cloned().collect::<Vec<_>>().join(", ")
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

/// The outcome of [`Workspace::follow_definition`]: where a cross-repo follow-hop
/// lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// Bridged past a `config_key` node to the hub **struct** that declares the
    /// setting, carrying the specific named field that matched (e.g. the
    /// `ServeConfig` struct for `serve.addr`, `field = "addr"`). The `node` is the
    /// real struct node, so a caller can center it in the hub graph.
    StructField {
        /// The defining struct node (`sym:rust:<file>#<Struct>`).
        node: Node,
        /// The struct field the dotted key resolved to (its declared identifier).
        field: String,
    },
    /// The resolved target node itself, unbridged — a `config_key` we could not map
    /// to a struct with confidence (the safe fallback), or any non-config target a
    /// spoke points straight at.
    Node {
        /// The resolved hub node.
        node: Node,
    },
    /// The target is well-formed but its node is gone — cross-repo drift.
    Drift,
}

/// Bridge a hub **`config_key`** node to the Rust **struct** that declares it, plus
/// the specific field matched — the net-new step behind [`Workspace::follow_definition`].
///
/// The mapping from a dotted config key (`serve.addr`) to a defining Rust field is
/// not recorded anywhere in the graph (the extractor models structs as nodes but
/// not their fields as nodes, and a field's *type* is not captured), so this is a
/// **resolve-time join** over two independent, deterministic signals — and it only
/// bridges when they agree on exactly one struct:
///
/// 1. **section → struct name.** The dotted key's head segment (`serve`) must name
///    the struct: its lower-cased name, with a trailing `Config` stripped, equals
///    the section (`ServeConfig` → `serve`; a bare `Serve` also matches). See
///    [`struct_matches_section`].
/// 2. **field presence.** The struct must actually declare a field whose
///    normalised name equals the key's leaf (`addr`, or `tls_cert` for
///    `serve.tls_cert`) — read from the struct's `meta.fields`. See
///    [`struct_field_matching`].
///
/// Requiring a **unique** `(struct, field)` hit is the correctness rule: a key that
/// matches zero structs (no such section, or the field isn't declared) or more than
/// one (genuinely ambiguous) returns `None`, and the caller falls back to the
/// config-key node rather than risk jumping to a wrong definition.
///
/// Known limits (documented, deliberate): a single-segment key (no section, e.g.
/// `port`) is never bridged; a key nested past one level (`serve.tls.cert` where
/// `tls` is a sub-struct) won't match a flat field and falls back; and a struct
/// whose name doesn't follow the `<Section>Config` convention won't be found. All
/// three degrade to the existing config-key target — never to a wrong one.
fn bridge_config_key(store: &Store, cfg_node: &Node) -> Result<Option<(Node, String)>, StoreError> {
    // The dotted key: authoritative from `meta.key`, falling back to the node name
    // (both are the dotted path in practice — see config-key extraction).
    let dotted = cfg_node
        .meta
        .get("key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(cfg_node.name.as_str());
    let Some((section, leaf)) = split_section_field(dotted) else {
        return Ok(None);
    };
    let leaf_norm = crate::config_keys::normalize(leaf);
    if leaf_norm.is_empty() {
        return Ok(None);
    }

    // Fetch only the CANDIDATE struct(s) for this section by name, rather than
    // loading and JSON-decoding every `struct` node in the graph on each hop
    // (a latency spike on a large hub). `section_struct_names` yields the exact
    // lower-cased names `struct_matches_section` would accept, so this narrows the
    // scan without changing the bridging semantics; `struct_matches_section` is
    // still applied below as the authoritative check.
    let mut candidates: Vec<Node> = Vec::new();
    for name in section_struct_names(section) {
        candidates.extend(store.nodes_by_kind_named(&crate::NodeKind::Struct, &name)?);
    }

    let mut hits = candidates
        .into_iter()
        .filter(|s| struct_matches_section(&s.name, section))
        .filter_map(|s| struct_field_matching(&s, &leaf_norm).map(|field| (s, field)));

    match (hits.next(), hits.next()) {
        // Exactly one confident match → bridge to it.
        (Some(one), None) => Ok(Some(one)),
        // Zero or ambiguous (>1) → fall back to the config-key node.
        _ => Ok(None),
    }
}

/// Split a dotted config key into `(section, leaf)` on its **first** separator:
/// `serve.addr` → `("serve", "addr")`, `serve.tls_cert` → `("serve", "tls_cert")`.
/// A single-segment key (`port`) has no section to identify a struct by, so it is
/// `None` (never bridged).
fn split_section_field(dotted: &str) -> Option<(&str, &str)> {
    dotted
        .split_once('.')
        .filter(|(section, leaf)| !section.is_empty() && !leaf.is_empty())
}

/// The section's canonical form for name-matching: normalised, separators removed
/// (`serve` → `serve`, `serve_mode` → `servemode`). Empty when the section carries
/// no alphanumerics.
fn section_key(section: &str) -> String {
    crate::config_keys::normalize(section).replace('.', "")
}

/// The lower-cased struct names a config `section` can map to — exactly the names
/// [`struct_matches_section`] accepts: `serve` → `["serve", "serveconfig"]`. Used
/// to fetch just the candidate struct(s) by name instead of scanning them all
/// (kept in lock-step with [`struct_matches_section`], which remains the check).
fn section_struct_names(section: &str) -> Vec<String> {
    let want = section_key(section);
    if want.is_empty() {
        return Vec::new();
    }
    let with_config = format!("{want}config");
    vec![want, with_config]
}

/// Whether a struct `name` is the one a config `section` maps to: its lower-cased
/// name with a trailing `config` stripped equals the section (case- and
/// separator-insensitive). `ServeConfig`/`Serve` both match section `serve`;
/// `ServeSettings` does not (so an unrelated struct is never bridged to).
fn struct_matches_section(name: &str, section: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    let base = lname.strip_suffix("config").unwrap_or(&lname);
    let want = section_key(section);
    !want.is_empty() && base == want
}

/// The struct field whose normalised identifier equals `leaf_norm`, read from the
/// struct node's `meta.fields` (see extraction). Returns the field's original
/// declared name (for display), or `None` when the struct declares no such field.
fn struct_field_matching(struct_node: &Node, leaf_norm: &str) -> Option<String> {
    struct_node
        .meta
        .get("fields")?
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|field| crate::config_keys::normalize(field) == leaf_norm)
        .map(ToOwned::to_owned)
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
        projects.insert(
            name,
            Source::Path {
                db,
                // The repository's own root, so its own config can be read later.
                root: repo.workdir().map(Path::to_path_buf),
            },
        );
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

/// Shallow git-repo discovery under `root`: the root itself if it is a repo, plus
/// each immediate subdirectory that is one, in sorted order. Shallow by design — a
/// code directory holding sibling checkouts is the common case, and a deep scan
/// would be slow and surprising. Shared by the CLI's workspace collection and
/// [`WorkspaceSet`] / config resolution, so the membership rule lives in one place.
///
/// A repo is any directory containing a `.git` entry (a directory in a normal
/// clone, a file in worktrees and submodules), so existence — not `is_dir` — is
/// tested.
///
/// The rule is invisible to whoever passes the root, which is a separate defect
/// from the rule being wrong: see [`RootScan`], and the `--workspace` help text
/// that now says "immediate subdirectories" rather than "under" (issue #580).
///
/// # Errors
/// [`WorkspaceError::Discover`] if `root` cannot be read.
pub fn discover_repos_under(root: &Path) -> Result<Vec<PathBuf>, WorkspaceError> {
    Ok(scan_root(root)?.repos)
}

/// Whether `dir` is a git repository: it holds a `.git` **entry**. A directory in
/// a normal clone, a file in worktrees and submodules — so existence is the test,
/// not `is_dir`.
fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Where `render okf` writes when `--out` is omitted, and therefore where a
/// workspace member's published bundle is looked for.
///
/// A convention rather than a discovery: nothing in OKF says where a bundle
/// lives in a repository, so the only directory we can name without guessing is
/// the one **this** tool writes to. A peer who publishes elsewhere is still
/// importable by hand with `roteiro import --from okf <path>`, which is the
/// reason that command survives automatic discovery (issue #706, decision 3).
pub const OKF_BUNDLE_DIR: &str = "okf";

/// The OKF bundle a repository at `repo_root` publishes, if it publishes one.
///
/// # The test is `okf_version`, not the directory's existence
///
/// A directory called `okf` proves nothing — it could be source, notes, or a
/// half-written experiment. OKF §10 says a bundle root's `index.md` declares
/// `okf_version`, and that declaration is the only thing in the format that says
/// "this is a bundle, and it is one of these". Requiring it is what stops
/// discovery from offering to import an arbitrary directory of markdown, and it
/// is deliberately the *stricter* of the two available tests: a false positive
/// here becomes a consent prompt about something that is not a bundle, which
/// trains the reader to dismiss the prompt.
///
/// # Why this parses a little YAML rather than calling the reader
///
/// `rto-render` depends on this crate, so the OKF reader cannot be called from
/// here without inverting the dependency. The probe is deliberately tiny — a
/// bounded read of the leading frontmatter block, looking for one key — rather
/// than a second parser: it decides only *whether to offer* the bundle, and the
/// reader still decides what the bundle contains.
#[must_use]
pub fn okf_bundle_in(repo_root: &Path) -> Option<PathBuf> {
    let dir = repo_root.join(OKF_BUNDLE_DIR);
    let index = dir.join("index.md");
    // Bounded: a bundle index's frontmatter is a few hundred bytes, and a file
    // that is not one should not be read into memory to find that out.
    let mut buf = Vec::new();
    {
        use std::io::Read as _;
        let file = std::fs::File::open(&index).ok()?;
        file.take(4096).read_to_end(&mut buf).ok()?;
    }
    let head = String::from_utf8_lossy(&buf);
    let rest = head
        .strip_prefix("---\n")
        .or_else(|| head.strip_prefix("---\r\n"))?;
    // The **closing** fence is required, not optional. `split(…).next()` returns
    // the whole remainder when there is no `\n---`, which would make any
    // `index.md` opening with `---` and mentioning `okf_version:` anywhere in
    // the first 4 KiB read as a bundle — including in ordinary prose under an
    // unterminated block. This probe exists to be *stricter* than "a directory
    // called okf", and a false positive here is a consent prompt about something
    // that is not a bundle, which teaches the reader to dismiss the prompt.
    //
    // The cost is a false negative on an index whose frontmatter does not close
    // within the bounded read. A bundle root's frontmatter is a handful of
    // lines, so that is the safe direction to be wrong in.
    let (block, _) = rest.split_once("\n---")?;
    block
        .lines()
        .any(|line| {
            line.split_once(':')
                .is_some_and(|(k, v)| k.trim() == "okf_version" && !v.trim().is_empty())
        })
        .then_some(dir)
}

/// A workspace member's published OKF bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfBundle {
    /// The member repository's working-tree root.
    pub repo: PathBuf,
    /// The bundle directory inside it.
    pub bundle: PathBuf,
    /// The peer name: the member repository's directory name, which is also the
    /// project name [`build_registry`] derives and the `--peer` default
    /// `roteiro import --from okf` uses. One name, so a bundle discovered
    /// automatically and the same bundle imported by hand land on **one** import
    /// layer rather than two.
    pub peer: String,
}

/// Every OKF bundle published by a member in `repo_roots`, in path order.
///
/// Pure filesystem probing: one `open` and one bounded read per member. It opens
/// no store and makes no decision — [`crate::Store::okf_consent_holds`] is what
/// says whether a bundle may be read, and that is a separate question asked of a
/// separate crate.
#[must_use]
pub fn discover_okf_bundles(repo_roots: &[PathBuf]) -> Vec<OkfBundle> {
    let mut out: Vec<OkfBundle> = repo_roots
        .iter()
        .filter_map(|repo| {
            let bundle = okf_bundle_in(repo)?;
            let peer = repo.file_name()?.to_str()?.to_owned();
            Some(OkfBundle {
                repo: repo.clone(),
                bundle,
                peer,
            })
        })
        .collect();
    out.sort_by(|a, b| a.repo.cmp(&b.repo));
    out
}

/// What a shallow scan of one root found, **including what it walked past**.
///
/// [`discover_repos_under`] answers the membership question and is what building
/// a workspace uses. This answers the diagnostic one, because the shallow rule is
/// invisible at exactly the moment it matters: a root whose repos all live one
/// level deeper (`~/GIT/<org>/<repo>`, a common layout) yields a near-empty
/// workspace and no error, so the failure presents later as "the graph tools
/// return nothing useful" rather than as a configuration mistake (issue #580).
///
/// The rule itself is deliberate and is not what this changes — see
/// [`discover_repos_under`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootScan {
    /// The root scanned.
    pub root: PathBuf,
    /// Repos found: the root itself if it is one, plus each immediate
    /// subdirectory that is one, sorted.
    pub repos: Vec<PathBuf>,
    /// Immediate subdirectories that are **not** repos, sorted. A repo nested
    /// inside one of these is not hosted; counting them is free here because the
    /// scan already read the directory, which is why the successful-start note
    /// can report it without a second pass.
    pub skipped: Vec<PathBuf>,
}

impl RootScan {
    /// Which skipped subdirectories hold a repo **directly** beneath them — the
    /// ones a user almost certainly meant to reach.
    ///
    /// Costs one `read_dir` per skipped directory, so it is **bounded** by `limit`
    /// and is for the path where the user is already stuck: a root that yielded
    /// nothing to serve. A successful start reports [`RootScan::skipped`] instead,
    /// which the scan already knows.
    #[must_use]
    pub fn nested_repo_parents(&self, limit: usize) -> Vec<&Path> {
        self.skipped
            .iter()
            .take(limit)
            .filter(|dir| {
                std::fs::read_dir(dir).is_ok_and(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .any(|e| e.path().is_dir() && is_repo(&e.path()))
                })
            })
            .map(PathBuf::as_path)
            .collect()
    }
}

/// The shallow scan behind [`discover_repos_under`], keeping what it skipped.
///
/// # Errors
/// [`WorkspaceError::Discover`] if `root` cannot be read.
pub fn scan_root(root: &Path) -> Result<RootScan, WorkspaceError> {
    let mut repos = Vec::new();
    if is_repo(root) {
        repos.push(root.to_path_buf());
    }
    let entries = std::fs::read_dir(root).map_err(|e| WorkspaceError::Discover {
        root: root.to_path_buf(),
        msg: e.to_string(),
    })?;
    let (mut children, mut skipped): (Vec<PathBuf>, Vec<PathBuf>) = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .partition(|p| is_repo(p));
    children.sort();
    skipped.sort();
    repos.extend(children);
    Ok(RootScan {
        root: root.to_path_buf(),
        repos,
        skipped,
    })
}

/// A workspace group after config normalisation ([`crate::WorkspaceSet`] input): a
/// name, its member `roots`/`repos` (unexpanded — discovered when the set is
/// built), and whether its repos are cross-**linked** (served as one multi-repo
/// graph) or **standalone** (each its own single-repo graph, no cross-repo links).
///
/// A `linked = false` (standalone) group denotes **exactly one** single-repo graph:
/// the config normaliser emits one such group per discovered repo, and
/// [`WorkspaceSet::from_resolved`] upholds the invariant by materialising a
/// standalone group as a one-repo [`Workspace`] per member — a standalone group can
/// never collapse several repos into one unlinked multi-repo graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspace {
    /// The workspace name (the `--workspace-name` selector).
    pub name: String,
    /// Directories to scan for member repos (as `[workspace] roots`).
    pub roots: Vec<String>,
    /// Explicit member repo paths, in addition to anything under `roots`.
    pub repos: Vec<String>,
    /// `true` ⇒ the repos form one linked graph; `false` ⇒ **standalone**: each
    /// member repo is its own single-repo graph (no cross-repo links).
    pub linked: bool,
}

/// Discover each resolved group's member repo paths as
/// `(workspace name, repo paths, linked)`, in config order.
///
/// The **one** place a `[[workspaces]]`/`[standalone]` group becomes a concrete
/// set of repos, shared by [`WorkspaceSet::from_resolved`] and
/// [`WorkspaceSet::plan_reload`] so a reloaded set is exactly the set a restart
/// would have produced. A **standalone** (`linked = false`) group is split into
/// one single-repo entry per member, upholding the "a standalone workspace is
/// exactly one repo" invariant structurally; the extras take a `-2`/`-3` suffix.
/// A group that resolves to no repos is skipped, so a stale root never aborts the
/// whole set.
fn discover_groups(
    resolved: Vec<ResolvedWorkspace>,
) -> Result<Vec<(String, Vec<PathBuf>, bool)>, WorkspaceError> {
    let mut out: Vec<(String, Vec<PathBuf>, bool)> = Vec::new();
    for rw in resolved {
        let mut paths: Vec<PathBuf> = Vec::new();
        for root in &rw.roots {
            paths.extend(discover_repos_under(Path::new(root))?);
        }
        for repo in &rw.repos {
            paths.push(PathBuf::from(repo));
        }
        if paths.is_empty() {
            // A group naming nothing (e.g. a `roots` dir with no repos) is simply
            // absent rather than an error.
            continue;
        }
        if rw.linked {
            out.push((rw.name.clone(), paths, true));
        } else {
            for (i, path) in paths.into_iter().enumerate() {
                let name = if i == 0 {
                    rw.name.clone()
                } else {
                    format!("{}-{}", rw.name, i + 1)
                };
                out.push((name, vec![path], false));
            }
        }
    }
    Ok(out)
}

/// One entry in a [`WorkspaceSet`]: a built [`Workspace`] plus whether its member
/// repos are cross-linked. The workspace is held behind an `Arc` so an
/// already-shared workspace (e.g. the one a `serve` process holds for its model
/// tools and MCP router) can be wrapped into a set without re-opening its stores
/// ([`WorkspaceSet::from_single`]).
struct WorkspaceEntry {
    /// The per-group workspace (one repo for a standalone singleton, several for a
    /// linked group).
    workspace: Arc<Workspace>,
    /// Whether the group's repos are cross-linked.
    linked: bool,
}

/// An install's **many** named workspaces: linked groups (multi-repo graphs) and
/// standalone singletons (one-repo graphs), keyed by name in stable order (ADR-0008
/// multi-workspace). The outer layer over [`Workspace`]: it selects *which*
/// workspace a command operates on, then hands back that `Workspace` to resolve
/// projects within it. Built from normalised config ([`WorkspaceSet::from_resolved`])
/// so the `serve`/`links` selection logic is shared.
pub struct WorkspaceSet {
    /// The named workspaces plus the default selection, behind one lock so the
    /// set is **reloadable in place** ([`WorkspaceSet::apply_reload`]) exactly as
    /// a [`Workspace`]'s project registry is. Held only long enough to clone the
    /// `Arc` a selection resolves to, never across a graph query.
    inner: std::sync::RwLock<SetInner>,
}

/// The mutable half of a [`WorkspaceSet`].
struct SetInner {
    /// Workspace name → its entry, in stable (`BTreeMap`) name order.
    entries: BTreeMap<String, WorkspaceEntry>,
    /// The workspace used when a selection omits a name (the sole workspace, if
    /// there is exactly one; otherwise `None` and a bare selection is ambiguous).
    default: Option<String>,
}

/// A fully-built set of named workspaces, ready to be swapped into a live
/// [`WorkspaceSet`]. The [`ReloadPlan`] counterpart for the outer layer — see
/// [`WorkspaceSet::plan_reload`].
pub struct SetReloadPlan {
    /// The entries to install, and for a **retained** workspace the project
    /// registry to swap into it (planned, not yet applied).
    entries: Vec<(String, WorkspaceEntry, Option<ReloadPlan>)>,
    /// The default selection the new set will carry.
    default: Option<String>,
    /// Every member repo path this plan discovered, across all groups, in group
    /// order — see [`SetReloadPlan::repo_paths`].
    repo_paths: Vec<PathBuf>,
}

impl SetReloadPlan {
    /// Every member repo path this plan discovered, across all groups, in group
    /// order.
    ///
    /// This exists so that a caller holding a **flattened** [`Workspace`] beside
    /// the set — `roteiro serve`/`mcp` does, one per surface — can plan its
    /// reload from *these very paths* rather than walking the same roots a second
    /// time. Two walks is two filesystem views: a repo created between them lands
    /// in one surface and not the other, which is a smaller version of the exact
    /// disagreement the whole reload-both change exists to remove. Not
    /// deduplicated here, because [`Workspace::from_repo_paths`] deduplicates by
    /// resolved `graph.db`, which is the stronger identity anyway.
    #[must_use]
    pub fn repo_paths(&self) -> &[PathBuf] {
        &self.repo_paths
    }
}

impl WorkspaceSet {
    /// Take the read lock for a **decision** — a selection, or the snapshot a
    /// reload plans against — reporting a poisoned lock as an error.
    ///
    /// The only writer is [`WorkspaceSet::apply_reload`], which replaces
    /// `entries` and `default` as two separate moves. If it panicked between
    /// them the pair is genuinely inconsistent, and resolving a default against a
    /// half-swapped set would hand back the wrong workspace. So a decision fails
    /// loudly here; see [`WorkspaceSet::peek`] for the reporting counterpart.
    fn read(&self) -> Result<std::sync::RwLockReadGuard<'_, SetInner>, WorkspaceError> {
        self.inner.read().map_err(|_| WorkspaceError::Poisoned)
    }

    /// Take the read lock for a **report** — a listing, never a resolution —
    /// reading *through* a poisoned lock.
    ///
    /// These accessors cannot return a `Result`, so the alternative is an empty
    /// list, and an empty list is a lie: it renders a poisoned set as "no
    /// workspaces configured", which is the confidently-wrong-message shape this
    /// whole change exists to remove — and it would empty the `known:` list in
    /// the very error a person is reading to find out what went wrong. The data
    /// behind the lock is a map of `Arc`s replaced by whole-value assignment, so
    /// reading it after a panicking writer yields the old or the new map, never
    /// a torn one.
    fn peek(&self) -> std::sync::RwLockReadGuard<'_, SetInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Assemble a set from pre-built named workspaces — the shared core of
    /// [`WorkspaceSet::from_resolved`] and the test constructor. With exactly one
    /// entry, that workspace is the default (a bare selection resolves to it).
    #[must_use]
    pub fn from_workspaces<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (String, Workspace, bool)>,
    {
        let entries: BTreeMap<String, WorkspaceEntry> = entries
            .into_iter()
            .map(|(name, workspace, linked)| {
                (
                    name,
                    WorkspaceEntry {
                        workspace: Arc::new(workspace),
                        linked,
                    },
                )
            })
            .collect();
        let default = (entries.len() == 1)
            .then(|| entries.keys().next().cloned())
            .flatten();
        Self {
            inner: std::sync::RwLock::new(SetInner { entries, default }),
        }
    }

    /// Wrap an already-built [`Workspace`] (shared via `Arc`) as a one-entry set
    /// under `name`, with `linked` recording whether that workspace is a
    /// cross-linked multi-repo group. Used where a single `Workspace` is served as
    /// the whole set — e.g. `roteiro serve` merges the read-only graph API over the
    /// one workspace it already holds for its model tools and MCP router, so the
    /// API's flat routes resolve to it as the sole (default) workspace. The store
    /// handles are shared, never re-opened.
    #[must_use]
    pub fn from_single(name: impl Into<String>, workspace: Arc<Workspace>, linked: bool) -> Self {
        let name = name.into();
        let mut entries = BTreeMap::new();
        entries.insert(name.clone(), WorkspaceEntry { workspace, linked });
        Self {
            inner: std::sync::RwLock::new(SetInner {
                entries,
                default: Some(name),
            }),
        }
    }

    /// Build a set from normalised config groups: each group's `roots`/`repos` are
    /// discovered into member repo paths and opened as [`Workspace`]s. A **linked**
    /// group becomes one multi-repo graph. A **standalone** (`linked = false`) group
    /// becomes one single-repo graph **per member repo** — the invariant that a
    /// standalone workspace is exactly one repo is upheld *here*, by splitting, so a
    /// hand-built group can never collapse several repos into one unlinked multi-repo
    /// graph (the config normaliser already emits standalone as per-repo singletons,
    /// so in practice each such group has exactly one repo and the split is a no-op).
    /// On a split, the extra members take a `-2`/`-3` suffix off the group name. A
    /// group that resolves to **no** repos is skipped, so a stale root never aborts
    /// the whole set.
    ///
    /// # Errors
    /// [`WorkspaceError::Discover`] if a group's root cannot be read, or
    /// [`WorkspaceError::Git`] if an explicit repo path is not inside a git repo.
    pub fn from_resolved(resolved: Vec<ResolvedWorkspace>) -> Result<Self, WorkspaceError> {
        let mut entries: BTreeMap<String, WorkspaceEntry> = BTreeMap::new();
        for (name, paths, linked) in discover_groups(resolved)? {
            entries.insert(
                name,
                WorkspaceEntry {
                    workspace: Arc::new(Workspace::from_repo_paths(&paths)?),
                    linked,
                },
            );
        }
        let default = (entries.len() == 1)
            .then(|| entries.keys().next().cloned())
            .flatten();
        Ok(Self {
            inner: std::sync::RwLock::new(SetInner { entries, default }),
        })
    }

    /// Re-discover `resolved` into the set a reload would install, **without
    /// touching the live set**. All of the reload's I/O (root scans, git
    /// discovery) happens here; [`WorkspaceSet::apply_reload`] is then a swap.
    ///
    /// A workspace whose **name and linkage** survive the reload keeps its very
    /// `Arc<Workspace>` — so its open stores stay warm and any handle already
    /// shared out (`workspace_handles`, a scoped tool registry) keeps pointing at
    /// the live workspace — and receives a planned [`ReloadPlan`] for its own
    /// project registry, which retains warm connections per
    /// [`Workspace::apply_reload`]. A workspace that is new, gone, or has flipped
    /// between linked and standalone is rebuilt or dropped, because in those
    /// cases the name no longer denotes the same thing.
    ///
    /// Planning reads the current entries; concurrent reloads must be serialised
    /// by the caller (the SIGHUP handler holds one lock for the whole reload), or
    /// the later plan simply wins.
    ///
    /// # Errors
    /// As [`WorkspaceSet::from_resolved`].
    pub fn plan_reload(
        &self,
        resolved: Vec<ResolvedWorkspace>,
    ) -> Result<SetReloadPlan, WorkspaceError> {
        let groups = discover_groups(resolved)?;
        // Snapshot the current entries (cheap `Arc` clones) and release the lock
        // before any further discovery.
        let current: BTreeMap<String, WorkspaceEntry> = {
            let inner = self.read()?;
            inner
                .entries
                .iter()
                .map(|(n, e)| {
                    (
                        n.clone(),
                        WorkspaceEntry {
                            workspace: e.workspace.clone(),
                            linked: e.linked,
                        },
                    )
                })
                .collect()
        };
        let mut entries: Vec<(String, WorkspaceEntry, Option<ReloadPlan>)> = Vec::new();
        // Every path this one walk found, kept so a flattened workspace beside
        // the set can be planned from the same discovery rather than a second.
        let mut repo_paths: Vec<PathBuf> = Vec::new();
        for (name, paths, linked) in groups {
            repo_paths.extend(paths.iter().cloned());
            match current.get(&name) {
                Some(existing) if existing.linked == linked => entries.push((
                    name,
                    WorkspaceEntry {
                        workspace: existing.workspace.clone(),
                        linked,
                    },
                    Some(Workspace::plan_reload(&paths)?),
                )),
                _ => entries.push((
                    name,
                    WorkspaceEntry {
                        workspace: Arc::new(Workspace::from_repo_paths(&paths)?),
                        linked,
                    },
                    None,
                )),
            }
        }
        // `from_resolved` collects into a `BTreeMap`, so a duplicated group name
        // keeps the last entry; count distinct names the same way here.
        let distinct: std::collections::BTreeSet<&String> =
            entries.iter().map(|(n, _, _)| n).collect();
        let default = (distinct.len() == 1)
            .then(|| distinct.into_iter().next().cloned())
            .flatten();
        Ok(SetReloadPlan {
            entries,
            default,
            repo_paths,
        })
    }

    /// Install a [`SetReloadPlan`], returning the new workspace names in stable
    /// order. Does no I/O: each retained workspace's planned registry is swapped
    /// in, then the entry map is replaced under one write lock.
    ///
    /// # Errors
    /// [`WorkspaceError::Poisoned`] if a lock was poisoned.
    pub fn apply_reload(&self, plan: SetReloadPlan) -> Result<Vec<String>, WorkspaceError> {
        let SetReloadPlan {
            entries, default, ..
        } = plan;
        let mut next: BTreeMap<String, WorkspaceEntry> = BTreeMap::new();
        for (name, entry, registry) in entries {
            if let Some(registry) = registry {
                entry.workspace.apply_reload(registry)?;
            }
            next.insert(name, entry);
        }
        let names: Vec<String> = next.keys().cloned().collect();
        let mut inner = self.inner.write().map_err(|_| WorkspaceError::Poisoned)?;
        inner.entries = next;
        inner.default = default;
        Ok(names)
    }

    /// Re-discover `resolved` and install it — [`WorkspaceSet::plan_reload`]
    /// followed by [`WorkspaceSet::apply_reload`]. Use the halves separately when
    /// another registry must be swapped in the same breath.
    ///
    /// # Errors
    /// As [`WorkspaceSet::plan_reload`].
    pub fn reload_from_resolved(
        &self,
        resolved: Vec<ResolvedWorkspace>,
    ) -> Result<Vec<String>, WorkspaceError> {
        self.apply_reload(self.plan_reload(resolved)?)
    }

    /// The configured workspace names, in stable order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.peek().entries.keys().cloned().collect()
    }

    /// Each configured workspace as a `(name, shared handle)` pair, in stable name
    /// order. The `Arc<Workspace>` is the very handle the set holds, so a caller can
    /// build a **per-workspace** view — e.g. a tool registry confined to one
    /// workspace's projects — over the same lazily-opened stores, never re-opening
    /// them. Used by `serve` to scope the workspace-level Ask to the selected
    /// workspace (ADR-0008), mirroring how [`WorkspaceSet::select`] scopes the
    /// read-only `/v1/graph/workspaces/{ws}/…` routes.
    #[must_use]
    pub fn workspace_handles(&self) -> Vec<(String, Arc<Workspace>)> {
        self.peek()
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.workspace.clone()))
            .collect()
    }

    /// Whether workspace `name` is linked (`Some(true)`), standalone
    /// (`Some(false)`), or unknown (`None`).
    #[must_use]
    pub fn linked(&self, name: &str) -> Option<bool> {
        self.peek().entries.get(name).map(|e| e.linked)
    }

    /// Select a workspace by `name`, or the default when `name` is `None`.
    ///
    /// Hands back the shared `Arc` rather than a borrow, because the set is
    /// reloadable: a caller that held a reference into the entry map would pin it
    /// against the swap. The handle stays valid across a reload — a retained
    /// workspace *is* reloaded in place, so a caller reading through it sees the
    /// new project set rather than a detached snapshot.
    ///
    /// # Errors
    /// [`WorkspaceError::UnknownWorkspace`] if named but absent,
    /// [`WorkspaceError::AmbiguousWorkspace`] if omitted with several configured,
    /// or [`WorkspaceError::Empty`] if none are configured.
    pub fn select(&self, name: Option<&str>) -> Result<Arc<Workspace>, WorkspaceError> {
        // One guard for the lookup *and* the error it may raise. Two reads would
        // let a reload land between them, so the `known:` list could name a set
        // the lookup never saw — a message that is confidently wrong about the
        // very thing the reader is consulting it for. (It also removes a nested
        // read-lock acquisition on one thread, which `RwLock` does not promise.)
        let inner = self.read()?;
        if let Some(n) = name {
            return inner
                .entries
                .get(n)
                .map(|e| e.workspace.clone())
                .ok_or_else(|| WorkspaceError::UnknownWorkspace {
                    name: n.to_owned(),
                    known: keys(&inner.entries),
                });
        }
        // No name given: the sole workspace, else ambiguous / empty.
        let name = inner.default.as_ref().ok_or_else(|| {
            if inner.entries.is_empty() {
                WorkspaceError::Empty
            } else {
                WorkspaceError::AmbiguousWorkspace {
                    known: keys(&inner.entries),
                }
            }
        })?;
        Ok(inner.entries[name].workspace.clone())
    }

    /// The **name** of the workspace [`WorkspaceSet::select`] resolves for `name`:
    /// the given name when present (and valid), else the sole/default workspace's
    /// name. Same resolution and errors as `select`, but returns the concrete name
    /// — so a caller (e.g. the `/follow` endpoint) can report which workspace it
    /// actually resolved in, even on a flat route where the default was implicit.
    ///
    /// # Errors
    /// As [`WorkspaceSet::select`].
    pub fn select_name(&self, name: Option<&str>) -> Result<String, WorkspaceError> {
        let inner = self.read()?;
        if let Some(n) = name {
            return inner
                .entries
                .get_key_value(n)
                .map(|(k, _)| k.clone())
                .ok_or_else(|| WorkspaceError::UnknownWorkspace {
                    name: n.to_owned(),
                    known: keys(&inner.entries),
                });
        }
        inner.default.clone().ok_or_else(|| {
            if inner.entries.is_empty() {
                WorkspaceError::Empty
            } else {
                WorkspaceError::AmbiguousWorkspace {
                    known: keys(&inner.entries),
                }
            }
        })
    }

    /// The name of the workspace whose member repos include the repo whose graph is
    /// `cwd_repo_db` (`<repo>/.git/roteiro/graph.db`), or `None` if no workspace
    /// contains it. Used to default `--workspace-name` to the workspace the current
    /// directory belongs to.
    #[must_use]
    pub fn containing(&self, cwd_repo_db: &Path) -> Option<String> {
        // Snapshot the handles first: `member_dbs` takes each workspace's own
        // lock, and holding the set's lock across that would nest two locks in an
        // order nothing else uses.
        self.workspace_handles().into_iter().find_map(|(name, ws)| {
            ws.member_dbs()
                .iter()
                .any(|db| db == cwd_repo_db)
                .then_some(name)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    /// A poisoned [`WorkspaceSet`] must still *report* what it holds, and must
    /// still *refuse* to resolve one.
    ///
    /// The split is a decision, not an accident, so it is asserted rather than
    /// left to a doc comment. A reader (`names`, `workspace_handles`, `linked`,
    /// and through them `containing` and the error messages' `known:` list) reads
    /// through the poisoning: returning an empty list instead would render a
    /// poisoned set as "no workspaces configured" and blank the `known:` list in
    /// the very error someone is reading to find out what broke. A resolver
    /// (`select`, `select_name`) still fails, because the one writer replaces
    /// `entries` and `default` as two moves and a default resolved against a
    /// half-swapped set is silently the wrong workspace.
    ///
    /// Without this, "simplifying" `peek` back to `unwrap_or_default()` is a
    /// green diff.
    #[test]
    fn a_poisoned_set_still_reports_but_refuses_to_resolve() {
        let set = WorkspaceSet::from_workspaces([
            ("api".to_owned(), Workspace::single("api", store()), true),
            ("web".to_owned(), Workspace::single("web", store()), false),
        ]);
        // Poison the lock the way a writer panicking mid-swap would.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = set.inner.write().expect("write lock");
            panic!("simulated panic while swapping the registry");
        }));
        assert!(poisoned.is_err(), "the closure must have panicked");
        assert!(set.inner.is_poisoned(), "the lock must be poisoned");

        // Reports still report.
        assert_eq!(set.names(), vec!["api".to_owned(), "web".to_owned()]);
        assert_eq!(set.workspace_handles().len(), 2);
        assert_eq!(set.linked("api"), Some(true));
        assert_eq!(set.linked("web"), Some(false));

        // Resolutions still refuse.
        assert!(matches!(
            set.select(Some("api")).err().expect("select must fail"),
            WorkspaceError::Poisoned
        ));
        assert!(matches!(
            set.select_name(None).expect_err("select_name must fail"),
            WorkspaceError::Poisoned
        ));
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
    fn from_stores_dedupes_colliding_names() {
        // Two stores sharing the base name `repo` must both survive: the second
        // is suffixed `repo-2` (like `from_repo_paths`), never dropped.
        let ws = Workspace::from_stores([("repo", store()), ("repo", store())]);
        let mut names = ws.names();
        names.sort();
        assert_eq!(names, vec!["repo".to_owned(), "repo-2".to_owned()]);
        assert!(ws.is_multi());
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

    // -- follow-the-link hop: config_key → struct bridge ------------------

    /// A config-key node as extraction emits it: key `cfgkey:<file>#<dotted>`,
    /// name the dotted key, `meta { key, value }`.
    fn cfg_node(dotted: &str) -> crate::model::Node {
        use crate::model::{Node, NodeKind};
        let mut n = Node::new(
            format!("cfgkey:config.toml#{dotted}"),
            NodeKind::Other("config_key".to_owned()),
            dotted,
        );
        n.meta = serde_json::json!({ "key": dotted, "value": "x" });
        n
    }

    /// A struct node as extraction emits it, carrying its declared field names in
    /// `meta.fields` (the bridge's join signal).
    fn struct_node(name: &str, fields: &[&str]) -> crate::model::Node {
        use crate::model::{Node, NodeKind};
        let mut n = Node::new(format!("sym:rust:config.rs#{name}"), NodeKind::Struct, name);
        n.meta = serde_json::json!({ "fields": fields });
        n
    }

    /// Build a hub with a `ServeConfig`/`addr` struct field AND its `serve.addr`
    /// config key — plus decoys — so the bridge's confidence rules are exercised.
    fn bridge_hub() -> Workspace {
        use crate::model::FactSet;
        let mut s = store();
        s.apply_factset(
            &FactSet::new()
                .with_node(struct_node("ServeConfig", &["addr", "tools", "tls_cert"]))
                .with_node(struct_node("ModelsConfig", &["embedding", "generative"]))
                .with_node(cfg_node("serve.addr"))
                .with_node(cfg_node("serve.tls_cert"))
                .with_node(cfg_node("serve.ghost")) // resolves, but no such field
                .with_node(cfg_node("mystery.addr")) // no struct for section `mystery`
                .with_node(cfg_node("port")), // single-segment: no section
        )
        .unwrap();
        Workspace::single("hub", s)
    }

    #[test]
    fn follow_bridges_config_key_to_its_defining_struct_field() {
        let ws = bridge_hub();
        // `serve.addr` bridges to the `ServeConfig` struct, field `addr`.
        match ws
            .follow_definition("hub::cfgkey:config.toml#serve.addr")
            .unwrap()
        {
            Follow::StructField { node, field } => {
                assert_eq!(node.key, "sym:rust:config.rs#ServeConfig");
                assert_eq!(field, "addr");
            }
            other => panic!("expected a struct-field bridge, got {other:?}"),
        }
        // Separator-insensitive on the leaf: `serve.tls_cert` → field `tls_cert`.
        match ws
            .follow_definition("hub::cfgkey:config.toml#serve.tls_cert")
            .unwrap()
        {
            Follow::StructField { node, field } => {
                assert_eq!(node.key, "sym:rust:config.rs#ServeConfig");
                assert_eq!(field, "tls_cert");
            }
            other => panic!("expected a struct-field bridge, got {other:?}"),
        }
    }

    #[test]
    fn follow_falls_back_to_config_key_when_not_confidently_bridgeable() {
        let ws = bridge_hub();
        // Section matches a struct, but the struct has no such field → fall back.
        let ghost = ws
            .follow_definition("hub::cfgkey:config.toml#serve.ghost")
            .unwrap();
        assert!(
            matches!(&ghost, Follow::Node { node } if node.name == "serve.ghost"),
            "unmatched field falls back to the config_key node, got {ghost:?}"
        );
        // No struct maps to section `mystery` → fall back.
        let mystery = ws
            .follow_definition("hub::cfgkey:config.toml#mystery.addr")
            .unwrap();
        assert!(matches!(&mystery, Follow::Node { node } if node.name == "mystery.addr"));
        // A single-segment key names no section → never bridged.
        let port = ws
            .follow_definition("hub::cfgkey:config.toml#port")
            .unwrap();
        assert!(matches!(&port, Follow::Node { node } if node.name == "port"));
    }

    #[test]
    fn follow_does_not_bridge_on_ambiguity() {
        use crate::model::FactSet;
        // TWO structs both map to section `serve` and both declare `addr` — a
        // genuinely ambiguous mapping must fall back, never guess a wrong node.
        let mut s = store();
        s.apply_factset(
            &FactSet::new()
                .with_node(struct_node("ServeConfig", &["addr"]))
                .with_node(struct_node("Serve", &["addr"])) // also matches `serve`
                .with_node(cfg_node("serve.addr")),
        )
        .unwrap();
        let ws = Workspace::single("hub", s);
        let out = ws
            .follow_definition("hub::cfgkey:config.toml#serve.addr")
            .unwrap();
        assert!(
            matches!(&out, Follow::Node { node } if node.name == "serve.addr"),
            "ambiguous (two matching structs) falls back, got {out:?}"
        );
    }

    #[test]
    fn follow_narrow_lookup_ignores_unrelated_structs_with_the_same_field() {
        use crate::model::FactSet;
        // The name-narrowed struct lookup must return exactly what a full scan
        // would: an unrelated struct that happens to declare `addr` is NOT the
        // `serve` section's struct, so `serve.addr` still bridges only to
        // `ServeConfig` — proving the narrowing preserves bridging semantics.
        let mut s = store();
        s.apply_factset(
            &FactSet::new()
                .with_node(struct_node("ServeConfig", &["addr"]))
                .with_node(struct_node("Unrelated", &["addr"]))
                .with_node(struct_node("Widget", &["addr", "size"]))
                .with_node(struct_node("ModelsConfig", &["embedding"]))
                .with_node(cfg_node("serve.addr")),
        )
        .unwrap();
        let ws = Workspace::single("hub", s);
        match ws
            .follow_definition("hub::cfgkey:config.toml#serve.addr")
            .unwrap()
        {
            Follow::StructField { node, field } => {
                assert_eq!(node.key, "sym:rust:config.rs#ServeConfig");
                assert_eq!(field, "addr");
            }
            other => panic!("expected a struct-field bridge to ServeConfig, got {other:?}"),
        }
    }

    #[test]
    fn follow_reports_drift_and_passes_through_non_config_targets() {
        use crate::model::{FactSet, Node, NodeKind};
        let mut s = store();
        s.apply_factset(&FactSet::new().with_node(Node::new(
            "sym:rust:a.rs#Thing",
            NodeKind::Struct,
            "Thing",
        )))
        .unwrap();
        let ws = Workspace::single("hub", s);
        // A well-formed target whose node is gone → drift.
        assert_eq!(
            ws.follow_definition("hub::cfgkey:config.toml#gone")
                .unwrap(),
            Follow::Drift
        );
        // A spoke pointing straight at a symbol (an authored link, not a config
        // key) passes the node through unbridged.
        match ws.follow_definition("hub::sym:rust:a.rs#Thing").unwrap() {
            Follow::Node { node } => assert_eq!(node.key, "sym:rust:a.rs#Thing"),
            other => panic!("expected pass-through, got {other:?}"),
        }
    }

    #[test]
    fn workspace_set_select_single_ambiguous_and_unknown() {
        // One workspace ⇒ the default; a bare or named select both resolve to it.
        let one = WorkspaceSet::from_workspaces([(
            "only".to_owned(),
            Workspace::single("only", store()),
            true,
        )]);
        assert_eq!(one.names(), vec!["only".to_owned()]);
        assert_eq!(one.linked("only"), Some(true));
        assert!(one.linked("nope").is_none());
        assert!(one.select(None).is_ok());
        assert!(one.select(Some("only")).is_ok());
        assert!(matches!(
            one.select(Some("ghost")),
            Err(WorkspaceError::UnknownWorkspace { .. })
        ));

        // Several workspaces ⇒ a bare select is ambiguous (listing the names), a
        // named select works, and an unknown name errors.
        let many = WorkspaceSet::from_workspaces([
            ("api".to_owned(), Workspace::single("api", store()), true),
            ("web".to_owned(), Workspace::single("web", store()), false),
        ]);
        assert_eq!(many.names(), vec!["api".to_owned(), "web".to_owned()]);
        assert_eq!(many.linked("web"), Some(false));
        // (`select` yields `&Workspace`, which isn't `Debug`, so match the error
        // out rather than `unwrap_err`.)
        let Err(err) = many.select(None) else {
            panic!("a bare select over several workspaces must be ambiguous");
        };
        assert!(matches!(err, WorkspaceError::AmbiguousWorkspace { .. }));
        assert!(err.to_string().contains("api"));
        assert!(err.to_string().contains("web"));
        assert!(many.select(Some("web")).is_ok());
        assert!(matches!(
            many.select(Some("ghost")),
            Err(WorkspaceError::UnknownWorkspace { .. })
        ));

        // No workspaces ⇒ a bare select reports the empty set.
        let none = WorkspaceSet::from_workspaces(std::iter::empty());
        assert!(matches!(none.select(None), Err(WorkspaceError::Empty)));
    }

    #[test]
    fn workspace_set_containing_finds_the_owning_workspace_by_db_path() {
        // Build two workspaces from explicit (name, graph.db) pairs — no git needed
        // — so `containing` can match a repo's db against each workspace's members.
        let api_db = PathBuf::from("/ws/api/svc/.git/roteiro/graph.db");
        let web_db = PathBuf::from("/ws/web/app/.git/roteiro/graph.db");
        let set = WorkspaceSet::from_workspaces([
            (
                "api".to_owned(),
                Workspace::from_named_dbs([("svc".to_owned(), api_db.clone())]),
                true,
            ),
            (
                "web".to_owned(),
                Workspace::from_named_dbs([("app".to_owned(), web_db.clone())]),
                false,
            ),
        ]);
        assert_eq!(set.containing(&api_db).as_deref(), Some("api"));
        assert_eq!(set.containing(&web_db).as_deref(), Some("web"));
        // A db in no workspace matches nothing.
        assert_eq!(
            set.containing(Path::new("/elsewhere/.git/roteiro/graph.db")),
            None
        );
    }

    /// The shallow rule is deliberate; being **invisible** is the defect
    /// (issue #580). A scan therefore reports what it walked past, so a caller
    /// can say so at the moment the project count surprises somebody.
    ///
    /// The layout is the one the issue reports: one repo at depth 1 beside
    /// organisation directories whose repos are one level further down.
    #[test]
    fn a_shallow_scan_reports_the_directories_it_walked_past() {
        let base = std::env::temp_dir().join(format!("rto-scan-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        for dir in ["direct/.git", "orgA/repo1/.git", "orgB/repo2/.git", "empty"] {
            std::fs::create_dir_all(base.join(dir)).expect("mkdir");
        }
        let scan = scan_root(&base).expect("scan");

        // Membership is unchanged — this is not a change to the rule.
        assert_eq!(scan.repos, vec![base.join("direct")]);
        assert_eq!(discover_repos_under(&base).expect("discover"), scan.repos);

        // And the three directories it did not descend into are recorded.
        assert_eq!(
            scan.skipped,
            vec![base.join("empty"), base.join("orgA"), base.join("orgB")],
        );

        // The deeper probe names only the ones that would have yielded a repo,
        // so a message built from it is actionable rather than a directory dump.
        assert_eq!(
            scan.nested_repo_parents(64),
            vec![base.join("orgA").as_path(), base.join("orgB").as_path()],
        );

        // Bounded: the probe costs a `read_dir` per candidate, so a caller can
        // cap it. `skipped` is sorted, so `limit` takes a defined prefix.
        assert_eq!(
            scan.nested_repo_parents(2),
            vec![base.join("orgA").as_path()],
            "`limit` bounds the directories examined, not the ones reported",
        );
        assert!(scan.nested_repo_parents(0).is_empty());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_bundle_is_a_closed_frontmatter_declaring_okf_version() {
        let base = std::env::temp_dir().join(format!("rto-okfprobe-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();

        let write = |repo: &str, index: &str| {
            let dir = base.join(repo).join(super::OKF_BUNDLE_DIR);
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join("index.md"), index).expect("write");
            base.join(repo)
        };

        let good = write("good", "---\nokf_version: \"0.2\"\n---\n\n# Peer\n");
        assert_eq!(
            super::okf_bundle_in(&good),
            Some(good.join(super::OKF_BUNDLE_DIR))
        );

        // Windows line endings throughout. Copilot suggested on #711 that the
        // closing fence would be missed, since it is written `\r\n---` while the
        // search is for `\n---`. It is **not** missed — `\r\n---` contains
        // `\n---` — and the `\r` left on the key's line is removed by the
        // `trim()` the check already does. Kept as a fixture rather than
        // dropped: the claim was plausible, and the next reader deserves the
        // answer without having to re-derive it.
        let crlf = write(
            "crlf",
            "---\r\nokf_version: \"0.2\"\r\n---\r\n\r\n# Peer\r\n",
        );
        assert_eq!(
            super::okf_bundle_in(&crlf),
            Some(crlf.join(super::OKF_BUNDLE_DIR))
        );

        // A directory called `okf` proves nothing.
        let plain = write("plain", "# Just some notes\n");
        assert_eq!(super::okf_bundle_in(&plain), None);

        // No closing fence: `okf_version` here is prose under an unterminated
        // block, not a declaration. Reported by Copilot on #711 — the earlier
        // `split(…).next()` accepted it.
        //
        // The line must be a *bare* `okf_version:` at the start of a line, not
        // prose mentioning it: the reader matches on the key before the first
        // colon, so "we should set okf_version: 0.2" never matched anyway and a
        // fixture using it proved nothing. This is an `index.md` whose
        // frontmatter is unterminated and whose body shows an example block —
        // an ordinary thing for a directory documenting the format.
        let unterminated = write(
            "unterminated",
            "---\ntitle: notes\n\nAn example bundle root looks like:\n\nokf_version: \"0.2\"\n",
        );
        assert_eq!(super::okf_bundle_in(&unterminated), None);

        // Frontmatter that closes but declares nothing.
        let no_version = write("no-version", "---\ntitle: notes\n---\n\n# Notes\n");
        assert_eq!(super::okf_bundle_in(&no_version), None);

        // An empty value is not a declaration either.
        let empty = write("empty", "---\nokf_version:\n---\n\n# Notes\n");
        assert_eq!(super::okf_bundle_in(&empty), None);

        // No bundle directory at all.
        std::fs::create_dir_all(base.join("none")).expect("mkdir");
        assert_eq!(super::okf_bundle_in(&base.join("none")), None);

        std::fs::remove_dir_all(&base).ok();
    }
}
