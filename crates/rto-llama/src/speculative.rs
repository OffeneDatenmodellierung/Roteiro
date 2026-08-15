//! MTP (multi-token-prediction) speculative decoding, behind the `llama`
//! feature (issue #320).
//!
//! # What it is
//!
//! Generation on Metal is memory-bandwidth bound: one decode moves the whole
//! model through the memory system to produce **one** token. Speculative
//! decoding amortises that. A cheap *drafter* proposes the next few tokens, the
//! target model verifies all of them in **one** batched decode, and every
//! proposal the target agrees with is free — the same bandwidth, several tokens.
//!
//! The drafter here is the model's own **MTP head**: an extra transformer block
//! (`blk.N.nextn.*`) that Qwen3.5/3.6/3.8 ship inside the GGUF, trained to
//! predict token *n+2* from the hidden state at *n* and the token at *n+1*. So
//! there is no second model to install, download or keep in step — if the GGUF
//! has the head, the draft is available.
//!
//! # The invariant
//!
//! **Speculative decoding must not change the output.** That is the whole
//! premise: it is a bandwidth optimisation, not a different sampler. This module
//! is built so that identity is a property of the *code*, not a hope about the
//! library:
//!
//! * llama.cpp's speculative helper only ever **proposes**. Every token that
//!   reaches the caller is sampled by [`LlamaSampler`] from the *target* model's
//!   logits — see [`run_generation`]. A proposal is used only as a prediction of
//!   what that sampler was going to produce anyway, and is discarded the moment
//!   it is wrong.
//! * the sampler is invoked **exactly once per emitted token**, in position
//!   order, with [`LlamaSampler::accept`] called between invocations. A stateful
//!   sampler (`dist`'s RNG stream, repetition penalties, a grammar) therefore
//!   sees precisely the call sequence it sees without drafting — which is what
//!   makes "same seed, same output" true rather than merely "same distribution".
//!
//! What remains outside the code's control is llama.cpp's own arithmetic: a
//! batch of four tokens and four batches of one token are not guaranteed to
//! produce bit-identical logits on a GPU, because the kernels differ. That is a
//! property of the backend, not of this module, and it is why
//! `tests/speculative.rs` checks the identity **empirically** against a real
//! model rather than asserting it from first principles.
//!
//! # When it is on
//!
//! Automatically, whenever the served GGUF ships a draft head and the target
//! request is text-only. There is nothing to configure because there is nothing
//! to trade off: the output is the same either way (above), the head is already
//! resident (below), and a model without one falls back by construction —
//! [`draft_head_layers`] reads the GGUF's own `<arch>.nextn_predict_layers`, and
//! even if that were wrong, llama.cpp refuses to build an MTP context for a model
//! with no MTP layers and we take the plain path. `ROTEIRO_SPECULATIVE=0` is a
//! kill switch for when a measurement (or a bug) needs the plain path on a model
//! that has a head.
//!
//! Multimodal requests keep the plain path: `mtmd_eval_chunks` decodes the prompt
//! itself, so the drafter's `process` hook cannot see those batches, and
//! llama.cpp's MTP drafter ignores embedding batches anyway.
//!
//! # What it costs
//!
//! Not a second model. The draft head lives in the target GGUF and — on the
//! llama.cpp build `llama-cpp-2` 0.1.154 vendors (b10200, which predates upstream
//! #26296, "load MTP tensors only if they are really used") — its tensors are
//! loaded into memory **whether or not** anything drafts with them. That cost is
//! set by the pin, not by this module; using the head is what finally gets
//! something back for it.
//!
//! What this module *adds* is a second [`LlamaContext`] over the already-resident
//! model, holding a KV cache for the MTP layers alone (llama.cpp filters it to
//! `il >= n_layer`, so one layer's worth on Qwen3.5) plus its compute buffers,
//! and a `n_embd × n_batch` float buffer on the target context for the hidden
//! states the drafter consumes. It is **per request**, not resident: it is
//! created for a generation and dropped at the end of it.
//!
//! # Teardown
//!
//! That last point is the whole of this module's relationship with the teardown
//! chain described in [`crate::llama`] and [`crate::backend`]: it adds **no new
//! link to it**. The draft context is a stack local inside one request, it
//! borrows the model (`LlamaContext<'m>`), and the borrow checker will not let it
//! outlive that borrow — so "drafts before engines before backend" holds without
//! anything new to remember, release, or get wrong. Nothing is cached, so nothing
//! can be forgotten.
//!
//! Two smaller decisions serve the same end:
//!
//! * the draft context is built with [`LlamaModel::new_context`], **not**
//!   `new_context_with_ctx_other`. The latter hands llama.cpp a raw pointer to the
//!   target context, which some architectures retain and share KV memory through
//!   — and [`MtpSpeculative`] drops its target context *before* its draft context,
//!   so a draft context that aliased the target's memory would be torn down after
//!   the thing it points into. Declining the alias makes the order moot. The cost
//!   is that shared-memory MTP architectures (llama.cpp's `GEMMA4_ASSISTANT`)
//!   cannot draft here — llama.cpp rejects the context and we fall back cleanly.
//! * [`MtpSpeculative`] owns both contexts, so the draft context cannot be
//!   dropped independently of the target one, or leaked past it.

