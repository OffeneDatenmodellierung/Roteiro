//! Whether a *local* attempt fell short — measured afterwards, never predicted.
//!
//! # This is the module that is deliberately not a router
//!
//! "Route to a frontier model when the local one can't help" sounds like a
//! routing rule and is a consent boundary wearing one. ADR-0019 §1 rejects it at
//! any model quality, because the cost asymmetry is not symmetric: mis-routing
//! among local models wastes tokens, and mis-routing *outward* sends source off
//! the machine for a reason nobody can inspect afterwards. Consent must not be
//! probabilistic.
//!
//! What survives is the deterministic half: **a check of the local result, after
//! the local attempt, with the measured value recorded.**
//!
//! ```text
//!   local attempt ──▶ measured result ──▶ check ──▶ (maybe) offer to escalate
//!                                                    ──▶ the consent gate
//! ```
//!
//! Note the last arrow. A trigger here **does not** send anything: it is an input
//! to a gate that still requires the user layer and the invocation. This module
//! can only ever say *"the local answer was empty"*; it can never say *"so send
//! it"*.
//!
//! # The type makes the ordering structural
//!
//! [`LocalAttempt`] holds nothing but measurements of a completed run —
//! character counts, tool-call counts, round counts. It carries no prompt, no
//! question, no embedding, no feature vector, and no field describing the
//! *request*. **It therefore cannot be constructed before the local attempt has
//! happened**, which is a stronger guarantee than a comment saying so.

use serde::{Deserialize, Serialize};

/// The measured outcome of a completed local attempt.
///
/// Every field is a count taken after the fact. See the module docs for why
/// there is nothing here describing the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAttempt {
    /// Characters of usable output the local model produced.
    pub output_chars: usize,
    /// How many tool calls it made.
    pub tool_calls: usize,
    /// How many rounds the agent loop ran.
    pub rounds: u32,
    /// The loop's `MAX_ROUNDS` for this run.
    pub max_rounds: u32,
}

/// The one tunable: how short an answer counts as no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Below this many characters, the local answer is treated as having failed.
    /// `0` disables the floor, leaving only the empty-output and no-tool-call
    /// triggers.
    pub length_floor: usize,
}

impl Default for Policy {
    /// A conservative floor. Short answers are often correct, and this check
    /// exists to catch a local model that produced *nothing usable*, not one
    /// that was brief.
    fn default() -> Self {
        Self { length_floor: 16 }
    }
}

/// What fell short. Each is a named, observable property of the finished local
/// attempt — the three ADR-0019 §1 lists, and no fourth.
///
/// **Deliberately not `#[non_exhaustive]`**, unlike most of this crate's public
/// enums (see [`crate::Reason`] for that decision and its cost). The set is
/// closed by the ADR, which enumerates these three; a fourth would be a change to
/// what escalation *means*, not an addition to a list, and it should break a
/// downstream `match` so that whoever wrote it re-reads §1. Exhaustiveness is the
/// right default where the set is closed by a decision rather than by today's
/// implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The local model produced no output at all.
    EmptyOutput,
    /// The agent loop exhausted `MAX_ROUNDS` without ever calling a tool.
    NoToolCallAfterMaxRounds,
    /// Output was shorter than [`Policy::length_floor`].
    BelowLengthFloor,
}

impl Trigger {
    /// Stable token for `--json` output and for the record.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmptyOutput => "empty_output",
            Self::NoToolCallAfterMaxRounds => "no_tool_call_after_max_rounds",
            Self::BelowLengthFloor => "below_length_floor",
        }
    }
}

/// The check's answer, carrying the numbers it was reached from.
///
/// The measurements travel with the verdict rather than being recomputed by a
/// caller, because ADR-0019 requires the *measured value* to be recorded: a
/// record saying "escalated: output too short" and not saying *how* short is a
/// record of an opinion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// What fell short, or `None` when the local attempt stands.
    pub trigger: Option<Trigger>,
    /// Characters of output measured.
    pub output_chars: usize,
    /// Tool calls measured.
    pub tool_calls: usize,
    /// Rounds run.
    pub rounds: u32,
    /// The loop's limit for this run.
    pub max_rounds: u32,
    /// The floor applied.
    pub length_floor: usize,
}

impl Check {
    /// Whether the local attempt fell short. **Not** whether anything may be
    /// sent — that is [`crate::consent::decide`], and it is a different question
    /// with different inputs.
    #[must_use]
    pub fn fell_short(&self) -> bool {
        self.trigger.is_some()
    }

    /// The sentence recorded, and shown to whoever is being asked to consent.
    /// Names the measured number, so the reason can be checked rather than
    /// trusted.
    #[must_use]
    pub fn because(&self) -> String {
        match self.trigger {
            None => format!("the local answer stands ({} characters)", self.output_chars),
            Some(Trigger::EmptyOutput) => "the local model produced no output".to_owned(),
            Some(Trigger::NoToolCallAfterMaxRounds) => format!(
                "the local model made no tool call in {} of {} rounds",
                self.rounds, self.max_rounds
            ),
            Some(Trigger::BelowLengthFloor) => format!(
                "the local answer was {} characters, below the {}-character floor",
                self.output_chars, self.length_floor
            ),
        }
    }
}

