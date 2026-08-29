//! The multimodal projector is loaded **once per model**, not once per blob
//! (issue #301).
//!
//! `chat_media` used to call `MtmdContext::init_from_file` on every request, so a
//! sync over N media files re-read the 688 MB Voxtral `mmproj` N times and built
//! N clip contexts to throw away. The projector is now cached in the residency
//! entry of the model it is bound to, and this is the end-to-end check of that:
//! several clips through one engine, one initialisation.
//!
//! The count is what this asserts on, not the clock. The reloads cost system CPU
//! rather than wall-clock on a host that keeps the `mmap`ed projector in its page
//! cache, so a timing assertion here would be measuring the weather.
//!
//! **Three things are asserted, and one of them is the process's own exit code.**
//!
//! 1. `LlamaEngine::projector_inits` — the count, which is the whole subject.
//! 2. The completions themselves. A cache that changes what the model produces is
//!    a bug and not a speedup, so the clips must still transcribe, and the two
//!    fixtures that carry identical samples in different containers must still
//!    transcribe identically — through the *reused* projector, which is exactly
//!    where a stale-state bug would show up.
//! 3. The **exit status of this test binary**, as in issues #292 and #298. A
//!    cached projector is one more native object that must die before ggml-metal's
//!    global destructors run at `exit()`; park it anywhere Rust never drops (a
//!    `static`, or a handle that outlives the engine) and `ggml_metal_rsets_free`
//!    finds a non-empty residency set and `abort()`s — SIGABRT, exit 134, *after*
//!    every test here has printed `ok`. So this test drops its engine and then
//!    asserts the shared backend releases; the binary's own exit code is what
//!    proves the projector went with it.
//!
//! The mechanism underneath — build-once per key, per-key isolation, release,
//! release-when-never-initialised — is pinned without a GPU or a model in
//! `rto_llama::slot`'s unit tests, which Ubuntu CI runs on the default feature
//! set. The two-projectors-in-one-process case needs both GGUFs and lives in
//! `rto-graph`'s `extract` tests, beside the vision fixtures it needs. This file
//! **self-skips** when the audio GGUF is not installed, so CI compiles it under
//! `--all-features` and prints a skip line instead of failing.
//!
//! ```text
//! roteiro model pull voxtral-mini-3b
//! cargo test -p rto-llama --features llama --test projector_cache -- --nocapture
//! ```
#![cfg(feature = "llama")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rto_llama::backend::release_shared_backend;
use rto_llama::llama::{LlamaEngine, Served};
use rto_llama::{ChatRequest, Engine, Message};

/// Registry name of the audio model (`~/.roteiro/models/<name>/`).
const MODEL: &str = "voxtral-mini-3b";

/// Committed audio fixtures (`crates/rto-graph/tests/fixtures/audio/`), embedded
/// at compile time so a renamed one is a build error rather than a panic inside a
/// test about projector residency.
///
/// The first two are the same samples in different containers — WAV and FLAC —
/// which `audio_ingest.rs` already establishes must transcribe identically. Here
/// that pair does its work through a *reused* projector, so it doubles as the
/// "the cache did not change the output" assertion.
///
/// The third is **silence**, and it is silence rather than the `syllables` clip
/// for a reason worth recording: the pair that must come back *different* has to
/// be one this model actually hears differently. Asked for 64 tokens, Voxtral
/// answers both the tone and the speech-shaped syllables with the same
/// "I think it's a good idea" loop — a real property of a 3B model given
/// wordless audio, not a decode fault — whereas digital silence sends it off into
/// a different invention entirely. `audio_ingest.rs` picks silence-versus-tone
/// for the same check and for the same reason.
const TONE_WAV: &[u8] =
    include_bytes!("../../rto-graph/tests/fixtures/audio/tone-500hz-16khz-mono-256ms.wav");
const TONE_FLAC: &[u8] =
    include_bytes!("../../rto-graph/tests/fixtures/audio/tone-500hz-16khz-mono-256ms.flac");
const SILENCE_WAV: &[u8] =
    include_bytes!("../../rto-graph/tests/fixtures/audio/silence-16khz-mono-256ms.wav");

