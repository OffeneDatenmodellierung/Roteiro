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

/// llama.cpp's backend is a process-global: `LlamaBackend::init()` fails with
/// `BackendAlreadyInitialized` while one is live (issue #296).
///
/// Both tests here need one, and the harness runs them **in parallel by
/// default** — which is what actually breaks, not the process lifetime.
/// `LlamaBackend`'s `Drop` clears the flag and calls `llama_backend_free`, so a
/// second `init()` succeeds once the first backend is gone. Serialising is
/// therefore enough, and each test can own its backend and let it drop while
/// still holding this lock.
///
/// That is better than caching one in a `static`: the backend would then live
/// until process exit, against the teardown ordering `backend.rs` documents
/// (#291/#292), where the backend must be freed *after* everything borrowing it.
/// Here nothing outlives the test that made it.
///
/// It also keeps two 27B model loads from overlapping on one machine.
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
    context_params_at(N_CTX)
}

fn context_params_at(n_ctx: u32) -> LlamaContextParams {
    LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(n_ctx).expect("nonzero")))
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
    // Backend created *inside* the lock and dropped before it is released, so it
    // never outlives the test that owns it.
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");
    let prompt = model
        .str_to_token(&"the quick brown fox. ".repeat(40), AddBos::Always)
        .expect("tokenises");
    let mut ctx = model
        .new_context(&backend, context_params())
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
    // Backend created *inside* the lock and dropped before it is released, so it
    // never outlives the test that owns it.
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
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
            .new_context(&backend, context_params())
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
            .new_context(&backend, context_params())
            .expect("context builds");
        feed(&mut ctx, &preamble, 0);
        ctx.state_seq_get(0, LlamaStateSeqFlags::empty())
            .expect("sequence state is readable")
    };

    // Restore into a *fresh* context, feed only the suffix, continue.
    let restored = {
        let mut ctx = model
            .new_context(&backend, context_params())
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

/// Can a snapshot cross window sizes, or is it welded to the `n_ctx` it was taken
/// at?
///
/// This is #578's question 1 in one measurement. Roteiro sizes a context to the
/// request (`window_for_request`), which is what makes a short question cheap. If
/// a restore requires the destination to match the source's `n_ctx`, then a cache
/// entry can only serve requests that happen to want the same window — which for
/// per-request sizing is almost none of them — and prefix reuse and per-request
/// sizing genuinely cannot coexist. The binding's own docs say `SizeMismatch`
/// "covers shape mismatches (different `n_ctx`, `n_layer`, quantization, etc.)",
/// so the pessimistic answer is the documented one and worth testing rather than
/// assuming in either direction.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; the decisive experiment for #578's question 1"]
fn a_snapshot_restores_into_a_context_of_a_different_size() {
    const CONTINUE: usize = 16;

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");

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

    // Snapshot once, at the window a cache would have happened to build it at.
    let (snapshot, taken_at) = {
        let mut ctx = model
            .new_context(&backend, context_params_at(4096))
            .expect("context builds");
        feed(&mut ctx, &preamble, 0);
        let size = ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty());
        (
            ctx.state_seq_get(0, LlamaStateSeqFlags::empty())
                .expect("sequence state is readable"),
            size,
        )
    };

    // The reference answer, produced the way the engine does it today.
    let baseline = {
        let mut ctx = model
            .new_context(&backend, context_params_at(4096))
            .expect("context builds");
        let whole: Vec<LlamaToken> = preamble.iter().chain(&suffix).copied().collect();
        feed(&mut ctx, &whole, 0);
        let last = i32::try_from(whole.len() - 1).expect("fits i32");
        let pos = i32::try_from(whole.len()).expect("fits i32");
        continue_greedy(&mut ctx, last, pos, CONTINUE)
    };

    // A larger window, a smaller one, and the source's own — the three cases a
    // per-request-sized engine would actually ask for.
    for target in [8192_u32, 2048, 4096] {
        let mut ctx = model
            .new_context(&backend, context_params_at(target))
            .expect("context builds");

        // **A refusal fails the test, at every window.** An earlier draft tolerated
        // one at 8192 — written before the answer was known, when growing past the
        // source's own window looked like the case llama.cpp would reject. It does
        // not, and the tolerance would have let exactly the regression this test
        // exists to catch pass silently. Raised in review of #578.
        //
        // The assertion below is not redundant with this one, and an injection
        // showed why: grafting a snapshot taken from a *different, empty* sequence
        // is **accepted** rather than refused, and produces a diverged
        // continuation. `state_seq_set` validates less than its `SizeMismatch`
        // documentation suggests, so "it restored" is not "it restored the right
        // thing" — the refusal check and the identity check catch different
        // failures and both are load-bearing.
        ctx.state_seq_set(&snapshot, 0).unwrap_or_else(|e| {
            panic!(
                "n_ctx {target}: refused ({e}) — a snapshot welded to the window it \
                 was taken at cannot coexist with per-request sizing, which is what \
                 `prefix_cache` is built on"
            )
        });

        let from = i32::try_from(preamble.len()).expect("fits i32");
        feed(&mut ctx, &suffix, from);
        let last = i32::try_from(suffix.len() - 1).expect("fits i32");
        let pos = from + i32::try_from(suffix.len()).expect("fits i32");
        let got = continue_greedy(&mut ctx, last, pos, CONTINUE);
        let same = got == baseline;
        eprintln!(
            "  n_ctx {target:>5}: restored, continuation {}",
            if same { "IDENTICAL" } else { "DIVERGED" }
        );
        assert!(
            same,
            "a restore that succeeds must be correct — silently wrong is worse than refused.\n\
             baseline={baseline:?}\ngot={got:?}"
        );
    }
    eprintln!("  snapshot bytes at n_ctx 4096: {taken_at}");
}

