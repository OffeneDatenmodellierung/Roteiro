//! Does a saved sequence state carry a hybrid model's **recurrent** half?
//!
//! Issue #578 wants prefix reuse: `serve` builds a fresh context and re-prefills
//! the whole prompt every request, so a multi-turn agent client pays for its
//! byte-identical preamble on every turn. llama.cpp exposes the primitives
//! (`llama_state_seq_get_data_ext` / `…_set_data_ext`, wrapped here as
//! `state_seq_get` / `state_seq_set`), so the question is not availability.
//!
//! It is correctness, and on this model that is not obvious. `qwen3.8-27b` is
//! `general.architecture = qwen35` with `full_attention_interval = 4` over
//! `block_count = 64`: **16 full-attention layers carrying KV, 48 Gated Delta Net
//! (SSM) layers carrying fixed recurrent state.** If the state API serialises
//! only the KV, a restore silently rebuilds three quarters of the model from
//! zero — no error, just wrong output — and #578's whole approach is dead until
//! llama.cpp changes.
//!
//! Two tests, cheap first:
//!
//! 1. [`measure_whether_the_sequence_state_includes_the_recurrent_module`] reads
//!    the state size. KV is ~64 KiB/token; the recurrent module is a fixed block
//!    (~150 MiB, allocated at one cell regardless of `n_ctx`). The two are far
//!    enough apart that the size alone says which is present.
//! 2. [`a_restored_sequence_state_continues_identically`] is the actual proof:
//!    greedy continuation after a restore must match greedy continuation from a
//!    full prefill, token for token. A size that looks right but a continuation
//!    that diverges is the dangerous outcome, and only this test sees it.
//!
//! Both are `#[ignore]`d and self-skip when the model is absent, like the other
//! instruments here.
//!
//! ```sh
//! cargo test -p rto-llama --features llama --test state_reuse -- --ignored --nocapture
//! ```
//!
//! # Result, 7 Sep 2026, `qwen3.8-27b` on macOS/Metal
//!
//! **Both pass. The state carries the recurrent module, and a restore is exact.**
//!
//! ```text
//! prompt tokens        : 201
//! state_seq size       :      162.2 MiB
//! KV alone would be    :       12.6 MiB
//!
//! baseline : [271, 5844, 11, 6105, 11, 321, 13358, 13, 248044, …]
//! restored : [271, 5844, 11, 6105, 11, 321, 13358, 13, 248044, …]
//!
//! identical for all 16 continued tokens, rendering as
//! "\n\nRed, blue, and yellow.<|endoftext|>…"
//! ```
//!
//! So prefix reuse is possible here, which #578 could not assume. But the size
//! is the other half of the finding, and it is the half that constrains a
//! design: `162.2 - 12.6` leaves a **fixed ~149.6 MiB recurrent block, paid on
//! every snapshot whatever the prompt length.** A 13-token preamble snapshots to
//! ~150 MiB just as a 6,600-token one snapshots to ~564 MiB.
//!
//! That inverts the usual cache economics. There is no cheap small entry, so a
//! prefix cache here wants to hold **few, large, long-lived** preambles — which
//! is exactly the agent-client shape #578 measures, and exactly wrong for a
//! general-purpose per-conversation cache. Sizing any eviction policy off prompt
//! length alone would under-count every entry by ~150 MiB.

#![cfg(feature = "llama")]

use std::num::NonZeroU32;
use std::path::PathBuf;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::session::LlamaStateSeqFlags;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;

const BIG_MODEL: &str = "qwen3.8-27b";
/// Derived, not measured: 16 attention layers x 4 KV heads x (256 + 256) x 2 B.
const KV_BYTES_PER_TOKEN: usize = 64 * 1024;
/// `llama_memory_recurrent` on this model, allocated at one cell whatever `n_ctx`
/// is. Approximate on purpose — the test only needs to tell ~0 from ~150 MiB.
const RECURRENT_BYTES_APPROX: usize = 150 * 1024 * 1024;
const N_CTX: u32 = 4096;

/// One backend for the whole binary.
///
/// llama.cpp's backend is a process-global — the second `LlamaBackend::init()`
/// in a process returns `BackendAlreadyInitialized` (issue #296, and the reason
/// `shared_backend.rs` exists). Both tests here need one and the harness runs
/// them in the same process, so initialising per test fails whichever runs
/// second. `context_window.rs` has the same shape and only works because its
/// instruments are run one at a time.
fn backend() -> &'static LlamaBackend {
    static BACKEND: std::sync::OnceLock<LlamaBackend> = std::sync::OnceLock::new();
    BACKEND.get_or_init(|| LlamaBackend::init().expect("backend initialises once"))
}

/// Two 27B loads must not overlap on one machine, and the harness would happily
/// run these concurrently.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn model_gguf(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join("model.gguf");
    path.exists().then_some(path)
}

fn context_params() -> LlamaContextParams {
    LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(N_CTX).expect("nonzero")))
}

/// Feed `tokens` starting at `from`, leaving logits on the last one.
fn feed(ctx: &mut llama_cpp_2::context::LlamaContext, tokens: &[LlamaToken], from: i32) {
    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    let last = tokens.len() - 1;
    for (i, token) in tokens.iter().enumerate() {
        let pos = from + i32::try_from(i).expect("fits i32");
        batch
            .add(*token, pos, &[0], i == last)
            .expect("batch accepts the token");
    }
    ctx.decode(&mut batch).expect("decodes");
}

