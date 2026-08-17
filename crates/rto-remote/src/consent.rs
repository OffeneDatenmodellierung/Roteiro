//! The consent gate — ADR-0019 §3, and the reason this crate exists.
//!
//! # The edge is a gate, not a routing decision
//!
//! Mis-routing among local models wastes tokens. Mis-routing *outward* sends
//! source off the machine for a reason nobody can inspect afterwards. So the
//! local→remote edge is **a boolean the user opened**, never a prediction: there
//! is no classifier here, no scoring, nothing probabilistic, and nothing in this
//! module reads anything about the request it is gating. [`decide`] takes four
//! `Option<bool>`-shaped facts about *layers* and returns one of six [`Reason`]s.
//!
//! # The precedence is inverted for this one key
//!
//! [[docs/adr/0007-configuration-file.md]] establishes **CLI flag > project
//! `roteiro.toml` > user `~/.roteiro/config.toml` > built-in default**, and for
//! every other key that is right. For the remote-enable key it is inverted,
//! because `roteiro.toml` is *committed and shared by design* — ADR-0007's own
//! words are "committed — so a team shares the same, reproducible settings". A
//! merged line in a shared file authorising egress on every teammate's machine is
//! not consent; it is consent by pull request, granted by someone else, noticed
//! by nobody.
//!
//! | Layer | May deny | May grant |
//! |---|---|---|
//! | Built-in default | denied by default | — |
//! | Project `roteiro.toml` | **yes** | **no** |
//! | User `~/.roteiro/config.toml` | yes | yes — necessary, not sufficient |
//! | Invocation (flag, or a TTY prompt) | yes | yes — necessary, not sufficient |
//!
//! **Both** the user layer and the invocation must grant, and neither alone
//! suffices: the user layer opts *the human* in, the invocation opts *the run*
//! in. A project may still switch it off for everyone — a locked-down repository
//! is a legitimate thing to express, and denial has none of the problems of
//! grant.
//!
//! # An ignored grant is reported, not swallowed
//!
//! A project file that says `enabled = true` is doing something reasonable and
//! wrong. Dropping it silently would leave a team wondering why their committed
//! setting does nothing, so [`Decision::project_grant_ignored`] carries the fact
//! and every surface that prints a decision prints it.

use serde::{Deserialize, Serialize};

/// What the two **configuration** layers jointly say about the remote tier —
/// the half of consent that can be answered from files alone.
///
/// Built by [`ConfigGrant::from_layers`], which is the single implementation of
/// "a project may deny but never grant". Nothing else in the workspace may
/// re-derive that rule: the binary's config layering calls this, so the value
/// `roteiro config` echoes and the value the gate consults are the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// `project` is `roteiro.toml`'s `[remote] enabled`; `user` is
    /// `~/.roteiro/config.toml`'s. Both are `None` when the key is absent.
    #[must_use]
    pub fn from_layers(project: Option<bool>, user: Option<bool>) -> Self {
        Self {
            project_denied: project == Some(false),
            project_grant_ignored: project == Some(true),
            user,
        }
    }

    /// The value `roteiro config` should echo for `[remote] enabled` — the
    /// config layers' *effective* contribution, with the invocation still
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

    /// Whether a committed project file tried to grant egress and was overruled.
    #[must_use]
    pub fn project_grant_ignored(self) -> bool {
        self.project_grant_ignored
    }

    /// Whether the project file denied the tier for everyone using this
    /// repository.
    #[must_use]
    pub fn project_denied(self) -> bool {
        self.project_denied
    }
}

/// Why the gate is open or shut. Every variant names a layer and, via
/// [`Reason::remedy`], says what would change it.
///
/// One enum rather than a bool because *"the remote tier is off"* is not an
/// answer anyone can act on, and because the reasons are not interchangeable:
/// [`Reason::ProjectDenied`] is a deliberate decision by the repository that no
/// flag can override, while [`Reason::InvocationUnset`] is one flag away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The user layer and the invocation both granted. The only granting reason.
    Granted,
    /// The project's `roteiro.toml` denied it. Outranks every other layer.
    ProjectDenied,
    /// This invocation denied it (`--no-remote`).
    InvocationDenied,
    /// The user config set `enabled = false`.
    UserLayerDenied,
    /// The user config does not set `enabled = true`. This is the default state.
    UserLayerUnset,
    /// The user opted in, but this run did not (`--allow-remote` absent).
    InvocationUnset,
}

