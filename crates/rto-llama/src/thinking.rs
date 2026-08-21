//! Reading a reasoning model's completion: the answer, or the fact that there
//! isn't one.
//!
//! A reasoning-capable GGUF (Qwen3, DeepSeek-R1, …) writes a `<think>…</think>`
//! block before its answer. Every consumer wants the answer and none of them
//! wants the block — a drafted spec section, a reviewer's findings, and an HTTP
//! caller's `message.content` are all made worse by scratch deliberation spliced
//! into them.
//!
//! # Why the rule lives here
//!
//! It used to live in `roteiro`'s `main.rs`, whose `strip_thinking` carried a
//! doc comment forbidding a second copy: *"a reviewer that parsed a model's
//! `<think>` block would read its scratch reasoning as findings"*. That rule was
//! right and the location was not — `rto-serve` is a fourth consumer of it, and
//! `roteiro` depends on `rto-serve` rather than the reverse, so `rto-serve`
//! could not call it without a dependency cycle (#582). The honest fix for "a
//! rule two crates need" is to move it to the crate both can see rather than to
//! copy it, which is what #523/#529 did for `markdown_dialect` and Stage 34
//! part 2b did for `ProducerTrust`.
//!
//! `rto-llama` is that crate: `rto-serve` depends on it unconditionally, and so
//! does every `roteiro` build that can reach a generation. It is also where
//! [`FinishReason`] already lives, which the rule below needs.
//!
//! Nothing here needs llama.cpp, so this module is compiled unconditionally and
//! its tests run in a build with no C/C++ toolchain — the same property
//! [`crate::slot`] has and for the same reason.
//!
//! # An unterminated block is not an answer
//!
//! The stripper this replaces returned text with no `</think>` **unchanged**, so
//! a generation that stopped inside its own reasoning returned the reasoning as
//! though it were the reply (#583). That is not a hypothetical edge: `rto-serve`'s
//! `DEFAULT_MAX_TOKENS` records Stage 35b measuring `qwen3.8-27b` spending an
//! entire 1,200-token budget inside `<think>` and emitting no answer at all. It
//! is the documented worst case of the model this project is configured to use.
//!
//! So [`answer`] refuses it, and the refusal is the shape the codebase has
//! already settled on twice. `rto-serve`'s `tools::finish` refuses an unfinished
//! `<tool_call>` rather than stripping it, because *"stripping would be the
//! silent downgrade `docs/REVIEW_CHECKLIST.md` §Refusals forbids: an incomplete
//! thing presented as the whole one, with the evidence removed"*. `rto-remote`'s
//! `response::parse` refuses a truncated remote generation for the same reason —
//! *"Roteiro will not hand you a truncated answer as though it were a whole
//! one"*. An unterminated reasoning block is the same fact arriving through a
//! third marker, and it gets the same answer.
//!
//! [`Unterminated`]'s two variants deliberately mirror `tools::Unfinished`'s
//! first two, down to their names: the marker differs, the judgement does not.
//!
//! # What this does *not* refuse
//!
//! **An answer that talks about the tags is an answer.** A block is content that
//! *opened* with `<think>`; a `<think>` or `</think>` appearing anywhere else is
//! prose about a tag, and both [`answer`] and [`StreamFilter`] hand it back whole.
//! That is not a corner: this repository's own `docs/SERVING.md`, #582 and #583
//! all discuss the tags, so asking Ask about them is the reproduction. Keying on
//! a close tag alone truncated such a reply at the quote — silently, with
//! [`FinishReason`] still reporting `Stop` and nothing saying anything had been
//! cut (#589).
//!
//! A generation that closed its block and was then cut off mid-answer is a
//! partial answer, not a missing one, and this hands it over. The caller already
//! has [`FinishReason`] and can decide — `rto-remote` refuses that case at its
//! own tier, and `/v1/chat/completions` reports it as `finish_reason: "length"`
//! exactly as an OpenAI client expects. Widening the refusal to cover it would
//! be a different decision about a different fact, and is not one this module
//! makes on a caller's behalf.

use crate::engine::FinishReason;

/// The tag a reasoning model opens its deliberation with.
const OPEN: &str = "<think>";

/// The tag that ends it — and, by its absence, the whole of [`Unterminated`].
const CLOSE: &str = "</think>";