use std::sync::atomic::{AtomicU64, Ordering};

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::speculative::{MtpSpeculative, MtpSpeculativeParams};
use llama_cpp_2::token::LlamaToken;

use crate::engine::{EngineError, FinishReason};

/// How many tokens the MTP head is asked to propose per verification round.
///
/// llama.cpp's own default, and measured rather than inherited. On
/// Qwen3.8-27B-Q4 on an M5 Pro, median speedup over five interleaved pairs:
///
/// | width | code  | tool-call | prose | acceptance | extra RS memory |
/// |------:|------:|----------:|------:|-----------:|----------------:|
/// |     1 | 1.45× |     1.22× | 1.28× |     81–85% |         150 MiB |
/// |     2 | 1.29× |     1.24× |     — |     76–82% |         300 MiB |
/// |     3 | 1.50× |     1.35× | 1.26× |     54–71% |         449 MiB |
///
/// Wider proposals trade acceptance for coverage — a single trained `nextn`
/// block predicts one step, and reaching three means running it on its own
/// output — and they cost a wider verification batch plus one recurrent-state
/// snapshot each (the `n_rs_seq` note in [`Mtp::try_new`]). Three is where the
/// two curves crossed here. It is a per-model optimum, not a law: on
/// Qwen3.5-9B the same sweep could not separate the widths from measurement
/// noise, because at that size the drafter's own cost is a much larger share of
/// the round.
const DRAFT_MAX: i32 = 3;

// `MtpSpeculative::new` rejects `n_max <= 0`, and the verification batch is sized
// `DRAFT_MAX + 1` as a `usize` — pin both here rather than discovering either at
// runtime.
const _: () = assert!(DRAFT_MAX > 0 && DRAFT_MAX < i32::MAX);

/// [`DRAFT_MAX`] as the width llama.cpp's `n_rs_seq` wants. Infallible by the
/// `const` assertion above; the fallback is only there because `const` blocks
/// cannot participate in `?`.
fn draft_max_u32() -> u32 {
    u32::try_from(DRAFT_MAX).unwrap_or(0)
}

/// The context parameters a generation uses, speculative or not — the shape
/// `LlamaEngine::new_context` builds, restated here because a speculative
/// generation needs to derive two contexts from it rather than one.
fn base_params(n_ctx: u32) -> llama_cpp_2::context::params::LlamaContextParams {
    let n_ctx = std::num::NonZeroU32::new(n_ctx).unwrap_or(std::num::NonZeroU32::MIN);
    llama_cpp_2::context::params::LlamaContextParams::default().with_n_ctx(Some(n_ctx))
}

/// The environment variable that turns speculative decoding on.
const SWITCH: &str = "ROTEIRO_SPECULATIVE";

/// The sequence id llama.cpp's MTP wrapper binds itself to. Every batch that
/// reaches [`MtpSpeculative::process`] must use this one and no other — the
/// wrapper rejects anything else — and it is the same sequence the plain decode
/// path already uses.
const SEQ: i32 = 0;

/// Whether speculative decoding is switched on in this process.
///
/// Off unless `ROTEIRO_SPECULATIVE` asks for it; see [`switch_enables`] for why
/// that way round, and for the accepted spellings.
pub(crate) fn speculative_enabled() -> bool {
    switch_enables(std::env::var(SWITCH).ok().as_deref())
}

