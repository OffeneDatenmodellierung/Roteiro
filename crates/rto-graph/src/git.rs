//! A thin `gix` wrapper exposing exactly the git facts the sync engine needs:
//! the HEAD tree id, the blobs in that tree, and blob contents. Kept small so
//! all `gix` coupling lives in one place.

use std::path::Path;

/// A blob in a tree: its repository-relative path and hex object id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Repository-relative path (forward-slash separated).
    pub path: String,
    /// Hex-encoded git blob object id.
    pub oid: String,
}

/// Which tree the graph — derived layer *and* authored layer — is built from:
/// the committed `HEAD`, the working tree (uncommitted edits on disk), or the
/// git index (the staged tree a commit would record).
///
/// It selects the sync engine ([`crate::sync`] / [`crate::sync_worktree`] /
/// [`crate::sync_index`]) and the authored-layer source
/// ([`Repo::read_source`]) **together**, which is the point of it being one
/// type: the two layers disagreeing about which tree they describe is issue
/// #330, and it was a silent wrong answer rather than a loud one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSource {
    /// The committed `HEAD` tree (the CI merge gate).
    Committed,
    /// The working tree: `HEAD` plus uncommitted edits to tracked files on disk.
    Worktree,
    /// The git index — exactly what a commit would record (the pre-commit gate).
    Index,
}

impl GraphSource {
    /// A short stable token for this source, for reports and tool documents.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Worktree => "worktree",
            Self::Index => "index",
        }
    }
}

/// Errors raised while reading from a git repository.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A `gix` operation failed (message preserved).
    #[error("git error: {0}")]
    Git(String),
    /// A tree entry path was not valid UTF-8.
    #[error("non-utf8 path in tree: {0:?}")]
    NonUtf8Path(Vec<u8>),
}

fn ge<E: std::fmt::Display>(e: E) -> GitError {
    GitError::Git(e.to_string())
}

/// Who last changed one path, and when — see [`Repo::last_authors`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAuthor {
    /// The commit author's name, exactly as git recorded it. The bare identity:
    /// any prefix a format requires (OKF's `human:`, §7) is the renderer's.
    pub name: String,
    /// That commit's commit time, in seconds since the Unix epoch, UTC.
    ///
    /// Carried alongside the name because the two are one claim. Stamping a
    /// person's confirmation with the *render's* clock would date a review to a
    /// moment the reviewer had nothing to do with, and would make the bundle
    /// differ on every render for a document nobody touched.
    pub at: i64,
}

/// The blob id at `path` within `tree`, memoised on `(tree, path)`.
///
/// `None` means the path does not exist in that tree — which is a state the
/// caller compares like any other, because a path appearing (or disappearing) is
/// as much a change as its content differing.
fn blob_at<'p>(
    repo: &gix::Repository,
    memo: &mut std::collections::HashMap<(gix::ObjectId, &'p str), Option<gix::ObjectId>>,
    tree: gix::ObjectId,
    path: &'p str,
) -> Result<Option<gix::ObjectId>, GitError> {
    if let Some(hit) = memo.get(&(tree, path)) {
        return Ok(*hit);
    }
    let found = repo
        .find_object(tree)
        .map_err(ge)?
        .try_into_tree()
        .map_err(ge)?
        .lookup_entry(path.split('/').map(str::as_bytes))
        .map_err(ge)?
        .map(|e| e.id().detach());
    memo.insert((tree, path), found);
    Ok(found)
}

/// A discovered git repository.
pub struct Repo {
    inner: gix::Repository,
}

impl Repo {
    /// Discover the repository containing `path` (walking upwards to the `.git`).
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if no repository is found or it cannot be opened.
    pub fn discover(path: &Path) -> Result<Self, GitError> {
        Ok(Self {
            inner: gix::discover(path).map_err(ge)?,
        })
    }

    /// The repository's *common* git directory. The cache lives under here so it
    /// is shared across linked worktrees (which each have their own git dir).
    #[must_use]
    pub fn common_dir(&self) -> &Path {
        self.inner.common_dir()
    }