/// A generation that opened a reasoning block and never closed it, so it never
/// reached an answer.
///
/// The two variants are **evidence, not inference**: [`FinishReason`] is set by
/// the decode loop, so the engine is reporting why it stopped rather than this
/// module guessing at it. They are separated because they have different ways
/// forward — a budget that ran out wants a bigger budget, a model that stopped
/// on its own wants a different model — which is the same reason
/// `tools::Unfinished` separates them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unterminated {
    /// The token budget (`max_tokens`) ran out inside the block.
    CutAtTokenCap,
    /// The model emitted end-of-generation inside the block: it stopped
    /// deliberating without ever starting to answer.
    CutShort,
}

impl Unterminated {
    /// Read a stop reason as a verdict on an unterminated block.
    #[must_use]
    pub fn from_finish_reason(finish_reason: FinishReason) -> Self {
        match finish_reason {
            FinishReason::Length => Unterminated::CutAtTokenCap,
            FinishReason::Stop => Unterminated::CutShort,
        }
    }

    /// One clause naming what happened, for a caller assembling a message.
    ///
    /// The clause and not the whole sentence: a refusal in an assistant slot, a
    /// CLI error and a review outcome want different framing around the same
    /// fact, and each caller writes its own.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Unterminated::CutAtTokenCap => {
                "the model spent its whole token budget inside its reasoning block and never \
                 started an answer"
            }
            Unterminated::CutShort => {
                "the model stopped part-way through its reasoning block, so it never started \
                 an answer"
            }
        }
    }
}

/// The answer in a completion, with any leading `<think>…</think>` block
/// removed.
///
/// Borrows from `content`: this decides what part of the generation is the
/// answer and never rewrites it, so a caller that wants an owned `String` says
/// so. Text carrying no reasoning block at all is returned exactly as it
/// arrived.
///
/// # Errors
/// [`Unterminated`] if the generation opened a block it never closed — see the
/// module documentation for why that is a refusal rather than a passthrough.
pub fn answer(content: &str, finish_reason: FinishReason) -> Result<&str, Unterminated> {
    // **Whether a block was opened is decided first, and once.** `starts_with`
    // rather than `contains`, and that precision is the point: a model asked what
    // a reasoning tag looks like will write `<think>` or `</think>` in the middle
    // of a perfectly good answer, and treating either as a block would be this
    // module inventing a truncation. Both live reproductions in #582 and #583
    // show llama.cpp emitting the opening tag at position zero, which is where a
    // block that was actually opened puts it.
    //
    // The check governs *both* readings below, because a close tag means nothing
    // without an open one. Asking it only about the unterminated case is how a
    // reply quoting `</think>` — which `docs/SERVING.md` and this PR's own issues
    // provoke — lost everything before the quote, silently and with a
    // `finish_reason` still saying `stop`. That is also the reading
    // [`StreamFilter`] has always taken, and the two surfaces answer the same
    // question the same way or they are the defect #582 was filed on.
    if !content.trim_start().starts_with(OPEN) {
        // No block at all — a non-reasoning model, a reasoning model that did not
        // use one, or an answer that merely talks about the tags. Handed back
        // untouched.
        return Ok(content);
    }
    match content.find(CLOSE) {
        // A closed block: the answer is what follows it. The *first* close tag,
        // so an answer that goes on to quote the tag keeps its quote. Leading
        // whitespace goes because the close tag is followed by the model's
        // newlines, not by the reader's.
        Some(end) => Ok(content[end + CLOSE.len()..].trim_start()),
        // Opened and never closed: the block never ended, so everything here is
        // deliberation.
        None => Err(Unterminated::from_finish_reason(finish_reason)),
    }
}

/// [`answer`] for a generation arriving token by token.
///
/// The streaming endpoint cannot call [`answer`], because by the time the whole
/// completion exists it has already been sent. So the rule is applied
/// incrementally instead: nothing is emitted until the reader knows whether it is
/// looking at a reasoning block, and while it is inside one it emits nothing at
/// all. The block is *withheld*, not delayed — a caller never receives it.
///
/// **Buffering is the behaviour, not a compromise.** A token stream whose first
/// hundred tokens are deliberation has nothing to show a reader during them, and
/// showing them anyway is the defect (#582), not the feature. What the caller
/// loses is time-to-first-token on a reasoning model; what it gains is that the
/// streaming and non-streaming surfaces answer the same question the same way.
///
/// Whole tokens are never split across the decision, because the decision is made
/// on the accumulated prefix rather than on one piece: `<think>` may arrive as one
/// token, as `<`/`think`/`>`, or glued to the text after it, and all three reach
/// the same verdict.
#[derive(Debug, Default)]
pub struct StreamFilter {
    /// Text held back: an undecided prefix, or the inside of an open block.
    held: String,
    /// Where the filter is in the three-state read below.
    state: State,
}

