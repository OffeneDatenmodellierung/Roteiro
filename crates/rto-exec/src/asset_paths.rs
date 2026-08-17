// Where the pinned-asset cache lives, and the precedence that decides it.
//
// # Why this file is `include!`d rather than imported
//
// Two places have to agree on this path and cannot share a crate graph: this
// library, which provisions into the cache and reports on it, and `build.rs`,
// which looks for the provisioned sandbox runtime there before it says anything
// about `BOXLITE_RUNTIME_URL`. A build script cannot depend on the crate it
// builds, so the single source of truth is this file and `build.rs` pulls it in
// with `include!` — the same arrangement `src/runtime_pins.rs` already uses, and
// for the same reason.
//
// The alternative was to restate the precedence in `build.rs`. Three lines is a
// cheap copy to make and an expensive one to keep: the moment either side gains
// a variable, a build script would be looking somewhere `roteiro security
// prefetch` does not write, and the symptom would be "the archive is right
// there and it says it is missing" — which is the bug this file was added to
// fix, reintroduced one level down.
//
// That constrains what may appear here: **no `use`, no `crate::` paths, no
// references to anything outside this file.** Every type is spelled out in full
// so it compiles standalone, inside a build script that has imported nothing.

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

/// The environment variables [`asset_root`] reads, in precedence order.
///
/// Named as data so that a build script can tell cargo to re-run when any of
/// them changes, and so a failure message can say which knobs were consulted
/// without a second list going stale beside this one.
pub const ASSET_ROOT_VARS: &[&str] = &[
    "ROTEIRO_SECURITY_ASSETS",
    "ROTEIRO_HOME",
    "HOME",
    "USERPROFILE",
];

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

    /// [`super::ASSET_ROOT_VARS`] names exactly the variables the code reads.
    ///
    /// The list is a restatement of what [`super::asset_root`] does, and this
    /// project has spent enough time on rules that drifted from their
    /// restatements to be unwilling to add another one on trust. `build.rs`
    /// drives `cargo:rerun-if-env-changed` off this list: a variable missing
    /// from it is a build that keeps a stale answer after the operator moved
    /// the cache, and an extra one is a re-run that never fires.
    ///
    /// Reading its own source is the only way to check the two against each
    /// other, since the reads are `var_os` calls rather than data.
    #[test]
    fn the_declared_variables_are_exactly_the_ones_that_are_read() {
        let source = include_str!("asset_paths.rs");
        let read: Vec<&str> = source
            .match_indices("var_os(\"")
            .map(|(at, marker)| {
                let rest = &source[at + marker.len()..];
                rest.split_once('"')
                    .expect("a var_os literal is closed on the same line")
                    .0
            })
            .collect();

        assert!(
            !read.is_empty(),
            "no var_os call was found to check against"
        );
        for var in &read {
            assert!(
                super::ASSET_ROOT_VARS.contains(var),
                "asset_root reads {var}, but ASSET_ROOT_VARS does not declare it"
            );
        }
        for var in super::ASSET_ROOT_VARS {
            assert!(
                read.contains(var),
                "ASSET_ROOT_VARS declares {var}, but nothing here reads it"
            );
        }
    }
}
