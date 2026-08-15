//! One llama.cpp backend per process, shared by every engine (issue #296).
//!
//! Before this, `LlamaEngine::new*` called `LlamaBackend::init()` itself, and
//! llama.cpp's backend is a process-global: the second engine in a process got
//! `BackendAlreadyInitialized`, and since every production call site swallows
//! that with `.ok()`, the second modality was **silently inert** rather than
//! loudly broken.
//!
//! **These tests are not `#[ignore]`d and need no model and no GPU.** An engine
//! over an empty served set loads nothing — construction only attaches to the
//! backend — so this runs for real in CI's `cargo test --workspace
//! --all-features` on Ubuntu, which is where a regression would otherwise slip
//! through unnoticed. The heavier "both modalities actually infer" pass lives in
//! `rto-graph`'s `extract` tests, which need the GGUFs and self-skip without
//! them.
#![cfg(feature = "llama")]

use std::sync::{Mutex, MutexGuard, PoisonError};

use rto_llama::backend::release_shared_backend;
use rto_llama::llama::LlamaEngine;

/// The backend under test is a process-global and these tests both build and
/// release it, so the harness's default parallelism would let one test's release
/// land inside another's engine lifetime. Every test takes this first, making the
/// binary's use of the global strictly sequential.
static SERIAL: Mutex<()> = Mutex::new(());

/// Enter the exclusive section, and start it from a known-clean state.
///
/// A poisoned lock only means an earlier test panicked; the backend is a global
/// either way, so recover rather than cascade the failure into every later test.
fn exclusive() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let _released = release_shared_backend();
    guard
}

/// An engine that serves nothing: it attaches to the backend and loads no model,
/// which is exactly the part of construction issue #296 broke.
fn engine() -> anyhow::Result<LlamaEngine> {
    LlamaEngine::new(Vec::new(), 0)
}

#[test]
fn a_second_engine_in_one_process_builds_instead_of_going_inert() {
    let _serial = exclusive();

    let first = engine().expect("the first engine builds");
    // The regression, exactly: this used to be `Err(BackendAlreadyInitialized)`,
    // which `vlm_engine`/`asr_engine`/`spec draft` turn into `None` — a modality
    // that is quietly missing rather than reported.
    let second = engine();
    assert!(
        second.is_ok(),
        "a second engine must share the first's backend, not fail: {:?}",
        second.err()
    );

    drop(second);
    drop(first);
    assert!(
        release_shared_backend(),
        "with both engines gone the backend is releasable"
    );
}

#[test]
fn concurrent_construction_starts_the_backend_exactly_once() {
    let _serial = exclusive();

    // Eight threads racing to be the first engine. If more than one reached
    // `llama_backend_init`, all but one would fail — so "every thread got an
    // engine" *is* the single-initialisation assertion at this level (the
    // mechanism itself is pinned without llama.cpp by
    // `rto_llama::slot`'s `concurrent_try_init_callers_initialise_once`).
    let engines: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8).map(|_| scope.spawn(engine)).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker thread panicked"))
            .collect()
    });
    for (i, built) in engines.iter().enumerate() {
        assert!(
            built.is_ok(),
            "thread {i} got no engine: {:?}",
            built.as_ref().err()
        );
    }

    drop(engines);
    assert!(release_shared_backend());
}

#[test]
fn the_backend_outlives_every_engine_that_borrows_it() {
    let _serial = exclusive();

    let held = engine().expect("engine builds");
    // Releasing while an engine is alive must be impossible, not merely
    // discouraged: this is #292's teardown ordering ("engines first, backend
    // last") expressed as ownership. A `false` here is the mechanism declining.
    assert!(
        !release_shared_backend(),
        "the backend must not be freed under a live engine"
    );

    // ...and the backend the engine holds is still the process's backend, so a
    // new engine attaches to that one rather than trying a second init.
    let joiner = engine();
    assert!(
        joiner.is_ok(),
        "a later engine still attaches: {:?}",
        joiner.err()
    );

    drop(joiner);
    drop(held);
    assert!(
        release_shared_backend(),
        "the last engine gone, it releases"
    );
}

#[test]
fn releasing_is_idempotent_and_leaves_the_process_able_to_start_again() {
    let _serial = exclusive();

    drop(engine().expect("engine builds"));
    assert!(release_shared_backend(), "the resident backend is released");
    assert!(
        !release_shared_backend(),
        "a second release has nothing to free, so every exit path may call it"
    );

    // A released backend is a *stopped* backend, not a poisoned one — the next
    // engine (a fresh `roteiro` command in an embedder, say) starts a new one.
    assert!(
        engine().is_ok(),
        "the process can start a backend again after releasing one"
    );
    assert!(release_shared_backend());
}
