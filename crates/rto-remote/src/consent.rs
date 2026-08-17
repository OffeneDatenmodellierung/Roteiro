//! The consent gate — ADR-0019 §3, and the reason this crate exists.
//!
//! # The edge is a gate, not a routing decision
//!
//! Mis-routing among local models wastes tokens. Mis-routing *outward* sends
//! source off the machine for a reason nobody can inspect afterwards. So the
//! local→remote edge is **a boolean the user opened**, never a prediction: there
//! is no classifier here, no scoring, nothing probabilistic, and nothing in this
//! module reads anything about the request it is gating. [`decide`] takes facts
//! about *layers* and returns one of seven [`Reason`]s.
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

/// How the invocation answered — **and by which of its two forms.**
///
/// ADR-0019 §3's table row is *"Invocation (flag, or a TTY prompt)"*: the ADR
/// always had two forms, and until this type existed the code collapsed them
/// into one `Option<bool>`. That collapse was not free. A person who answered
/// *no* at a prompt was told they had passed `--no-remote` — a flag they never
/// typed, and would not find in their shell history if they went looking. A
/// message about consent that misreports how consent was withheld undermines the
/// thing it is reporting on, so the two forms are distinguishable here and
/// produce different [`Reason`]s.
///
/// [`decide`] takes the flag form directly, because that is the common one and
/// changing its signature would have bought nothing; [`decide_with`] takes this.
///
/// Marked `#[non_exhaustive]` for the reason recorded on [`Reason`]: the ADR
/// names two forms today, and a third — an environment grant, an MCP client's
/// own consent — would otherwise be breaking to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Invocation {
    /// Neither flag was passed, and nobody was asked. **Not a grant** — the run
    /// still has to opt in.
    Unset,
    /// `--allow-remote` (`true`) or `--no-remote` (`false`).
    Flag(bool),
    /// A person was shown the exact bytes at a TTY and answered: `true` for yes,
    /// `false` for anything else.
    ///
    /// A prompt may only ever supply this half of consent, and only from
    /// [`Reason::InvocationUnset`] — it may never stand in for the user layer,
    /// or the two grants ADR-0019 §3 requires separately collapse into one
    /// keystroke. That rule is enforced at the call site (`may_prompt`), because
    /// it is a rule about when to *ask*, and this type only records the answer.
    Prompt(bool),
}

impl Invocation {
    /// What this invocation said, discarding *how* it said it.
    fn answer(self) -> Option<bool> {
        match self {
            Self::Unset => None,
            Self::Flag(granted) | Self::Prompt(granted) => Some(granted),
        }
    }
}

