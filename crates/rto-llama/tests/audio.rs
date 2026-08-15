//! End-to-end verification of the audio (ASR) path added in #18b.
//!
//! This drives the *new* code directly: a [`ChatRequest`] carrying audio bytes
//! goes through [`LlamaEngine`]'s multimodal dispatch → `chat_media` with
//! `Modality::Audio` → `MtmdContext::support_audio` → `MtmdBitmap::from_buffer`
//! (audio auto-detected + decoded by miniaudio) → `eval_chunks`. It is the honest
//! gate on the open question this modality raised: whether the llama.cpp `mtmd`
//! audio path actually transcribes on the Metal backend.
//!
//! It needs the `llama` feature and the Voxtral audio model on disk, so it is
//! `#[ignore]`d and self-skips when the model is missing — CI compiles it under
//! `--all-features` but does not run it:
//!
//! ```text
//! cargo test -p rto-llama --features llama --test audio -- --ignored --nocapture
//! ```
//!
//! The clip defaults to a committed, synthesised fixture (see
//! `crates/rto-graph/tests/fixtures/audio/README.md`), which exercises the decode
//! and projection path but is not speech — the model will not find real words in
//! it. To check the *transcription* rather than the path, point
//! `ROTEIRO_TEST_AUDIO_WAV` at an actual speech clip. On macOS (note
//! `-v Samantha`: the *default* `say` voice can emit near-silence, which the
//! model would then "transcribe" into hallucinated text):
//!
//! ```text
//! say -v Samantha -o /tmp/fox.aiff "The quick brown fox jumps over the lazy dog."
//! afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/fox.aiff /tmp/fox.wav
//! ROTEIRO_TEST_AUDIO_WAV=/tmp/fox.wav \
//!   cargo test -p rto-llama --features llama --test audio -- --ignored --nocapture
//! ```
#![cfg(feature = "llama")]

use std::path::PathBuf;

use rto_llama::llama::{LlamaEngine, Served};
use rto_llama::{ChatRequest, Engine, Message};

/// Registry name of the audio model (`~/.roteiro/models/<name>/`).
const MODEL: &str = "voxtral-mini-3b";

/// The default model store (`~/.roteiro/models/<name>/<file>`).
fn model_file(name: &str, file: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join(file);
    path.exists().then_some(path)
}

/// The clip used when `ROTEIRO_TEST_AUDIO_WAV` is unset: the committed,
/// synthesised speech-*shaped* fixture from `rto-graph`. It makes the test
/// runnable with nothing but the model installed, which is the point — before
/// the fixtures existed this test could not run at all without hand-making a
/// clip first.
fn default_clip() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rto-graph/tests/fixtures/audio/syllables-16khz-mono-512ms.wav")
}

#[test]
#[ignore = "needs the `llama` feature and the Voxtral GGUF on disk; slow + Metal-dependent"]
fn transcribes_speech_audio_via_mtmd() {
    let (Some(gguf), Some(mmproj)) = (
        model_file(MODEL, "model.gguf"),
        model_file(MODEL, "mmproj.gguf"),
    ) else {
        eprintln!("SKIP: `{MODEL}` not installed (run `roteiro model pull {MODEL}`)");
        return;
    };
    let wav = std::env::var_os("ROTEIRO_TEST_AUDIO_WAV").map_or_else(default_clip, PathBuf::from);
    let bytes = std::fs::read(&wav).expect("read audio fixture");
    eprintln!("clip: {} ({} bytes)", wav.display(), bytes.len());

    let engine = LlamaEngine::new(
        vec![Served {
            name: MODEL.to_owned(),
            path: gguf,
            mmproj: Some(mmproj),
        }],
        0,
    )
    .expect("engine inits");

    let out = engine
        .chat(&ChatRequest {
            model: MODEL.to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: std::env::var("ROTEIRO_TEST_PROMPT").unwrap_or_else(|_| {
                    "Transcribe this audio recording. Output only the spoken words.".to_owned()
                }),
            }],
            images: Vec::new(),
            audio: vec![bytes],
            temperature: 0.0,
            max_tokens: 128,
        })
        .expect("audio transcription completes");

    eprintln!("transcript: {:?}", out.content);
    // The projector decoded and transcribed the clip: a non-empty completion with
    // real token accounting. A broken audio path would panic, hang, or return an
    // empty completion here.
    assert!(
        !out.content.trim().is_empty(),
        "expected a non-empty transcript"
    );
    assert!(out.completion_tokens > 0, "expected generated tokens");
}
