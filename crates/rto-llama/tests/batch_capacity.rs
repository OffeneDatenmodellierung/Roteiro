//! An over-long prompt must come back as an error, not take the process with it
//! (issue #346).
//!
//! llama.cpp bounds how many tokens may be submitted in one batch, and it
//! enforces that bound with a `GGML_ASSERT` — which calls `ggml_abort` and
//! terminates the process. There is no Rust error to catch: a `roteiro serve`
//! that reaches it dies mid-request, taking the graph API and the web UI with it,
//! and an ordinary long chat message was enough to do it.
//!
//! Before the fix, a **2608-token** prompt — comfortably inside the 4096-token
//! context the engine advertises, but past llama.cpp's default 2048-token logical
//! batch — produced
//!
//! ```text
//! llama-context.cpp:1768: GGML_ASSERT(n_tokens_all <= cparams.n_batch) failed
//! ```
//!
//! and exit status 134, with the HTTP client seeing a dropped connection rather
//! than a response. That gap — input the server accepts by its own advertised
//! limit and then dies on — is the defect; a prompt that overran the *context*
//! would not have shown it.
//!
//! Two bounds are checked here, because llama.cpp has two and they are not the
//! same number:
//!
//! * **generative** models decode through `llama_context::decode`, which splits a
//!   logical batch into physical pieces itself. Their bound is `n_batch`.
//! * **encoder-only** (BERT/`bge-*`) embedding models are routed to
//!   `llama_context::encode`, where llama.cpp's own comment says "micro-batching
//!   is not possible for non-causal encoding, so we process the batch in a single
//!   shot". Their bound is `n_ubatch` — 512 by default, four times tighter — and
//!   it aborts at a *different* assert, `llama-context.cpp:1421`.
//!
//! These need the `llama` feature **and** a GGUF on disk, so they are
//! `#[ignore]`d and self-skip with a printed reason when the model is absent —
//! CI compiles them under `--all-features` without running them. The arithmetic
//! and the wording of the refusal are pinned by model-free unit tests in
//! `src/llama.rs`, which *do* run in CI; what these add is that the guard is
//! actually wired into both request paths and that llama.cpp agrees about where
//! the bound sits.
//!
//! ```text
//! cargo test -p rto-llama --features llama --test batch_capacity -- --ignored --nocapture
//! ```
#![cfg(feature = "llama")]

use std::path::PathBuf;

use rto_llama::llama::{LlamaEngine, Served};
use rto_llama::{ChatRequest, Engine, EngineError, Message};

/// A small generative model — the bound is a property of the context, not of the
/// model, so the cheapest one to load will do.
const TEXT_MODEL: &str = "smolvlm-500m-gguf";

/// A BERT-family embedding model, for the tighter encoder bound.
const EMBED_MODEL: &str = "bge-large-en-v1.5";

/// Words in the prompt that must be *refused*: past the 4096-token context, so
/// it is over the bound on any reasonable configuration.
const OVERLONG_WORDS: usize = 6000;

/// Words in the prompt that must be *served*: past llama.cpp's default
/// 2048-token logical batch — the width that used to abort — and inside the
/// 4096-token context, with room left to generate.
///
/// [`filler`] emits one common word per token, so this is close to a token
/// count; the test asserts on the engine's own reported `prompt_tokens` rather
/// than trusting the estimate.
const SERVED_WORDS: usize = 2600;

/// The default model store (`~/.roteiro/models/<name>/model.gguf`).
fn model_gguf(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join("model.gguf");
    path.exists().then_some(path)
}

/// An engine serving `name` alone, or `None` (with a printed skip) when the model
/// is not installed.
fn engine_for(name: &str) -> Option<LlamaEngine> {
    let Some(path) = model_gguf(name) else {
        eprintln!("SKIP: need `{name}` under ~/.roteiro/models (`roteiro model pull {name}`)");
        return None;
    };
    LlamaEngine::new(
        vec![Served {
            name: name.to_owned(),
            path,
            mmproj: None,
        }],
        0,
    )
    .ok()
}

