//! Concurrency stress + verification for the llama.cpp engine (#18a).
//!
//! These tests exercise the per-model concurrency introduced in [`LlamaEngine`]:
//! the cache lock is released before generation, so requests to *different*
//! resident models decode concurrently, while requests to the *same* model
//! instance serialise on that model's `gen_lock`.
//!
//! They need the `llama` feature (a real llama.cpp build) **and** two small GGUF
//! models on disk, so they are `#[ignore]`d — CI compiles them under
//! `--all-features` but does not run them, and they self-skip when the models are
//! absent. Run locally against `~/.roteiro/models` with:
//!
//! ```text
//! cargo test -p rto-llama --features llama --test concurrency -- --ignored --nocapture
//! ```
//!
//! The cross-model test is the honest gate on the open question the design flags:
//! whether concurrent decode across two models is stable on the Metal backend. If
//! it ever crashes or corrupts output here, the fix is to revert to a single
//! shared generation lock and document why — not to force concurrency.
#![cfg(feature = "llama")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rto_llama::llama::{LlamaEngine, Served};
use rto_llama::{ChatRequest, Engine, Message};

/// Two small text models kept resident together, keyed by registry name.
const MODEL_A: &str = "qwen3-0.6b";
const MODEL_B: &str = "qwen2.5-0.5b-instruct";

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

/// Build an engine serving both text models, with a budget large enough to keep
/// both resident (1 GiB) so cross-model requests hold *distinct* generation
/// locks. Returns `None` (with a printed skip) if either model is missing.
fn engine_with_both() -> Option<Arc<LlamaEngine>> {
    let (Some(a), Some(b)) = (model_gguf(MODEL_A), model_gguf(MODEL_B)) else {
        eprintln!(
            "SKIP: need `{MODEL_A}` and `{MODEL_B}` under ~/.roteiro/models \
             (run `roteiro model pull <name>`)"
        );
        return None;
    };
    let served = vec![
        Served {
            name: MODEL_A.to_owned(),
            path: a,
            mmproj: None,
        },
        Served {
            name: MODEL_B.to_owned(),
            path: b,
            mmproj: None,
        },
    ];
    // Small context to keep the runs quick; 1 GiB budget holds both models.
    let engine = LlamaEngine::new_with_budget(served, 512, 1 << 30).expect("engine inits");
    Some(Arc::new(engine))
}

/// A short, deterministic (temperature 0) chat request for `model`.
fn req(model: &str) -> ChatRequest {
    ChatRequest {
        tools: None,
        model: model.to_owned(),
        messages: vec![Message {
            role: "user".to_owned(),
            content: "Reply with the single word: ok".to_owned(),
        }],
        images: Vec::new(),
        audio: Vec::new(),
        temperature: 0.0,
        max_tokens: 16,
    }
}

#[test]
#[ignore = "needs the `llama` feature and two GGUF models on disk; slow + Metal-dependent"]
fn concurrent_requests_across_models_do_not_deadlock_or_corrupt() {
    let Some(engine) = engine_with_both() else {
        return;
    };

    // Warm both models resident (single-threaded) so the stress loop exercises
    // steady-state concurrent decode rather than concurrent loads.
    for m in [MODEL_A, MODEL_B] {
        engine.chat(&req(m)).expect("warm-up completes");
    }

    // Hammer both models from several threads at once. Each request must return a
    // non-empty completion with sane token accounting; a crash/corruption on the
    // Metal backend would surface as a panic, hang, or empty/garbled result here.
    let threads = 8;
    let per_thread = 4;
    let ok = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for t in 0..threads {
        let engine = Arc::clone(&engine);
        let ok = Arc::clone(&ok);
        // Alternate which model each thread favours so same- and cross-model
        // contention overlap.
        let model = if t % 2 == 0 { MODEL_A } else { MODEL_B };
        handles.push(std::thread::spawn(move || {
            for _ in 0..per_thread {
                let out = engine.chat(&req(model)).expect("completion");
                assert!(!out.content.trim().is_empty(), "empty content from {model}");
                assert!(out.completion_tokens > 0, "no tokens from {model}");
                ok.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }
    assert_eq!(ok.load(Ordering::Relaxed), threads * per_thread);
}

#[test]
#[ignore = "needs the `llama` feature and two GGUF models on disk; slow + Metal-dependent"]
fn distinct_models_decode_concurrently() {
    let Some(engine) = engine_with_both() else {
        return;
    };
    // Warm both resident so we time steady-state decode, not loading.
    for m in [MODEL_A, MODEL_B] {
        engine.chat(&req(m)).expect("warm-up completes");
    }

    // Baseline: the two requests back-to-back on one thread.
    let serial = {
        let start = Instant::now();
        engine.chat(&req(MODEL_A)).expect("A");
        engine.chat(&req(MODEL_B)).expect("B");
        start.elapsed()
    };

    // Concurrent: the same two requests on two threads. With the cache lock
    // released before generation and distinct per-model locks, they overlap, so
    // wall-clock should fall well below the serial sum. A generous 0.85 factor
    // tolerates scheduling noise while still failing if generation were globally
    // serialised (which would leave concurrent ≈ serial).
    let concurrent = {
        let start = Instant::now();
        let e1 = Arc::clone(&engine);
        let e2 = Arc::clone(&engine);
        let h1 = std::thread::spawn(move || e1.chat(&req(MODEL_A)).expect("A"));
        let h2 = std::thread::spawn(move || e2.chat(&req(MODEL_B)).expect("B"));
        h1.join().expect("A thread");
        h2.join().expect("B thread");
        start.elapsed()
    };

    eprintln!("serial={serial:?} concurrent={concurrent:?}");
    assert!(
        concurrent.as_secs_f64() < serial.as_secs_f64() * 0.85,
        "expected cross-model concurrency to beat serial: serial={serial:?} concurrent={concurrent:?}"
    );
}