impl Reason {
    /// Stable token for `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::ProjectDenied => "project_denied",
            Self::InvocationDenied => "invocation_denied",
            Self::UserLayerDenied => "user_layer_denied",
            Self::UserLayerUnset => "user_layer_unset",
            Self::InvocationUnset => "invocation_unset",
        }
    }

    /// One sentence saying which layer decided this, in the terms a reader of
    /// that layer's file would recognise.
    #[must_use]
    pub fn explain(self) -> &'static str {
        match self {
            Self::Granted => {
                "your user config and this invocation both granted it (both are required)"
            }
            Self::ProjectDenied => {
                "this repository's `roteiro.toml` sets `[remote] enabled = false`, which \
                 switches the remote tier off for everyone working in it"
            }
            Self::InvocationDenied => "this invocation denied it (`--no-remote`)",
            Self::UserLayerDenied => {
                "your `~/.roteiro/config.toml` sets `[remote] enabled = false`"
            }
            Self::UserLayerUnset => {
                "your `~/.roteiro/config.toml` does not set `[remote] enabled = true`"
            }
            Self::InvocationUnset => {
                "this invocation did not grant it — your user config opts *you* in, and an \
                 invocation still has to opt *this run* in"
            }
        }
    }

    /// What to do about it, or `None` when the answer is "nothing you can do
    /// from here" — which is the honest answer for a repository-wide denial.
    #[must_use]
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            Self::Granted => None,
            Self::ProjectDenied => Some(
                "a project may deny the remote tier and no flag overrides that; take it up \
                 with the repository rather than with your own configuration",
            ),
            Self::InvocationDenied => Some("drop `--no-remote` to leave the decision to consent"),
            Self::UserLayerDenied | Self::UserLayerUnset => Some(
                "set `[remote] enabled = true` in `~/.roteiro/config.toml`. It has to be that \
                 file: a committed `roteiro.toml` may deny egress but never grant it, so nobody \
                 can turn this on for you",
            ),
            Self::InvocationUnset => Some("pass `--allow-remote` to grant this run"),
        }
    }

    /// Whether this reason is a grant.
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.explain())?;
        if let Some(remedy) = self.remedy() {
            write!(f, " — {remedy}")?;
        }
        Ok(())
    }
}

/// The gate's answer: whether egress is permitted, which layer decided, and
/// whether a committed grant was discarded on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Which layer decided. [`Reason::Granted`] iff egress is permitted.
    pub reason: Reason,
    /// A project file's `[remote] enabled = true` was read and ignored.
    ///
    /// Independent of [`Decision::reason`]: an ignored project grant does not
    /// change the outcome by definition, so it is reported alongside rather than
    /// folded in.
    pub project_grant_ignored: bool,
}

impl Decision {
    /// Whether this run may send anything.
    #[must_use]
    pub fn granted(self) -> bool {
        self.reason.granted()
    }

    /// The note printed beside a decision when a committed grant was discarded,
    /// or `None` when there was nothing to discard.
    #[must_use]
    pub fn ignored_project_grant_note(self) -> Option<&'static str> {
        self.project_grant_ignored.then_some(
            "note: this repository's `roteiro.toml` sets `[remote] enabled = true`, which was \
             read and ignored. A committed file may deny egress but never grant it, because a \
             merged line would otherwise authorise it on every teammate's machine (ADR-0019 §3)",
        )
    }
}

