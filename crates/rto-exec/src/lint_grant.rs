//! ADR-0020 §6's grant: **may a linter run on this host?**
//!
//! `roteiro lint` runs the linter sandboxed by default, and the host is
//! something a person opts into. This module is the whole of that rule — the
//! layering, the precedence between layers, and the sentence each refusal shows
//! the reader.
//!
//! # Why it compiles when the linter does not
//!
//! [`crate::lint`] needs `exec-subprocess`; this module needs nothing. That is
//! deliberate rather than incidental. ADR-0020 spends its length refusing one
//! specific failure — *the availability of a capability quietly deciding a
//! question that was supposed to be decided on purpose* — and a policy that
//! existed only in the builds able to act on it would be an instance of it. So
//! the answer to "may this machine run builds?" is available to `roteiro config`
//! and to the layering in a build with no linter at all, and it says the same
//! thing there as everywhere else.
//!
//! @rto:0020

/// What the two **configuration** layers jointly say about running a linter on
/// this host — the half of the grant that can be answered from files alone.
///
/// Built by [`ConfigGrant::from_layers`], which is this crate's single
/// implementation of ADR-0020 §6's "a project may deny but never grant". Nothing
/// else may re-derive that rule: the binary's config layering calls this, so the
/// value `roteiro config` echoes and the value [`decide`] consults are the same
/// value.
///
/// # Why the project layer cannot grant
///
/// `roteiro.toml` is committed and shared by design — ADR-0007's own reason for
/// it existing. A merged line that starts running builds on every teammate's
/// machine is consent by pull request: granted by someone else, noticed by
/// nobody. Denial has none of those problems, so the project layer keeps it: a
/// repository that wants the sandbox enforced for everyone can say so.
///
/// # Its twin in `rto-remote`, and the one place they part company
///
/// `rto_remote::ConfigGrant` implements the same inversion for ADR-0019's remote
/// tier, and the two agree exactly on this half — a cross-check in the binary's
/// test suite pins all nine layer combinations against each other, because two
/// implementations of one rule are a rule that will drift.
///
/// They are two implementations rather than one because `rto-remote` is an
/// optional, off-by-default crate and `lint` ships in the default feature set,
/// so this could not depend on that.
///
/// Where they deliberately differ is the **invocation**, and that difference is
/// in [`decide`] rather than here: ADR-0019 needs the user layer *and* the flag,
/// ADR-0020 §6 needs *either*. Remote egress sends your source elsewhere and is
/// worth re-consenting to per run; building on your own machine is a standing
/// preference somebody may reasonably express once. Do not "make them
/// consistent" — requiring both here would make the config key useless, since
/// you would still type the flag every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigGrant {
    /// The project file said `false`.
    project_denied: bool,
    /// The project file said `true`, and it was discarded.
    project_grant_ignored: bool,
    /// What the user layer said, verbatim.
    user: Option<bool>,
}

impl ConfigGrant {
    /// Read the two config layers, applying the inversion.
    ///
    /// `project` is `roteiro.toml`'s `[lint] allow_unsandboxed`; `user` is
    /// `~/.roteiro/config.toml`'s. Both are `None` when the key is absent.
    #[must_use]
    pub fn from_layers(project: Option<bool>, user: Option<bool>) -> Self {
        Self {
            project_denied: project == Some(false),
            project_grant_ignored: project == Some(true),
            user,
        }
    }

    /// The value `roteiro config` should echo for `[lint] allow_unsandboxed` —
    /// the config layers' *effective* contribution, with the invocation still
    /// outstanding.
    ///
    /// `Some(false)` when the project denied, otherwise the user layer's own
    /// value. A project grant never appears here, because it never becomes
    /// effective; [`ConfigGrant::project_grant_ignored`] is how it is reported
    /// instead.
    #[must_use]
    pub fn as_effective(self) -> Option<bool> {
        if self.project_denied {
            return Some(false);
        }
        self.user
    }

    /// Whether a committed project file tried to grant host execution and was
    /// overruled.
    #[must_use]
    pub fn project_grant_ignored(self) -> bool {
        self.project_grant_ignored
    }