fn argmax(logits: &[f32]) -> usize {
    logits
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |(bi, bv), (i, v)| {
            if *v > bv { (i, *v) } else { (bi, bv) }
        })
        .0
}

/// Greedy-continue `n` tokens from a context whose last decode left logits at
/// `logits_at`.
fn continue_greedy(
    ctx: &mut llama_cpp_2::context::LlamaContext,
    mut logits_at: i32,
    mut pos: i32,
    n: usize,
) -> Vec<LlamaToken> {
    let mut out = Vec::with_capacity(n);
    let mut batch = LlamaBatch::new(1, 1);
    for _ in 0..n {
        let id = argmax(ctx.get_logits_ith(logits_at));
        let token = LlamaToken(i32::try_from(id).expect("vocab fits i32"));
        out.push(token);
        batch.clear();
        batch.add(token, pos, &[0], true).expect("batch");
        ctx.decode(&mut batch).expect("decode");
        logits_at = 0;
        pos += 1;
    }
    out
}

#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn measure_whether_the_sequence_state_includes_the_recurrent_module() {
    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let backend = backend();
    let model = LlamaModel::load_from_file(backend, &path, &LlamaModelParams::default())
        .expect("model loads");
    let prompt = model
        .str_to_token(&"the quick brown fox. ".repeat(40), AddBos::Always)
        .expect("tokenises");
    let mut ctx = model
        .new_context(backend, context_params())
        .expect("context builds");
    feed(&mut ctx, &prompt, 0);

    let size = ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty());
    let kv_only = prompt.len() * KV_BYTES_PER_TOKEN;
    // Byte counts here are a few hundred MiB at most — nowhere near f64's 52-bit
    // mantissa — and the figure is a printed diagnostic, not a comparison the
    // test asserts on.
    #[allow(clippy::cast_precision_loss)]
    let mib = |b: usize| b as f64 / (1024.0 * 1024.0);

    eprintln!("  prompt tokens        : {}", prompt.len());
    eprintln!("  state_seq size       : {:>10.1} MiB", mib(size));
    eprintln!("  KV alone would be    : {:>10.1} MiB", mib(kv_only));
    eprintln!(
        "  recurrent alone      : {:>10.1} MiB (approx, fixed)",
        mib(RECURRENT_BYTES_APPROX)
    );
    eprintln!(
        "  VERDICT              : {}",
        if size > kv_only + RECURRENT_BYTES_APPROX / 2 {
            "state is far larger than KV — recurrent state appears INCLUDED"
        } else {
            "state is about KV-sized — recurrent state appears MISSING"
        }
    );
    eprintln!(
        "  (a size that looks right still proves nothing on its own — see \
         `a_restored_sequence_state_continues_identically`)"
    );
}

#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; the decisive experiment for #578"]
fn a_restored_sequence_state_continues_identically() {
    const CONTINUE: usize = 16;

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let backend = backend();
    let model = LlamaModel::load_from_file(backend, &path, &LlamaModelParams::default())
        .expect("model loads");

    // A byte-identical preamble, then a turn-specific suffix — the shape #578 is
    // about, rather than an arbitrary split.
    let preamble = model
        .str_to_token(
            "You are a careful assistant. Answer briefly and never guess. ",
            AddBos::Always,
        )
        .expect("tokenises");
    let suffix = model
        .str_to_token(
            "User: name three primary colours.\nAssistant:",
            AddBos::Never,
        )
        .expect("tokenises");

    // Baseline: one context, whole prompt, greedy continuation.
    let baseline = {
        let mut ctx = model
            .new_context(backend, context_params())
            .expect("context builds");
        let whole: Vec<LlamaToken> = preamble.iter().chain(&suffix).copied().collect();
        feed(&mut ctx, &whole, 0);
        let last = i32::try_from(whole.len() - 1).expect("fits i32");
        let pos = i32::try_from(whole.len()).expect("fits i32");
        continue_greedy(&mut ctx, last, pos, CONTINUE)
    };

    // Snapshot after the preamble only, in one context.
    let snapshot = {
        let mut ctx = model
            .new_context(backend, context_params())
            .expect("context builds");
        feed(&mut ctx, &preamble, 0);
        ctx.state_seq_get(0, LlamaStateSeqFlags::empty())
            .expect("sequence state is readable")
    };

    // Restore into a *fresh* context, feed only the suffix, continue.
    let restored = {
        let mut ctx = model
            .new_context(backend, context_params())
            .expect("context builds");
        ctx.state_seq_set(&snapshot, 0)
            .expect("sequence state is restorable");
        let from = i32::try_from(preamble.len()).expect("fits i32");
        feed(&mut ctx, &suffix, from);
        let last = i32::try_from(suffix.len() - 1).expect("fits i32");
        let pos = from + i32::try_from(suffix.len()).expect("fits i32");
        continue_greedy(&mut ctx, last, pos, CONTINUE)
    };

    // Token ids rather than rendered text: the ids are what the assertion is
    // about, and rendering could normalise away a difference that matters.
    let ids = |ts: &[LlamaToken]| ts.iter().map(|t| t.0).collect::<Vec<_>>();
    eprintln!("  baseline : {:?}", ids(&baseline));
    eprintln!("  restored : {:?}", ids(&restored));

    assert_eq!(
        baseline, restored,
        "a restored sequence state must continue exactly as a full prefill does.\n\
         Divergence here means the state did not carry the 48 Gated Delta Net \
         layers' recurrent state, only the 16 attention layers' KV — which is \
         silent, and fatal to prefix reuse on this model."
    );
}
