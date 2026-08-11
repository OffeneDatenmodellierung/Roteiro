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

    /// Every blob reachable from the `HEAD` tree, with full paths.
    ///
    /// # Errors
    /// Returns [`GitError`] if the tree cannot be traversed or a path is not
    /// valid UTF-8.
    pub fn walk_blobs(&self) -> Result<Vec<BlobRef>, GitError> {
        let tree = self.inner.head_tree().map_err(ge)?;
        walk_tree_blobs(&tree)
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

        // Reuse the subtree-pruning tree diff, then flatten to the (path, deleted)
        // shape this API exposes. `diff_trees` already sorts and prunes unchanged
        // subtrees, so this is O(change) rather than a full walk of both trees.
        let diff = self.diff_trees(&base_oid, &head_oid)?;
        let mut out: Vec<ChangedFile> = diff
            .changed
            .into_iter()
            .map(|b| ChangedFile {
                path: b.path,
                deleted: false,
            })
            .chain(diff.deleted.into_iter().map(|path| ChangedFile {
                path,
                deleted: true,
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
                            deleted: false,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push(ChangedFile {
                    path: blob.path,
                    deleted: true,
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

    /// Untracked, non-ignored regular files in the working tree — brand-new files
    /// that are in neither `HEAD` nor the index, so [`Repo::walk_blobs`] and
    /// [`Repo::changed_files`] (both HEAD-tree based) miss them. The working-tree
    /// `sync`/`check`/`review` overlay these so a new-but-unstaged file is seen.
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

/// A tracked file that differs between the working tree and `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// `true` when the file was removed from the working tree.
    pub deleted: bool,
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
}
