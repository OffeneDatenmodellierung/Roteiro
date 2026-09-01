//! Who wrote the change under review, read from its commit trailers — so a
//! reviewer can say when it is reviewing its own output (issue #649, part 3).
//!
//! A model reviewing code it wrote is the weakest possible reviewer: it shares
//! the blind spot that produced the defect. The authorship is already recorded,
//! because commits made by an agent harness carry a trailer:
//!
//! ```text
//! Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
//! ```
//!
//! This module is the pure half — parse the trailers, decide whether one names
//! the model that is about to review. Reading a commit range and printing a
//! warning belong to the binary, where git and the engine already are.
//!
//! # Warn, never refuse
//!
//! Settled by the owner on 27 Aug 2026, and the reason matters for what is here:
//! a same-model review is weakened, not worthless, and refusing would trade it
//! for **no** review on exactly the machine most likely to have one model
//! installed. So nothing in this module returns an error or a veto — the widest
//! answer it gives is "these two names denote the same model", and the caller's
//! only move is to say so.
//!
//! A commit with no `Co-Authored-By` — a human wrote it — yields no trailer, so
//! no match, so no warning, and the review proceeds exactly as it did before.
//! Human-authored changes are never harder to review than machine-authored ones,
//! which was the thing to avoid.
//!
//! # Does the trailer name the model, or the harness that ran it?
//!
//! **The trailer is read as naming a model, and the comparison is model-to-model
//! only.** That is a decision about what can be *compared*, not a claim about
//! what harnesses write.
//!
//! The registry side is unambiguous: [`crate::ModelTask::Review`] resolves to a
//! model, never to a harness. So a model-to-harness comparison is a category
//! error whichever way the trailer happens to be written, and there is no rule
//! that could rescue it. The trailer side, by contrast, is free text with no
//! schema — a harness may write its product name (`Claude Code`, `Cursor`,
//! `Aider`) instead of the weights it ran, and nothing in the string says which
//! it did. Any rule that *decided* "this one is a harness" would be guessing at
//! the one thing the format does not record.
//!
//! So [`names_same_model`] attempts an identity match against the model name and
//! gives up silently when it fails. A trailer naming a harness normalises onto no
//! registry model, does not match, and produces nothing.
//!
//! ## Why a mismatch is the benign direction
//!
//! The two ways to be wrong are not symmetric.
//!
//! A **false negative** — a same-model review that goes unwarned — costs exactly
//! what today costs, because today there is no warning at all. It is a feature
//! that did not fire, not a regression.
//!
//! A **false positive** — warning that the reviewer wrote the change when it
//! demonstrably did not — is worse than it looks, because the alternative rule
//! that would "never miss" is *warn whenever any AI trailer is present*. On this
//! repository, whose commits are Claude-authored and whose local reviewer is a
//! Qwen GGUF, that rule fires on every single run while being false every single
//! time. A warning that is always on is a warning nobody reads, and it would take
//! the true ones down with it.
//!
//! Hence exact identity or silence. [`identity_tokens`] absorbs the spelling
//! differences that are certainly not identity differences — case, the separators
//! between a family and its size, and a parenthesised qualifier like
//! `(1M context)`, which describes how a model was *configured* rather than which
//! model it is — and nothing else.

/// One `Co-Authored-By` trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoAuthor {
    /// The display name, exactly as the trailer wrote it — `Claude Opus 5 (1M
    /// context)`. Kept verbatim so a warning can quote what it actually read
    /// rather than a normalised form the reader would not find in `git log`.
    pub name: String,
    /// The address inside the angle brackets, or the empty string when the
    /// trailer carried none.
    pub email: String,
}