/// The pure decision behind [`speculative_enabled`], split out so it can be
/// tested without touching the process environment.
///
/// **Unset means off**, and that is the one judgement in this module that a
/// measurement decided rather than a principle. Speculative decoding is exact in
/// exact arithmetic, so "on by default" would have been right — but llama.cpp's
/// logits for a token are not identical at batch width 1 and width 4, and on the
/// hybrid Qwen3.5 family the gap is large enough to flip a greedy argmax at a
/// near-tie. `tests/batch_numerics.rs` measures the gap and
/// `tests/speculative.rs` catches the flips. So turning it on can change a
/// completion, and *that* is a decision to be taken deliberately rather than
/// inherited by upgrading.
///
/// Only an explicit, recognised "on" enables it: an unrecognised value is not
/// treated as consent, because the failure this default exists to prevent is
/// output changing without anyone having asked.
fn switch_enables(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes"
        )
    })
}

/// How many MTP (next-token-prediction) draft layers `model`'s GGUF ships, or
/// `0` for a model with no draft head.
///
/// Read from the GGUF's own `<arch>.nextn_predict_layers` key — the exact key
/// llama.cpp reads into `hparams.n_layer_nextn`, and the one that decides whether
/// it will build an MTP context at all. Doing the check here rather than by
/// attempting the context keeps a model without a head from logging llama.cpp's
/// "context type MTP requested but model doesn't contain MTP layers" warning on
/// every single request.
///
/// A model whose architecture cannot be read, or that records no such key, has no
/// draft head as far as this engine is concerned: `0`.
///
/// Works for both shapes the head ships in: a **bundled** GGUF records the key
/// alongside everything else, and a **split** `mtp-*.gguf` — which is a whole
/// model file carrying only the head's tensors — records it too, which is what
/// lets [`draft_gguf_beside`]'s find be confirmed rather than assumed.
#[must_use]
pub fn draft_head_layers(model: &LlamaModel) -> u32 {
    let Ok(arch) = model.meta_val_str("general.architecture") else {
        return 0;
    };
    model
        .meta_val_str(&format!("{arch}.nextn_predict_layers"))
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// The filename a **split** draft head is looked for under, beside the model it
/// drafts for.
pub(crate) const DRAFT_GGUF: &str = "mtp.gguf";

/// The split draft head installed beside `model_gguf`, if there is one.
///
/// Not every MTP model bundles its head. `ggml-org/Qwen3.8-27B-GGUF` — the shape
/// Roteiro's registry installs — ships it as a separate `mtp-*.gguf`, which is a
/// complete model file containing only the head's tensors (about 1.7 GB at `Q4_0`
/// for a 27B, against 19 GB for the target). The main GGUF of such a model
/// records no `nextn_predict_layers` at all, so without this the model would
/// simply fall back and the head would never be used.
///
/// Found by **convention rather than configuration**: `mtp.gguf` beside
/// `model.gguf`, exactly as `mmproj.gguf` already sits beside it for a
/// multimodal model. That keeps the registry, the config file and [`Served`]
/// untouched — installing the file is all it takes — and it is checked, not
/// trusted: the file still has to load and still has to report a draft head
/// before anything drafts with it.
///
/// [`Served`]: crate::llama::Served
#[must_use]
pub fn draft_gguf_beside(model_gguf: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidate = model_gguf.parent()?.join(DRAFT_GGUF);
    candidate.is_file().then_some(candidate)
}

/// Counters describing how a speculative generation actually went, so the
/// speedup can be attributed rather than merely observed.
///
/// Acceptance rate is the number that matters: it is what separates a
/// tool-call completion (highly predictable, most proposals stand) from prose
/// (few do), and a single averaged tok/s across both would hide exactly that.
#[derive(Debug, Default)]
pub(crate) struct SpecCounters {
    /// Generations that ran with a draft head attached.
    activations: AtomicU64,
    /// Verification rounds run — one batched target decode each.
    rounds: AtomicU64,
    /// Tokens the MTP head proposed, across all rounds.
    drafted: AtomicU64,
    /// Proposals the target model agreed with, and so did not have to be
    /// decoded one at a time.
    accepted: AtomicU64,
    /// Rounds where llama.cpp refused to produce a draft at all.
    draft_failures: AtomicU64,
}

impl SpecCounters {
    /// Note that a generation is about to run speculatively.
    pub(crate) fn activate(&self) {
        self.activations.fetch_add(1, Ordering::Relaxed);
    }

    /// Fold one round's outcome in.
    fn record(&self, drafted: usize, accepted: usize) {
        self.rounds.fetch_add(1, Ordering::Relaxed);
        self.drafted.fetch_add(drafted as u64, Ordering::Relaxed);
        self.accepted.fetch_add(accepted as u64, Ordering::Relaxed);
    }

    /// Note a round that got no draft out of llama.cpp. The round still runs —
    /// as a plain single-token decode — so this is a lost speedup rather than a
    /// failed request, and counting it is how that stays visible without this
    /// crate taking on a logging dependency it does not otherwise have.
    fn draft_failed(&self) {
        self.draft_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// A consistent-enough snapshot for reporting: the counters are `Relaxed`
    /// because nothing branches on them, only reports them.
    pub(crate) fn snapshot(&self) -> SpeculativeStats {
        SpeculativeStats {
            activations: self.activations.load(Ordering::Relaxed),
            rounds: self.rounds.load(Ordering::Relaxed),
            drafted: self.drafted.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            draft_failures: self.draft_failures.load(Ordering::Relaxed),
        }
    }
}

/// What speculative decoding actually did, for reporting rather than for
/// control flow.
///
/// The point of surfacing this is that **the speedup is not one number**.
/// Acceptance is high on code and on `<tool_call>` emission — structured output
/// the draft head predicts well — and low on prose, and a single averaged tok/s
/// across a mixed workload would hide precisely that difference. So the raw
/// counts are exposed and the interpretation is left to whoever is measuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpeculativeStats {
    /// Generations that ran with a draft head attached. `0` means every request
    /// so far took the plain path — because the model ships no head, or because
    /// `ROTEIRO_SPECULATIVE` turned it off.
    pub activations: u64,
    /// Verification rounds: one batched target decode each.
    pub rounds: u64,
    /// Tokens proposed by the draft head.
    pub drafted: u64,
    /// Proposals the target model agreed with. Each one is a token that did not
    /// need a decode of its own.
    pub accepted: u64,
    /// Rounds where llama.cpp produced no draft at all. Those rounds decoded a
    /// single token, exactly as the plain loop would, so this is lost speedup
    /// and not lost correctness — but a number that is not near-zero means the
    /// draft head is not doing what it is resident for.
    pub draft_failures: u64,
}

impl SpeculativeStats {
    /// The fraction of proposals the target model agreed with, or `None` when
    /// nothing has been proposed yet.
    ///
    /// This is the number that predicts the speedup: it is what differs between
    /// a tool call and a paragraph of prose.
    #[must_use]
    pub fn acceptance_rate(self) -> Option<f64> {
        (self.drafted > 0).then(|| {
            // Both counts are token tallies bounded by the generation length, so
            // the f64 conversion is exact for anything a process will really see.
            #[allow(clippy::cast_precision_loss)]
            {
                self.accepted as f64 / self.drafted as f64
            }
        })
    }
}

/// A target context paired with the MTP draft head of the very model it decodes.
///
/// Owns both llama.cpp contexts (through [`MtpSpeculative`]) for the duration of
/// one generation; see the [module docs](self) on why that ownership, rather than
/// a cache entry, is what keeps the teardown ordering honest.
pub(crate) struct Mtp<'m> {
    spec: MtpSpeculative<'m>,
    /// Every token committed to the target's KV cache so far — prompt plus each
    /// accepted token. llama.cpp's drafting API takes the running prefix on every
    /// call, so it is kept rather than reconstructed.
    prefix: Vec<LlamaToken>,
}

