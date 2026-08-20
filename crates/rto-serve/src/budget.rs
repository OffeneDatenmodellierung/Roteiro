//! **What one request may allocate**, as a number with a name (#556).
//!
//! Three constants in three modules jointly decide the context window a single
//! `/v1/chat/completions` request asks the engine to allocate, and until this
//! module nothing said so:
//!
//! | constant | module | what it bounds |
//! |---|---|---|
//! | `MAX_TOOL_ROUNDS` | [`crate::server`] | tool round-trips one request may take |
//! | `MAX_TOOL_RESULT` | [`crate::tools`] | bytes of one tool result fed back into the prompt |
//! | `DEFAULT_MAX_TOKENS` | [`crate::types`] | the generation budget when a request names none |
//!
//! Since #496 the window is sized **per request** —
//! `rto_llama`'s `window_for_request(prompt_tokens, max_tokens, ceiling,
//! trained)`, which allocates a KV cache eagerly for every generation — so their
//! sum is a real per-request memory figure rather than a tidiness concern.
//!
//! #555 is the reason this exists. It raised the round budget 4 → 10 and the
//! default generation 512 → 2,048, and the effect on the window had to be
//! *discovered by measurement*: on `qwen3.8-27b` a worst-case request moved from
//! a 9,518-token window costing 784 MiB to an 18,626-token one costing
//! 1,318 MiB. Nothing in the code noticed that the prompt had grown, and the
//! doc comment on `DEFAULT_MAX_TOKENS` asking a reader to remember the coupling
//! was already there while it happened.
//!
//! # The mechanism
//!
//! [`WORST_CASE_REQUEST_TOKENS`] is computed from the three by
//! [`worst_case_tokens`], and [`REQUEST_CONTEXT_BUDGET_TOKENS`] is the ceiling
//! it is asserted against — **at compile time**, for the reason
//! `roteiro`'s `ASK_CONTEXT_NODES` gives for the same choice: a bound that can
//! only be violated by editing a literal should fail the build that edits it
//! rather than a test run that might not be looked at. Raising any of the three
//! past the budget is a compile error naming this module, so the raise becomes a
//! decision about 2.25 GiB of KV cache per request rather than an edit to a loop
//! counter.
//!
//! The tests below cover the other direction — that the guard still *binds*
//! after a raise, rather than being disarmed by a generous budget.
//!
//! # Why the budget lives in `rto-serve`
//!
//! The quantity is consumed in `rto-llama`, which is where it would be tempting
//! to put it, and that would be wrong twice over. The dependency runs
//! `rto-serve` → `rto-llama`, so nothing there can see these three constants;
//! and `window_for_request` serves every caller in the process — `infer`
//! embeddings, `sync` image understanding, `spec draft`, the graph reviewer —
//! sizing to whatever prompt it is handed. This budget is a property of *this
//! crate's tool loop*, not of the engine, and it belongs beside the constants
//! that determine it.
//!
//! The module is compiled unconditionally, feature flags or none, so the
//! assertion is checked by every build of the crate.
//!
//! # What this does **not** bound
//!
//! Every bound has an escape, and these are this one's. None of them is a
//! defect in the mechanism; all of them are limits on what a claim built from
//! these three constants can mean.
//!
//! * **A caller-supplied `max_tokens`.** The loop generates with
//!   `req.max_tokens`, and `DEFAULT_MAX_TOKENS` is only what that resolves to
//!   when the request names neither `max_tokens` nor `max_completion_tokens`.
//!   Nothing clamps a value the client *does* name, so a request asking for
//!   200,000 tokens is sized against the model's trained window and this budget
//!   never enters it. What this bounds is therefore **the default request** —
//!   which is every Ask, since the panel sends no budget of its own.
//! * **The conversation.** `messages` is unbounded, and a client's own tool
//!   results are deliberately not truncated on the way in (see
//!   [`crate::types::ChatCompletionRequest::normalise`]). The caller's half of
//!   the prompt is the caller's context budget to spend.
//! * **The tool-surface system prompt.** Advertising the graph tools costs a
//!   measured 3,146 tokens (`rto-llama/tests/context_window.rs`), and a
//!   client's own `tools` array is bounded separately by `MAX_CLIENT_TOOL_BYTES`
//!   at 32 KiB. Both are outside this sum, so the window a request really
//!   allocates is **larger** than this budget: what is bounded here is the
//!   growth the three constants govern, not the window.
//! * **The bytes → tokens conversion is an estimate** — see
//!   [`BYTES_PER_TOKEN_ESTIMATE`]. A tool result that tokenises worse than the
//!   margin allows (dense JSON, CJK, base64) exceeds the figure.
//! * **Concurrency.** This is one request. N in flight cost N times it; there is
//!   no global ceiling, and nothing at run time compares an actual window
//!   against this number. It is a design bound, checked where the design is
//!   written down.