/// Every `Co-Authored-By` trailer in one commit message, in the order it appears.
///
/// Deliberately not a full RFC-822-ish trailer parser: this reads the one key it
/// needs, case-insensitively, from any line of the message. Git's own trailer
/// rules require the block to be at the end and unbroken, and a message that
/// slightly violates them still records who wrote the code — which is the fact
/// wanted here, not a syntactic verdict about the commit.
///
/// Duplicates are kept. Two identical trailers are a fact about the message, and
/// the caller de-duplicates across a range where that is what it wants.
#[must_use]
pub fn co_authors(message: &str) -> Vec<CoAuthor> {
    const KEY: &str = "co-authored-by:";
    let mut out = Vec::new();
    for line in message.lines() {
        let line = line.trim();
        let Some(head) = line.get(..KEY.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(KEY) {
            continue;
        }
        let value = line[KEY.len()..].trim();
        // The address is the bracketed tail, if there is one. A trailer with no
        // `<...>` is still an authorship claim and is kept with an empty email
        // rather than dropped — the name is the half this module compares.
        let (name, email) = match value.rfind('<') {
            Some(at) => {
                let email = value[at + 1..].trim_end().trim_end_matches('>');
                (value[..at].trim(), email.trim())
            }
            None => (value, ""),
        };
        if name.is_empty() && email.is_empty() {
            continue;
        }
        out.push(CoAuthor {
            name: name.to_owned(),
            email: email.to_owned(),
        });
    }
    out
}

/// Normalise a model or trailer name to the tokens that identify it.
///
/// Lowercased, with `-`, `_`, `.` and `/` treated as spaces so `claude-opus-5`
/// and `Claude Opus 5` reach the same answer, and with any parenthesised span
/// dropped: `(1M context)` says how a model was *configured*, not which model it
/// is, and two runs of one model at two context sizes are the same weights with
/// the same blind spot.
///
/// Everything else is left alone. This absorbs spelling, never meaning — see the
/// module docs for why the rule stops here.
#[must_use]
pub fn identity_tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '(' | '[' => {
                depth += 1;
                continue;
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if depth > 0 {
            continue;
        }
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Whether a trailer's `name` denotes the same model as `model`.
///
/// Exact equality of the [`identity_tokens`] sequence, and nothing looser. A
/// trailer that names a harness, a person, or a different model returns `false`,
/// which the caller renders as silence — see the module docs for why that is the
/// direction to fail in.
#[must_use]
pub fn names_same_model(name: &str, model: &str) -> bool {
    let left = identity_tokens(name);
    // An empty token list matches nothing, including another empty one: two names
    // that normalise to nothing are not evidence that they are the same model.
    !left.is_empty() && left == identity_tokens(model)
}

/// How much of a commit range the reviewing model wrote — see
/// [`reviewers_own_work`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnWork {
    /// How many of the messages carry at least one matching trailer.
    ///
    /// Counted separately from [`OwnWork::names`] because the two answer
    /// different questions and are easy to conflate into a sentence that is
    /// quietly false: one model spelled two ways across nine commits is two names
    /// and nine commits, and a warning that says "2 commits" would be wrong about
    /// the only number a reader would act on.
    pub commits: usize,
    /// The matching trailer names as written, de-duplicated, first-seen order —
    /// so a warning quotes what `git log` would show rather than a normalised
    /// form the reader could not search for.
    pub names: Vec<String>,
}

impl OwnWork {
    /// Whether the reviewing model wrote any of the range.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commits == 0
    }
}