    /// This worktree's git directory (per-worktree; the graph DB lives here).
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        self.inner.git_dir()
    }

    /// The directory git actually looks in for hooks. Honours `core.hooksPath`
    /// (absolute, or relative to the working-tree root — else the git dir); when
    /// unset it is `<common git dir>/hooks`, so managed hooks are shared across
    /// linked worktrees. `roteiro init` installs into this so its hooks run
    /// wherever git expects them.
    #[must_use]
    pub fn hooks_dir(&self) -> std::path::PathBuf {
        let configured = self.inner.config_snapshot().string("core.hooksPath");
        // An empty `core.hooksPath` (e.g. `git -c core.hooksPath=`) means "unset".
        let configured = configured.filter(|c| !AsRef::<[u8]>::as_ref(c).is_empty());
        if let Some(configured) = configured {
            let bytes: &[u8] = configured.as_ref();
            let path = std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned());
            if path.is_absolute() {
                return path;
            }
            let base = self.inner.workdir().unwrap_or_else(|| self.inner.git_dir());
            return base.join(path);
        }
        self.common_dir().join("hooks")
    }

    /// The working directory, if this is not a bare repository. The dirty
    /// overlay reads uncommitted file contents from here.
    #[must_use]
    pub fn workdir(&self) -> Option<&Path> {
        self.inner.workdir()
    }

    /// The hex git blob object id that `bytes` would have, without writing
    /// anything. Used to detect whether a working-copy file differs from the
    /// committed blob (same content ⇒ same id).
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if hashing fails.
    pub fn blob_oid(&self, bytes: &[u8]) -> Result<String, GitError> {
        let id = gix::objs::compute_hash(self.inner.object_hash(), gix::objs::Kind::Blob, bytes)
            .map_err(ge)?;
        Ok(id.to_hex().to_string())
    }

    /// Hex object id of the tree at `HEAD`.
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if `HEAD` cannot be resolved to a tree.
    pub fn head_tree_id(&self) -> Result<String, GitError> {
        let tree = self.inner.head_tree().map_err(ge)?;
        Ok(tree.id().to_hex().to_string())
    }

    /// Hex object id of the commit at `HEAD` — a stable permalink ref for the tree
    /// the graph was built from (used to build source links).
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if `HEAD` cannot be resolved to a commit.
    pub fn head_commit_id(&self) -> Result<String, GitError> {
        Ok(self
            .inner
            .head_id()
            .map_err(ge)?
            .detach()
            .to_hex()
            .to_string())
    }

    /// Seconds since the Unix epoch of the `HEAD` commit's commit time, in UTC.
    ///
    /// Added for analyzer-asset provisioning: an advisory database that is a git
    /// checkout has no publication date of its own, and `cargo audit` reports
    /// none at all when it is pointed at a database with `--db` rather than
    /// resolving one itself. The commit time is the publication date, and it is
    /// what lets a result be labelled *possibly stale* with a number attached
    /// (ADR-0012).
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if `HEAD` cannot be resolved to a commit or the
    /// commit carries no readable time.
    pub fn head_commit_time(&self) -> Result<i64, GitError> {
        let commit = self.inner.head_commit().map_err(ge)?;
        Ok(commit.time().map_err(ge)?.seconds)
    }

    /// Who last changed each of `paths`, and when — the equivalent of
    /// `git log -1 --format='%an %ct' -- <path>` for a whole set at once.
    ///
    /// Used to attribute the **authored** layer to a person, which is what puts a
    /// concept in OKF's human-reviewed trust tier (§5.3) rather than the
    /// machine-confirmed one. The `human:` prefix is applied by the renderer, not
    /// here — this returns the bare identity.
    ///
    /// # Per path, because the claim is per document
    ///
    /// The obvious cheap answer is the `HEAD` commit's author, and it is wrong in
    /// a way the format cannot survive: `verified: [{ by: human:<id> }]` asserts
    /// that *that person* stands behind *that document*, so attributing the whole
    /// repository to whoever pushed last records a confirmation nobody made. A
    /// bot merge at `HEAD` would mark every ADR as human-reviewed by the bot.
    ///
    /// # What "last changed" means here
    ///
    /// Walking newest-first by commit time, a commit changed a path when the blob
    /// at that path differs from the blob in **every** parent — the same
    /// definition `git log -- <path>` uses, so a merge that only carried a change
    /// across is not credited with making it. A path present in a root commit was
    /// changed by that commit.
    ///
    /// A path absent from the result was never resolved: the history ran out
    /// first (a shallow clone), a commit's parents could not be read (a partial
    /// clone), or the walk failed. The caller must read that as *no
    /// confirmation*, never as the tool's — substituting a machine actor would
    /// move a concept down a trust tier silently, which is worse than claiming
    /// nothing.
    ///
    /// Every commit this cannot fully compare is skipped rather than guessed at,
    /// for the same reason: the only wrong answer that costs anything here is a
    /// confident one.
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if `HEAD` cannot be resolved or the history
    /// cannot be walked.
    pub fn last_authors(
        &self,
        paths: &std::collections::BTreeSet<String>,
    ) -> Result<std::collections::BTreeMap<String, PathAuthor>, GitError> {
        use std::collections::{BTreeMap, BTreeSet, HashMap};

        let mut out: BTreeMap<String, PathAuthor> = BTreeMap::new();
        if paths.is_empty() {
            return Ok(out);
        }
        let head = self.inner.head_id().map_err(ge)?.detach();

        // A local handle carrying an object cache. The walk reads each commit's
        // tree and its parents' trees, and every parent is itself visited later
        // in the walk, so without a cache the same objects are decoded twice.
        let mut repo = self.inner.clone();
        repo.object_cache_size_if_unset(8 * 1024 * 1024);

        // The shallow boundary, where the parents a comparison needs are simply
        // absent. gix reports such a commit as parentless, which would otherwise
        // read as "a root commit introduced everything it contains" — and in a
        // `fetch-depth: 1` checkout that is *every* path, attributed to whoever
        // made the one commit present. That is precisely the false claim this
        // method exists to stop, so a boundary commit resolves nothing.
        let boundary: BTreeSet<gix::ObjectId> = repo
            .shallow_commits()
            .map_err(ge)?
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();

        let mut pending: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
        // (tree id, path) -> blob id there. Keyed on the *tree* rather than the
        // commit so a parent looked up as a parent and later walked as a commit
        // is one lookup, not two.
        let mut memo: HashMap<(gix::ObjectId, &str), Option<gix::ObjectId>> = HashMap::new();

        let walk = repo
            .rev_walk([head])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(ge)?;

        for info in walk {
            if pending.is_empty() {
                break;
            }
            let info = info.map_err(ge)?;
            if boundary.contains(&info.id) {
                continue;
            }
            let commit = info.object().map_err(ge)?;
            let tree_id = commit.tree_id().map_err(ge)?.detach();
            // **Every** parent, or none of this commit's answers. A parent that
            // cannot be read is not a parent that is absent: dropping it silently
            // shrinks the set this commit is compared against, and dropping the
            // last one turns the commit into an apparent root that introduced
            // everything it contains. Both directions produce an attribution, and
            // an attribution derived from history we could not read is the false
            // claim this whole method exists to avoid. A partial clone reaches
            // here without a `shallow` file to warn us.
            let parent_trees: Option<Vec<gix::ObjectId>> = info
                .parent_ids
                .iter()
                .map(|id| {
                    repo.find_object(*id)
                        .ok()
                        .and_then(|o| o.try_into_commit().ok())
                        .and_then(|c| c.tree_id().ok())
                        .map(gix::Id::detach)
                })
                .collect();
            // `collect` into an `Option<Vec<_>>` is the all-or-nothing above: one
            // unreadable parent makes the whole set `None`.
            let Some(parent_trees) = parent_trees else {
                continue;
            };

            let mut resolved: Vec<&str> = Vec::new();
            for path in &pending {
                let here = blob_at(&repo, &mut memo, tree_id, path)?;
                // A root commit introduces whatever it contains; otherwise the
                // commit changed the path only if no parent already had this blob.
                let changed = if parent_trees.is_empty() {
                    here.is_some()
                } else {
                    let mut differs = true;
                    for parent in &parent_trees {
                        if blob_at(&repo, &mut memo, *parent, path)? == here {
                            differs = false;
                            break;
                        }
                    }
                    differs
                };
                if changed {
                    resolved.push(path);
                }
            }
            if resolved.is_empty() {
                continue;
            }
            // Read the author only once a path actually resolved: it decodes the
            // commit's header, and most commits in the walk touch none of `paths`.
            let author = commit
                .author()
                .ok()
                .map(|a| a.name.to_string())
                .filter(|n| !n.trim().is_empty());
            let at = commit.time().map_err(ge)?.seconds;
            for path in resolved {
                pending.remove(path);
                if let Some(name) = author.clone() {
                    out.insert(path.to_owned(), PathAuthor { name, at });
                }
            }
        }
        Ok(out)
    }

    /// The `origin` remote's fetch URL, if one is configured — e.g. to derive a
    /// web "blob" base for source links. `None` when there is no `origin` remote.
    #[must_use]
    pub fn origin_url(&self) -> Option<String> {
        let remote = self.inner.find_remote("origin").ok()?;
        let url = remote.url(gix::remote::Direction::Fetch)?;
        Some(url.to_bstring().to_string())
    }

    /// Every blob reachable from the `HEAD` tree, with full paths.
    ///
    /// # Errors
    /// Returns [`GitError`] if the tree cannot be traversed or a path is not
    /// valid UTF-8.
    pub fn walk_blobs(&self) -> Result<Vec<BlobRef>, GitError> {
        let tree = self.inner.head_tree().map_err(ge)?;
        walk_tree_blobs(&tree)
    }

    /// Every blob reachable from an arbitrary commit-or-tree `rev` (a hex oid),
    /// with full paths — like [`Repo::walk_blobs`] but for any point in history,
    /// not just `HEAD`. A commit oid is peeled to its tree, so a submodule pin (a
    /// commit sha) works directly. The primitive for extracting a repo's graph at
    /// the version a spoke pins (ADR-0009 step 8 — version-pin resolution).
    ///
    /// # Errors
    /// Returns [`GitError`] if `rev` cannot be resolved to a tree, the tree cannot
    /// be traversed, or a path is not valid UTF-8.
    pub fn blobs_at(&self, rev: &str) -> Result<Vec<BlobRef>, GitError> {
        let tree = self.tree_by_rev(rev)?;
        walk_tree_blobs(&tree)
    }

    /// The hex tree id an arbitrary revspec resolves to (a commit peels to its
    /// tree) — an **O(1)** resolution that does not walk the tree, so it doubles as
    /// a cheap "does this ref exist?" check (ADR-0009 step 8b/8c).
    ///
    /// # Errors
    /// Returns [`GitError`] if `rev` cannot be resolved to a tree.
    pub fn tree_id_at(&self, rev: &str) -> Result<String, GitError> {
        Ok(self.tree_by_rev(rev)?.id().to_hex().to_string())
    }

    /// Every git submodule pinned in the `HEAD` tree, sorted by path: a gitlink
    /// (commit) entry gives the path and the commit it points at, enriched with
    /// its `.gitmodules` URL when declared. The pinned commit is the **version a
    /// deployment repo ships** (ADR-0009 derived facts). Empty when there are none.
    ///
    /// # Errors
    /// Returns [`GitError`] if the tree cannot be traversed, `.gitmodules` cannot
    /// be read, or a path is not valid UTF-8.
    pub fn submodules(&self) -> Result<Vec<Submodule>, GitError> {
        let tree = self.inner.head_tree().map_err(ge)?;
        self.submodules_in_tree(&tree)
    }

    /// Every git submodule pinned at an arbitrary commit/tree `rev`, sorted by path
    /// — like [`Repo::submodules`] but for a historical point, so a hub graph
    /// extracted at a pinned version (ADR-0009 step 8) carries its own submodules
    /// as they were then.
    ///
    /// # Errors
    /// As [`Repo::submodules`], plus if `rev` cannot be resolved to a tree.
    pub fn submodules_at(&self, rev: &str) -> Result<Vec<Submodule>, GitError> {
        let tree = self.tree_by_rev(rev)?;
        self.submodules_in_tree(&tree)
    }

    /// Collect the submodule gitlinks (and `.gitmodules` URLs) in `tree`.
    fn submodules_in_tree(&self, tree: &gix::Tree<'_>) -> Result<Vec<Submodule>, GitError> {
        let mut recorder = gix::traverse::tree::Recorder::default();
        tree.traverse().breadthfirst(&mut recorder).map_err(ge)?;

        let mut links: Vec<(String, String)> = Vec::new();
        let mut gitmodules: Option<gix::ObjectId> = None;
        for entry in &recorder.records {
            if entry.mode.is_commit() {
                let path = String::from_utf8(entry.filepath.clone().into())
                    .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))?;
                links.push((path, entry.oid.to_hex().to_string()));
            } else if entry.mode.is_blob() && entry.filepath.as_slice() == b".gitmodules" {
                gitmodules = Some(entry.oid);
            }
        }
        self.assemble_submodules(links, gitmodules)
    }

    /// Every git submodule pinned in the **staged index** (the tree a commit would
    /// record), sorted by path. Same shape as [`Repo::submodules`] but reads the
    /// gitlinks (and `.gitmodules`) from the index, so the index-aware sync — the
    /// pre-commit gate — reflects a *staged* submodule bump, not the `HEAD` pin.
    ///
    /// # Errors
    /// As [`Repo::submodules`], plus index-load failure.
    pub fn index_submodules(&self) -> Result<Vec<Submodule>, GitError> {
        use gix::index::entry::Mode;
        let index = self.inner.index_or_load_from_head().map_err(ge)?;
        let mut links: Vec<(String, String)> = Vec::new();
        let mut gitmodules: Option<gix::ObjectId> = None;
        for entry in index.entries() {
            if entry.stage_raw() != 0 {
                continue;
            }
            let path = String::from_utf8(entry.path(&index).to_vec())
                .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))?;
            if entry.mode == Mode::COMMIT {
                links.push((path, entry.id.to_hex().to_string()));
            } else if path == ".gitmodules"
                && matches!(entry.mode, Mode::FILE | Mode::FILE_EXECUTABLE)
            {
                gitmodules = Some(entry.id);
            }
        }
        self.assemble_submodules(links, gitmodules)
    }

    /// Assemble `(path, sha)` gitlinks into sorted [`Submodule`]s, resolving each
    /// path's URL from the `.gitmodules` blob at `gitmodules` (when present). Shared
    /// by the `HEAD`-tree and index submodule readers.
    fn assemble_submodules(
        &self,
        links: Vec<(String, String)>,
        gitmodules: Option<gix::ObjectId>,
    ) -> Result<Vec<Submodule>, GitError> {
        if links.is_empty() {
            return Ok(Vec::new());
        }
        let urls = match gitmodules {
            Some(oid) => {
                let bytes = self.read_blob(&oid.to_hex().to_string())?;
                parse_gitmodules(&String::from_utf8_lossy(&bytes))
            }
            None => std::collections::HashMap::new(),
        };
        let mut out: Vec<Submodule> = links
            .into_iter()
            .map(|(path, sha)| {
                let url = urls.get(&path).cloned();
                Submodule { path, sha, url }
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The tracked files that differ between `base` (any revspec — a branch,
    /// `HEAD~3`, a sha) and the current `HEAD`, sorted by path. Used for
    /// change-scoped tooling over a commit range (e.g. `roteiro review --base
    /// main`), distinct from [`Repo::changed_files`], which compares the working
    /// tree to `HEAD`. A path only in `HEAD` is added, only in `base` is deleted.
    ///
    /// # Errors
    /// Returns [`GitError`] if `base` cannot be resolved to a tree, a tree cannot
    /// be traversed, or a path is not valid UTF-8.
    pub fn changed_between(&self, base: &str) -> Result<Vec<ChangedFile>, GitError> {
        let base_tree = self
            .inner
            .rev_parse_single(base)
            .map_err(ge)?
            .object()
            .map_err(ge)?
            .peel_to_tree()
            .map_err(ge)?;
        let base_oid = base_tree.id().to_hex().to_string();
        let head_oid = self.head_tree_id()?;

        // Reuse the subtree-pruning tree diff, then flatten to the `ChangedFile`
        // (path, status) shape this API exposes. `diff_trees` already sorts and
        // prunes unchanged subtrees, so this is O(change), not a full walk.
        let diff = self.diff_trees(&base_oid, &head_oid)?;
        // A tree diff's `changed` set conflates genuinely-new files with edits to
        // existing ones, so range review labels them `Modified` rather than
        // distinguishing `Added` (which would need the base file set).
        let mut out: Vec<ChangedFile> = diff
            .changed
            .into_iter()
            .map(|b| ChangedFile {
                path: b.path,
                status: ChangeStatus::Modified,
            })
            .chain(diff.deleted.into_iter().map(|path| ChangedFile {
                path,
                status: ChangeStatus::Deleted,
            }))
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Read the bytes of the blob with hex object id `oid`.
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if the id is malformed or the object is absent.
    pub fn read_blob(&self, oid: &str) -> Result<Vec<u8>, GitError> {
        let id = gix::ObjectId::from_hex(oid.as_bytes()).map_err(ge)?;
        // `detach()` moves the owned data out without cloning; `Object` itself
        // implements `Drop`, so the bare field cannot be moved out directly.
        Ok(self.inner.find_object(id).map_err(ge)?.detach().data)
    }

    /// The bytes of a tracked file's **authored source**, from the tree named by
    /// `source`: the committed `HEAD` blob, the staged blob, or the file as it
    /// stands on disk (unstaged edits included, and *not* the git index).
    ///
    /// The `Worktree` reading matches [`crate::sync_worktree`], which the derived
    /// graph is built from, so the authored and derived layers stay consistent —
    /// see [`GraphSource`] for why that pairing is one type rather than two
    /// independent choices.
    ///
    /// Returns `Ok(None)` when a worktree file has been deleted, so the caller
    /// drops it.
    ///
    /// # Errors
    /// Returns [`GitError::Git`] if the blob cannot be read, or if reading the
    /// working-tree copy fails for any reason other than the file being absent.
    pub fn read_source(
        &self,
        blob: &BlobRef,
        source: GraphSource,
    ) -> Result<Option<Vec<u8>>, GitError> {
        match source {
            // Worktree: the file as it stands on disk (unstaged edits included),
            // or `None` if it was deleted there.
            GraphSource::Worktree => match self.workdir() {
                Some(workdir) => match std::fs::read(workdir.join(&blob.path)) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    // Folded into `GitError::Git` with the path rather than
                    // carried as its own variant: `GitError` is not
                    // `#[non_exhaustive]`, so a new variant would break every
                    // downstream exhaustive match for a message this already
                    // preserves.
                    Err(e) => Err(GitError::Git(format!("reading {}: {e}", blob.path))),
                },
                None => Ok(Some(self.read_blob(&blob.oid)?)),
            },
            // Committed reads the `HEAD` blob; Index reads the staged blob — for
            // both, `blob.oid` is already the right object (the blob list came
            // from that tree), so read it directly.
            GraphSource::Committed | GraphSource::Index => Ok(Some(self.read_blob(&blob.oid)?)),
        }
    }

    /// Tracked files whose working-tree content differs from `HEAD` — the change
    /// about to be committed. A file is *changed* when its working-copy bytes hash
    /// to a different blob id than the committed one (content, not mtime), and
    /// *deleted* when it is absent from the working tree. Untracked new files are
    /// not reported (they are not in the `HEAD` tree). Same detection as
    /// [`crate::sync_worktree`], surfaced for change-scoped tooling.
    ///
    /// # Errors
    /// Returns [`GitError`] on a git failure. In a bare repo (no working tree)
    /// the change set is empty.
    pub fn changed_files(&self) -> Result<Vec<ChangedFile>, GitError> {
        let mut out = Vec::new();
        let Some(workdir) = self.workdir() else {
            return Ok(out);
        };
        for blob in self.walk_blobs()? {
            match std::fs::read(workdir.join(&blob.path)) {
                Ok(bytes) => {
                    if self.blob_oid(&bytes)? != blob.oid {
                        out.push(ChangedFile {
                            path: blob.path,
                            status: ChangeStatus::Modified,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push(ChangedFile {
                    path: blob.path,
                    status: ChangeStatus::Deleted,
                }),
                Err(e) => return Err(GitError::Git(e.to_string())),
            }
        }
        // `walk_blobs` order is an implementation detail; sort so `roteiro review`
        // output is deterministic across platforms and gix versions.
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The **staged** files: each regular blob in the git index with its staged
    /// object id, sorted by path. This is the tree that a commit would record —
    /// unlike [`Repo::changed_files`] (the working tree) — so it lets tooling gate
    /// exactly what is about to be committed (the pre-commit index-aware `check`).
    /// Conflict (unmerged) entries, directories, submodules and symlinks are
    /// skipped.
    ///
    /// # Errors
    /// Returns [`GitError`] if the index cannot be loaded or a path is not valid
    /// UTF-8.
    pub fn index_files(&self) -> Result<Vec<BlobRef>, GitError> {
        use gix::index::entry::Mode;
        let index = self.inner.index_or_load_from_head().map_err(ge)?;
        let mut out = Vec::new();
        for entry in index.entries() {
            if entry.stage_raw() != 0 || !matches!(entry.mode, Mode::FILE | Mode::FILE_EXECUTABLE) {
                continue;
            }
            let path = String::from_utf8(entry.path(&index).to_vec())
                .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))?;
            out.push(BlobRef {
                path,
                oid: entry.id.to_hex().to_string(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Every path the working tree has that `head_paths` does not — the files
    /// a commit would **add**, whether or not they have been staged yet.
    ///
    /// # Why this exists rather than `untracked_files` alone
    ///
    /// The obvious spelling of "new files in the working tree" is
    /// [`Repo::untracked_files`], and it is wrong in a way that reads as
    /// correct. The two sets classify against **different trees**:
    /// `untracked_files` is defined against the **index**, and a caller's
    /// `head_paths` comes from **HEAD**. So `git add` on a new file removes it
    /// from the untracked set without adding it to HEAD, and the union of the
    /// two has a hole exactly the size of "staged, not yet committed".
    ///
    /// That hole has been found three times, in three surfaces, each time as a
    /// silent wrong answer rather than a failure:
    ///
    /// - issue #636 — `sync` deleted a node from the graph on `git add`;
    /// - issue #649 — `review` said "no working-tree changes to review" on a
    ///   tree with a staged addition in it;
    /// - issue #657 — `check` reported **0 violations** on drift it had caught
    ///   one `git add` earlier, silencing the gate at the moment it matters most.
    ///
    /// Each was fixed where it was found, which left three copies of one rule.
    /// This is the rule, once, so the fourth surface inherits it instead of
    /// re-deriving it.
    ///
    /// `head_paths` is a parameter rather than something walked here because
    /// every caller already holds HEAD's paths for its own reasons; walking the
    /// tree again to re-derive them would make the shared version cost more than
    /// the copies it replaces.
    ///
    /// `.gitignore` is honoured, and the union states *how*: an ignored file is
    /// absent from the dirwalk, so it enters only by being in the index — which
    /// takes a deliberate `git add -f`. That is the right outcome rather than a
    /// leak, because force-adding overrides the ignore and the file will be
    /// committed regardless.
    ///
    /// # Errors
    /// Returns [`GitError`] if the dirwalk or the index cannot be read.
    pub fn added_since_head(
        &self,
        head_paths: &std::collections::BTreeSet<&str>,
    ) -> Result<std::collections::BTreeSet<String>, GitError> {
        // **Both** sources are filtered against HEAD, not only the index one.
        // `untracked_files` classifies against the index, so a path can be in
        // HEAD *and* reported untracked simultaneously: `git rm --cached f`
        // drops `f` from the index and leaves it on disk, and git then calls it
        // untracked while HEAD still carries it. Taking that set wholesale
        // labels a tracked file an addition. Found by Copilot on #656 and fixed
        // there inline; collapsing the call sites onto this helper would have
        // undone it, which is what the rebase conflict was really about.
        let mut out: std::collections::BTreeSet<String> = self
            .untracked_files()?
            .into_iter()
            .filter(|p| !head_paths.contains(p.as_str()))
            .collect();
        for entry in self.index_files()? {
            if !head_paths.contains(entry.path.as_str()) {
                out.insert(entry.path);
            }
        }
        Ok(out)
    }

    /// Untracked, non-ignored regular files in the working tree: everything the
    /// dirwalk finds that the **index** does not carry.
    ///
    /// Not "files in neither `HEAD` nor the index", which this said until #662
    /// pointed at the contradiction with [`Repo::added_since_head`] directly
    /// above. The set is defined against the index *alone*, so a path can be in
    /// `HEAD` and in here at once: `git rm --cached f` drops `f` from the index
    /// and leaves it on disk, and git then reports it untracked while `HEAD`
    /// still carries it.
    ///
    /// **This is not "the new files in the working tree".** It is defined against
    /// the **index**, so `git add` removes a file from it. A caller that unions
    /// this with a HEAD-derived set has a hole exactly the size of "staged, not
    /// yet committed" — which is issues #636, #649 and #657, three surfaces that
    /// each made that union by hand and each gave a silently wrong answer. Use
    /// [`Repo::added_since_head`] instead; it is that union, correct, in one place.
    ///
    /// Respects `.gitignore` / `.git/info/exclude` / global excludes, skips nested
    /// repositories and non-regular files (symlinks, dirs, submodules), and returns
    /// repository-relative, unix-separated paths, sorted. Empty in a bare repo.
    ///
    /// # Errors
    /// Returns [`GitError`] on a git failure or a non-UTF-8 path.
    pub fn untracked_files(&self) -> Result<Vec<String>, GitError> {
        use gix::dir::entry::{Kind, Status};
        use gix::dir::walk::EmissionMode;

        if self.inner.workdir().is_none() {
            return Ok(Vec::new());
        }
        // Classify the working tree against the index; emit each untracked file
        // (not whole collapsed dirs), leaving ignored files unemitted (the default)
        // so `.gitignore` is honoured.
        let index = self.inner.index_or_empty().map_err(ge)?;
        let options = self
            .inner
            .dirwalk_options()
            .map_err(ge)?
            .emit_untracked(EmissionMode::Matching);
        // A never-set interrupt flag: the walk is a bounded, synchronous pass, so
        // there is nothing to cancel it from. (`gix` wants an owned/static flag;
        // its private wrapper type isn't nameable, so build one via `Arc`.)
        let never = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let iter = self
            .inner
            .dirwalk_iter(index, std::iter::empty::<&str>(), never.into(), options)
            .map_err(ge)?;

        let mut out = Vec::new();
        for item in iter {
            let entry = item.map_err(ge)?.entry;
            // Only brand-new regular files; symlinks/dirs/submodules are excluded
            // by the `File` disk kind, ignored files by the emission mode above.
            if entry.status == Status::Untracked && entry.disk_kind == Some(Kind::File) {
                let path = String::from_utf8(entry.rela_path.into())
                    .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))?;
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }
}

/// Collect every blob reachable from `tree`, with full repository-relative paths.
fn walk_tree_blobs(tree: &gix::Tree<'_>) -> Result<Vec<BlobRef>, GitError> {
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder).map_err(ge)?;
    let mut out = Vec::new();
    for entry in recorder.records {
        if !entry.mode.is_blob() {
            continue;
        }
        let path = String::from_utf8(entry.filepath.into())
            .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))?;
        out.push(BlobRef {
            path,
            oid: entry.oid.to_hex().to_string(),
        });
    }
    Ok(out)
}

/// A git submodule pinned in a tree: its repo-relative path, the commit it points
/// at (the gitlink oid — the **version pin** a deployment ships), and its
/// configured URL from `.gitmodules` when registered there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submodule {
    /// Repo-relative path the submodule is mounted at.
    pub path: String,
    /// Hex commit id the gitlink points at (the pinned version).
    pub sha: String,
    /// The submodule's URL from `.gitmodules`, if declared there.
    pub url: Option<String>,
}

/// Parse a `.gitmodules` file into a `path → url` map. INI-like: each
/// `[submodule "<name>"]` section carries a `path` and a `url`.
fn parse_gitmodules(text: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let (mut path, mut url) = (None, None);
    let mut in_submodule = false;
    let mut flush = |path: &mut Option<String>, url: &mut Option<String>| {
        if let (Some(p), Some(u)) = (path.take(), url.take()) {
            map.insert(p, u);
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut path, &mut url);
            in_submodule = line.starts_with("[submodule");
        } else if in_submodule {
            if let Some(v) = line
                .strip_prefix("path")
                .and_then(|r| r.trim_start().strip_prefix('='))
            {
                path = Some(v.trim().to_owned());
            } else if let Some(v) = line
                .strip_prefix("url")
                .and_then(|r| r.trim_start().strip_prefix('='))
            {
                url = Some(v.trim().to_owned());
            }
        }
    }
    flush(&mut path, &mut url);
    map
}

/// How a file changed relative to the comparison baseline — for review labelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    /// A new file, absent from the baseline (e.g. a brand-new untracked file).
    Added,
    /// Present on both sides, with different content.
    Modified,
    /// Removed from the working tree (or the `HEAD` side of a range).
    Deleted,
}

impl ChangeStatus {
    /// Stable lowercase label (`added` | `modified` | `deleted`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        }
    }
}

/// A file that differs between the working tree (or a base revision) and `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// How the file changed.
    pub status: ChangeStatus,
}

/// The blob-level difference between two trees: paths added or modified (with
/// their new blob oid) and paths deleted. See [`Repo::diff_trees`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeDiff {
    /// Blobs whose *tree entry* differs from the old tree — a changed blob oid,
    /// or a mode change (e.g. the executable bit) on otherwise-identical content —
    /// as `(path, new blob oid)`. These are the paths to re-extract; a mode-only
    /// change re-extracts to identical facts (extraction is content-addressed), a
    /// harmless cache hit.
    pub changed: Vec<BlobRef>,
    /// Blobs present in the old tree but absent from the new — paths whose facts
    /// must be dropped.
    pub deleted: Vec<String>,
}

impl Repo {
    /// The blob-level diff between two tree object ids (`old` → `new`), pruning
    /// unchanged subtrees: gix descends only into subtrees whose oid differs, so
    /// the cost is proportional to the *change*, not the tree size. Renames are
    /// reported as a delete plus an add (rewrite tracking is off), which is what
    /// the path-scoped extractor wants. Results are sorted by path for determinism.
    ///
    /// This is the incremental-sync counterpart to [`Repo::walk_blobs`]: given the
    /// last-synced tree and `HEAD`, it yields exactly the paths that changed.
    ///
    /// # Errors
    /// Returns [`GitError`] if either id is not a tree, the diff fails, or a path
    /// is not valid UTF-8.
    pub fn diff_trees(&self, old: &str, new: &str) -> Result<TreeDiff, GitError> {
        let old_tree = self.tree_by_hex(old)?;
        let new_tree = self.tree_by_hex(new)?;

        let mut changed = Vec::new();
        let mut deleted = Vec::new();
        let mut err: Option<GitError> = None;

        let mut platform = old_tree.changes().map_err(ge)?;
        platform.options(|o| {
            o.track_rewrites(None);
        });
        platform
            .for_each_to_obtain_tree(&new_tree, |change| {
                use gix::object::tree::diff::Change;
                let record = |path: &gix::bstr::BStr| -> Result<String, GitError> {
                    String::from_utf8(path.to_vec())
                        .map_err(|e| GitError::NonUtf8Path(e.into_bytes()))
                };
                match change {
                    Change::Addition {
                        location,
                        entry_mode,
                        id,
                        ..
                    }
                    | Change::Modification {
                        location,
                        entry_mode,
                        id,
                        ..
                    } => {
                        if entry_mode.is_blob() {
                            match record(location) {
                                Ok(path) => changed.push(BlobRef {
                                    path,
                                    oid: id.to_hex().to_string(),
                                }),
                                Err(e) => err = Some(e),
                            }
                        }
                    }
                    Change::Deletion {
                        location,
                        entry_mode,
                        ..
                    } => {
                        if entry_mode.is_blob() {
                            match record(location) {
                                Ok(path) => deleted.push(path),
                                Err(e) => err = Some(e),
                            }
                        }
                    }
                    // Rewrite tracking is disabled, so renames arrive as
                    // Deletion + Addition; this arm is unreachable in practice.
                    Change::Rewrite { .. } => {}
                }
                Ok::<_, std::convert::Infallible>(gix::object::tree::diff::Action::Continue(()))
            })
            .map_err(ge)?;

        if let Some(e) = err {
            return Err(e);
        }
        changed.sort_by(|a, b| a.path.cmp(&b.path));
        deleted.sort();
        Ok(TreeDiff { changed, deleted })
    }

    /// Resolve a hex object id to a [`gix::Tree`].
    fn tree_by_hex(&self, hex: &str) -> Result<gix::Tree<'_>, GitError> {
        let id = gix::ObjectId::from_hex(hex.as_bytes()).map_err(ge)?;
        self.inner
            .find_object(id)
            .map_err(ge)?
            .peel_to_tree()
            .map_err(ge)
    }

    /// Resolve **any git revspec** — a sha, a tag, a branch, `HEAD~1` — to its
    /// tree. Unlike [`Repo::tree_by_hex`] (raw oids only), this accepts the tag /
    /// branch names the pinned-version resolution (`--hub-rev`, an image tag) can
    /// carry. Mirrors the resolution in [`Repo::changed_between`].
    fn tree_by_rev(&self, rev: &str) -> Result<gix::Tree<'_>, GitError> {
        self.inner
            .rev_parse_single(rev)
            .map_err(ge)?
            .object()
            .map_err(ge)?
            .peel_to_tree()
            .map_err(ge)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_gitmodules;

    #[test]
    fn parse_gitmodules_maps_path_to_url_in_either_field_order() {
        let text = "\
[submodule \"vendor/app\"]\n\
\tpath = vendor/app\n\
\turl = https://github.com/acme/app.git\n\
[submodule \"libs/util\"]\n\
\turl = git@github.com:acme/util.git\n\
\tpath = libs/util\n";
        let map = parse_gitmodules(text);
        assert_eq!(
            map.get("vendor/app").map(String::as_str),
            Some("https://github.com/acme/app.git")
        );
        // URL declared before path in its section still maps.
        assert_eq!(
            map.get("libs/util").map(String::as_str),
            Some("git@github.com:acme/util.git")
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_gitmodules_ignores_non_submodule_sections() {
        let map = parse_gitmodules("[core]\n\tbare = false\n[submodule \"a\"]\npath=a\nurl=u\n");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("a").map(String::as_str), Some("u"));
    }
}
