//! The recorded answer to "may this peer's bundle be read into our graph?"
//! (issue #706 phase 2, ADR-0021).
//!
//! # This is a consent question, not a convenience one
//!
//! Automatic discovery finds a workspace member's OKF bundle without anyone
//! asking for it. Reading it puts a **foreign repository's prose into our
//! graph**, where [`crate::query`] returns it to a language model as grounding
//! (see [`crate::screen`] for that path in full). Doing that because a directory
//! appeared is consent-by-installation, which is exactly what the prompt exists
//! to prevent.
//!
//! So the posture is ADR-0019's: the gate is a policy, decided by a person, and
//! never inferred. What is reused is the *shape* — a decision type that is
//! computed rather than guessed at, a hard refusal when there is nobody to ask,
//! and a record of what happened — not the code, which is about network egress.
//!
//! # Where this departs from ADR-0019, and why it has to
//!
//! ADR-0019 persists **nothing**: "a remote grant [does not survive] the
//! process, [is not] persisted anywhere, or [inferred] from a previous session".
//! Issue #706 settles the opposite for this question — "record the answer against
//! that source so it is asked once, not per sync" — and the two are not in
//! conflict, because the questions differ in how often they recur.
//!
//! A remote call happens on demand and is over when it returns; asking each time
//! costs one prompt per deliberate act. A workspace scan runs on every `sync`,
//! every `links`, every server start. Asking each time would be a prompt nobody
//! reads by the third day, and a habituated `y` is a worse gate than a recorded
//! answer — ADR-0019 §3 makes that argument itself when it refuses to prompt on
//! the default path.
//!
//! The price of persisting is that a stored grant is one nobody re-reads, and
//! [`ConsentState`] is where that price is paid: a grant is not unconditional,
//! and [`Store::okf_consent_holds`](crate::Store::okf_consent_holds) says what
//! makes it lapse.

use serde::{Deserialize, Serialize};

/// The answer to the trust question, as settled by issue #706.
///
/// Deliberately **not `#[non_exhaustive]`**. These are the three answers to a
/// consent question — adopt their confirmations, take their information without
/// them, or decline — and the set is closed by that question rather than by
/// implementation convenience. A fourth would mean the question had changed, and
/// a caller's `match` should be made to stop compiling rather than fold it into
/// a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OkfDecision {
    /// Import at `external-<the peer's tier>`, preserving what they claimed.
    Trust,
    /// Import at `external-inferred` **regardless** of the peer's claimed tier:
    /// their information without their confirmation.
    Acknowledge,
    /// Leave the `extref:` placeholder exactly as it is. Not a mode of
    /// importing — the decision not to.
    Ignore,
}

impl OkfDecision {
    /// The stable token stored in the record and printed in reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trust => "trust",
            Self::Acknowledge => "acknowledge",
            Self::Ignore => "ignore",
        }
    }

    /// Parse a stored or typed token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "trust" => Some(Self::Trust),
            "acknowledge" => Some(Self::Acknowledge),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }

    /// Whether this answer imports anything at all.
    #[must_use]
    pub fn imports(self) -> bool {
        matches!(self, Self::Trust | Self::Acknowledge)
    }
}

/// One recorded answer, for one peer, in one consuming graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkfConsent {
    /// The peer name — the same string that namespaces the import layer's
    /// `src_ref` (`import:okf/<peer>`), so a grant and the layer it authorises
    /// share a key.
    pub peer: String,
    /// What was answered.
    pub decision: OkfDecision,
    /// The bundle directory the answer was given about.
    pub root: String,
    /// The screening finding classes the bundle produced **at the moment the
    /// question was answered**, sorted and comma-joined — see
    /// [`screen_fingerprint`].
    pub screen_classes: String,
    /// When it was answered, RFC 3339 UTC.
    pub decided_at: String,
}

