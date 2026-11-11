//! ADR-0020 §6's grant: **may a linter run on this host?**
//!
//! Ungated, unlike `crate::lint` itself — deliberately **not** a link, because
//! this module is compiled unconditionally while `lint` is behind
//! `exec-subprocess`, so a link here would be unresolved in exactly the builds
//! the sentence is about. A policy that existed only where the
//! capability does would be the conversion ADR-0014 warns about, so the answer
//! is the same in a build that cannot run a linter as in one that can — see the
//! module's own documentation.
//!
//! `roteiro lint` runs the linter on this host only when a person has opted
//! into it, and refuses otherwise. (It ran sandboxed by default while a microVM
//! backend existed; ADR-0030 removed it, and saying nothing went back to being
//! a refusal.) This module is the whole of that rule — the
//! layering, the precedence between layers, and the sentence each refusal shows
//! the reader.
//!
//! # Why it compiles when the linter does not
//!
//! `crate::lint` needs `exec-subprocess`; this module needs nothing. That is
//! deliberate rather than incidental. ADR-0020 spends its length refusing one
//! specific failure — *the availability of a capability quietly deciding a
//! question that was supposed to be decided on purpose* — and a policy that
//! existed only in the builds able to act on it would be an instance of it. So
//! the answer to "may this machine run builds?" is available to `roteiro config`
//! and to the layering in a build with no linter at all, and it says the same
//! thing there as everywhere else.
//!
//! @rto:0020

use crate::guidance::{Guidance, Line};

/// What the two **configuration** layers jointly say about running a linter on
/// this host — the half of the grant that can be answered from files alone.
///
/// Built by [`ConfigGrant::from_layers`], which is this crate's single
/// implementation of ADR-0020 §6's "a project may deny but never grant". Nothing
/// else may re-derive that rule: the binary's config layering calls this, so the
/// value `roteiro config` echoes and the value `decide` consults are the same
/// value.
///
/// # Why the project layer cannot grant
///
/// `roteiro.toml` is committed and shared by design — ADR-0007's own reason for
/// it existing. A merged line that starts running builds on every teammate's
/// machine is consent by pull request: granted by someone else, noticed by
/// nobody. Denial has none of those problems, so the project layer keeps it: a
/// repository that wants no builds run by `roteiro lint` can say so.
///
/// It had a twin in `rto-remote` for the remote tier's `[remote] enabled`; that
/// crate and key were removed by ADR-0028, leaving this the only grant key.
///
/// Where they deliberately differ is the **invocation**, and that difference is
/// in `decide` rather than here: ADR-0019 needs the user layer *and* the flag,
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
    ///
    /// Delegates the rule itself to [`rto_graph::layering::Grant`], the
    /// workspace's one implementation of ADR-0007 v1.4's inversion, rather than
    /// carrying a private copy — the decay ADR-0007 §111 names by name. The predicates below stay here because they report
    /// what this *file* said, which is a `bool`-shaped question, not a layering one.
    #[must_use]
    pub fn as_effective(self) -> Option<bool> {
        // `false` is the built-in: an unset `[lint] allow_unsandboxed` is no grant.
        rto_graph::layering::Grant::from_layers(self.project(), self.user, false).as_effective()
    }

    /// The project layer's literal value, reassembled from the two flags stored.
    fn project(self) -> Option<bool> {
        match (self.project_denied, self.project_grant_ignored) {
            (true, _) => Some(false),
            (_, true) => Some(true),
            _ => None,
        }
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
/// invocation half of the grant, with no prompt form, because a
/// linter is run non-interactively far more often than it is not and a prompt
/// that a script cannot answer is a hang rather than a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Requested {
    /// No flag was passed. **Not a grant.**
    #[default]
    Unset,
    /// `--allow-unsandboxed`: run it here.
    Host,
}

/// Which layer decided, and therefore what the person should be told.
///
/// Every variant carries one sentence of explanation, because a decision the
/// person did not make is one they have to be able to account for — most of all
/// when it overrules something they *did* say. `--allow-unsandboxed` in a
/// repository that denies host execution is refused, and that has to be a
/// sentence rather than a silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// Host execution was granted by `--allow-unsandboxed` on this run.
    GrantedByInvocation,
    /// Host execution was granted by the user's own config, with no flag needed.
    GrantedByUserLayer,
    /// Nobody granted anything — **the default**, and refused.
    RefusedByDefault,
    /// The user's own config says `false`, so the host is off the table.
    RefusedByUserLayer,
    /// This repository's `roteiro.toml` denied host execution. Nothing
    /// overrides this, including `--allow-unsandboxed`.
    RefusedByProjectDenial,
}

