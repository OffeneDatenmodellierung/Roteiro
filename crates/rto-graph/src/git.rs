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
        Ok(out)
    }
}

/// A tracked file that differs between the working tree and `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// `true` when the file was removed from the working tree.
    pub deleted: bool,
}
