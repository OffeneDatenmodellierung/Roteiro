//! Where the pinned-asset cache lives, and the precedence that decides it.
//!
//! The paths here are spelled out in full (`std::path::PathBuf`) because this
//! file used to be `include!`d by a build script that fetched the sandbox
//! runtime; that build script went with the sandbox (ADR-0030), and the
//! spelling is harmless.

/// Resolve the root of the asset cache from its inputs, without touching the
/// environment — so it is testable.
fn root_from(
    security_root: Option<std::path::PathBuf>,
    roteiro_home: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    if let Some(dir) = security_root {
        return dir;
    }
    if let Some(dir) = roteiro_home {
        return dir.join("security");
    }
    home.unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".roteiro")
        .join("security")
}

/// Root of the asset cache (`~/.roteiro/security`), honouring
/// `ROTEIRO_SECURITY_ASSETS` and then `ROTEIRO_HOME`.
///
/// It sits beside the model store rather than inside the repository: assets are
/// per-user, are shared across every checkout, and must never be committed.
#[must_use]
pub fn asset_root() -> std::path::PathBuf {
    root_from(
        std::env::var_os("ROTEIRO_SECURITY_ASSETS").map(std::path::PathBuf::from),
        std::env::var_os("ROTEIRO_HOME").map(std::path::PathBuf::from),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from),
    )
}

#[cfg(test)]
mod asset_paths_tests {
    use super::root_from;
    use std::path::PathBuf;

    #[test]
    fn the_cache_root_prefers_the_explicit_override_then_roteiro_home() {
        assert_eq!(
            root_from(
                Some("/explicit".into()),
                Some("/home/.roteiro".into()),
                None
            ),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            root_from(None, Some("/home/.roteiro".into()), None),
            PathBuf::from("/home/.roteiro/security")
        );
        assert_eq!(
            root_from(None, None, Some("/home/me".into())),
            PathBuf::from("/home/me/.roteiro/security")
        );
    }
}
