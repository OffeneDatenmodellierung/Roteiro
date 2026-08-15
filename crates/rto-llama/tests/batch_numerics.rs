//! Does llama.cpp give the same logits for a token whether it is decoded alone
//! or inside a batch? (Issue #320.)
//!
//! This asks a question about **llama.cpp**, not about Roteiro, and it is here
//! because the answer decides how far the speculative-decoding invariant can
//! honestly be stated.
//!
//! Speculative decoding is sound *in exact arithmetic*: every emitted token is
//! sampled from the target model's own distribution, so the drafter cannot change
//! the result (`rto_llama::speculative` sets out why, and its unit tests pin the
//! sampler's call sequence). But the target model verifies proposals in a **batch
//! of up to four**, where plain decoding runs a **batch of one** — and a GPU
//! backend does not promise those two produce bit-identical floats. Different
//! widths take different kernels: a matrix-vector product and a matrix-matrix
//! product accumulate in a different order, and on a hybrid model the recurrent
//! scan does too.
//!
//! So this decodes the same tokens twice, one at a time and all at once, and
//! reports whether the logits and the greedy argmax agree. It **prints** rather
//! than asserting a tolerance: the answer is a property of llama.cpp, the
//! backend, and the model, and a threshold baked in here would be a claim about
//! all three that this file is not in a position to make. The one thing it does
//! assert is that both paths ran, so a silent no-op cannot be read as agreement.
//!
//! ```text
//! export ROTEIRO_MTP_TEST_GGUF=/path/to/model.gguf
//! cargo test -p rto-llama --features llama --test batch_numerics -- --ignored --nocapture
//! ```
#![cfg(feature = "llama")]

use std::num::NonZeroU32;
use std::path::PathBuf;

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;

/// The GGUF to probe. Any model will do — the question is about batch width, not
/// about draft heads — but pointing it at the same model the speculative tests
/// use is what makes the two sets of numbers comparable.
const GGUF_ENV: &str = "ROTEIRO_MTP_TEST_GGUF";

/// How many tokens to decode after the prompt. Matches the widest verification
/// batch a speculative round builds (`DRAFT_MAX + 1`).
const WIDTH: usize = 4;

/// Decode `prompt`, then `follow`, and return the logits at each of `follow`'s
/// positions. `batched` decides whether `follow` goes in as one batch or as one
/// batch per token — which is the entire subject of this file.
fn logits_for(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &[LlamaToken],
    follow: &[LlamaToken],
    batched: bool,
) -> Vec<Vec<f32>> {
    let params =
        LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(4096).expect("nonzero")));
    let mut ctx: LlamaContext<'_> = model
        .new_context(backend, params)
        .expect("context builds for the probe");

    let mut batch = LlamaBatch::new(prompt.len().max(WIDTH), 1);
    let last = prompt.len() - 1;
    for (i, token) in prompt.iter().enumerate() {
        batch
            .add(*token, i32::try_from(i).expect("prompt fits i32"), &[0], i == last)
            .expect("prompt batch");
    }
    ctx.decode(&mut batch).expect("prompt decodes");

    let base = i32::try_from(prompt.len()).expect("prompt fits i32");
    let mut out = Vec::with_capacity(follow.len());
    if batched {
        batch.clear();
        for (i, token) in follow.iter().enumerate() {
            let offset = i32::try_from(i).expect("width fits i32");
            batch.add(*token, base + offset, &[0], true).expect("batch");
        }
        ctx.decode(&mut batch).expect("batched decode");
        for i in 0..follow.len() {
            out.push(ctx.get_logits_ith(i32::try_from(i).expect("width fits i32")).to_vec());
        }
    } else {
        for (i, token) in follow.iter().enumerate() {
            let offset = i32::try_from(i).expect("width fits i32");
            batch.clear();
            batch.add(*token, base + offset, &[0], true).expect("batch");
            ctx.decode(&mut batch).expect("single decode");
            out.push(ctx.get_logits_ith(0).to_vec());
        }
    }
    out
}