/// Decide whether this run may send.
///
/// `invocation` is the flag (or, later, a TTY prompt): `Some(true)` for
/// `--allow-remote`, `Some(false)` for `--no-remote`, `None` for neither.
///
/// # The order the layers are consulted, and why it is this order
///
/// Denials are reported before absences, and the project's before anyone's,
/// because the reasons carry different remedies and reporting the wrong one
/// wastes the reader's time. Someone who passed `--no-remote` does not need to be
/// told their user config is also unset; someone in a repository that denies the
/// tier must not be told to edit their user config, because it would not help.
#[must_use]
pub fn decide(config: ConfigGrant, invocation: Option<bool>) -> Decision {
    let reason = match (config.project_denied(), invocation, config.as_effective()) {
        (true, _, _) => Reason::ProjectDenied,
        (false, Some(false), _) => Reason::InvocationDenied,
        (false, _, Some(false)) => Reason::UserLayerDenied,
        (false, _, None) => Reason::UserLayerUnset,
        (false, None, Some(true)) => Reason::InvocationUnset,
        (false, Some(true), Some(true)) => Reason::Granted,
    };
    Decision {
        reason,
        project_grant_ignored: config.project_grant_ignored(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigGrant, Reason, decide};

    /// Decide from raw layer values: `(project, user, invocation)`.
    fn gate(
        project: Option<bool>,
        user: Option<bool>,
        invocation: Option<bool>,
    ) -> super::Decision {
        decide(ConfigGrant::from_layers(project, user), invocation)
    }

    /// **Neither half of the grant suffices alone.** The user layer opts the
    /// human in and the invocation opts the run in; ADR-0019 requires both, so
    /// this walks every combination of the two with the project silent.
    #[test]
    fn the_user_layer_and_the_invocation_are_both_necessary() {
        assert!(gate(None, Some(true), Some(true)).granted(), "both grant");
        for (user, invocation, expected) in [
            (Some(true), None, Reason::InvocationUnset),
            (None, Some(true), Reason::UserLayerUnset),
            (None, None, Reason::UserLayerUnset),
            (Some(false), Some(true), Reason::UserLayerDenied),
            (Some(true), Some(false), Reason::InvocationDenied),
        ] {
            let decision = gate(None, user, invocation);
            assert!(
                !decision.granted(),
                "user={user:?} invocation={invocation:?} must not grant"
            );
            assert_eq!(
                decision.reason, expected,
                "user={user:?} invocation={invocation:?}"
            );
        }
    }

    /// **A project-layer grant does not enable egress.** This is the whole of
    /// ADR-0019 §3: `roteiro.toml` is committed and shared, so a merged line
    /// authorising egress on every teammate's machine is consent granted by
    /// someone else and noticed by nobody.
    ///
    /// Asserted with the invocation *granting*, so nothing but the missing user
    /// layer can be what stops it.
    #[test]
    fn a_project_layer_grant_does_not_enable_egress() {
        let decision = gate(Some(true), None, Some(true));
        assert!(!decision.granted(), "a committed file cannot grant egress");
        assert_eq!(decision.reason, Reason::UserLayerUnset);
        assert!(
            decision.project_grant_ignored,
            "the discarded grant is reported, not swallowed"
        );
        assert!(
            ConfigGrant::from_layers(Some(true), None)
                .as_effective()
                .is_none(),
            "and it never becomes the effective config value either"
        );
    }

    /// **A project-layer denial holds even when both granting layers grant.** A
    /// locked-down repository is a legitimate thing to express, and denial has
    /// none of the problems of grant, so it outranks everything.
    #[test]
    fn a_project_layer_denial_beats_the_user_layer_and_the_invocation() {
        let decision = gate(Some(false), Some(true), Some(true));
        assert!(!decision.granted());
        assert_eq!(decision.reason, Reason::ProjectDenied);
        assert_eq!(
            ConfigGrant::from_layers(Some(false), Some(true)).as_effective(),
            Some(false),
            "and the effective config value says so too"
        );
        assert!(
            decision
                .reason
                .remedy()
                .is_some_and(|r| r.contains("no flag overrides")),
            "the remedy must not send the reader to edit a file that would not help"
        );
    }

    /// A project grant is discarded even when it would have agreed with the
    /// outcome — the flag reports what the file *said*, not whether it mattered.
    #[test]
    fn an_ignored_project_grant_is_reported_even_when_the_gate_opens() {
        let decision = gate(Some(true), Some(true), Some(true));
        assert!(
            decision.granted(),
            "the user layer and the invocation granted"
        );
        assert!(decision.project_grant_ignored);
        assert!(
            decision
                .ignored_project_grant_note()
                .is_some_and(|n| n.contains("read and ignored")),
            "a discarded committed setting is explained rather than left mysterious"
        );
    }

    /// Explicit denials are reported ahead of absent grants, because their
    /// remedies differ and the wrong remedy is worse than none.
    #[test]
    fn an_explicit_denial_is_reported_ahead_of_an_absent_grant() {
        // `--no-remote` with nothing else set: the flag is the answer, not the
        // unset user layer.
        assert_eq!(
            gate(None, None, Some(false)).reason,
            Reason::InvocationDenied
        );
        // A project denial with `--no-remote` too: the project's is the one that
        // cannot be undone, so it is the one reported.
        assert_eq!(
            gate(Some(false), None, Some(false)).reason,
            Reason::ProjectDenied
        );
    }

    /// Every reason explains itself, and every non-granting one says what would
    /// change it. A gate that only says "no" is not a consent model.
    #[test]
    fn every_reason_explains_itself_and_only_the_grant_needs_no_remedy() {
        for reason in [
            Reason::Granted,
            Reason::ProjectDenied,
            Reason::InvocationDenied,
            Reason::UserLayerDenied,
            Reason::UserLayerUnset,
            Reason::InvocationUnset,
        ] {
            assert!(!reason.explain().is_empty(), "{reason:?}");
            assert!(!reason.as_str().is_empty(), "{reason:?}");
            assert_eq!(
                reason.remedy().is_none(),
                reason.granted(),
                "{reason:?}: only a grant needs no remedy"
            );
        }
        // The user-layer remedy has to name the *user* file, since naming the
        // project file would be advice that cannot work.
        let remedy = Reason::UserLayerUnset.remedy().expect("a remedy");
        assert!(remedy.contains("~/.roteiro/config.toml"), "{remedy}");
        assert!(remedy.contains("never grant"), "{remedy}");
    }
}