/// Whether a recorded answer still covers what is on disk now, and why not.
///
/// Deliberately **not `#[non_exhaustive]`**: an answer either still applies or it
/// lapsed for one of exactly two reasons, and both are things a caller must
/// render differently to the person being asked again. A third reason would be a
/// change to what a grant means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentState {
    /// No answer has ever been recorded for this peer.
    Unasked,
    /// The recorded answer stands.
    Holds(OkfDecision),
    /// The peer name now resolves to a different directory. That is a different
    /// source wearing a familiar name, and an answer about the old one says
    /// nothing about it.
    Moved {
        /// The directory the answer was given about.
        was: String,
    },
    /// The bundle now screens worse than it did when the question was answered:
    /// it carries a class of finding the person answering was never shown.
    ///
    /// # Why *this* is the invalidator, and not a content hash
    ///
    /// A digest over the bundle's bytes would lapse on every commit the peer
    /// makes, which would turn "asked once" back into "asked every sync" and
    /// re-create the habituation the record exists to avoid. And it would be the
    /// wrong question: the answer was about a **source**, not about a specific
    /// paragraph, and trusting a peer means accepting that they will edit.
    ///
    /// What the answer was *not* about is a bundle that has since started
    /// carrying hidden control characters or text addressed to a model. The
    /// screening summary is the part of "what you were shown" that changes
    /// meaning, so it is the part that is fingerprinted.
    ///
    /// The limitation this leaves is real and is stated rather than hidden: a
    /// peer can change their prose to say something new, in plain visible
    /// language that trips no pattern, and the grant will stand. It is a grant
    /// over a source. `roteiro import --from okf --peer <name>` re-answers it at
    /// any time.
    Lapsed {
        /// What the bundle screened as when the answer was given.
        was: String,
        /// What it screens as now.
        now: String,
    },
}

impl ConsentState {
    /// The decision to act on, or `None` when somebody has to be asked.
    #[must_use]
    pub fn decision(&self) -> Option<OkfDecision> {
        match self {
            Self::Holds(d) => Some(*d),
            _ => None,
        }
    }

    /// A short sentence for the prompt or the note, explaining why this peer is
    /// being raised. `None` when the answer holds and nothing need be said.
    #[must_use]
    pub fn why_asking(&self) -> Option<String> {
        match self {
            Self::Holds(_) => None,
            Self::Unasked => Some("not seen before".to_owned()),
            Self::Moved { was } => Some(format!("the bundle moved (it was at {was})")),
            Self::Lapsed { was, now } => Some(format!(
                "the bundle now screens differently (it was [{was}], it is now [{now}])"
            )),
        }
    }
}

/// The fingerprint stored in [`OkfConsent::screen_classes`]: the sorted, unique
/// screening finding tokens across every concept in a bundle, comma-joined.
///
/// An empty string means the bundle screened clean, which is the common case and
/// the one a later finding must be able to invalidate.
#[must_use]
pub fn screen_fingerprint(classes: &[&str]) -> String {
    let mut sorted: Vec<&str> = classes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.join(",")
}

/// Whether `now` introduces a finding class that `was` did not carry.
///
/// Monotone on purpose: a bundle that has been **cleaned up** does not re-ask.
/// Re-prompting because a peer removed their stray HTML comment would be pure
/// noise, and the answer that was given covered a worse bundle than the one now
/// on disk.
#[must_use]
pub fn screen_regressed(was: &str, now: &str) -> bool {
    let before: Vec<&str> = was.split(',').filter(|s| !s.is_empty()).collect();
    now.split(',')
        .filter(|s| !s.is_empty())
        .any(|class| !before.contains(&class))
}

#[cfg(test)]
mod tests {
    use super::{OkfDecision, screen_fingerprint, screen_regressed};

    #[test]
    fn tokens_round_trip_and_are_a_closed_set() {
        for d in [
            OkfDecision::Trust,
            OkfDecision::Acknowledge,
            OkfDecision::Ignore,
        ] {
            assert_eq!(OkfDecision::from_token(d.as_str()), Some(d));
        }
        assert_eq!(OkfDecision::from_token("external-inferred"), None);
    }

    #[test]
    fn only_ignore_declines_to_import() {
        assert!(OkfDecision::Trust.imports());
        assert!(OkfDecision::Acknowledge.imports());
        assert!(!OkfDecision::Ignore.imports());
    }

    #[test]
    fn the_fingerprint_is_sorted_and_deduplicated() {
        assert_eq!(
            screen_fingerprint(&["model-directive", "invisible-characters", "model-directive"]),
            "invisible-characters,model-directive"
        );
        assert_eq!(screen_fingerprint(&[]), "");
    }

    #[test]
    fn a_new_finding_class_regresses_and_a_removed_one_does_not() {
        assert!(screen_regressed("", "invisible-characters"));
        assert!(screen_regressed(
            "hidden-presentation",
            "hidden-presentation,model-directive"
        ));
        // Cleaned up: no re-prompt.
        assert!(!screen_regressed("hidden-presentation", ""));
        assert!(!screen_regressed(
            "hidden-presentation,model-directive",
            "model-directive"
        ));
        // Unchanged.
        assert!(!screen_regressed("model-directive", "model-directive"));
    }
}
