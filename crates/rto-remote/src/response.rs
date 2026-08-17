//! What came back — read here, where there is no socket to read it from.
//!
//! # Why the parsing lives in the crate with no transport
//!
//! [`crate::call_with`] hands back the transport's bytes verbatim and interprets
//! nothing, so the whole of "was that an answer?" is a pure function of a
//! `&str`. Putting it here rather than beside the socket has two consequences
//! worth stating:
//!
//! 1. **Every response failure is testable with no network at all.** A truncated
//!    body, a malformed body, a body the endpoint filled with its own error —
//!    all of them are string literals in the tests below. The rule part 1 set for
//!    the send path holds for the receive path: a test cannot accidentally become
//!    the first thing that talks to a server.
//! 2. **The transport cannot decide what counts as an answer.** It reports
//!    whether bytes arrived; this decides whether they mean anything. Those are
//!    different questions, and a transport that answered both could quietly
//!    return `Ok("")` for a call that failed.
//!
//! # Nothing here degrades
//!
//! ADR-0019 §6 keeps Principle 10's second half and makes it bind harder: the
//! failure mode this capability most needs to prevent is *a different answer with
//! no signal that anything changed*. A generation that stopped at a token limit
//! **is** a different answer, so [`parse`] refuses it
//! ([`ResponseError::Incomplete`]) rather than handing over a sentence that stops
//! mid-clause and looks finished. So is an empty completion, and so is a body
//! that is not the shape it claims to be. There is no lenient mode and no
//! salvage path: the caller consented to a remote answer, and half of one is not
//! it.
//!
//! # Completeness is established positively, and silence does not establish it
//!
//! A response that carries **no** `finish_reason` is refused too
//! ([`ResponseError::Indeterminate`]). This is the rule #367 already applied to
//! the asset fetch — *"a length that cannot be established is not a length that
//! checks out"*, so a close-delimited body is refused rather than digested and
//! pinned — pointed at the other end of the same wire. Reading an absent field as
//! *"it must have finished"* would be strictly weaker than the `length` case this
//! module already refuses: `length` at least tells you something.
//!
//! Nothing this tier can address legitimately omits it. `rto-serve`'s own
//! `ChatChoice::finish_reason` is a non-optional `&'static str`, always `stop` or
//! `length` — and that is the endpoint the loopback-gateway case exists for. The
//! one place the field is legitimately absent, or `null`, is a **streaming
//! delta**, and [`Payload::body`](crate::Payload::body) pins `"stream": false`,
//! so this tier structurally never asks for one. A `null` arriving here therefore
//! means the endpoint sent a streaming chunk to a non-streaming request, which is
//! the least complete thing a body can be.

use serde::{Deserialize, Serialize};

/// The most of a response body quoted back in an error.
///
/// Bounded because the honest thing to show someone whose gateway answered with
/// an HTML login page is the first line of it, not the page.
const EXCERPT: usize = 200;

/// `finish_reason` values that mean the generation ran to its own end.
///
/// A short list on purpose, and an **allow-list**: a response is complete only
/// if it says one of these. Anything outside it — `length`, `content_filter`, a
/// vendor's own token — is treated as an *incomplete* answer, because the
/// alternative is deciding on a vendor's behalf that their new stop reason was
/// benign; and saying nothing at all is [`ResponseError::Indeterminate`], because
/// an allow-list that silence passes is not one.
const COMPLETE: [&str; 4] = ["stop", "end_turn", "eos", "complete"];

/// A remote answer that arrived whole.
///
/// Constructed only by [`parse`], so an `Answer` in hand is one that has already
/// been checked: non-empty, and produced by a generation that finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The completion text, trimmed of surrounding whitespace.
    pub text: String,
    /// The model string the *endpoint* said produced it, when it said.
    ///
    /// Kept because ADR-0019 §5 turns on a vendor model string being a **mutable
    /// pointer**: the one thing this machine can check is whether the name that
    /// answered is the name that was asked for. It cannot check the weights, and
    /// [`ProducerTrust::VendorAsserted`](crate::ProducerTrust::VendorAsserted)
    /// says so — but a name that changed under the request is a discrepancy
    /// worth reporting rather than discarding.
    pub model: Option<String>,
}