    /// Whether the project file denied host execution for everyone using this
    /// repository.
    #[must_use]
    pub fn project_denied(self) -> bool {
        self.project_denied
    }
}

/// What the invocation asked for.
///
/// Named `Requested` rather than `Invocation` only because that name is already
/// taken in this crate by an analyzer's argv ([`crate::Invocation`]); it is the
/// same concept as `rto_remote::Invocation`, minus the prompt form, because a
/// linter is run non-interactively far more often than it is not and a prompt
/// that a script cannot answer is a hang rather than a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Requested {
    /// Neither flag was passed. **Not a grant** — the default is the sandbox.
    #[default]
    Unset,
    /// `--allow-unsandboxed`: run it here.
    Host,
    /// `--sandboxed`: run it in the sandbox, or not at all.
    Sandbox,
}

/// Which layer decided, and therefore what the person should do about it.
///
/// Every variant is one sentence of explanation and one of remedy, because with
/// the sandbox unbuilt a refusal **is** the command's entire user interface. A
/// gate that only says no teaches the reader to route around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// Host execution was granted by `--allow-unsandboxed` on this run.
    GrantedByInvocation,
    /// Host execution was granted by the user's own config, with no flag needed.
    GrantedByUserLayer,
    /// This repository's `roteiro.toml` denied it. Nothing overrides this.
    ProjectDenied,
    /// `--sandboxed` was passed, and no sandbox exists to honour it.
    InvocationDenied,
    /// The user's own config says `false`.
    UserLayerDenied,
    /// Nobody granted it — **the default**, and the common case.
    Ungranted,
}

impl Reason {
    /// Whether the linter may run on this host.
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::GrantedByInvocation | Self::GrantedByUserLayer)
    }

    /// Why the run was refused, and exactly what to do to allow it.
    ///
    /// `None` for a grant. Every refusal names a remedy that would actually
    /// work *for that reason* — telling someone in a repository that denies host
    /// execution to edit their user config would waste their time, and telling
    /// someone who typed `--sandboxed` that they forgot to opt in would be
    /// untrue.
    #[must_use]
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            Self::GrantedByInvocation | Self::GrantedByUserLayer => None,
            Self::ProjectDenied => Some(
                "this repository's `roteiro.toml` sets `[lint] allow_unsandboxed = false`, which \
                 denies host execution for everyone working in it. Neither your own config nor \
                 `--allow-unsandboxed` overrides that — a project may always deny. If it should \
                 not, that is a change to the repository, not to your machine.",
            ),
            Self::InvocationDenied => Some(
                "you passed `--sandboxed`, and this build has no sandboxed builder to honour it \
                 (ADR-0020 conditions 1-2 are unbuilt). Nothing was run, and nothing fell back to \
                 this host: asking for isolation and getting execution is the one outcome this \
                 command will not produce.",
            ),
            Self::UserLayerDenied => Some(
                "your `~/.roteiro/config.toml` sets `[lint] allow_unsandboxed = false`. Pass \
                 `--allow-unsandboxed` to override it for this run, or change the key.",
            ),
            // Built line-by-line rather than as one wrapped string: this is the
            // only text most users will ever see from this command, and a
            // remedy whose indentation drifts when someone reflows a comment is
            // a remedy that stops being copy-pasteable.
            Self::Ungranted => Some(concat!(
                "`roteiro lint` runs the linter sandboxed by default, and the sandboxed builder \
                 is not built yet (ADR-0020 conditions 1-2), so there is nothing to run it in. \
                 Nothing was run.\n",
                "  Running it on this host instead means `cargo clippy` compiles this tree here, \
                 which executes its build scripts and loads its proc macros with your filesystem \
                 and your credentials. In your own repository that is the build you were going \
                 to run anyway; in a branch you are reviewing it is somebody else's code. \
                 Roteiro will not make that choice for you.\n",
                "  To allow it, either one of these is enough — you do not need both:\n",
                "    for this run:  roteiro lint <analyzer> --allow-unsandboxed\n",
                "    standing:      add to ~/.roteiro/config.toml\n",
                "                     [lint]\n",
                "                     allow_unsandboxed = true\n",
                "  A project's `roteiro.toml` cannot grant it — a committed file may deny host \
                 execution and never grant it, because a merged line would otherwise start \
                 running builds on every teammate's machine (ADR-0020 §6).",
            )),
        }
    }
}