/// What a [`StreamFilter`] currently believes about the stream so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not yet enough text to say whether a block was opened. Everything is
    /// held, because emitting a `<` that turns out to start `<think>` cannot be
    /// taken back.
    #[default]
    Deciding,
    /// Inside an open block. Everything is held and most of it is discarded.
    Reasoning,
    /// The block closed and the answer has not started yet.
    ///
    /// Distinct from [`State::Passthrough`] because [`answer`] trims the
    /// whitespace between `</think>` and the first word, and a model puts a
    /// newline or two there. Those can arrive in a later piece than the close
    /// tag, so the trim has to outlive the token that ended the block — without
    /// this state the streamed answer began with the blank lines the
    /// non-streaming one drops, and the two surfaces disagreed by exactly the
    /// whitespace this test suite compares them on.
    AfterBlock,
    /// Past any block: emit as it arrives, which is ordinary streaming.
    Passthrough,
}

impl StreamFilter {
    /// A filter for one generation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one piece of generated text; returns what to emit now, if anything.
    ///
    /// Returns `None` rather than an empty string when there is nothing to emit,
    /// so a caller cannot accidentally send an empty delta for every token of a
    /// reasoning block.
    pub fn push(&mut self, piece: &str) -> Option<String> {
        match self.state {
            State::Passthrough => (!piece.is_empty()).then(|| piece.to_owned()),
            State::Reasoning => {
                self.held.push_str(piece);
                // Searched from the start each time. The held text is bounded by
                // the request's `max_tokens`, and a block is closed once, so this
                // is a scan over a few kilobytes rather than a hot loop.
                let end = self.held.find(CLOSE)?;
                let rest = self.held[end + CLOSE.len()..].trim_start().to_owned();
                self.held.clear();
                if rest.is_empty() {
                    // The answer has not started; keep trimming until it does.
                    self.state = State::AfterBlock;
                    return None;
                }
                self.state = State::Passthrough;
                Some(rest)
            }
            State::AfterBlock => {
                let started = piece.trim_start();
                if started.is_empty() {
                    return None;
                }
                self.state = State::Passthrough;
                Some(started.to_owned())
            }
            State::Deciding => {
                self.held.push_str(piece);
                let seen = self.held.trim_start();
                if let Some(rest) = seen.strip_prefix(OPEN) {
                    // A block was opened. Everything from here is deliberation
                    // until the close tag arrives.
                    let rest = rest.to_owned();
                    self.held.clear();
                    self.state = State::Reasoning;
                    return self.push(&rest);
                }
                // Still a possible prefix of the open tag (`<`, `<thi`, or only
                // whitespace so far) — keep holding rather than guess.
                if OPEN.starts_with(seen) {
                    return None;
                }
                // It is not a block and cannot become one: release the prefix and
                // stream normally from here.
                let out = std::mem::take(&mut self.held);
                self.state = State::Passthrough;
                (!out.is_empty()).then_some(out)
            }
        }
    }