/// End to end, through the engine: a cached second turn must answer *identically*
/// to an uncached one, and faster.
///
/// The two halves are both load-bearing and neither alone is enough. Faster and
/// different is a silent regression — the worst outcome this feature can have,
/// since nothing would fail. Identical and no faster means the cache is inert and
/// the wiring is decorative. So this asserts sameness and prints the saving.
///
/// The shape is #578's own workload: a large system preamble both turns share,
/// and a short question that differs.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn a_cached_second_turn_answers_identically_and_faster() {
    use rto_llama::engine::{ChatRequest, Engine, Message};
    use rto_llama::llama::{LlamaEngine, Served};

    // **Three turns, not two.** The boundary is learned by comparing consecutive
    // prompts, so turn 1 has nothing to compare against, turn 2 is where the
    // shared preamble first becomes visible and is snapshotted, and turn 3 is the
    // first that can hit. A 30-turn session therefore pays full price twice — the
    // benefit is not immediate, and a test that only ran two turns would conclude
    // the feature does not work.
    //
    // The questions differ. Identical prompts share *everything*, which makes the
    // boundary the whole prompt — correctly declined, since a fully restored
    // prompt would leave no token to carry logits.
    const Q: [&str; 3] = [
        "Name three primary colours.",
        "Name two primary colours.",
        "Name four primary colours.",
    ];

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Comfortably over `MIN_PREFIX_TOKENS`, and the kind of thing an agent client
    // actually re-sends: standing instructions that never vary.
    let preamble = "You are a meticulous assistant working inside a large \
        repository. Answer briefly. Never guess. Cite what you relied on. "
        .repeat(60);
    let turn = |question: &str| ChatRequest {
        model: BIG_MODEL.to_owned(),
        messages: vec![
            Message {
                role: "system".to_owned(),
                content: preamble.clone(),
            },
            Message {
                role: "user".to_owned(),
                content: question.to_owned(),
            },
        ],
        images: Vec::new(),
        audio: Vec::new(),
        tools: None,
        temperature: 0.0,
        max_tokens: 24,
    };
    let served = || {
        vec![Served {
            name: BIG_MODEL.to_owned(),
            path: path.clone(),
            mmproj: None,
        }]
    };
    let run = |engine: &LlamaEngine, q: &str| {
        let started = std::time::Instant::now();
        let out = engine.chat(&turn(q)).expect("generation succeeds");
        (out.content, out.prompt_tokens, started.elapsed())
    };

    // Without the cache — the behaviour today, and the reference answer.
    let (plain_third, prompt_tokens, plain_elapsed) = {
        let engine = LlamaEngine::new(served(), 0).expect("engine builds");
        run(&engine, Q[0]);
        run(&engine, Q[1]);
        run(&engine, Q[2])
    };

    // With it: turn 2 snapshots the shared preamble, turn 3 restores it.
    let (cached_third, cached_elapsed, entries, held) = {
        let engine = LlamaEngine::new(served(), 0)
            .expect("engine builds")
            .with_prefix_cache_bytes(1 << 30);
        run(&engine, Q[0]);
        run(&engine, Q[1]);
        let (content, _, elapsed) = run(&engine, Q[2]);
        let (entries, held) = engine.prefix_cache_stats();
        (content, elapsed, entries, held)
    };
    // `LlamaEngine` attaches to the process's *shared* backend (issue #296) rather
    // than owning one, and a shared backend is not freed by dropping the engine —
    // `release_shared_backend` is explicit by design, so that teardown cannot run
    // ahead of something still borrowing it. Without this call the three
    // `LlamaBackend::init()` instruments beside this one are refused with
    // `BackendAlreadyInitialized` for the rest of the binary, which is exactly
    // what happened the first time all four ran together.
    //
    // Before the assertions, so a failure releases it too.
    assert!(
        rto_llama::backend::release_shared_backend(),
        "the release declines while anything still holds the backend, so `false` \
         here is the leak itself rather than a tidy-up that was not needed"
    );

    eprintln!("  prompt tokens        : {prompt_tokens}");
    eprintln!("  third turn, no cache : {plain_elapsed:?}");
    eprintln!("  third turn, cached   : {cached_elapsed:?}");
    eprintln!(
        "  cache holds          : {entries} entry, {} MiB",
        held / (1024 * 1024)
    );

    assert_eq!(
        entries, 1,
        "the second turn must have snapshotted the shared preamble"
    );
    assert_eq!(
        cached_third, plain_third,
        "a restored preamble must not change a single token of the answer"
    );
    assert!(
        cached_elapsed < plain_elapsed,
        "a cache that is not faster is only a memory cost: {cached_elapsed:?} vs {plain_elapsed:?}"
    );
}