/// The llama.cpp backend is a process-global and this binary builds and releases
/// it, so the harness's default parallelism would let one test's release land
/// inside another's engine lifetime. Every test takes this first.
static SERIAL: Mutex<()> = Mutex::new(());

/// Enter the exclusive section from a known-clean state. A poisoned lock only
/// means an earlier test panicked; recover rather than cascade.
fn exclusive() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let _released = release_shared_backend();
    guard
}

/// A file in the default model store (`~/.roteiro/models/<name>/<file>`), or
/// `None` when it is not installed.
fn model_file(name: &str, file: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join(file);
    path.exists().then_some(path)
}

/// The model's GGUF pair as a [`Served`] entry, or `None` (with a skip line) when
/// it is not installed.
fn served() -> Option<Served> {
    let (Some(path), Some(mmproj)) = (
        model_file(MODEL, "model.gguf"),
        model_file(MODEL, "mmproj.gguf"),
    ) else {
        eprintln!("SKIP: `{MODEL}` not installed (run `roteiro model pull {MODEL}`)");
        return None;
    };
    Some(Served {
        name: MODEL.to_owned(),
        path,
        mmproj: Some(mmproj),
    })
}

/// Transcribe one clip through `engine` — the production shape, byte-for-byte
/// what `rto-graph`'s extractor sends for an audio blob.
fn transcribe(engine: &LlamaEngine, clip: &[u8]) -> String {
    let completion = engine
        .chat(&ChatRequest {
            tools: None,
            model: MODEL.to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "Transcribe this audio recording. Output only the spoken words, verbatim."
                    .to_owned(),
            }],
            images: Vec::new(),
            audio: vec![clip.to_vec()],
            temperature: 0.0,
            max_tokens: 64,
        })
        .expect("the clip reaches the projector and completes");
    completion.content
}

#[test]
fn several_clips_through_one_engine_load_the_projector_once() {
    let _serial = exclusive();
    let Some(served) = served() else {
        return;
    };

    let engine = LlamaEngine::new(vec![served], 0).expect("engine builds");
    assert_eq!(
        engine.projector_inits(),
        0,
        "a fresh engine has loaded no projector: it is built on the first media blob, \
         not at construction"
    );

    // Four blobs, one modality — the shape of a `roteiro sync` over a tree of
    // clips, which is what paid the ~5 s projector load four times over.
    let tone_wav = transcribe(&engine, TONE_WAV);
    let tone_flac = transcribe(&engine, TONE_FLAC);
    let silence = transcribe(&engine, SILENCE_WAV);
    let tone_again = transcribe(&engine, TONE_WAV);
    eprintln!("tone(wav): {tone_wav:?}\ntone(flac): {tone_flac:?}\nsilence: {silence:?}");

    assert_eq!(
        engine.projector_inits(),
        1,
        "four media blobs must load the projector once, not four times (#301)"
    );

    // The cache must not have changed what the model produces. What a 3B model
    // hears in a 500 Hz tone is not a contract, so these are relations between
    // outputs rather than expected words:
    for (name, text) in [
        ("tone.wav", &tone_wav),
        ("tone.flac", &tone_flac),
        ("silence.wav", &silence),
    ] {
        assert!(
            !text.trim().is_empty(),
            "{name}: reached the projector but produced nothing"
        );
    }
    assert_eq!(
        tone_wav, tone_flac,
        "the WAV and FLAC hold the same samples, so at temperature 0 they must transcribe \
         identically — through the reused projector as they did through a fresh one"
    );
    assert_eq!(
        tone_wav, tone_again,
        "the same bytes, later in the run, must still transcribe the same: a projector that \
         accumulated state across blobs would show up here"
    );
    assert_ne!(
        tone_wav, silence,
        "different signals must reach the model differently — equality everywhere would also \
         be satisfied by a projector that had stopped seeing the samples at all, which is the \
         one way a cached projector could break while every other assertion here still held"
    );

    // Teardown, and the reason this binary's exit code is an assertion: the
    // engine owns the cached projector, so dropping it frees the projector's ggml
    // buffers, and only then can the backend go.
    drop(engine);
    assert!(
        release_shared_backend(),
        "with the engine gone the backend is releasable — nothing outlives it to exit"
    );
}