use crate::server::MAX_TOOL_ROUNDS;
use crate::tools::MAX_TOOL_RESULT;
use crate::types::DEFAULT_MAX_TOKENS;

/// The context a single default request may allocate, in tokens.
///
/// **The number is chosen as memory, not as tokens.** 36,864 tokens is exactly
/// 2.25 GiB of KV cache on `qwen3.8-27b` at the 64 KiB per token
/// `rto-llama/tests/context_window.rs` measures — the reference figure #496 and
/// #555 both priced against — and it is the smallest quarter-gibibyte step
/// today's constants fit under: 2 GiB does not hold them, because
/// [`WORST_CASE_REQUEST_TOKENS`] costs 2,237 MiB.
///
/// So it is deliberately **tight**, with 1,062 tokens of slack — under a third
/// of one tool round, which is what makes an eleventh round a compile error
/// rather than a shrug. A ceiling that accommodates the next raise is not a
/// ceiling, and the slack is stated here so a reader can see how close the
/// assertion is instead of discovering it when the build breaks.
///
/// Raising this is a legitimate move; raising it *silently* is what this module
/// exists to prevent. Whoever raises it is deciding that one request may cost
/// more than 2.25 GiB, and the raise is where that is said.
pub const REQUEST_CONTEXT_BUDGET_TOKENS: usize = 36_864;

/// Bytes assumed per token when converting `tools::MAX_TOOL_RESULT` — which is
/// **bytes** — into the tokens `window_for_request` counts.
///
/// **This is an estimate, and the bound built on it is only as good as it is.**
/// Four bytes per token is the ratio `review_llm`'s prompt sizing already uses
/// (`len / 4`), applied here with the same 30% margin it applies for the same
/// reason: to leave room for the estimate to have understated the count. A
/// figure that read as exact would be its own defect.
///
/// The margin is roughly right rather than merely cautious, and the live
/// measurement is there to check it against. #555's two windows — 9,518 tokens
/// at 4 rounds / 512, and 18,626 at 10 / 2,048 — differ by six rounds and one
/// raised generation budget, which puts a real round at ~1,262 tokens for the
/// result **and** the tool call that asked for it.
/// [`TOOL_RESULT_TOKENS_PER_ROUND`] estimates 1,321 for the result alone, so the
/// conversion is on the right side of what was observed.
///
/// [`WORST_CASE_REQUEST_TOKENS`] then charges that tool call at the full
/// `types::DEFAULT_MAX_TOKENS`, where the measurement saw about 262 tokens, so the
/// worst case comes out around twice the measured figure. That gap is the
/// difference between a worst case and a typical one, and it is why the budget
/// is not simply the number #555 measured.
pub const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

/// Percent of itself added to a byte-derived token count, so the estimate above
/// is allowed to have been wrong in the direction that matters.
const TOKENISATION_MARGIN_PERCENT: usize = 30;

/// Bytes the loop wraps a tool result in on top of `tools::MAX_TOOL_RESULT`: the
/// `<tool_response>…</tool_response>` markers and, when the result was cut, the
/// `"… (truncated)"` marker. 46 bytes as those literals stand.
///
/// Carried as 64 rather than 46 so an edit to either string does not silently
/// under-state the worst case — and pinned against the real wrapper by
/// `the_envelope_bound_holds_against_the_real_wrapper` below, which builds a
/// message with `tools::tool_response_turn` rather than trusting this comment.
const TOOL_RESULT_ENVELOPE_BYTES: usize = 64;