/// Which commits in a range `model` co-authored, by its trailers.
///
/// A default [`OwnWork`] means no commit in the range was written by this model,
/// which is the ordinary case and is never an error: see the module docs for why
/// silence is the direction this fails in.
#[must_use]
pub fn reviewers_own_work(messages: &[String], model: &str) -> OwnWork {
    let mut out = OwnWork::default();
    for message in messages {
        let mut matched = false;
        for author in co_authors(message) {
            if !names_same_model(&author.name, model) {
                continue;
            }
            matched = true;
            if !out.names.contains(&author.name) {
                out.names.push(author.name);
            }
        }
        out.commits += usize::from(matched);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{co_authors, identity_tokens, names_same_model, reviewers_own_work};

    #[test]
    fn a_trailer_is_read_from_a_real_commit_message() {
        let message = "feat(review): do the thing\n\
                       \n\
                       A body paragraph.\n\
                       \n\
                       Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>\n\
                       Claude-Session: https://example.invalid/s\n";
        let authors = co_authors(message);
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].name, "Claude Opus 5 (1M context)");
        assert_eq!(authors[0].email, "noreply@anthropic.com");
    }

    #[test]
    fn the_key_is_matched_case_insensitively_and_a_missing_address_is_kept() {
        let authors = co_authors("x\n\nco-authored-by: qwen3-8b\nCO-AUTHORED-BY: A B <a@b>\n");
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0].name, "qwen3-8b");
        assert_eq!(
            authors[0].email, "",
            "a trailer with no address is still an authorship claim"
        );
        assert_eq!(authors[1].email, "a@b");
    }

    #[test]
    fn a_human_commit_carries_no_trailer_and_therefore_no_match() {
        // The case that must never become a reason to refuse: a human-authored
        // commit yields nothing, so nothing is warned about and the review
        // proceeds exactly as it did before.
        let message = "fix: correct the off-by-one\n\nNoticed while reading.\n";
        assert!(co_authors(message).is_empty());
        assert!(
            reviewers_own_work(&[message.to_owned()], "qwen3-8b").is_empty(),
            "a human-authored commit must never be harder to review"
        );
    }

    #[test]
    fn spelling_differences_that_are_not_identity_differences_are_absorbed() {
        assert_eq!(
            identity_tokens("Claude Opus 5 (1M context)"),
            ["claude", "opus", "5"]
        );
        assert_eq!(identity_tokens("claude-opus-5"), ["claude", "opus", "5"]);
        assert_eq!(identity_tokens("qwen3.8-27b"), ["qwen3", "8", "27b"]);
        assert!(names_same_model(
            "Claude Opus 5 (1M context)",
            "claude-opus-5"
        ));
        assert!(
            names_same_model("QWEN3_8B", "qwen3-8b"),
            "case and separator are spelling, not identity"
        );
    }

    /// The context qualifier is dropped **because it is a configuration, not an
    /// identity**: the same weights at two window sizes carry the same blind spot,
    /// which is the whole reason the warning exists.
    #[test]
    fn a_context_qualifier_does_not_make_it_a_different_model() {
        assert!(names_same_model(
            "Claude Opus 5 (1M context)",
            "Claude Opus 5"
        ));
        assert!(names_same_model(
            "Claude Opus 5 (200K context)",
            "claude opus 5"
        ));
    }

    /// A harness name normalises onto no model name, so it does not match, so
    /// nothing is printed. This is the mismatch case being benign by
    /// construction rather than by intention.
    #[test]
    fn a_harness_name_matches_no_model_and_is_therefore_silent() {
        for harness in ["Claude Code", "Cursor", "Aider", "GitHub Copilot"] {
            assert!(
                !names_same_model(harness, "claude-opus-5"),
                "{harness} names a harness, and a harness is not a model"
            );
            assert!(!names_same_model(harness, "qwen3-8b"));
        }
    }

    /// The realistic negative on this repository, asserted so the feature's
    /// *silence* here is a measured fact rather than an assumption: commits are
    /// Claude-authored and the local reviewer is a Qwen GGUF, so nothing fires.
    #[test]
    fn a_different_model_is_never_reported_as_the_same_one() {
        let message = "x\n\nCo-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>\n";
        assert!(reviewers_own_work(&[message.to_owned()], "qwen3-8b").is_empty());
        assert!(reviewers_own_work(&[message.to_owned()], "qwen3.8-27b").is_empty());
    }

    /// The two counts are separate because a sentence that conflates them is
    /// quietly false: one model spelled two ways over three commits is **two**
    /// names and **three** commits, and the commit count is the number a reader
    /// would act on.
    #[test]
    fn distinct_names_and_matching_commits_are_counted_separately() {
        let one = "a\n\nCo-Authored-By: Qwen3 8B <x@y>\n".to_owned();
        let two = "b\n\nCo-Authored-By: Qwen3 8B <x@y>\n".to_owned();
        let three = "c\n\nCo-Authored-By: qwen3-8b <x@y>\n".to_owned();
        let human = "d\n\nnobody else\n".to_owned();
        let hit = reviewers_own_work(&[one, two, three, human], "qwen3-8b");
        assert_eq!(
            hit.names,
            vec!["Qwen3 8B".to_owned(), "qwen3-8b".to_owned()],
            "quoted as written, de-duplicated, first-seen order"
        );
        assert_eq!(
            hit.commits, 3,
            "three commits matched; the fourth is human-authored"
        );
    }

    /// One commit naming the model twice is still one commit.
    #[test]
    fn a_commit_is_counted_once_however_many_trailers_it_carries() {
        let message = "a\n\nCo-Authored-By: Qwen3 8B <x@y>\nCo-Authored-By: qwen3-8b <x@y>\n";
        let hit = reviewers_own_work(&[message.to_owned()], "qwen3-8b");
        assert_eq!(hit.commits, 1);
        assert_eq!(hit.names.len(), 2);
    }

    #[test]
    fn a_name_that_normalises_to_nothing_matches_nothing() {
        assert!(
            !names_same_model("", ""),
            "two names that say nothing are not evidence of the same model"
        );
        assert!(!names_same_model("(1M context)", ""));
    }
}