impl Answer {
    /// Whether the endpoint named the model that was asked for.
    ///
    /// `true` when the endpoint named no model at all: silence is not a
    /// discrepancy, and reporting one would train a reader to ignore the notice.
    #[must_use]
    pub fn model_matches(&self, requested: &str) -> bool {
        self.model
            .as_ref()
            .is_none_or(|answered| answered == requested)
    }

    /// The sentence to print when the endpoint answered as a different model
    /// than the one requested, or `None` when it did not.
    #[must_use]
    pub fn model_discrepancy(&self, requested: &str) -> Option<String> {
        let answered = self.model.as_ref().filter(|m| *m != requested)?;
        Some(format!(
            "note: this run asked for `{requested}` and the endpoint answered as `{answered}`. \
             A hosted model's identity is a claim (vendor-asserted), so Roteiro cannot tell \
             which weights produced this — but it can tell you the name changed"
        ))
    }
}

/// Why a response did not yield an answer.
///
/// Every variant is a refusal. None of them is recoverable by using part of what
/// arrived, and none of them falls back to a local model — see the module docs.
/// Marked `#[non_exhaustive]` for the reason recorded on
/// [`crate::Reason`]: this crate is published at 1.x, and error sets grow.
/// Taken while the crate had no consumer that could exist; it will not be
/// taken again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResponseError {
    /// The body is not the shape a chat completion has.
    #[error(
        "the endpoint's response could not be read as a chat completion ({detail}). \
         What arrived began: {excerpt:?}"
    )]
    Malformed {
        /// What went wrong, in one clause.
        detail: String,
        /// The first [`EXCERPT`] characters of the body.
        excerpt: String,
    },
    /// The endpoint reported an error in its own body.
    #[error("the endpoint refused this request: {message}")]
    EndpointReported {
        /// The endpoint's own words.
        message: String,
    },
    /// A well-formed response carrying no completion.
    #[error(
        "the endpoint returned a well-formed response with no completion in it — \
         nothing was generated, so there is no answer to give you"
    )]
    NoChoices,
    /// The generation stopped for a reason other than reaching its own end.
    #[error(
        "the endpoint stopped generating after {chars} character(s) with \
         `finish_reason: {finish_reason}` — the answer is incomplete, and Roteiro will not \
         hand you a truncated answer as though it were a whole one. Nothing was retried and \
         no local model was substituted"
    )]
    Incomplete {
        /// The reason the endpoint gave.
        finish_reason: String,
        /// How much text arrived before it stopped.
        chars: usize,
    },
    /// The response never says whether the generation finished.
    ///
    /// Separate from [`ResponseError::Incomplete`] because the facts differ: that
    /// one is an endpoint reporting that it stopped early, this one is an
    /// endpoint reporting nothing at all. The consequence is the same, and it is
    /// the consequence that matters — completeness that cannot be established is
    /// not completeness.
    #[error(
        "the endpoint's response never says whether the generation finished — its first \
         choice carries no `finish_reason` string, so the {chars} character(s) that arrived \
         cannot be told apart from an answer cut short. Refusing rather than presenting \
         bytes of unknown completeness as a whole answer; no local model was substituted. \
         A `finish_reason` of `null` means the same thing and is refused the same way: it is \
         what a *streaming* chunk carries, and this tier always asks for `stream: false`"
    )]
    Indeterminate {
        /// How much text arrived, whole or not.
        chars: usize,
    },
    /// A completed generation that produced nothing.
    #[error(
        "the endpoint finished generating and produced an empty answer. \
         Roteiro did **not** substitute a local model's answer for it"
    )]
    Empty,
}