/// Check a finished local attempt against `policy`.
///
/// Triggers are tested most-specific first, so the recorded reason is the most
/// informative true one: an empty answer is reported as empty rather than as
/// "below the floor", which it also technically is.
#[must_use]
pub fn check(attempt: LocalAttempt, policy: Policy) -> Check {
    let trigger = if attempt.output_chars == 0 {
        Some(Trigger::EmptyOutput)
    } else if attempt.tool_calls == 0
        && attempt.rounds >= attempt.max_rounds
        && attempt.max_rounds > 0
    {
        Some(Trigger::NoToolCallAfterMaxRounds)
    } else if attempt.output_chars < policy.length_floor {
        Some(Trigger::BelowLengthFloor)
    } else {
        None
    };
    Check {
        trigger,
        output_chars: attempt.output_chars,
        tool_calls: attempt.tool_calls,
        rounds: attempt.rounds,
        max_rounds: attempt.max_rounds,
        length_floor: policy.length_floor,
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, LocalAttempt, Policy, Trigger, check};

    fn attempt(output_chars: usize, tool_calls: usize, rounds: u32) -> LocalAttempt {
        LocalAttempt {
            output_chars,
            tool_calls,
            rounds,
            max_rounds: 4,
        }
    }

    /// A local attempt that answered is left alone. The check exists to catch
    /// *nothing usable*, not brevity, so a short-but-real answer above the floor
    /// must not escalate.
    #[test]
    fn a_local_answer_that_stands_does_not_escalate() {
        let verdict = check(attempt(120, 2, 2), Policy::default());
        assert!(!verdict.fell_short());
        assert_eq!(verdict.trigger, None);
        assert!(verdict.because().contains("120"), "{}", verdict.because());
    }

    /// Each of ADR-0019 §1's three named conditions triggers, and each records
    /// the number it was reached from — a reason without its measurement is an
    /// opinion.
    #[test]
    fn each_named_condition_triggers_and_records_its_measurement() {
        let empty = check(attempt(0, 0, 1), Policy::default());
        assert_eq!(empty.trigger, Some(Trigger::EmptyOutput));

        let stalled = check(attempt(50, 0, 4), Policy::default());
        assert_eq!(stalled.trigger, Some(Trigger::NoToolCallAfterMaxRounds));
        let why = stalled.because();
        assert!(why.contains("4 of 4"), "records the rounds measured: {why}");

        let short = check(attempt(5, 1, 1), Policy::default());
        assert_eq!(short.trigger, Some(Trigger::BelowLengthFloor));
        let why = short.because();
        assert!(why.contains('5') && why.contains("16"), "{why}");
    }

    /// The most informative true reason wins. An empty answer is also below the
    /// floor, and recording it as "below the floor" would send a reader looking
    /// for a threshold to tune when the model returned nothing at all.
    #[test]
    fn the_most_specific_true_reason_is_the_one_recorded() {
        let verdict = check(attempt(0, 0, 4), Policy::default());
        assert_eq!(
            verdict.trigger,
            Some(Trigger::EmptyOutput),
            "empty beats both the stall and the floor"
        );
        assert_eq!(
            check(attempt(5, 0, 4), Policy::default()).trigger,
            Some(Trigger::NoToolCallAfterMaxRounds),
            "a stall beats the floor"
        );
    }

    /// A loop that has not run out of rounds has not stalled, and a floor of
    /// zero disables the floor — so neither trigger fires on a run that simply
    /// finished early with a brief answer.
    #[test]
    fn a_loop_with_rounds_left_and_a_disabled_floor_does_not_trigger() {
        assert_eq!(check(attempt(50, 0, 2), Policy::default()).trigger, None);
        assert_eq!(
            check(attempt(1, 1, 1), Policy { length_floor: 0 }).trigger,
            None
        );
        // `max_rounds: 0` is not "every run has stalled" — it is a loop with no
        // configured limit, and reading it the other way would escalate every
        // run that made no tool call.
        let unlimited = LocalAttempt {
            output_chars: 50,
            tool_calls: 0,
            rounds: 0,
            max_rounds: 0,
        };
        assert_eq!(check(unlimited, Policy::default()).trigger, None);
    }

    /// **The structural argument, asserted.** A `Check` is a function of the
    /// finished attempt and nothing else: the same measurements always give the
    /// same verdict, and there is no field on either type describing the request.
    /// That is what makes this deterministic and recorded rather than predicted.
    #[test]
    fn the_verdict_is_a_function_of_the_measured_attempt_alone() {
        let measured = attempt(3, 1, 1);
        let first = check(measured, Policy::default());
        let second = check(measured, Policy::default());
        assert_eq!(first, second);
        // Serializing the verdict is how it reaches the record; every number
        // that produced it survives the trip.
        let json = serde_json::to_string(&first).expect("serialize");
        let back: Check = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, first);
        assert!(json.contains("\"output_chars\":3"), "{json}");
        assert!(json.contains("\"length_floor\":16"), "{json}");
    }
}
