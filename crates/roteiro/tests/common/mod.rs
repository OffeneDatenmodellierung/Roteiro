//! Shared helpers for the `roteiro` integration tests.

use std::path::PathBuf;
use std::process::Command;

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
    /// Create a fresh, empty config home tagged with `label` and the current PID
    /// (so parallel test binaries don't collide on the same path).
    pub fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("roteiro-{label}-home-{}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
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