    /// End of generation: any text still held, or the verdict that the stream
    /// never got past its reasoning block.
    ///
    /// # Errors
    /// [`Unterminated`] if the block was still open when generation stopped —
    /// the streaming half of the rule [`answer`] applies to a whole completion.
    pub fn end(self, finish_reason: FinishReason) -> Result<Option<String>, Unterminated> {
        match self.state {
            State::Reasoning => Err(Unterminated::from_finish_reason(finish_reason)),
            // A stream that ended while still undecided held a prefix of the open
            // tag and nothing more (`<thi`, say, or pure whitespace). No block was
            // ever opened, so it is text, and it is owed to the caller.
            State::Deciding => Ok((!self.held.is_empty()).then_some(self.held)),
            // A block that closed with only whitespace after it is an empty
            // answer, which [`answer`] also reports as `Ok("")`.
            State::AfterBlock | State::Passthrough => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamFilter, Unterminated, answer};
    use crate::engine::FinishReason;

    /// Drive a filter over `pieces` and return everything it emitted, plus the
    /// end verdict — the shape every streaming test below asserts on.
    fn stream(pieces: &[&str], finish_reason: FinishReason) -> Result<String, Unterminated> {
        let mut filter = StreamFilter::new();
        let mut out = String::new();
        for piece in pieces {
            if let Some(text) = filter.push(piece) {
                out.push_str(&text);
            }
        }
        if let Some(tail) = filter.end(finish_reason)? {
            out.push_str(&tail);
        }
        Ok(out)
    }

    /// The ordinary case, and the one the original stripper got right.
    #[test]
    fn a_closed_block_leaves_the_answer() {
        assert_eq!(
            answer(
                "<think>\n2+2 is basic arithmetic.\n</think>\n\nfour",
                FinishReason::Stop
            ),
            Ok("four")
        );
    }

    /// **The defect #583 was filed on.** The reasoning block *is* the whole
    /// generation, and the old stripper returned it verbatim — so `spec draft`
    /// wrote a model's scratch deliberation into a document as prose.
    #[test]
    fn an_unterminated_block_is_not_an_answer() {
        let cut = "<think>\nOkay, I need to compare B-trees and LSM trees";
        assert_eq!(
            answer(cut, FinishReason::Length),
            Err(Unterminated::CutAtTokenCap)
        );
        assert_eq!(answer(cut, FinishReason::Stop), Err(Unterminated::CutShort));
    }

    /// The two reasons are told apart, because they have different ways forward.
    /// A caller that collapsed them would tell someone to raise a budget that was
    /// never the binding constraint.
    #[test]
    fn the_stop_reason_decides_which_refusal_it_is() {
        assert_eq!(
            Unterminated::from_finish_reason(FinishReason::Length),
            Unterminated::CutAtTokenCap
        );
        assert_eq!(
            Unterminated::from_finish_reason(FinishReason::Stop),
            Unterminated::CutShort
        );
        assert_ne!(
            Unterminated::CutAtTokenCap.as_str(),
            Unterminated::CutShort.as_str()
        );
    }

    /// A model with no reasoning block gets its text back byte for byte —
    /// including the leading whitespace, which this function has no business
    /// editing when there was no block to strip.
    #[test]
    fn text_with_no_block_is_untouched() {
        assert_eq!(answer("  four", FinishReason::Stop), Ok("  four"));
        assert_eq!(answer("", FinishReason::Stop), Ok(""));
    }

    /// **Talking about the tag is not opening one.** The refusal keys on a block
    /// that was opened, which is a claim about position zero — not on the string
    /// appearing anywhere in the reply. Getting this wrong would refuse a correct
    /// answer to a question about Roteiro's own handling of reasoning models.
    #[test]
    fn a_mention_of_the_tag_inside_an_answer_is_still_an_answer() {
        let reply = "A reasoning model opens its scratchpad with <think> and closes it after.";
        assert_eq!(answer(reply, FinishReason::Stop), Ok(reply));
    }

    /// **And neither is talking about the *closing* tag.** The same argument as
    /// the test above, on the other half of the `match`: a close tag counts only
    /// when the content opened with a block, so a reply that merely quotes
    /// `</think>` keeps everything before it. Getting this wrong discards the
    /// whole answer up to the quoted tag, silently and with no `finish_reason`
    /// saying so — and `docs/SERVING.md`, #582 and #583 are all documents that
    /// would provoke exactly that reply.
    #[test]
    fn a_mention_of_the_close_tag_is_not_the_end_of_a_block() {
        let reply = "A reasoning model ends its scratchpad with </think>, and Roteiro strips it.";
        assert_eq!(answer(reply, FinishReason::Stop), Ok(reply));
    }

    /// A real block wins over a later mention: the strip keys on the *first*
    /// close tag, so an answer that goes on to quote the tag keeps the quote.
    #[test]
    fn a_real_block_is_stripped_and_a_later_mention_is_not() {
        assert_eq!(
            answer(
                "<think>hm</think>\n\nthe tag is spelled </think>",
                FinishReason::Stop
            ),
            Ok("the tag is spelled </think>")
        );
    }

    /// A block that closed and *then* ran out of budget is a partial answer, not
    /// a missing one — see the module docs. The caller still has the stop reason
    /// and decides for itself; this hands over what was written.
    #[test]
    fn a_closed_block_followed_by_a_truncated_answer_is_still_an_answer() {
        assert_eq!(
            answer(
                "<think>done</think>\n\nB-trees keep their",
                FinishReason::Length
            ),
            Ok("B-trees keep their")
        );
    }

    /// A closed block with nothing after it yields an empty answer rather than an
    /// error. That is deliberate: emptiness is a fact the callers already act on
    /// (`spec draft` skips an empty section, `rto-remote` refuses an empty
    /// completion at its own tier), and this module's subject is the reasoning
    /// block, not the length of what follows it.
    #[test]
    fn a_closed_block_with_nothing_after_it_is_an_empty_answer() {
        assert_eq!(answer("<think>hm</think>", FinishReason::Stop), Ok(""));
    }

    /// **The streaming and non-streaming reads agree.** The two surfaces answer
    /// the same question, and the whole of #582 is that they had not been. Driven
    /// one character at a time, which is the most adversarial tokenisation there
    /// is.
    #[test]
    fn the_stream_filter_and_answer_agree() {
        for text in [
            "<think>\nreasoning\n</think>\n\nfour",
            "four",
            "<think>a</think>b",
            "",
            "a reply mentioning <think> in passing",
            "a reply mentioning </think> in passing",
            "<think>a</think>b then </think> again",
        ] {
            let pieces: Vec<String> = text.chars().map(|c| c.to_string()).collect();
            let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
            assert_eq!(
                stream(&refs, FinishReason::Stop),
                answer(text, FinishReason::Stop).map(str::to_owned),
                "disagreed on {text:?}"
            );
        }
    }

    /// The block is withheld, not delayed: no delta carries any of it.
    #[test]
    fn no_piece_of_a_reasoning_block_is_ever_emitted() {
        let mut filter = StreamFilter::new();
        let mut emitted = String::new();
        for piece in [
            "<think>", "\nOkay, ", "2+2 ", "is 4.\n", "</think>", "\n\nfour",
        ] {
            if let Some(text) = filter.push(piece) {
                emitted.push_str(&text);
            }
        }
        assert_eq!(emitted, "four");
        assert!(!emitted.contains("Okay"), "deliberation reached the caller");
        assert_eq!(filter.end(FinishReason::Stop), Ok(None));
    }

    /// The open tag split across tokens still opens a block. A filter that
    /// decided on the first piece alone would emit a bare `<` and then discard
    /// the reasoning it introduced — which is worse than not filtering at all.
    #[test]
    fn an_open_tag_split_across_tokens_is_still_an_open_tag() {
        assert_eq!(
            stream(
                &["<", "thi", "nk", ">", "hm", "</think>", "four"],
                FinishReason::Stop
            ),
            Ok("four".to_owned())
        );
        // And so is the close tag.
        assert_eq!(
            stream(
                &["<think>hm", "</", "think", ">", "four"],
                FinishReason::Stop
            ),
            Ok("four".to_owned())
        );
    }

    /// A stream that ends inside its block is the streaming half of #583: the
    /// caller has been sent nothing, and must be told why rather than handed the
    /// deliberation as a consolation.
    #[test]
    fn a_stream_that_ends_inside_the_block_has_no_answer() {
        assert_eq!(
            stream(&["<think>", "still thinking"], FinishReason::Length),
            Err(Unterminated::CutAtTokenCap)
        );
        assert_eq!(
            stream(&["<think>", "still thinking"], FinishReason::Stop),
            Err(Unterminated::CutShort)
        );
    }

    /// Text that merely *starts* like the tag is owed to the caller, not eaten.
    /// The filter holds a prefix while it cannot tell, so the failure mode to
    /// rule out is holding it forever.
    #[test]
    fn a_held_prefix_that_never_became_a_tag_is_still_released() {
        assert_eq!(stream(&["<thi"], FinishReason::Stop), Ok("<thi".to_owned()));
        assert_eq!(
            stream(&["<thinker>", " wrote this"], FinishReason::Stop),
            Ok("<thinker> wrote this".to_owned())
        );
        assert_eq!(stream(&["   "], FinishReason::Stop), Ok("   ".to_owned()));
    }

    /// Once past the block, streaming is ordinary again: each piece is emitted as
    /// it arrives rather than accumulated to the end.
    #[test]
    fn passthrough_is_token_incremental() {
        let mut filter = StreamFilter::new();
        assert_eq!(filter.push("<think>x</think>"), None);
        assert_eq!(filter.push("one "), Some("one ".to_owned()));
        assert_eq!(filter.push("two"), Some("two".to_owned()));
    }
}