impl<'m> Mtp<'m> {
    /// Pair `target` with an MTP draft context over the same model.
    ///
    /// Returns `None` — never an error — when the pairing is not available, so
    /// every caller's fallback is the plain decode path rather than a failed
    /// request. That covers the kill switch, a GGUF with no draft head,
    /// llama.cpp declining to build an MTP context (an architecture that needs
    /// shared KV memory, or a head this build cannot use), and a speculative
    /// helper that fails to initialise.
    ///
    /// Builds **both** contexts, because the target context is not the same one
    /// a plain generation would use — see the `n_rs_seq` note below.
    ///
    /// `draft_model` is where the head lives: `None` for a **bundled** GGUF
    /// (Unsloth's `Qwen3.5-*-MTP-GGUF`), where the draft context is a second,
    /// MTP-typed view of tensors already resident in `model` and there is no
    /// second load at all; `Some` for a **split** one (`ggml-org`'s
    /// `mtp-*.gguf`), where the head is its own file and so its own
    /// [`LlamaModel`], which the engine keeps resident beside the target rather
    /// than re-reading it per request.
    pub(crate) fn try_new(
        backend: &llama_cpp_2::llama_backend::LlamaBackend,
        model: &'m LlamaModel,
        draft_model: Option<&'m LlamaModel>,
        n_ctx: u32,
    ) -> Option<Self> {
        use llama_cpp_2::context::params::LlamaContextType;

        let head = draft_model.unwrap_or(model);
        // llama.cpp's MTP drafter checks this with a `GGML_ASSERT`, which
        // **aborts the process** rather than returning an error — so a mismatched
        // pair has to be refused here, before the drafter is built. Only a split
        // head can be mismatched: a bundled one is the same model, so the widths
        // are equal by construction and this is free.
        if head.n_embd_out() != model.n_embd() {
            return None;
        }

        // `n_rs_seq = 0`: the MTP context holds an attention KV cache for the
        // draft layers only and has no recurrent state to roll back, which is
        // what llama.cpp's own MTP context construction asks for.
        let draft_params = base_params(n_ctx)
            .with_context_type(LlamaContextType::Mtp)
            .with_n_rs_seq(0);

        // llama.cpp declining here is expected for any model it will not draft
        // with, and it has already said why through the native-log bridge
        // `LlamaEngine` installs — so this is a fallback, not an error.
        let draft = head.new_context(backend, draft_params).ok()?;

        // `n_rs_seq = DRAFT_MAX`: the target keeps that many snapshots of its
        // recurrent state so a rejected proposal can be *rolled back*. Qwen3.5 is
        // a hybrid — attention KV plus an SSM state — and an SSM state cannot be
        // partially rewound after the fact, only restored from a snapshot. Without
        // this the first rejected proposal fails the rollback with "couldn't
        // remove partial sequence", which is llama.cpp saying exactly that.
        // llama.cpp clamps it back to 0 for architectures with no recurrent state
        // to snapshot, so it costs nothing on the models that do not need it.
        let target = model
            .new_context(backend, base_params(n_ctx).with_n_rs_seq(draft_max_u32()))
            .ok()?;

        let params = MtpSpeculativeParams {
            n_max: DRAFT_MAX,
            ..MtpSpeculativeParams::default()
        };
        MtpSpeculative::new(target, draft, params)
            .ok()
            .map(|spec| Self {
                spec,
                prefix: Vec::new(),
            })
    }