/// The slack `rto_llama`'s `window_for_request` adds on top of
/// `prompt + max_tokens`.
///
/// Restated rather than imported, exactly as `rto-llama/tests/context_window.rs`
/// restates it and for a related reason: the constant is private to a module
/// behind the `llama` feature, and this module is compiled whether that feature
/// is on or not. At 64 tokens it is under two parts in ten thousand of the
/// total — carried for completeness of the arithmetic, not for precision.
const WINDOW_HEADROOM: usize = 64;

/// Tokens one round contributes as a **tool result**: the capped result plus its
/// envelope, converted at [`BYTES_PER_TOKEN_ESTIMATE`] with the margin applied.
pub const TOOL_RESULT_TOKENS_PER_ROUND: usize =
    tokens_for_bytes(MAX_TOOL_RESULT + TOOL_RESULT_ENVELOPE_BYTES);

/// Tokens one round contributes to the next prompt, in full.
///
/// **The tool result is not the whole of it, and this is the part #556's own
/// arithmetic left out.** Every round appends *two* turns: the assistant's
/// generation — which was a tool call, and is bounded only by the request's
/// generation budget — and then the tool response. So the generation budget is
/// not "the other addend" added once; it is charged
/// `MAX_TOOL_ROUNDS + 1` times, once per round that fed a call back and once for
/// the final answer.
pub const TOKENS_PER_ROUND: usize = TOOL_RESULT_TOKENS_PER_ROUND + DEFAULT_MAX_TOKENS as usize;

/// The window the three constants permit a single default request to allocate,
/// worst case: every round spent, every generation at the token cap, every tool
/// result at its byte cap.
pub const WORST_CASE_REQUEST_TOKENS: usize = worst_case_tokens(
    MAX_TOOL_ROUNDS,
    MAX_TOOL_RESULT,
    DEFAULT_MAX_TOKENS as usize,
);

/// **The relationship, as a fact the compiler checks.**
///
/// Raising `MAX_TOOL_ROUNDS`, `MAX_TOOL_RESULT` or `DEFAULT_MAX_TOKENS` past
/// what [`REQUEST_CONTEXT_BUDGET_TOKENS`] allows stops the crate compiling,
/// wherever in the crate the edit was made.
const _: () = assert!(
    WORST_CASE_REQUEST_TOKENS <= REQUEST_CONTEXT_BUDGET_TOKENS,
    "the worst-case request now allocates more context than \
     `budget::REQUEST_CONTEXT_BUDGET_TOKENS` permits. MAX_TOOL_ROUNDS, \
     MAX_TOOL_RESULT and DEFAULT_MAX_TOKENS jointly size every request's KV \
     cache (rto_llama::window_for_request, #496); at 64 KiB/token the current \
     budget is 2,304 MiB per request. Raise the budget deliberately, in \
     crates/rto-serve/src/budget.rs, or lower what you just raised — but do not \
     raise one of the three without pricing the total (#555, #556)."
);

/// Convert a byte count to an estimated token count, margin included.
///
/// Rounds up at both steps: a bound that rounded down would be a bound the
/// arithmetic itself could break.
#[must_use]
pub const fn tokens_for_bytes(bytes: usize) -> usize {
    let tokens = bytes.div_ceil(BYTES_PER_TOKEN_ESTIMATE);
    (tokens * (100 + TOKENISATION_MARGIN_PERCENT)).div_ceil(100)
}

/// The worst case for an arbitrary `(rounds, tool_result_bytes, max_tokens)`,
/// so a caller — or a test asking "what would raising this cost?" — computes it
/// with the same function that produced [`WORST_CASE_REQUEST_TOKENS`] rather
/// than restating the arithmetic.
///
/// The shape, which is the relationship #556 asks to have expressed:
///
/// ```text
/// rounds × (assistant tool call + tool response)   ← the prompt the loop grows
///   + max_tokens                                   ← the final generation
///   + WINDOW_HEADROOM                              ← what the engine adds
/// ```
#[must_use]
pub const fn worst_case_tokens(
    rounds: usize,
    tool_result_bytes: usize,
    max_tokens: usize,
) -> usize {
    let per_round = tokens_for_bytes(tool_result_bytes + TOOL_RESULT_ENVELOPE_BYTES) + max_tokens;
    rounds * per_round + max_tokens + WINDOW_HEADROOM
}