/// Why the gate is open or shut. Every variant names a layer and, via
/// [`Reason::remedy`], says what would change it.
///
/// One enum rather than a bool because *"the remote tier is off"* is not an
/// answer anyone can act on, and because the reasons are not interchangeable:
/// [`Reason::ProjectDenied`] is a deliberate decision by the repository that no
/// flag can override, while [`Reason::InvocationUnset`] is one flag away.
///
/// The invocation denies in **two** ways, and they are separate variants rather
/// than one variant carrying a source. Three reasons, in increasing order of how
/// much they matter: the remedies genuinely differ (*"drop the flag"* against
/// *"answer yes next time"*); a new variant makes every exhaustive `match` in the
/// workspace fail to compile until it is considered, where a new payload on an
/// existing variant would silently widen every `matches!` already written; and
/// [`Reason::as_str`] is the stable token in `remote status --json`, so a new
/// variant adds a token while a payload would change the *shape* of one readers
/// already parse.
///
/// # Why this is `#[non_exhaustive]`, and what it cost to make it so
///
/// Adding [`Reason::PromptDeclined`] to a **published**, non-`#[non_exhaustive]`
/// enum is a breaking change: a downstream crate matching exhaustively stops
/// compiling. `rto-remote` was published at 1.19.0 hours before this, with nine
/// downloads and no consumer that could exist. So the attribute went on **at the
/// same time**, and the record of that is here rather than in a version number.
///
/// The nearest precedent is `rto_graph::StoreError`, and it went the other way.
/// `rto_graph::store`'s comment records the same trap and **designed around** it
/// (#342/#348) — a new `SchemaAhead` type rather than a variant — because by then
/// the enum had shipped without the attribute and a variant would have cost a
/// major bump of all seven crates. That was the right call *there*, where the
/// fact had a natural home outside the enum. It does not transfer here: the fact
/// that needed reporting **is which reason to report**, and this is by
/// construction the type that answers that. Moving it out would leave
/// `remote status --json` emitting `invocation_denied` for a prompt and
/// `call_with` still saying `--no-remote`, which is the defect the variant
/// exists to fix, reinstated one layer down.
///
/// The workspace convention is the attribute, not the workaround —
/// `rto_exec::ExecError`, `SubprocessError`, `AssetError`, `AssetSource` and
/// `rto_graph::NetworkPolicy` all carry it, and `AssetSource`'s own docs record
/// that *"the enum being `#[non_exhaustive]` is what made adding it a
/// non-breaking change"*. `StoreError` is the one that missed the convention and
/// paid for it. This one is not going to.
///
/// **The cost, stated rather than glossed.** Downstream `match`es now need a
/// wildcard arm and lose exhaustiveness checking — the trade `NetworkPolicy`
/// names explicitly ("match with equality rather than exhaustively"). That is
/// worth it for a set that has already grown once and will grow again if consent
/// ever acquires another layer or another form.
///
/// **What was deliberately *not* done.** The public structs in this crate —
/// [`Decision`], `Egress`, `Outcome`, `ContextItem`, `Answer`, `Check` — still
/// have public fields, so adding a field to any of them is breaking in the same
/// way. They were left alone because `LocalAttempt` and `Policy` are built *by*
/// callers and `#[non_exhaustive]` would make them unconstructible, so the sweep
/// is not uniform and a uniform-looking change would have been a lie. That
/// hazard is real and is recorded here rather than discovered later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reason {
    /// The user layer and the invocation both granted. The only granting reason.
    Granted,
    /// The project's `roteiro.toml` denied it. Outranks every other layer.
    ProjectDenied,
    /// This invocation denied it **with the flag** (`--no-remote`).
    ///
    /// Only ever produced by [`Invocation::Flag`]. A prompt answered *no* is
    /// [`Reason::PromptDeclined`], because telling someone they passed a flag
    /// they did not pass is worse than saying nothing.
    InvocationDenied,
    /// A person was shown the exact bytes at a prompt and said no.
    ///
    /// The most deliberate refusal in this enum — the only one made by someone
    /// who had already read what would leave — and the one whose message must not
    /// blame a flag.
    PromptDeclined,
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
            Self::PromptDeclined => "prompt_declined",
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
            Self::PromptDeclined => {
                "you were shown exactly what would be sent and answered no, so nothing was sent"
            }
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
            // Deliberately does not lead with the flag. Someone who has just read
            // the disclosure and declined is the last person who should be told,
            // first, how to skip being asked.
            Self::PromptDeclined => Some(
                "nothing is wrong — run it again and answer `y` if you change your mind. \
                 `--allow-remote` grants the run without the question, which is what a script \
                 needs; at a terminal, being asked is the point",
            ),
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

/// Decide whether this run may send, from **the flag form** of the invocation.
///
/// `invocation` is the flag: `Some(true)` for `--allow-remote`, `Some(false)`
/// for `--no-remote`, `None` for neither. Kept as it was because it is the
/// common form and every existing caller passes it; a run whose invocation came
/// from a **prompt** must use [`decide_with`], so that declining at a prompt is
/// not reported as a flag nobody passed.
///
/// A thin wrapper over [`decide_with`], not a second implementation — there is
/// one place that knows which layer outranks which, and this is not it.
#[must_use]
pub fn decide(config: ConfigGrant, invocation: Option<bool>) -> Decision {
    decide_with(
        config,
        invocation.map_or(Invocation::Unset, Invocation::Flag),
    )
}