    /// The target context, for sampling and for the callers that only ever read
    /// logits from it.
    pub(crate) fn target(&self) -> &LlamaContext<'m> {
        self.spec.target_context()
    }

    /// Decode the prompt and hand the same batch to the drafter.
    ///
    /// The drafter needs the target's hidden state at **every** prompt position
    /// to have any idea what comes next, and it gets them by being shown the same
    /// batch the target just decoded. `begin` is called afterwards rather than
    /// before, so that the draft context has already caught up with the prompt
    /// when llama.cpp checks that it has.
    pub(crate) fn prime(
        &mut self,
        batch: &mut LlamaBatch,
        prompt: &[LlamaToken],
    ) -> Result<(), EngineError> {
        self.spec
            .target_context_mut()
            .decode(batch)
            .map_err(|e| EngineError::Inference(format!("prompt decode: {e}")))?;
        self.spec
            .process(batch)
            .map_err(|e| EngineError::Inference(format!("MTP prompt hook: {e}")))?;
        self.spec
            .begin(prompt)
            .map_err(|e| EngineError::Inference(format!("MTP begin: {e}")))?;
        self.prefix.clear();
        self.prefix.extend_from_slice(prompt);
        Ok(())
    }
}

/// The speculative sampling loop: the counterpart of `crate::llama`'s plain
/// `run_generation`, and deliberately the same shape so the two can be read side
/// by side.
///
/// One round is: ask the MTP head for up to [`DRAFT_MAX`] proposals, decode the
/// last committed token **and every proposal** in a single target batch, then
/// walk the resulting logits sampling one token per position and stopping at the
/// first proposal the sampler does not itself produce.
///
/// The accounting that makes this sound:
///
/// * `id_last` is a token that has been *sampled* but not yet *decoded*; it sits
///   at position `pos_last`. Every round decodes it, so the target's KV cache is
///   only ever asked to hold tokens that were really produced.
/// * a proposal is accepted only when [`LlamaSampler::sample`] at that position
///   returns the identical token — that is, only when the target model would have
///   emitted it anyway. Nothing else is emitted.
/// * the round emits `n_accepted + 1` tokens and calls the sampler exactly
///   `n_accepted + 1` times, in position order, with `accept` between them:
///   identical to `n_accepted + 1` turns of the plain loop.
/// * rejected proposals are removed from **both** KV caches before the next
///   round, so neither context ever attends to a token that was not emitted.
///
/// The degenerate case is the plain loop: when the head proposes nothing, the
/// round decodes one token, samples once, and emits one token.
pub(crate) fn run_generation(
    model: &LlamaModel,
    mtp: &mut Mtp<'_>,
    start_pos: i32,
    sampler: &mut LlamaSampler,
    max_tokens: u32,
    counters: &SpecCounters,
    on_token: &mut dyn FnMut(&str),
) -> Result<(u32, FinishReason), EngineError> {
    // One decoder for the whole run: a multi-byte character whose UTF-8 bytes
    // span two tokens is stitched across `token_to_piece` calls.
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    // Reused every round: the last committed token plus at most `DRAFT_MAX`
    // proposals.
    let mut batch = LlamaBatch::new(usize::try_from(DRAFT_MAX).unwrap_or(0) + 1, 1);

    let mut emitted = 0u32;
    let mut finish_reason = FinishReason::Length;
    let mut pos_last = start_pos;
    // The first token comes from the prompt's last logits — exactly as the plain
    // loop's first turn does.
    let mut id_last = sampler.sample(mtp.target(), -1);

    'generate: loop {
        // Emit `id_last`. This is the plain loop's body verbatim: accept, stop on
        // end-of-generation, then stream the exact detokenized piece.
        sampler.accept(id_last);
        if model.is_eog_token(id_last) {
            finish_reason = FinishReason::Stop;
            break;
        }
        let piece = model
            .token_to_piece(id_last, &mut decoder, false, None)
            .map_err(|e| EngineError::Inference(format!("detokenize: {e}")))?;
        on_token(&piece);
        emitted += 1;
        if emitted >= max_tokens {
            break;
        }

        // --- propose ------------------------------------------------------
        // Proposals are for the positions *after* `id_last`. A failure here is
        // not a failed request: an empty draft degrades this round to a plain
        // single-token decode.
        let drafts = mtp
            .spec
            .draft(pos_last, id_last, &mtp.prefix)
            .inspect_err(|_| counters.draft_failed())
            .unwrap_or_default();

        // --- verify -------------------------------------------------------
        // One target decode covering `id_last` and every proposal, with logits at
        // each so the sampler can be asked what the target would really produce
        // at that position.
        batch.clear();
        batch
            .add(id_last, pos_last, &[SEQ], true)
            .map_err(|e| EngineError::Inference(format!("verify batch: {e}")))?;
        for (i, token) in drafts.iter().enumerate() {
            let offset = i32::try_from(i).unwrap_or(i32::MAX);
            batch
                .add(*token, pos_last + 1 + offset, &[SEQ], true)
                .map_err(|e| EngineError::Inference(format!("verify batch: {e}")))?;
        }
        // The draft context wrote its own speculative cells at these positions
        // while proposing; clear them so `process` re-decodes the region rather
        // than appending a second set of cells the attention would also see.
        clear_from(mtp.spec.draft_context_mut(), pos_last, "draft")?;
        mtp.spec
            .target_context_mut()
            .decode(&mut batch)
            .map_err(|e| EngineError::Inference(format!("verify decode: {e}")))?;
        mtp.spec
            .process(&batch)
            .map_err(|e| EngineError::Inference(format!("MTP verify hook: {e}")))?;

        // --- accept -------------------------------------------------------
        // Sample one token per position, in order, accepting each into the
        // sampler before the next: the sampler sees the same call sequence it
        // would see decoding these tokens one at a time. The walk stops at the
        // first position where the sampler's own token differs from the
        // proposal — that token is the truth, and everything after it was
        // predicated on a wrong prefix.
        let target = mtp.spec.target_context();
        let (n_accepted, next) = accepted_prefix(&drafts, |idx, previous| {
            if let Some(token) = previous {
                sampler.accept(token);
            }
            sampler.sample(target, i32::try_from(idx).unwrap_or(i32::MAX))
        });
        // The walk deliberately leaves its *last* token unaccepted: it becomes
        // `id_last`, and the top of the next turn accepts it — the same place the
        // plain loop does.
        counters.record(drafts.len(), n_accepted);

        // Tell llama.cpp how much of its draft stood, so it carries the hidden
        // state of the right position into the next proposal. Only legal when
        // there was a draft pending.
        if !drafts.is_empty() {
            let accepted = u16::try_from(n_accepted).unwrap_or(u16::MAX);
            mtp.spec
                .accept(accepted)
                .map_err(|e| EngineError::Inference(format!("MTP accept: {e}")))?;
        }

        // --- commit -------------------------------------------------------
        // The accepted proposals *are* the tokens the sampler produced, so they
        // are emitted here rather than re-derived. They were already
        // `sampler.accept`ed above, in position order, so this pass only streams
        // and counts.
        for token in &drafts[..n_accepted] {
            if model.is_eog_token(*token) {
                finish_reason = FinishReason::Stop;
                break 'generate;
            }
            let piece = model
                .token_to_piece(*token, &mut decoder, false, None)
                .map_err(|e| EngineError::Inference(format!("detokenize: {e}")))?;
            on_token(&piece);
            emitted += 1;
            if emitted >= max_tokens {
                break 'generate;
            }
        }

        // Drop every rejected proposal from both caches. `pos_last + n_accepted`
        // is the last position that was really committed; `next` will be decoded
        // at the position after it, on the next round.
        let committed_end = pos_last + i32::try_from(n_accepted).unwrap_or(i32::MAX) + 1;
        clear_from(mtp.spec.target_context_mut(), committed_end, "target")?;
        clear_from(mtp.spec.draft_context_mut(), committed_end, "draft")?;

        mtp.prefix.push(id_last);
        mtp.prefix.extend_from_slice(&drafts[..n_accepted]);
        pos_last = committed_end;
        id_last = next;
    }

    Ok((emitted, finish_reason))
}