/// Read a chat-completion response body.
///
/// The mirror of [`Payload::body`](crate::Payload::body) — that function decides
/// the exact bytes that leave, this one decides what the bytes that come back
/// are worth — and like it, a pure function of its argument.
///
/// # Errors
/// See [`ResponseError`]. The checks run most-specific first, so the reported
/// reason is the most informative true one: an endpoint that filled the body
/// with its own error message is reported as having refused, not as having sent
/// something malformed.
pub fn parse(raw: &str) -> Result<Answer, ResponseError> {
    let malformed = |detail: &str| ResponseError::Malformed {
        detail: detail.to_owned(),
        excerpt: excerpt_of(raw),
    };

    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| malformed(&format!("it is not JSON: {e}")))?;

    // An endpoint's own error first: a gateway that answers `200` with
    // `{"error": …}` has told us exactly what went wrong, and reporting that as
    // "malformed" would throw away the only useful sentence in the body.
    if let Some(message) = endpoint_error(&value) {
        return Err(ResponseError::EndpointReported { message });
    }

    let choice = value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| malformed("it has no `choices` array"))?
        .first()
        .ok_or(ResponseError::NoChoices)?;
    let text = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("its first choice has no `message.content` string"))?
        .trim();

    // Completeness is established before emptiness, because "it stopped at the
    // token limit" explains an empty answer where "it was empty" leaves a reader
    // looking for the wrong cause. And it is established *positively*: silence
    // does not count as a yes.
    match choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str)
    {
        Some(reason) if COMPLETE.contains(&reason.to_ascii_lowercase().as_str()) => {}
        Some(reason) => {
            return Err(ResponseError::Incomplete {
                finish_reason: reason.to_owned(),
                chars: text.chars().count(),
            });
        }
        None => {
            return Err(ResponseError::Indeterminate {
                chars: text.chars().count(),
            });
        }
    }
    if text.is_empty() {
        return Err(ResponseError::Empty);
    }

    Ok(Answer {
        text: text.to_owned(),
        model: value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// The endpoint's own error message, in the two shapes the OpenAI-compatible
/// endpoints this tier can address use: an `error` object with a `message`, or a
/// bare `error` string.
fn endpoint_error(value: &serde_json::Value) -> Option<String> {
    let error = value.get("error")?;
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| error.as_str())?
        .trim();
    (!message.is_empty()).then(|| message.to_owned())
}

/// The first [`EXCERPT`] characters, on a character boundary, marked when
/// clipped — the same rule the payload's prose excerpts follow, for the same
/// reason: a reader must be able to tell a whole thing from a piece of one.
fn excerpt_of(raw: &str) -> String {
    let raw = raw.trim();
    match raw.char_indices().nth(EXCERPT) {
        None => raw.to_owned(),
        Some((cut, _)) => format!("{}…[truncated]", &raw[..cut]),
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, EXCERPT, ResponseError, parse};

    /// A complete response, in the shape the endpoints this tier addresses send.
    fn ok_body(content: &str, finish_reason: &str) -> String {
        serde_json::json!({
            "model": "a-vendor-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": finish_reason,
            }],
        })
        .to_string()
    }

    /// The happy path, and the one thing it is allowed to carry beyond the text:
    /// the model string the endpoint answered as.
    #[test]
    fn a_complete_response_yields_its_text_and_the_model_that_answered() {
        let answer = parse(&ok_body("  the remote answer  ", "stop")).expect("a whole answer");
        assert_eq!(answer.text, "the remote answer", "trimmed");
        assert_eq!(answer.model.as_deref(), Some("a-vendor-model"));
        assert!(answer.model_matches("a-vendor-model"));
        assert!(answer.model_discrepancy("a-vendor-model").is_none());
    }

    /// **A truncated answer is refused, not handed over.** This is Principle
    /// 10's second half at its sharpest: a completion that stopped at a token
    /// limit reads as finished, and returning it would produce a different answer
    /// with no signal that anything changed — the failure ADR-0019 most needs to
    /// prevent.
    #[test]
    fn a_generation_that_stopped_short_is_refused_rather_than_returned() {
        let err = parse(&ok_body("The answer is that the system", "length"))
            .expect_err("an incomplete generation");
        let ResponseError::Incomplete {
            ref finish_reason,
            chars,
        } = err
        else {
            panic!("expected Incomplete, got {err:?}");
        };
        assert_eq!(finish_reason, "length");
        assert_eq!(chars, 29, "the measurement travels with the refusal");

        let text = err.to_string();
        assert!(text.contains("incomplete"), "{text}");
        assert!(text.contains("no local model was substituted"), "{text}");

        // Any stop reason outside the completion list is treated the same way —
        // a vendor's new one is not assumed benign.
        assert!(matches!(
            parse(&ok_body("blocked", "content_filter")),
            Err(ResponseError::Incomplete { .. })
        ));
        // …and the ones that do mean "it finished" are accepted, case-insensitively.
        for reason in ["stop", "STOP", "end_turn", "eos", "complete"] {
            assert!(parse(&ok_body("done", reason)).is_ok(), "{reason}");
        }
    }

    /// **Silence does not establish completeness.** A response carrying no
    /// `finish_reason` says nothing about whether the generation finished — less
    /// than the `length` case above, which at least says it did not — so reading
    /// the absence as "it must have finished" would be the weakest possible
    /// reading of the strictest rule in this module.
    ///
    /// It is the rule #367 already applied at the other end of the wire: a body
    /// whose completeness cannot be established is refused rather than digested
    /// and pinned. Nothing this tier can address omits the field on a complete
    /// response — `rto-serve`'s own `ChatChoice::finish_reason` is a
    /// non-optional `&'static str` — and the one shape that legitimately carries
    /// `null` is a streaming delta, which `Payload::body`'s pinned
    /// `"stream": false` means this tier never asks for.
    #[test]
    fn a_response_that_never_says_it_finished_is_refused_rather_than_assumed_complete() {
        for (raw, why) in [
            // The field is simply not there.
            (
                r#"{"choices":[{"message":{"content":"a whole-looking answer"}}]}"#,
                "absent",
            ),
            // …or there and `null`, which is what a *streaming* chunk carries —
            // so this is an endpoint streaming at a request that said not to.
            (
                r#"{"choices":[{"message":{"content":"a whole-looking answer"},"finish_reason":null}]}"#,
                "null",
            ),
            // …or there and not a string at all, which establishes exactly as
            // little as the other two.
            (
                r#"{"choices":[{"message":{"content":"a whole-looking answer"},"finish_reason":42}]}"#,
                "not a string",
            ),
        ] {
            let err = parse(raw).expect_err("completeness was never established");
            assert_eq!(
                err,
                ResponseError::Indeterminate { chars: 22 },
                "finish_reason {why}"
            );
            let text = err.to_string();
            // The message has to say *why* it was refused, not merely that a
            // field was missing: "no `finish_reason`" is a fact about JSON, and
            // "this cannot be told apart from an answer cut short" is the reason.
            assert!(text.contains("cannot be told apart"), "{why}: {text}");
            assert!(text.contains("unknown completeness"), "{why}: {text}");
            assert!(
                text.contains("no local model was substituted"),
                "{why}: {text}"
            );
        }

        // The refusal is about completeness, not about the field's presence for
        // its own sake: a response that *does* say it finished is still fine.
        assert!(parse(&ok_body("a whole-looking answer", "stop")).is_ok());
    }

    /// The stop reason is reported ahead of emptiness, because it explains the
    /// emptiness — a reader told "it was empty" goes looking for a prompt
    /// problem, where "it stopped at the token limit" names the actual cause.
    #[test]
    fn an_empty_answer_reports_its_stop_reason_when_there_is_one() {
        assert!(matches!(
            parse(&ok_body("", "length")),
            Err(ResponseError::Incomplete { chars: 0, .. })
        ));
        let err = parse(&ok_body("   ", "stop")).expect_err("an empty completion");
        assert_eq!(err, ResponseError::Empty);
        assert!(
            err.to_string().contains("did **not** substitute"),
            "an empty remote answer is still not a local one: {err}"
        );
    }

    /// **A malformed body names itself and quotes what arrived.** "Could not
    /// parse the response" without the bytes leaves an operator with a gateway
    /// answering an HTML login page and no way to discover it.
    #[test]
    fn a_malformed_body_is_quoted_back_and_bounded() {
        for (raw, clue) in [
            ("", "not JSON"),
            ("<html><body>401 Unauthorized</body></html>", "not JSON"),
            (r#"{"ok":true}"#, "no `choices`"),
            (r#"{"choices":[{"message":{}}]}"#, "no `message.content`"),
            (
                r#"{"choices":[{"message":{"content":42}}]}"#,
                "no `message.content`",
            ),
        ] {
            let err = parse(raw).expect_err("malformed");
            let text = err.to_string();
            assert!(matches!(err, ResponseError::Malformed { .. }), "{raw:?}");
            assert!(text.contains(clue), "{raw:?} → {text}");
        }

        // A truncated JSON body — a connection dropped mid-response — is the
        // shape this most often takes in the wild.
        let cut = &ok_body("an answer", "stop")[..30];
        assert!(matches!(parse(cut), Err(ResponseError::Malformed { .. })));

        // And the quoted excerpt is bounded, so a megabyte of HTML does not
        // become a megabyte of error message.
        let huge = "x".repeat(EXCERPT * 10);
        let err = parse(&huge).expect_err("malformed");
        let ResponseError::Malformed { ref excerpt, .. } = err else {
            panic!("expected Malformed, got {err:?}");
        };
        assert_eq!(
            excerpt.chars().count(),
            EXCERPT + "…[truncated]".chars().count()
        );
    }

    /// An endpoint that put its own error in a `200` body has said the useful
    /// thing; reporting that as "malformed" would throw it away.
    #[test]
    fn an_endpoint_reported_error_is_repeated_rather_than_reclassified() {
        for raw in [
            r#"{"error":{"message":"model `x` does not exist","type":"invalid_request_error"}}"#,
            r#"{"error":"model `x` does not exist"}"#,
        ] {
            let err = parse(raw).expect_err("the endpoint refused");
            assert_eq!(
                err,
                ResponseError::EndpointReported {
                    message: "model `x` does not exist".to_owned()
                },
                "{raw}"
            );
        }
        // An empty or absent error object is not a refusal — it falls through to
        // the ordinary shape checks rather than reporting a blank reason.
        assert!(matches!(
            parse(r#"{"error":{"message":"  "}}"#),
            Err(ResponseError::Malformed { .. })
        ));
    }

    /// A well-formed response with an empty `choices` array generated nothing,
    /// which is a different fact from a body that was the wrong shape.
    #[test]
    fn a_response_with_no_choices_says_nothing_was_generated() {
        let err = parse(r#"{"model":"m","choices":[]}"#).expect_err("no choices");
        assert_eq!(err, ResponseError::NoChoices);
        assert!(err.to_string().contains("nothing was generated"), "{err}");
    }

    /// **The one identity check this machine can actually make.** ADR-0019 §5:
    /// a vendor model string is a mutable pointer, so the weights cannot be
    /// verified — but the *name* that answered can be compared to the name that
    /// was asked for, and a change in it is reported rather than dropped.
    #[test]
    fn a_model_that_answered_under_a_different_name_is_reported() {
        let answer = parse(&ok_body("hi", "stop")).expect("an answer");
        assert!(!answer.model_matches("some-other-model"));
        let note = answer
            .model_discrepancy("some-other-model")
            .expect("a discrepancy is reported");
        assert!(note.contains("some-other-model"), "{note}");
        assert!(note.contains("a-vendor-model"), "{note}");
        assert!(note.contains("vendor-asserted"), "{note}");

        // Silence is not a discrepancy: an endpoint that named no model at all
        // must not produce a notice, or the notice stops meaning anything.
        let quiet = Answer {
            text: "hi".to_owned(),
            model: None,
        };
        assert!(quiet.model_matches("anything"));
        assert!(quiet.model_discrepancy("anything").is_none());
    }
}