/// Decide whether this run may send, given **how** the invocation answered.
///
/// # The order the layers are consulted, and why it is this order
///
/// Denials are reported before absences, and the project's before anyone's,
/// because the reasons carry different remedies and reporting the wrong one
/// wastes the reader's time. Someone who passed `--no-remote` does not need to be
/// told their user config is also unset; someone in a repository that denies the
/// tier must not be told to edit their user config, because it would not help.
///
/// The same argument is why the invocation's two denial forms are two reasons
/// and not one: *"drop the flag"* is the wrong advice for someone who declined a
/// prompt, and *"you passed `--no-remote`"* is not merely unhelpful but untrue.
#[must_use]
pub fn decide_with(config: ConfigGrant, invocation: Invocation) -> Decision {
    let reason = match (config.project_denied(), invocation, config.as_effective()) {
        (true, _, _) => Reason::ProjectDenied,
        (false, Invocation::Flag(false), _) => Reason::InvocationDenied,
        (false, Invocation::Prompt(false), _) => Reason::PromptDeclined,
        (false, _, Some(false)) => Reason::UserLayerDenied,
        (false, _, None) => Reason::UserLayerUnset,
        (false, Invocation::Unset, Some(true)) => Reason::InvocationUnset,
        (false, Invocation::Flag(true) | Invocation::Prompt(true), Some(true)) => Reason::Granted,
    };
    debug_assert_eq!(
        reason.granted(),
        invocation.answer() == Some(true) && config.as_effective() == Some(true),
        "a grant needs both halves, whichever form the invocation took"
    );
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

    /// **Declining at a prompt must not be reported as a flag nobody passed.**
    ///
    /// The failure this guards is not a wording slip: it tells someone they did
    /// something they did not do. A person who read the disclosure and answered
    /// *no* was being told they had passed `--no-remote` — and if they went
    /// looking for it in their shell history or their scripts, they would not
    /// find it. This is the consent path, so a message that misreports **how**
    /// consent was withheld undermines the thing it is reporting on.
    ///
    /// Asserted on the **rendered error text**, not on the variant: the variant
    /// being right is not the property that was broken, and a `Reason` that
    /// classified correctly while still printing the flag would be the same bug.
    #[test]
    fn declining_at_a_prompt_is_not_reported_as_a_flag_nobody_passed() {
        // The state a prompt is reachable from: the user layer granted, this run
        // had not — and then the person said no.
        let config = ConfigGrant::from_layers(None, Some(true));
        let declined = super::decide_with(config, super::Invocation::Prompt(false));
        assert!(!declined.granted());
        assert_eq!(declined.reason, Reason::PromptDeclined);

        // What the user actually sees, through the error `remote call` returns.
        let text = crate::RemoteError::NotConsented(declined.reason).to_string();
        assert!(
            !text.contains("--no-remote"),
            "it names a flag the person never typed: {text}"
        );
        assert!(
            text.contains("answered no"),
            "it has to say what they actually did: {text}"
        );
        assert!(
            text.contains("shown exactly what would be sent"),
            "…and that they saw the disclosure first, which is the point of asking: {text}"
        );
        assert!(
            text.contains("run it again and answer `y`"),
            "the remedy for a decline is not a flag: {text}"
        );

        // The flag form is unchanged and still names the flag, because there it
        // is true — the two must not be made interchangeable by fixing this.
        let by_flag = super::decide_with(config, super::Invocation::Flag(false));
        assert_eq!(by_flag.reason, Reason::InvocationDenied);
        let flag_text = crate::RemoteError::NotConsented(by_flag.reason).to_string();
        assert!(flag_text.contains("--no-remote"), "{flag_text}");
        assert_ne!(
            text, flag_text,
            "two different acts must not produce one message"
        );

        // A yes is a yes whichever form asked, and `decide` keeps meaning the
        // flag form for every caller that already passes an `Option<bool>`.
        assert!(super::decide_with(config, super::Invocation::Prompt(true)).granted());
        assert_eq!(decide(config, Some(false)).reason, Reason::InvocationDenied);
        assert_eq!(decide(config, None).reason, Reason::InvocationUnset);
        assert!(decide(config, Some(true)).granted());
    }

    /// **A prompt supplies the invocation and nothing else.** A *yes* at a prompt
    /// cannot reach past its own layer: a project denial still wins, and an unset
    /// user layer is still unset, because the human opting *themselves* in is a
    /// separate act from opting *this run* in (ADR-0019 §3).
    ///
    /// A *no* is a different matter, and the asymmetry is deliberate rather than
    /// an oversight: an explicit refusal is reported ahead of an absent grant,
    /// exactly as `--no-remote` is by
    /// `an_explicit_denial_is_reported_ahead_of_an_absent_grant`. Someone who has
    /// just declined a prompt should be told that is why nothing was sent, not
    /// sent to edit a config file they may already have set correctly. Denial has
    /// none of the problems of grant — which is the whole shape of this gate.
    #[test]
    fn a_prompt_answers_for_the_run_and_never_for_another_layer() {
        // A yes reaches nothing beyond its own layer.
        assert_eq!(
            super::decide_with(
                ConfigGrant::from_layers(Some(false), Some(true)),
                super::Invocation::Prompt(true)
            )
            .reason,
            Reason::ProjectDenied,
            "a yes at a prompt cannot overrule a repository"
        );
        assert_eq!(
            super::decide_with(
                ConfigGrant::from_layers(None, None),
                super::Invocation::Prompt(true)
            )
            .reason,
            Reason::UserLayerUnset,
            "…nor stand in for the user layer"
        );

        // A no is reported as itself, ahead of any absence — the flag's rule,
        // applied to the other form of the same layer.
        assert_eq!(
            super::decide_with(
                ConfigGrant::from_layers(None, None),
                super::Invocation::Prompt(false)
            )
            .reason,
            Reason::PromptDeclined,
            "a decline explains itself rather than blaming an unset user layer"
        );
        // …but never ahead of the project's, which no local act overrides.
        assert_eq!(
            super::decide_with(
                ConfigGrant::from_layers(Some(false), Some(true)),
                super::Invocation::Prompt(false)
            )
            .reason,
            Reason::ProjectDenied
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
        // Every variant, so a seventh cannot be added with an empty explanation
        // — `roteiro remote status` renders whatever this returns, for any
        // reason it is handed.
        let all = [
            Reason::Granted,
            Reason::ProjectDenied,
            Reason::InvocationDenied,
            Reason::PromptDeclined,
            Reason::UserLayerDenied,
            Reason::UserLayerUnset,
            Reason::InvocationUnset,
        ];
        for reason in all {
            assert!(!reason.explain().is_empty(), "{reason:?}");
            assert!(!reason.as_str().is_empty(), "{reason:?}");
            assert_eq!(
                reason.remedy().is_none(),
                reason.granted(),
                "{reason:?}: only a grant needs no remedy"
            );
        }
        // The `--json` tokens are the stable contract, so two reasons must never
        // share one: a reader parsing `prompt_declined` is being told something
        // `invocation_denied` does not say.
        let mut tokens: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        tokens.sort_unstable();
        let distinct = tokens.len();
        tokens.dedup();
        assert_eq!(
            tokens.len(),
            distinct,
            "two reasons share a token: {tokens:?}"
        );
        // The user-layer remedy has to name the *user* file, since naming the
        // project file would be advice that cannot work.
        let remedy = Reason::UserLayerUnset.remedy().expect("a remedy");
        assert!(remedy.contains("~/.roteiro/config.toml"), "{remedy}");
        assert!(remedy.contains("never grant"), "{remedy}");
    }
}