/// The outcome of consulting every layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Which layer decided. [`Reason::granted`] iff the linter may run here.
    pub reason: Reason,
    /// A project file's `[lint] allow_unsandboxed = true` was read and ignored.
    ///
    /// Independent of [`Decision::reason`]: an ignored project grant cannot
    /// change the outcome by definition, so it is reported alongside rather than
    /// folded in. Dropping it silently would leave a team wondering why their
    /// committed setting does nothing.
    pub project_grant_ignored: bool,
}

impl Decision {
    /// Whether this run may execute the linter on this host.
    #[must_use]
    pub fn granted(self) -> bool {
        self.reason.granted()
    }

    /// The note printed beside a decision when a committed grant was discarded,
    /// or `None` when there was nothing to discard.
    #[must_use]
    pub fn ignored_project_grant_note(self) -> Option<&'static str> {
        self.project_grant_ignored.then_some(
            "note: this repository's `roteiro.toml` sets `[lint] allow_unsandboxed = true`, which \
             was read and ignored. A committed file may deny host execution but never grant it, \
             because a merged line would otherwise start running builds on every teammate's \
             machine (ADR-0020 §6)",
        )
    }
}

/// Decide whether this run may execute the linter on this host.
///
/// # The order the layers are consulted, and why it is this order
///
/// 1. **The project's denial outranks everything.** A locked-down repository is
///    a legitimate thing to express, and someone working in one must not be told
///    to edit their user config, because it would not help.
/// 2. **Then the invocation**, in both directions. This is ADR-0007's ordinary
///    *flag beats config* rule, and it is why `--allow-unsandboxed` overrides a
///    user layer that said `false`: the standing preference is the user's own,
///    and a person may override their own standing preference for one run.
/// 3. **Then the user layer**, which grants on its own — no flag needed. This is
///    where ADR-0020 §6 parts company with ADR-0019 §3 deliberately: **either**
///    the user layer or the invocation suffices, rather than both. See
///    [`ConfigGrant`] for why, and do not reconcile them.
/// 4. **Otherwise ungranted**, which is the default and the common case.
///
/// A project *grant* appears nowhere in that list, because it never becomes
/// effective — it is reported through [`Decision::project_grant_ignored`].
#[must_use]
pub fn decide(config: ConfigGrant, requested: Requested) -> Decision {
    let reason = match (config.project_denied(), requested, config.as_effective()) {
        (true, _, _) => Reason::ProjectDenied,
        (false, Requested::Sandbox, _) => Reason::InvocationDenied,
        (false, Requested::Host, _) => Reason::GrantedByInvocation,
        (false, Requested::Unset, Some(true)) => Reason::GrantedByUserLayer,
        (false, Requested::Unset, Some(false)) => Reason::UserLayerDenied,
        (false, Requested::Unset, None) => Reason::Ungranted,
    };
    debug_assert_eq!(
        reason.granted(),
        !config.project_denied()
            && (requested == Requested::Host
                || (requested == Requested::Unset && config.as_effective() == Some(true))),
        "either the user layer or the invocation grants, and the project may always deny"
    );
    Decision {
        reason,
        project_grant_ignored: config.project_grant_ignored(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigGrant, Decision, Reason, Requested, decide};

    fn at(project: Option<bool>, user: Option<bool>, requested: Requested) -> Decision {
        decide(ConfigGrant::from_layers(project, user), requested)
    }

    /// The default, and the reason this module exists: saying nothing asks for
    /// the sandbox, and the sandbox is unbuilt, so nothing runs.
    #[test]
    fn saying_nothing_does_not_grant_the_host() {
        let decision = at(None, None, Requested::Unset);
        assert_eq!(decision.reason, Reason::Ungranted);
        assert!(!decision.granted());
    }

    /// ADR-0020 §6's table, one assertion per cell, over every combination of
    /// the three layers. Written exhaustively rather than as spot-checks because
    /// the table *is* the decision — a rule with an untested row is a rule with
    /// a row somebody will change.
    #[test]
    fn every_layer_combination_matches_the_adr_table() {
        for requested in [Requested::Unset, Requested::Host, Requested::Sandbox] {
            for user in [None, Some(true), Some(false)] {
                // The project layer may deny, and its denial is absolute.
                assert_eq!(
                    at(Some(false), user, requested).reason,
                    Reason::ProjectDenied,
                    "project denial must outrank user={user:?} requested={requested:?}"
                );
                assert!(!at(Some(false), user, requested).granted());

                // The project layer may never grant: with `project = Some(true)`
                // the outcome must be identical to the key being absent.
                assert_eq!(
                    at(Some(true), user, requested).reason,
                    at(None, user, requested).reason,
                    "a project grant must change nothing (user={user:?} requested={requested:?})"
                );
            }
        }
    }

    /// **Either** suffices — the one place ADR-0020 §6 parts company with
    /// ADR-0019 §3, and the part the ADR says not to reconcile.
    #[test]
    fn either_the_user_layer_or_the_invocation_grants_alone() {
        assert_eq!(
            at(None, Some(true), Requested::Unset).reason,
            Reason::GrantedByUserLayer,
            "a standing preference needs no flag, or the key would be useless"
        );
        assert_eq!(
            at(None, None, Requested::Host).reason,
            Reason::GrantedByInvocation,
            "a flag needs no standing preference"
        );
        // And both together is still a grant, reported as the flag — the more
        // recent and more specific of the two acts.
        assert_eq!(
            at(None, Some(true), Requested::Host).reason,
            Reason::GrantedByInvocation
        );
    }

    /// ADR-0007's ordinary rule, which this key does *not* invert: a flag beats
    /// the config. It is your own standing preference, and you may override it
    /// for one run without editing a file.
    #[test]
    fn the_flag_overrides_the_users_own_denial_but_never_the_projects() {
        assert_eq!(
            at(None, Some(false), Requested::Host).reason,
            Reason::GrantedByInvocation
        );
        assert_eq!(
            at(Some(false), Some(false), Requested::Host).reason,
            Reason::ProjectDenied
        );
    }

    /// Asking for the sandbox is a denial of the host, and it outranks a
    /// standing grant: `--sandboxed` is how someone with the key set opts *one*
    /// run back out.
    #[test]
    fn asking_for_the_sandbox_denies_the_host_whatever_the_config_says() {
        for user in [None, Some(true), Some(false)] {
            let decision = at(None, user, Requested::Sandbox);
            assert_eq!(decision.reason, Reason::InvocationDenied, "user={user:?}");
            assert!(!decision.granted());
        }
    }

    /// An ignored project grant is reported whichever way the decision went, and
    /// never confused with the decision itself.
    #[test]
    fn an_ignored_project_grant_is_reported_beside_the_outcome_not_folded_into_it() {
        let refused = at(Some(true), None, Requested::Unset);
        assert!(!refused.granted());
        assert!(refused.project_grant_ignored);
        assert!(refused.ignored_project_grant_note().is_some());

        // Also reported when the run went ahead for an unrelated reason: the
        // committed line still did nothing, and the team still needs telling.
        let granted = at(Some(true), None, Requested::Host);
        assert!(granted.granted());
        assert!(granted.project_grant_ignored);

        assert!(
            at(None, Some(true), Requested::Unset)
                .ignored_project_grant_note()
                .is_none(),
            "there was nothing to discard"
        );
    }

    /// The config half, as `roteiro config` echoes it. A project grant must not
    /// appear here — echoing it would tell a team their committed line worked.
    #[test]
    fn the_effective_config_value_never_shows_a_project_grant() {
        assert_eq!(
            ConfigGrant::from_layers(Some(true), None).as_effective(),
            None
        );
        assert_eq!(
            ConfigGrant::from_layers(Some(true), Some(false)).as_effective(),
            Some(false)
        );
        // A denial does show, because it took effect.
        assert_eq!(
            ConfigGrant::from_layers(Some(false), Some(true)).as_effective(),
            Some(false)
        );
        assert_eq!(
            ConfigGrant::from_layers(None, Some(true)).as_effective(),
            Some(true)
        );
        assert_eq!(ConfigGrant::from_layers(None, None).as_effective(), None);
    }

    /// With the sandbox unbuilt, a refusal is this command's entire user
    /// interface — so every refusal must carry a remedy, and no grant may.
    #[test]
    fn every_refusal_carries_a_remedy_and_no_grant_does() {
        for reason in [
            Reason::Ungranted,
            Reason::UserLayerDenied,
            Reason::ProjectDenied,
            Reason::InvocationDenied,
        ] {
            let remedy = reason
                .remedy()
                .unwrap_or_else(|| panic!("{reason:?} has no remedy"));
            assert!(!remedy.is_empty());
            assert!(
                remedy.contains("ADR-0020") || remedy.contains("--allow-unsandboxed"),
                "{reason:?}: a refusal must point somewhere: {remedy}"
            );
        }
        for reason in [Reason::GrantedByInvocation, Reason::GrantedByUserLayer] {
            assert!(reason.remedy().is_none(), "{reason:?} was granted");
            assert!(reason.granted());
        }
    }

    /// Each remedy has to work *for its own reason*. Telling someone in a
    /// denying repository to edit their user config wastes their time, and
    /// telling someone who typed `--sandboxed` that they forgot to opt in is
    /// untrue — so the two must not share wording.
    #[test]
    fn each_remedy_names_the_layer_that_actually_refused() {
        let ungranted = Reason::Ungranted.remedy().expect("remedy");
        assert!(ungranted.contains("--allow-unsandboxed"), "{ungranted}");
        assert!(
            ungranted.contains("[lint]"),
            "the standing form: {ungranted}"
        );
        assert!(
            ungranted.contains("~/.roteiro/config.toml"),
            "and where it goes: {ungranted}"
        );
        assert!(
            ungranted.contains("sandboxed by default"),
            "and why it refused: {ungranted}"
        );
        assert!(
            ungranted.contains("not built yet"),
            "and that the intended path is unbuilt: {ungranted}"
        );
        assert!(
            ungranted.contains("cannot grant"),
            "and that roteiro.toml is not the place: {ungranted}"
        );
        // The asymmetry with ADR-0019, in the one place a user meets it. Two
        // remedies listed without this sentence read as two *steps*, and someone
        // who set the config key would go on typing the flag forever — which is
        // the outcome ADR-0020 §6 gives as its reason for the asymmetry.
        assert!(
            ungranted.contains("do not need both"),
            "the remedy must say either one suffices: {ungranted}"
        );

        let project = Reason::ProjectDenied.remedy().expect("remedy");
        assert!(project.contains("roteiro.toml"), "{project}");
        assert!(
            !project.contains("~/.roteiro/config.toml"),
            "editing your own config would not help, so it must not be offered: {project}"
        );

        let sandboxed = Reason::InvocationDenied.remedy().expect("remedy");
        assert!(sandboxed.contains("--sandboxed"), "{sandboxed}");
        assert!(
            sandboxed.contains("fell back") || sandboxed.contains("not produce"),
            "asking for isolation and getting execution is what it must promise \
             against: {sandboxed}"
        );

        let user = Reason::UserLayerDenied.remedy().expect("remedy");
        assert!(user.contains("~/.roteiro/config.toml"), "{user}");
        assert!(
            user.contains("--allow-unsandboxed"),
            "your own denial is yours to override: {user}"
        );
    }

    /// The remedy is copy-pasteable, so its shape is pinned: an editor that
    /// reflows the string must not silently turn the config snippet into prose.
    #[test]
    fn the_default_remedy_keeps_its_config_snippet_on_its_own_lines() {
        let remedy = Reason::Ungranted.remedy().expect("remedy");
        let snippet: Vec<&str> = remedy
            .lines()
            .map(str::trim)
            .skip_while(|l| *l != "[lint]")
            .take(2)
            .collect();
        assert_eq!(
            snippet,
            vec!["[lint]", "allow_unsandboxed = true"],
            "the TOML must stand alone, in order, one key per line"
        );
        // And the flag form is a whole line too, so `--allow-unsandboxed` is
        // never left dangling at a wrap point where a copy would lose it.
        assert!(
            remedy
                .lines()
                .any(|l| l.trim() == "for this run:  roteiro lint <analyzer> --allow-unsandboxed"),
            "{remedy}"
        );
    }
}