/// KV cache cost of one token on the model both #496 and #555 priced against
/// (`qwen3.8-27b`, f16 KV), in KiB — measured in
/// `rto-llama/tests/context_window.rs`, restated here so the budget can be
/// stated in the unit an operator actually feels.
///
/// Model-specific, and the largest of the models this registry serves: a smaller
/// model costs less per token for the same window. It converts the budget into
/// memory; it is not part of the bound.
pub const KV_KIB_PER_TOKEN: usize = 64;

/// [`REQUEST_CONTEXT_BUDGET_TOKENS`] as MiB of KV cache on that model — the
/// figure the budget is actually chosen for, computed rather than quoted so the
/// prose above cannot drift from the number.
pub const REQUEST_CONTEXT_BUDGET_MIB: usize =
    REQUEST_CONTEXT_BUDGET_TOKENS * KV_KIB_PER_TOKEN / 1024;

#[cfg(test)]
mod tests {
    use super::{
        BYTES_PER_TOKEN_ESTIMATE, KV_KIB_PER_TOKEN, MAX_TOOL_RESULT, MAX_TOOL_ROUNDS,
        REQUEST_CONTEXT_BUDGET_MIB, REQUEST_CONTEXT_BUDGET_TOKENS, TOKENS_PER_ROUND,
        TOOL_RESULT_TOKENS_PER_ROUND, WORST_CASE_REQUEST_TOKENS, tokens_for_bytes,
        worst_case_tokens,
    };
    use crate::types::DEFAULT_MAX_TOKENS;

    /// MiB of KV cache a window of `tokens` costs on the reference model.
    fn mib(tokens: usize) -> usize {
        tokens * KV_KIB_PER_TOKEN / 1024
    }

    /// The numbers the module documentation quotes, pinned — so prose and
    /// arithmetic cannot drift apart, and so a reader who wants the decomposition
    /// can read it out of a run rather than re-deriving it.
    #[test]
    fn the_worst_case_decomposes_as_documented() {
        assert_eq!(TOOL_RESULT_TOKENS_PER_ROUND, 1_321);
        assert_eq!(TOKENS_PER_ROUND, 3_369);
        assert_eq!(WORST_CASE_REQUEST_TOKENS, 35_802);
        assert_eq!(REQUEST_CONTEXT_BUDGET_TOKENS, 36_864);
        assert_eq!(mib(WORST_CASE_REQUEST_TOKENS), 2_237);
        assert_eq!(REQUEST_CONTEXT_BUDGET_MIB, 2_304);
        assert_eq!(
            mib(REQUEST_CONTEXT_BUDGET_TOKENS),
            REQUEST_CONTEXT_BUDGET_MIB
        );

        eprintln!(
            "worst case {WORST_CASE_REQUEST_TOKENS} tok ({} MiB) of \
             {REQUEST_CONTEXT_BUDGET_TOKENS} tok ({REQUEST_CONTEXT_BUDGET_MIB} MiB): \
             {MAX_TOOL_ROUNDS} rounds × ({TOOL_RESULT_TOKENS_PER_ROUND} result + \
             {DEFAULT_MAX_TOKENS} call) + {DEFAULT_MAX_TOKENS} answer + 64 headroom",
            mib(WORST_CASE_REQUEST_TOKENS),
        );
    }

    /// **The guard has to bind, not merely exist.** A budget raised far enough
    /// to be comfortable is a budget nobody ever meets, and this module would
    /// then be the doc comment it was written to replace — `types.rs` already
    /// had one of those and #555 walked past it.
    ///
    /// One more round is the raise #555 actually made, six times over. It must
    /// not fit.
    #[test]
    fn one_more_tool_round_would_breach_the_budget() {
        let one_more = worst_case_tokens(
            MAX_TOOL_ROUNDS + 1,
            MAX_TOOL_RESULT,
            DEFAULT_MAX_TOKENS as usize,
        );
        assert!(
            one_more > REQUEST_CONTEXT_BUDGET_TOKENS,
            "an eleventh round costs {one_more} tokens and the budget is \
             {REQUEST_CONTEXT_BUDGET_TOKENS}: the const assertion in this module \
             would not notice a raise, which is the whole of what it is for"
        );
    }

