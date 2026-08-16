//! An over-long prompt must come back as an error, not take the process with it
//! (issue #346).
//!
//! llama.cpp bounds how many tokens may be submitted in one batch, and it
//! enforces that bound with a `GGML_ASSERT` — which calls `ggml_abort` and
//! terminates the process. There is no Rust error to catch: a `roteiro serve`
//! that reaches it dies mid-request, taking the graph API and the web UI with it,
//! and an ordinary long chat message was enough to do it. Before the fix, driving
//! a ~2600-token prompt at a server with a 4096-token context produced
//!
//! ```text
//! llama-context.cpp:1768: GGML_ASSERT(n_tokens_all <= cparams.n_batch) failed
//! ```
//!
//! and exit status 134, with the HTTP client seeing a dropped connection rather
//! than a response.
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

/// More tokens than any default batch this could be run against, and still
/// inside the 4096-token context the engine advertises — which is the whole
/// point: this is input the server claims to accept.
const OVERLONG_WORDS: usize = 3000;

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

/// `n` plain words — one token or so each, and no special tokens to complicate
/// the count.
fn long_text(n: usize) -> String {
    (0..n)
        .map(|i| format!("word{i}"))
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
        .chat(&request(long_text(OVERLONG_WORDS)))
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
        .embed(EMBED_MODEL, &[long_text(700)])
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
    // ~2600 words: over llama.cpp's default 2048-token logical batch, under the
    // 4096-token context. This is the exact input that used to abort.
    let completion = engine
        .chat(&ChatRequest {
            model: TEXT_MODEL.to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: long_text(2600),
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