/// Walk a round's proposals against what the target model actually samples, and
/// return `(proposals accepted, the first token the sampler produced that was
/// not proposed)`.
///
/// This is the correctness invariant in one function, which is why it is split
/// out from the decode plumbing and tested on its own. `sample(idx, previous)`
/// must (a) feed `previous` — the token accepted at the preceding position, if
/// any — back into the sampler, then (b) sample position `idx` of the
/// verification batch. So the sampler is driven exactly as the plain loop drives
/// it: one sample per emitted token, in position order, with an `accept` in
/// between.
///
/// The returned token is *not* accepted: it has been sampled but not yet
/// decoded, and it is the caller's `id_last` for the next round — which is where
/// it gets accepted, exactly as the plain loop accepts its freshly sampled token
/// at the top of a turn. The walk always ends by returning a token, so a round
/// yields `accepted + 1` tokens and `accepted + 1` sampler calls even when every
/// proposal stands.
fn accepted_prefix(
    drafts: &[LlamaToken],
    mut sample: impl FnMut(usize, Option<LlamaToken>) -> LlamaToken,
) -> (usize, LlamaToken) {
    let mut accepted = 0usize;
    let mut previous = None;
    loop {
        let token = sample(accepted, previous);
        match drafts.get(accepted) {
            // The target model would have produced this token here anyway, so
            // the proposal costs nothing and changes nothing.
            Some(&drafted) if drafted == token => {
                accepted += 1;
                previous = Some(token);
            }
            // Either the proposal was wrong (everything after it was predicated
            // on a prefix that never happened) or there are no more proposals.
            _ => return (accepted, token),
        }
    }
}