    /// The other two, so the guard binds on whichever of the three moves — a
    /// bound that only catches one of them catches the last thing that changed
    /// rather than the next thing.
    #[test]
    fn a_larger_tool_result_or_generation_would_breach_the_budget() {
        let bigger_result = worst_case_tokens(
            MAX_TOOL_ROUNDS,
            MAX_TOOL_RESULT * 5 / 4,
            DEFAULT_MAX_TOKENS as usize,
        );
        assert!(
            bigger_result > REQUEST_CONTEXT_BUDGET_TOKENS,
            "a 25% larger tool result costs {bigger_result} tokens against a \
             budget of {REQUEST_CONTEXT_BUDGET_TOKENS}"
        );

        let bigger_generation = worst_case_tokens(
            MAX_TOOL_ROUNDS,
            MAX_TOOL_RESULT,
            DEFAULT_MAX_TOKENS as usize * 2,
        );
        assert!(
            bigger_generation > REQUEST_CONTEXT_BUDGET_TOKENS,
            "doubling the default generation costs {bigger_generation} tokens \
             against a budget of {REQUEST_CONTEXT_BUDGET_TOKENS}"
        );
    }

    /// The generation budget is charged `MAX_TOOL_ROUNDS + 1` times, not once:
    /// every round appends the tool call it generated to the next prompt. The
    /// claim is worth a test because it is the counter-intuitive half of the
    /// relationship — #556's own table reads it as a single addend — and because
    /// it is what makes `DEFAULT_MAX_TOKENS` the dominant term rather than a
    /// rounding error against the tool results.
    #[test]
    fn the_generation_budget_is_charged_once_per_round_and_once_for_the_answer() {
        let at_zero = worst_case_tokens(MAX_TOOL_ROUNDS, MAX_TOOL_RESULT, 0);
        let charged = WORST_CASE_REQUEST_TOKENS - at_zero;
        assert_eq!(charged, DEFAULT_MAX_TOKENS as usize * (MAX_TOOL_ROUNDS + 1));
        assert!(
            charged * 2 > WORST_CASE_REQUEST_TOKENS,
            "the generation budget is over half the worst case ({charged} of \
             {WORST_CASE_REQUEST_TOKENS}); if that stops being true the doc \
             above is wrong about which constant dominates"
        );
    }

    /// [`super::TOOL_RESULT_ENVELOPE_BYTES`] is an upper bound on what the loop
    /// wraps a result in, checked against the code that does the wrapping rather
    /// than against a comment counting characters. An edit to either literal
    /// that outgrows the allowance fails here.
    #[test]
    fn the_envelope_bound_holds_against_the_real_wrapper() {
        let oversized = "x".repeat(MAX_TOOL_RESULT * 4);
        let turn = crate::tools::tool_response_turn(&oversized);
        assert!(
            turn.len() <= MAX_TOOL_RESULT + super::TOOL_RESULT_ENVELOPE_BYTES,
            "a wrapped, truncated tool result is {} bytes; the worst case is \
             computed from {}",
            turn.len(),
            MAX_TOOL_RESULT + super::TOOL_RESULT_ENVELOPE_BYTES
        );
    }

    /// The conversion rounds **up** at both steps. A bound that rounded down
    /// would understate by up to a token per result and call itself a worst
    /// case.
    #[test]
    fn the_byte_conversion_never_rounds_down() {
        assert_eq!(tokens_for_bytes(0), 0);
        assert_eq!(tokens_for_bytes(1), 2); // 1 byte → 1 token → +30%, rounded up
        assert_eq!(
            tokens_for_bytes(BYTES_PER_TOKEN_ESTIMATE * 100),
            130,
            "100 tokens of bytes must carry the full margin"
        );
        for bytes in 0..512 {
            assert!(
                tokens_for_bytes(bytes) * BYTES_PER_TOKEN_ESTIMATE >= bytes,
                "{bytes} bytes estimated as fewer tokens than the ratio itself allows"
            );
        }
    }
}
