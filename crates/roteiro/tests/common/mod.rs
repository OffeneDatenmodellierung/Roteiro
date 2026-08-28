//! Shared helpers for the `roteiro` integration tests.

// Each test binary that declares `mod common;` compiles this whole module, so an
// item only some of them use reads as dead code in the rest. That is a property
// of how Rust shares test helpers, not a warning about this code — and the
// alternative is what this module exists to prevent: every binary keeping its own
// copy.
#![allow(dead_code)]

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The workspace root, from this crate's manifest directory.
#[must_use]
pub fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Whether the tests are running inside a repository checkout rather than
/// against a packaged crate (which carries no `.github`, no `docs/`).
///
/// # Why this cannot be `.ok()`
///
/// This marker has to be as loud as the thing it guards, or it is the same
/// defect one level up with more steps: a marker that read `false` on an IO
/// error would turn "cannot read the repository" into "this is not a
/// repository", and skip. So only `NotFound` means absent, and every other
/// error panics.
#[must_use]
pub fn is_repository_checkout() -> bool {
    let manifest = repo_root().join("Cargo.toml");
    match std::fs::read_to_string(&manifest) {
        Ok(text) => text.lines().any(|line| line.trim() == "[workspace]"),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). Without it a guard cannot tell a packaged \
             crate from a repository checkout, and guessing would make it skip in \
             silence — which is the failure the CI guards exist to rule out.",
            manifest.display(),
            e.kind(),
        ),
    }
}

/// A repository file's contents, or `None` when this is not a repository
/// checkout at all.
///
/// The skip is legitimate: a packaged crate has no `.github/`, and these guards
/// are about *this repository*. Collapsing every IO error into that one meaning
/// is not. These guards exist to catch defects that are **invisible by
/// construction** — #482's gap was unreachable from any branch not named
/// `release-plz-*`, which is why nothing else could find it — so a green that
/// actually means "could not read the subject" is worse than no guard at all,
/// because by then the green is load-bearing. #401's fragment guard set the
/// standard by failing on an empty scan rather than skipping.
///
/// So the skip is made *verifiable* rather than merely narrower. Absent **and**
/// not a checkout is the skip. Absent **in** a checkout is a failure — the file
/// is committed, so it is supposed to be there. Anything else — permissions, a
/// bad symlink, an IO error mid-read — panics naming the path and the kind.
///
/// Shared because three CI guards needed this and grew three copies. Two had
/// already drifted apart in their panic message, and the third was
/// `read_to_string(..).ok()` — the version that silently skips on any error,
/// which is the defect this doc comment spends its length arguing against.
#[must_use]
pub fn repo_file(rel: &str) -> Option<String> {
    let path = repo_root().join(rel);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound && !is_repository_checkout() => None,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). This guard asserts a property of that \
             file, so skipping here would be a green that means \"could not \
             look\". If the file moved or was deliberately deleted, the guard \
             moves or goes with it.",
            path.display(),
            e.kind(),
        ),
    }
}

/// A fresh, empty scratch directory tagged with `label`, this process's id, and
/// a process-wide monotonic counter.
///
/// Both halves of the key are load-bearing. The **pid** keeps parallel test
/// *binaries* from colliding. The **counter** keeps concurrent tests *within* a
/// binary unique even when they pass the same label — without it, a path keyed
/// on the pid alone is shared by every test in the file, and Rust runs those in
/// parallel by default, so two of them race to delete and recreate the directory
/// the other is using. That failure needs a second caller to appear before it
/// bites, which means it arrives as a flake in somebody else's change.
///
/// Any existing directory at the path is removed, so a caller starts clean.
#[must_use]
pub fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("roteiro-{label}-{}-{seq}", std::process::id()));
    std::fs::remove_dir_all(&path).ok();
    path
}

/// A throwaway config home for a spawned `roteiro` child, so the process can
/// never discover the developer's real `~/.roteiro/config.toml`.
///
/// Config discovery resolves the user config under `$ROTEIRO_HOME` (else
/// `~/.roteiro`, derived from `$HOME`/`$USERPROFILE`; see
/// `config::roteiro_home`/`home_dir` in `crates/roteiro/src/config.rs`). Left to
/// the inherited environment, a developer machine carrying `[[workspaces]]`
/// config would put the server in workspace mode over live repos instead of
/// single-repo mode over the test's own fixture — so the test would silently
/// exercise the wrong graph and fail (or pass for the wrong reason). CI has no
/// `~/.roteiro`, which is why the leak went unnoticed there.
///
/// The directory is created on construction and removed on drop.
pub struct IsolatedHome {
    path: PathBuf,
}

impl IsolatedHome {
    /// Create a fresh, empty config home tagged with `label`, the current PID,
    /// and a process-wide monotonic counter. The PID keeps parallel test
    /// *binaries* from colliding; the counter keeps concurrent tests *within* a
    /// binary unique even when they pass the same `label` — otherwise a shared
    /// deterministic path would let two instances race-delete each other's dir.
    pub fn new(label: &str) -> Self {
        let path = scratch_dir(&format!("{label}-home"));
        std::fs::create_dir_all(&path).expect("mkdir isolated config home");
        Self { path }
    }

    /// Point `cmd` at this isolated home for every config-discovery route:
    /// `ROTEIRO_HOME` (the direct override) plus `HOME`/`USERPROFILE` (the
    /// `~/.roteiro` fallback), so the child is hermetic regardless of the real
    /// `~/.roteiro`.
    pub fn apply<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        cmd.env("ROTEIRO_HOME", &self.path)
            .env("HOME", &self.path)
            .env("USERPROFILE", &self.path)
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}
