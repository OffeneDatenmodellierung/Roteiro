//! What an analyzer's environment is, in one place.
//!
//! **Private because it is a seam inside this crate** rather than a contract
//! with a caller. It used to be shared by two backends — the host subprocess
//! and a microVM guest — and the guest went with the sandbox (ADR-0030); the
//! host half is what remains, used by [`crate::subprocess`] and [`crate::lint`].
//!
//! Two things reach an analyzer's environment and they read alike while doing
//! opposite things:
//!
//! - **Inheriting locates.** `CARGO_HOME`, `RUSTUP_HOME` — where the toolchain
//!   is *on this machine*, which only the parent environment knows and which is
//!   the user's to choose. This process has no opinion on the value.
//! - **Setting constrains.** `CARGO_TARGET_DIR` — where the build may write,
//!   which is a property the runner **guarantees** and therefore must choose
//!   itself. A guarantee that reads its own precondition out of the ambient
//!   environment is not a guarantee.
//!
//! Conflating them is not hypothetical. `CARGO_TARGET_DIR` was once listed as a
//! name to *inherit* by a module whose documentation promised it *set* one, and
//! inheriting a name the parent has not set is a no-op that reads as a
//! configuration — so `roteiro lint` wrote into the tree it was reviewing under
//! a paragraph saying it did not (ADR-0020 v1.4).
//!
//! @rto:0014
//! @rto:0020

/// Variables put on every analyzer, whatever launched it.
pub(crate) const BASE: &[(&str, &str)] = &[
    // Deterministic, locale-independent formatting and sorting.
    ("LC_ALL", "C"),
    // Semgrep reads this to decide whether to phone home.
    ("SEMGREP_SEND_METRICS", "off"),
];

/// The names a host child inherits from this process whatever the caller asked
/// for.
///
/// `PATH` because the analyzer needs to find its own helpers, `HOME` because
/// tools that cannot locate a home directory fail in confusing ways, and the
/// rest because a temporary directory is not optional on either platform. Both
/// are the parent's, unchanged.
#[cfg(feature = "exec-subprocess")]
pub(crate) const HOST_FLOOR: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SystemRoot",
    "TMPDIR",
    "TEMP",
];

/// The two ways a variable can reach an analyzer's environment, kept apart.
///
/// See the module documentation for why they are two fields rather than one
/// list.
#[derive(Default)]
pub(crate) struct ChildEnv<'a> {
    /// Names passed through from the parent when the parent has them. A name
    /// the parent does not have reaches the child not at all — it is never
    /// invented, and never defaulted.
    pub(crate) inherit: &'a [&'a str],
    /// Name/value pairs this process chooses outright, applied last so they win
    /// over anything of the same name in [`ChildEnv::inherit`].
    pub(crate) set: &'a [(&'a str, std::ffi::OsString)],
}

/// Give a host child a minimal, explicit environment.
///
/// A third-party binary running on a developer's machine inherits everything by
/// default, and a developer's environment is where `GITHUB_TOKEN`, `AWS_*`,
/// `SEMGREP_APP_TOKEN` and an SSH agent socket live. None of that is an analyzer
/// input, so none of it is passed.
///
/// This is a *reduction* in what the process can reach, **not a boundary**. It
/// stops an analyzer from picking up a credential by accident; it does not stop
/// one that goes looking. Since ADR-0030 there is no boundary behind it, which is
/// why a host run needs `--allow-unsandboxed` every time.
///
/// [`ChildEnv::inherit`] is for a caller whose tool needs more than a parser
/// does — the linter in [`crate::lint`] needs the variables that locate a Rust
/// toolchain, and would otherwise be handed a `PATH` shim with no toolchain
/// behind it. It is a **named list per caller**, not a pattern or an
/// inherit-everything escape: each addition is a variable somebody wrote down
/// and justified.
#[cfg(feature = "exec-subprocess")]
pub(crate) fn scrub_environment(command: &mut std::process::Command, env: &ChildEnv<'_>) {
    command.env_clear();
    for key in HOST_FLOOR
        .iter()
        .copied()
        .chain(env.inherit.iter().copied())
    {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    // **After** the inherited names, so a value Roteiro chose always beats the
    // same name arriving from the parent. A caller that both inherits and sets
    // one variable has expressed a contradiction, and the constraint is the half
    // that must win: inheriting is how the child finds things, setting is how
    // this process bounds what the child may do.
    for (key, value) in env.set {
        command.env(key, value);
    }
    for (key, value) in BASE {
        command.env(key, value);
    }
}

/// The last few lines of an analyzer's standard error, for a failure message.
///
/// Bounded, because a failing analyzer can be extremely talkative and a wall of
/// output in an error message hides the error. One implementation for the
/// analyzer runner and the linter, so a failure is shaped identically.
pub(crate) fn stderr_tail(stderr: &[u8]) -> String {
    const MAX_LINES: usize = 8;
    const MAX_BYTES: usize = 4_000;
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = trimmed
        .lines()
        .rev()
        .take(MAX_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut joined = tail.join("\n  ");
    if joined.len() > MAX_BYTES {
        // `truncate` panics off a char boundary, and stderr is not ASCII-only.
        let mut cut = MAX_BYTES;
        while !joined.is_char_boundary(cut) {
            cut -= 1;
        }
        joined.truncate(cut);
        joined.push('…');
    }
    format!("\n  its stderr ended:\n  {joined}")
}

#[cfg(test)]
mod tests {
    use super::{BASE, stderr_tail};

    /// A multi-byte character straddling the byte bound must be cut before,
    /// not through: `String::truncate` panics off a char boundary.
    #[test]
    fn a_stderr_tail_truncates_on_a_char_boundary() {
        // 3,999 ASCII bytes then a 3-byte `€` covering bytes 3,999..4,002.
        let stderr = format!("{}€€€", "a".repeat(3_999));
        let tail = stderr_tail(stderr.as_bytes());
        assert!(tail.ends_with("a…"), "{:?}", &tail[tail.len() - 8..]);
    }
    /// The base is what every analyzer gets, so it has to be
    /// non-empty and free of anything that could carry a credential.
    #[test]
    fn the_base_environment_is_configuration_rather_than_credentials() {
        assert!(!BASE.is_empty());
        for (key, _) in BASE {
            assert!(
                !key.contains("TOKEN") && !key.contains("KEY") && !key.contains("SECRET"),
                "{key} does not belong in an environment every analyzer gets"
            );
        }
    }
}
