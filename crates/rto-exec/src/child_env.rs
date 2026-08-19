//! What an analyzer's environment is, in one place, for both backends.
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
//! # Why this module exists rather than the two backends each having their own
//!
//! Because they did, and that is the shape that produced the defect above.
//! [`crate::subprocess`] had [`ChildEnv`] and [`crate::boxlite`] had a
//! `guest_environment()` built from nothing — two mechanisms for one concept,
//! which is how the two drift into disagreeing about what an analyzer is
//! allowed to see.
//!
//! They are now one type with **two consumers**, and the difference between the
//! consumers is real rather than tidied away:
//!
//! | | host child | microVM guest |
//! |---|---|---|
//! | [`ChildEnv::set`] | applied | applied |
//! | [`ChildEnv::inherit`] | applied | **cannot mean anything** |
//! | base ([`BASE`]) | applied | applied |
//!
//! The `inherit` half is host-only **by construction, not by omission**. A guest
//! does not share this machine's filesystem, so `CARGO_HOME=/Users/you/.cargo`
//! names nothing there; and it does not share this machine's environment block,
//! so there is no parent to inherit *from*. Everything a guest needs to locate
//! is a property of its image or of a mount, and both are chosen by the runner —
//! so in a guest **everything is set**, and the inherit half has nothing it
//! could express. [`ChildEnv::guest_pairs`] says so where it would otherwise be
//! silently dropped.
//!
//! @rto:0014
//! @rto:0020

/// Variables both backends put on every analyzer, whatever launched it.
///
/// One list rather than two identical ones, so a change to what an analyzer is
/// told cannot land on one backend and not the other — which is the drift the
/// module documentation describes.
pub(crate) const BASE: &[(&str, &str)] = &[
    // Deterministic, locale-independent formatting and sorting.
    ("LC_ALL", "C"),
    // Semgrep reads this to decide whether to phone home. A sandboxed run cannot
    // reach anything regardless; saying so costs nothing and keeps the two
    // backends' configurations identical, which is this module's whole point.
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
/// list, and why only one of them can mean anything in a guest.
#[derive(Default)]
pub(crate) struct ChildEnv<'a> {
    /// Names passed through from the parent when the parent has them. A name
    /// the parent does not have reaches the child not at all — it is never
    /// invented, and never defaulted.
    ///
    /// **Host-only.** A guest has no parent environment; see
    /// [`ChildEnv::guest_pairs`].
    pub(crate) inherit: &'a [&'a str],
    /// Name/value pairs this process chooses outright, applied last so they win
    /// over anything of the same name in [`ChildEnv::inherit`].
    pub(crate) set: &'a [(&'a str, std::ffi::OsString)],
}

impl ChildEnv<'_> {
    /// This environment as a **guest's**: the `set` half and the base, and
    /// nothing else.
    ///
    /// Built up rather than filtered down, which is what makes "no ambient
    /// credentials" structural here rather than a list that has to stay
    /// complete: a microVM inherits nothing, so the only variables that exist
    /// are the ones returned here.
    ///
    /// # Why [`ChildEnv::inherit`] is absent rather than dropped
    ///
    /// It has no meaning to drop. Every name in it is a *locator on this
    /// machine* — `CARGO_HOME` points into the host filesystem, `RUSTUP_HOME`
    /// names a toolchain built for the host's OS — and none of those paths
    /// exists in a guest, which is a different kernel with a different mount
    /// table. Passing one through would not be a leak so much as a lie: a
    /// variable naming a directory that is not there.
    ///
    /// So a caller that reaches a guest states everything, and the
    /// `debug_assert!` below is where a caller that forgot finds out. It is a
    /// programming error rather than a user-facing one: a guest-bound
    /// [`ChildEnv`] with a non-empty `inherit` is a caller who believes a
    /// variable is being carried across a boundary that cannot carry it.
    #[cfg(feature = "exec-boxlite")]
    #[must_use]
    pub(crate) fn guest_pairs(&self) -> Vec<(String, String)> {
        debug_assert!(
            self.inherit.is_empty(),
            "a guest has no parent environment to inherit {:?} from — name the value instead",
            self.inherit
        );
        let mut pairs: Vec<(String, String)> = self
            .set
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.to_string_lossy().into_owned()))
            .collect();
        // **After** the caller's, matching the host order below: the base is what
        // roteiro asserts about every analyzer everywhere, and a caller must not
        // be able to turn `LC_ALL` off by accident.
        pairs.extend(
            BASE.iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        pairs
    }
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
/// one that goes looking. The boundary is [`crate::boxlite`], and the difference
/// between the two is the whole of ADR-0014.
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

#[cfg(test)]
mod tests {
    use super::BASE;
    // Only the guest half of this module has tests that need the type, and it is
    // the half that exists only in an `exec-boxlite` build.
    #[cfg(feature = "exec-boxlite")]
    use super::ChildEnv;

    /// The base is what the two backends must agree on, so it has to be
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

    /// The guest half carries what the caller set **and** the base, and the
    /// base wins — a caller must not be able to turn `LC_ALL` off by accident.
    #[cfg(feature = "exec-boxlite")]
    #[test]
    fn a_guest_environment_is_what_was_set_plus_the_base() {
        let set = [("CARGO_TARGET_DIR", std::ffi::OsString::from("/scratch"))];
        let pairs = ChildEnv {
            inherit: &[],
            set: &set,
        }
        .guest_pairs();
        assert!(pairs.contains(&("CARGO_TARGET_DIR".to_owned(), "/scratch".to_owned())));
        for (key, value) in BASE {
            assert!(
                pairs.contains(&((*key).to_owned(), (*value).to_owned())),
                "{key} must reach the guest"
            );
        }
    }

    /// A guest is *built up*, so this checks the **absence** of everything a
    /// developer's shell carries — the property that matters — rather than the
    /// presence of what we wrote.
    #[cfg(feature = "exec-boxlite")]
    #[test]
    fn a_guest_environment_carries_no_ambient_credentials() {
        let pairs = ChildEnv::default().guest_pairs();
        let names: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        for secret in [
            "GITHUB_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "SEMGREP_APP_TOKEN",
            "CARGO_REGISTRY_TOKEN",
            "SSH_AUTH_SOCK",
            "HOME",
            "PATH",
        ] {
            assert!(!names.contains(&secret), "{secret} reaches the guest");
        }
    }
}