impl Reason {
    /// Whether the linter may run on this host.
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::GrantedByInvocation | Self::GrantedByUserLayer)
    }

    /// One sentence naming which layer decided, for the line printed before the
    /// run.
    ///
    /// A granted reason completes "running clippy …"; a refused one completes
    /// "refused: …", so a person can tell at a glance which one they got.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Self::GrantedByInvocation => "on this host, granted by `--allow-unsandboxed`",
            Self::GrantedByUserLayer => {
                "on this host, granted by `[lint] allow_unsandboxed` in your own config"
            }
            Self::RefusedByDefault => "nothing granted host execution, and refusing is the default",
            Self::RefusedByUserLayer => {
                "your `~/.roteiro/config.toml` sets `[lint] allow_unsandboxed = false`"
            }
            Self::RefusedByProjectDenial => {
                "this repository's `roteiro.toml` sets `[lint] allow_unsandboxed = false`, which \
                 denies host execution for everyone working in it and is not overridden by \
                 `--allow-unsandboxed`"
            }
        }
    }

    /// How this person could grant a run on the host — or `None` when they
    /// could not, or already have.
    ///
    /// Consulted only by a refusal, and it exists so that a refusal names a way
    /// forward **that would actually work for this reason**. Telling someone in
    /// a repository that denies host execution to pass `--allow-unsandboxed`
    /// would waste their time, and telling someone who is already on the host
    /// how to get there would be nonsense. Both were live bugs in the shape this
    /// replaces (#426's refusals rule).
    ///
    /// A [`Guidance`] rather than a `&'static str`, because this text is
    /// multi-line and carries commands people paste. Written as one wrapped
    /// literal it leaked its own source indentation into shipped output; written
    /// as lines and fragments it cannot.
    #[must_use]
    pub fn host_escape(self) -> Option<Guidance> {
        match self {
            // Two reasons, one answer, and both are "there is nothing to offer":
            //
            // - a granted run is **already** on the host, so an escape to it
            //   would be nonsense;
            // - a project denial **cannot** be escaped, so offering
            //   `--allow-unsandboxed` would waste the reader's time. The way
            //   forward there is a change to the repository, not to a machine.
            //
            // They share an arm because clippy is right that the bodies are
            // identical; they are listed separately above because a future
            // editor changing one must not silently change the other.
            Self::GrantedByInvocation | Self::GrantedByUserLayer | Self::RefusedByProjectDenial => {
                None
            }
            Self::RefusedByDefault => Some(Guidance::new(&[
                Line::Note(&[
                    "To run it, accept an unisolated run. `cargo clippy` will then compile",
                    "this tree here, executing its build scripts and loading its proc macros",
                    "with your filesystem and your credentials. In your own repository that is",
                    "the build you were going to run anyway; in a branch you are reviewing it",
                    "is somebody else's code.",
                ]),
                Line::Note(&["Either one of these is enough — you do not need both:"]),
                // Aligned on purpose, and rendered verbatim so the alignment
                // survives. These are the two lines people copy.
                Line::Command("for this run:  roteiro lint <analyzer> --allow-unsandboxed"),
                Line::Command(
                    "standing:      add `[lint] allow_unsandboxed = true` to ~/.roteiro/config.toml",
                ),
                // Kept from the refusal this replaces. Someone who has just been
                // shown a config key will reach for the file they already edit,
                // and `roteiro.toml` is the wrong one — silently, since a
                // committed grant is read and discarded.
                Line::Note(&[
                    "A project's `roteiro.toml` cannot grant it — a committed file may deny",
                    "host execution and never grant it, because a merged line would otherwise",
                    "start running builds on every teammate's machine (ADR-0020 §6).",
                ]),
            ])),
            Self::RefusedByUserLayer => Some(Guidance::new(&[Line::Note(&[
                "Override your own `[lint] allow_unsandboxed = false` for this run with",
                "`--allow-unsandboxed`, accepting that the tree is then compiled on this host.",
            ])])),
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
        (true, _, _) => Reason::RefusedByProjectDenial,
        (false, Requested::Host, _) => Reason::GrantedByInvocation,
        (false, Requested::Unset, Some(true)) => Reason::GrantedByUserLayer,
        (false, Requested::Unset, Some(false)) => Reason::RefusedByUserLayer,
        (false, Requested::Unset, None) => Reason::RefusedByDefault,
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

    /// The default, and the reason this module exists: saying nothing refuses.
    ///
    /// It asserted "saying nothing selects the sandbox" while a microVM backend
    /// existed; with that removed (ADR-0030) the same table means refusal again,
    /// which is what it meant before the sandbox was built.
    #[test]
    fn saying_nothing_refuses() {
        let decision = at(None, None, Requested::Unset);
        assert_eq!(decision.reason, Reason::RefusedByDefault);
        assert!(!decision.granted());
    }

    /// ADR-0020 §6's table, one assertion per cell, over every combination of
    /// the three layers. Written exhaustively rather than as spot-checks because
    /// the table *is* the decision — a rule with an untested row is a rule with
    /// a row somebody will change.
    #[test]
    fn every_layer_combination_matches_the_adr_table() {
        for requested in [Requested::Unset, Requested::Host] {
            for user in [None, Some(true), Some(false)] {
                // The project layer may deny host execution, and its denial is
                // absolute.
                let denied = at(Some(false), user, requested);
                assert_eq!(
                    denied.reason,
                    Reason::RefusedByProjectDenial,
                    "project denial must outrank user={user:?} requested={requested:?}"
                );
                assert!(!denied.granted());

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
    fn the_flag_overrides_a_denial_by_the_user_but_never_by_the_project() {
        assert_eq!(
            at(None, Some(false), Requested::Host).reason,
            Reason::GrantedByInvocation
        );
        assert_eq!(
            at(Some(false), Some(false), Requested::Host).reason,
            Reason::RefusedByProjectDenial
        );
    }

    /// An ignored project grant is reported whichever way the decision went, and
    /// never confused with the decision itself.
    #[test]
    fn an_ignored_project_grant_is_reported_beside_the_outcome_not_folded_into_it() {
        let refused = at(Some(true), None, Requested::Unset);
        assert!(!refused.granted());
        assert!(refused.project_grant_ignored);
        assert!(refused.ignored_project_grant_note().is_some());

        // Also reported when the run went to the host for an unrelated reason:
        // the committed line still did nothing, and the team still needs telling.
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

    /// Every reason is printed before a run, so every reason has to have
    /// something to print — and it has to name the layer, because a decision
    /// nobody typed is one the person has to be able to account for.
    #[test]
    fn every_reason_explains_which_layer_decided() {
        for reason in [
            Reason::GrantedByInvocation,
            Reason::GrantedByUserLayer,
            Reason::RefusedByDefault,
            Reason::RefusedByUserLayer,
            Reason::RefusedByProjectDenial,
        ] {
            let explanation = reason.explanation();
            assert!(!explanation.trim().is_empty(), "{reason:?} says nothing");
            let names_the_layer = explanation.contains("--allow-unsandboxed")
                || explanation.contains("config")
                || explanation.contains("roteiro.toml")
                || explanation.contains("default");
            assert!(
                names_the_layer,
                "{reason:?} does not say who decided: {explanation}"
            );
            // A grant and a refusal must be told apart at a glance, since this
            // is the sentence a person reads to know what happened to their tree.
            assert_eq!(
                explanation.contains("on this host"),
                reason.granted(),
                "{reason:?}: {explanation}"
            );
        }
    }

    /// A refusal names a way forward **that would work for this person**, and
    /// the one person it must never offer `--allow-unsandboxed` to is the one in
    /// a repository that denies it (#426).

    #[test]
    fn the_host_escape_is_offered_only_to_someone_who_could_take_it() {
        // Nothing overrides a project denial, so there is no escape to name.
        assert!(
            Reason::RefusedByProjectDenial.host_escape().is_none(),
            "a project denial cannot be escaped, so offering a flag wastes the reader's time"
        );
        // Already on the host: there is nothing to escape to.
        for granted in [Reason::GrantedByInvocation, Reason::GrantedByUserLayer] {
            assert!(granted.host_escape().is_none(), "{granted:?}");
            assert!(granted.granted());
        }
        // And every reason that *can* be escaped says how, in a way that fits
        // what that person actually did.
        let default = Reason::RefusedByDefault
            .host_escape()
            .expect("escape")
            .to_string();
        assert!(default.contains("--allow-unsandboxed"), "{default}");
        assert!(
            default.contains("[lint] allow_unsandboxed = true"),
            "{default}"
        );
        assert!(
            default.contains("~/.roteiro/config.toml"),
            "and where it goes: {default}"
        );
        assert!(
            default.contains("build scripts"),
            "and what is being accepted: {default}"
        );
        // The asymmetry with ADR-0019, in the one place a user meets it. Two
        // forms listed without this sentence read as two *steps*, and someone
        // who set the config key would go on typing the flag forever — which is
        // the outcome ADR-0020 §6 gives as its reason for the asymmetry.
        assert!(
            default.contains("do not need both"),
            "the escape must say either one suffices: {default}"
        );
        assert!(
            default.contains("cannot grant"),
            "and that the committed file is not the place to put it: {default}"
        );

        let user = Reason::RefusedByUserLayer
            .host_escape()
            .expect("escape")
            .to_string();
        assert!(user.contains("--allow-unsandboxed"), "{user}");
        assert!(
            user.contains("your own"),
            "your own denial is yours to override: {user}"
        );
    }

    /// The default escape is copy-pasteable, so its shape is pinned: an editor
    /// that reflows the string must not silently turn the two forms into prose,
    /// and `--allow-unsandboxed` must never be left dangling at a wrap point
    /// where a copy would lose it.
    #[test]
    fn the_default_escape_keeps_each_form_on_its_own_line() {
        let escape = Reason::RefusedByDefault
            .host_escape()
            .expect("escape")
            .to_string();
        let lines: Vec<&str> = escape.lines().map(str::trim).collect();
        assert!(
            lines.contains(&"for this run:  roteiro lint <analyzer> --allow-unsandboxed"),
            "{escape}"
        );
        assert!(
            lines.contains(
                &"standing:      add `[lint] allow_unsandboxed = true` to ~/.roteiro/config.toml"
            ),
            "{escape}"
        );
    }
}