/// `n` words of ordinary English filler.
///
/// The words are deliberately short and common — every one of them is a single
/// token in the BPE vocabularies these models ship, so the word count and the
/// token count stay within a few percent of each other. An earlier version of
/// this file generated `word0 word1 word2 …`, which looks like one token per word
/// and is in fact about four and a half: digits tokenize separately. That
/// mistake matters here, because the whole claim being tested is that a prompt
/// *inside* the advertised context used to abort — a prompt that also overran the
/// context would not have shown that.
fn filler(n: usize) -> String {
    const WORDS: [&str; 8] = ["the", "and", "for", "with", "that", "from", "this", "have"];
    (0..n)
        .map(|i| WORDS[i % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

/// The chat path: an over-long prompt is refused as an invalid request, and — the
/// part that actually matters — the process is still here afterwards to be asked
/// a second, shorter question.
#[test]
#[ignore = "needs a GGUF under ~/.roteiro/models"]
fn an_overlong_chat_prompt_is_refused_and_the_process_survives() {
    let Some(engine) = engine_for(TEXT_MODEL) else {
        return;
    };

    let request = |content: String| ChatRequest {
        tools: None,
        model: TEXT_MODEL.to_owned(),
        messages: vec![Message {
            role: "user".to_owned(),
            content,
        }],
        images: vec![],
        audio: vec![],
        temperature: 0.0,
        max_tokens: 8,
    };

    let err = engine
        .chat(&request(filler(OVERLONG_WORDS)))
        .expect_err("a prompt past the batch bound must be refused, not decoded");
    assert!(
        matches!(err, EngineError::InvalidRequest(_)),
        "must be a client error (400), not a 500: {err:?}"
    );
    eprintln!("refusal: {err}");

    // Before the fix this line was unreachable — the process was gone. A short
    // prompt succeeding afterwards is what distinguishes "refused the request"
    // from "broke the server politely".
    let after = engine
        .chat(&request("Say hi.".to_owned()))
        .expect("the engine still serves after refusing an over-long prompt");
    assert!(
        after.prompt_tokens > 0,
        "a following request must really run: {after:?}"
    );
}

/// The embeddings path, whose bound is the tighter encoder one — an input well
/// under the generative limit still has to be refused rather than aborting.
#[test]
#[ignore = "needs a GGUF under ~/.roteiro/models"]
fn an_overlong_embedding_input_is_refused_and_the_process_survives() {
    let Some(engine) = engine_for(EMBED_MODEL) else {
        return;
    };

    // 700 words is far below the generative bound and far above the encoder one,
    // so this fails only if the *right* bound is being applied.
    let err = engine
        .embed(EMBED_MODEL, &[filler(700)])
        .expect_err("an input past the encoder bound must be refused, not encoded");
    assert!(
        matches!(err, EngineError::InvalidRequest(_)),
        "must be a client error (400), not a 500: {err:?}"
    );
    eprintln!("refusal: {err}");

    let after = engine
        .embed(EMBED_MODEL, &["a short sentence".to_owned()])
        .expect("the engine still embeds after refusing an over-long input");
    assert_eq!(after.len(), 1, "one vector per input");
    assert!(!after[0].is_empty(), "the vector must be real");
}

/// A prompt that fits must still be *served*, not merely not-crash. Guards that
/// are set too tight are the obvious way to "fix" an abort while breaking the
/// feature, and nothing above would catch that.
#[test]
#[ignore = "needs a GGUF under ~/.roteiro/models"]
fn a_prompt_past_the_old_2048_token_bound_now_succeeds() {
    let Some(engine) = engine_for(TEXT_MODEL) else {
        return;
    };
    // Over llama.cpp's default 2048-token logical batch, under the 4096-token
    // context. This is the band that used to abort.
    let completion = engine
        .chat(&ChatRequest {
            tools: None,
            model: TEXT_MODEL.to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: filler(SERVED_WORDS),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 8,
        })
        .expect("a prompt inside the advertised context must be served");
    eprintln!(
        "served {} prompt tokens, {} completion tokens",
        completion.prompt_tokens, completion.completion_tokens
    );
    assert!(
        completion.prompt_tokens > 2048,
        "this test is only meaningful past the old bound, got {}",
        completion.prompt_tokens
    );
}

/// What raising `n_batch` from llama.cpp's default to `n_ctx` actually costs, in
/// memory. **Prints rather than asserting a threshold** — the numbers are a
/// property of llama.cpp, the backend and the model, and a limit baked in here
/// would be a claim about all three that this file cannot make. What it does
/// assert is that the two configurations really differed, so a silent no-op
/// cannot be read as "free".
///
/// The reason the answer is small is structural, and worth stating because it is
/// what makes the change defensible rather than merely convenient:
///
/// * `n_ubatch` — the *physical* batch — is what sizes the compute graph, and it
///   is left at llama.cpp's 512. The graph does not grow.
/// * the logits/embeddings output buffer is reserved from the number of tokens
///   actually flagged for output (`output_reserve(n_outputs_all)`), not from
///   `n_batch`. Generation flags one token, so it does not grow either.
/// * what does scale with `n_batch` is the batch allocator's per-token
///   bookkeeping, including `output_ids`, at a handful of bytes per token.
///
/// ```text
/// cargo test -p rto-llama --features llama --test batch_capacity -- --ignored --nocapture measure
/// ```
#[test]
#[ignore = "needs a GGUF under ~/.roteiro/models; prints a measurement"]
fn measure_what_a_wider_batch_costs() {
    use std::num::NonZeroU32;

    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::model::LlamaModel;
    use llama_cpp_2::model::params::LlamaModelParams;

    const N_CTX: u32 = 4096;
    /// Contexts built per arm: one reading cannot resolve tens of KiB against a
    /// ~160 MiB allocation, and the spread across repeats is the evidence for
    /// saying so rather than merely asserting it.
    const REPS: usize = 5;

    let Some(path) = model_gguf(TEXT_MODEL) else {
        eprintln!("SKIP: need `{TEXT_MODEL}` under ~/.roteiro/models");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");

    // Resident-set size of this process, in KiB, via `ps` — the workspace forbids
    // `unsafe_code`, which rules out asking the kernel directly.
    let rss_kib = || -> u64 {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };

    let ctx_of = |n_batch: Option<u32>| {
        let base = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(N_CTX).expect("nonzero")));
        let params = match n_batch {
            Some(n) => base.with_n_batch(n),
            None => base,
        };
        model.new_context(&backend, params).expect("context builds")
    };

    // One arm: build a context, note what llama.cpp settled on, and how much
    // resident memory appeared while it existed.
    let arm = |n_batch: Option<u32>| -> (u32, u32, Vec<i64>) {
        let (mut widths, mut costs) = ((0, 0), Vec::with_capacity(REPS));
        for _ in 0..REPS {
            let before = rss_kib();
            let ctx = ctx_of(n_batch);
            widths = (ctx.n_batch(), ctx.n_ubatch());
            let after = rss_kib();
            drop(ctx);
            costs.push(
                i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(0),
            );
        }
        costs.sort_unstable();
        (widths.0, widths.1, costs)
    };

    // Before: `n_batch` unset, so llama.cpp's 2048 default, clamped to n_ctx.
    let (narrow_logical, narrow_physical, narrow_costs) = arm(None);
    // After: `n_batch` asked for as the full context, as `base_params` now does.
    let (wide_logical, wide_physical, wide_costs) = arm(Some(N_CTX));

    let median = |v: &[i64]| v[v.len() / 2];
    let spread = |v: &[i64]| v[v.len() - 1] - v[0];
    eprintln!(
        "n_ctx={N_CTX}, {REPS} repeats per arm, RSS in KiB\n\
         before: n_batch={narrow_logical} n_ubatch={narrow_physical}  median +{}  spread {}  {narrow_costs:?}\n\
         after:  n_batch={wide_logical} n_ubatch={wide_physical}  median +{}  spread {}  {wide_costs:?}\n\
         median delta: {:+} KiB, against a within-arm spread of {} KiB",
        median(&narrow_costs),
        spread(&narrow_costs),
        median(&wide_costs),
        spread(&wide_costs),
        median(&wide_costs) - median(&narrow_costs),
        spread(&narrow_costs).max(spread(&wide_costs)),
    );

    // The point of the change: the batch is now as wide as the advertised window.
    assert_eq!(
        wide_logical, N_CTX,
        "the wider context must accept a full window"
    );
    assert!(
        narrow_logical < wide_logical,
        "nothing was measured — the default was already {narrow_logical}"
    );
    // And the physical batch — the one that sizes the compute graph, and so the
    // one that would have made this expensive — did not move. This is the real
    // assertion of the test; the RSS figures above are context for reading it.
    assert_eq!(
        narrow_physical, wide_physical,
        "n_ubatch must be untouched; it is what would make this expensive"
    );
}