/// Drop everything at or after `pos` from `ctx`'s KV cache for the one sequence
/// this path uses.
///
/// A negative `pos` cannot happen (positions only ever advance from `start_pos ≥
/// 0`), but it is rejected rather than silently coerced, because the sign is what
/// distinguishes "from here on" from llama.cpp's "everything".
fn clear_from(ctx: &mut LlamaContext<'_>, pos: i32, which: &str) -> Result<(), EngineError> {
    let from = u32::try_from(pos)
        .map_err(|_| EngineError::Inference(format!("negative {which} rollback position {pos}")))?;
    ctx.kv_cache_seq_rm(SEQ, Some(from), None)
        .map_err(|e| EngineError::Inference(format!("{which} KV rollback: {e}")))
}

#[cfg(test)]
mod tests {
    use super::{accepted_prefix, switch_enables};
    use llama_cpp_2::token::LlamaToken;

    /// A stand-in for the sampler + target model: `truth` is what the target
    /// would emit at each position, and the recorded calls are what the sampler
    /// was actually asked to do. No model, no GPU — this is the mechanism, and
    /// it runs in CI.
    struct FakeSampler {
        truth: Vec<i32>,
        /// `(batch index sampled, token accepted immediately before it)`.
        calls: Vec<(usize, Option<i32>)>,
    }