/// The model's own greedy continuation of `prompt`, `n` tokens long, decoded one
/// at a time — i.e. exactly the plain decode path.
fn greedy_continuation(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &[LlamaToken],
    n: usize,
) -> Vec<LlamaToken> {
    let params =
        LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(4096).expect("nonzero")));
    let mut ctx = model
        .new_context(backend, params)
        .expect("context builds for the probe");

    let mut batch = LlamaBatch::new(prompt.len().max(1), 1);
    let last = prompt.len() - 1;
    for (i, token) in prompt.iter().enumerate() {
        batch
            .add(*token, i32::try_from(i).expect("fits i32"), &[0], i == last)
            .expect("prompt batch");
    }
    ctx.decode(&mut batch).expect("prompt decodes");

    let mut out = Vec::with_capacity(n);
    let base = i32::try_from(prompt.len()).expect("fits i32");
    // The prompt batch put logits on its last entry only; every batch after it
    // holds a single token, so the readable index is 0 from then on.
    let mut logits_at = i32::try_from(last).expect("fits i32");
    for pos in (base..).take(n) {
        let (id, _) = argmax_and_margin(ctx.get_logits_ith(logits_at));
        logits_at = 0;
        let token = LlamaToken(i32::try_from(id).expect("vocab fits i32"));
        out.push(token);
        batch.clear();
        batch.add(token, pos, &[0], true).expect("batch");
        ctx.decode(&mut batch).expect("decode");
    }
    out
}

/// The greedy pick and the top-two margin of one position's logits: the margin is
/// what says whether a divergence is a coin-flip on a near-tie or a real
/// disagreement about which token comes next.
fn argmax_and_margin(logits: &[f32]) -> (usize, f32) {
    let mut best = (0usize, f32::NEG_INFINITY);
    let mut second = f32::NEG_INFINITY;
    for (i, &v) in logits.iter().enumerate() {
        if v > best.1 {
            second = best.1;
            best = (i, v);
        } else if v > second {
            second = v;
        }
    }
    (best.0, best.1 - second)
}

#[test]
#[ignore = "probe: needs a GGUF and answers a question about llama.cpp, not a regression"]
fn one_at_a_time_versus_all_at_once() {
    let Some(raw) = std::env::var_os(GGUF_ENV) else {
        eprintln!("SKIP: set {GGUF_ENV} to a GGUF path");
        return;
    };
    let path = PathBuf::from(raw);
    assert!(path.exists(), "{} does not exist", path.display());

    let backend = LlamaBackend::init().expect("backend starts");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");

    let prompt = model
        .str_to_token(
            "Write a Rust function that returns the sum of a slice of integers.",
            AddBos::Always,
        )
        .expect("prompt tokenizes");

    // Probe with the model's *own* greedy continuation. Feeding arbitrary tokens
    // would measure its behaviour off its own distribution, which is not the
    // situation a speculative round is in: the proposals it verifies are, by
    // construction, tokens the model finds likely.
    let follow = greedy_continuation(&backend, &model, &prompt, WIDTH);

    let single = logits_for(&backend, &model, &prompt, &follow, false);
    let batched = logits_for(&backend, &model, &prompt, &follow, true);
    assert_eq!(single.len(), WIDTH, "the one-at-a-time path ran");
    assert_eq!(batched.len(), WIDTH, "the batched path ran");

    eprintln!("model: {}", path.display());
    eprintln!(
        "{:>3}  {:>12}  {:>12}  {:>12}  {:>10}  {:>10}",
        "pos", "max |Δlogit|", "argmax(1)", "argmax(4)", "margin(1)", "same?"
    );
    let mut disagreements = 0;
    for (i, (a, b)) in single.iter().zip(batched.iter()).enumerate() {
        let delta = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        let (ia, ma) = argmax_and_margin(a);
        let (ib, _) = argmax_and_margin(b);
        if ia != ib {
            disagreements += 1;
        }
        eprintln!("{i:>3}  {delta:>12.6}  {ia:>12}  {ib:>12}  {ma:>10.6}  {:>10}", ia == ib);
    }
    eprintln!(
        "greedy pick differs at {disagreements}/{WIDTH} positions in this sample; \
         a non-zero max |Δlogit| with zero disagreements means the arithmetic differs \
         but the argmax has not (yet) flipped"
    );

    drop(model);
    drop(backend);
}