    impl FakeSampler {
        fn new(truth: &[i32]) -> Self {
            Self {
                truth: truth.to_vec(),
                calls: Vec::new(),
            }
        }

        fn run(&mut self, drafts: &[i32]) -> (usize, i32) {
            let drafts: Vec<LlamaToken> = drafts.iter().copied().map(LlamaToken).collect();
            let truth = self.truth.clone();
            let calls = &mut self.calls;
            let (accepted, next) = accepted_prefix(&drafts, |idx, previous| {
                calls.push((idx, previous.map(|t| t.0)));
                LlamaToken(truth[idx])
            });
            (accepted, next.0)
        }
    }

    #[test]
    fn a_wrong_proposal_stops_the_round_at_the_target_token() {
        // The target would emit 10, 11, 12; the head proposed 10, 99, 12. The
        // second proposal is wrong, so the round yields one accepted proposal
        // and the target's own token (11) — never the wrong 99, and never the
        // 12 that was only right by accident on a prefix that never happened.
        let mut s = FakeSampler::new(&[10, 11, 12, 13]);
        assert_eq!(s.run(&[10, 99, 12]), (1, 11));
    }

    #[test]
    fn every_proposal_standing_still_costs_one_extra_sample() {
        // All three proposals match, so the round also samples the position
        // after them: four tokens produced from one batched decode.
        let mut s = FakeSampler::new(&[10, 11, 12, 13]);
        assert_eq!(s.run(&[10, 11, 12]), (3, 13));
        assert_eq!(s.calls.len(), 4, "one sample per token produced");
    }

    #[test]
    fn no_proposals_degrades_to_the_plain_loop() {
        // An empty draft must behave exactly like a plain single-token turn:
        // one sample, one token, nothing accepted.
        let mut s = FakeSampler::new(&[10]);
        assert_eq!(s.run(&[]), (0, 10));
        assert_eq!(s.calls, vec![(0, None)]);
    }

    #[test]
    fn a_wrong_first_proposal_accepts_nothing() {
        let mut s = FakeSampler::new(&[10, 11]);
        assert_eq!(s.run(&[99, 11]), (0, 10));
        assert_eq!(s.calls, vec![(0, None)], "no sampling past the mismatch");
    }

    /// The invariant the whole technique rests on: the sampler is driven
    /// **identically** to the plain loop. One `sample` per token produced, in
    /// position order, each preceded by an `accept` of the token before it — so a
    /// stateful sampler (an RNG stream, a repetition penalty, a grammar) cannot
    /// tell the two paths apart.
    #[test]
    fn the_sampler_sees_the_plain_loops_call_sequence() {
        let mut s = FakeSampler::new(&[10, 11, 12, 13]);
        let (accepted, next) = s.run(&[10, 11, 99]);
        assert_eq!((accepted, next), (2, 12));
        assert_eq!(
            s.calls,
            vec![(0, None), (1, Some(10)), (2, Some(11))],
            "positions in order, each accepting the one before it"
        );
        // Exactly `accepted + 1` sampler calls for `accepted + 1` tokens.
        assert_eq!(s.calls.len(), accepted + 1);
    }

    #[test]
    fn speculation_is_off_unless_explicitly_asked_for() {
        // Unset is off: turning it on can change a completion (llama.cpp's logits
        // are not identical across batch widths), so it is opted into.
        assert!(!switch_enables(None));
        for on in ["1", "on", "true", "yes", "ON", " true "] {
            assert!(switch_enables(Some(on)), "{on} should enable");
        }
        // Anything unrecognised is *not* consent — including the spellings that
        // would read as "off" and a typo that would otherwise silently opt in.
        for off in ["0", "off", "false", "no", "", "yes please", "2"] {
            assert!(!switch_enables(Some(off)), "{off:?} should not enable");
        }
    }
}
