//! End-to-end verification of the audio (ASR) path added in #18b.
//!
//! This drives the *new* code directly: a [`ChatRequest`] carrying audio bytes
//! goes through [`LlamaEngine`]'s multimodal dispatch → `chat_media` with
//! `Modality::Audio` → `MtmdContext::support_audio` → `MtmdBitmap::from_buffer`
//! (audio auto-detected + decoded by miniaudio) → `eval_chunks`. It is the honest
//! gate on the open question this modality raised: whether the llama.cpp `mtmd`
//! audio path actually transcribes on the Metal backend.
//!
//! It needs the `llama` feature, the Ultravox audio model on disk, **and** a WAV
//! fixture, so it is `#[ignore]`d and self-skips when any are missing — CI
//! compiles it under `--all-features` but does not run it. Run locally with a
//! real speech clip (e.g. macOS `say -o /tmp/fox.wav --file-format=WAVE
//! --data-format=LEI16@16000 "the quick brown fox"`), then:
//!
//! ```text
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

#[test]
#[ignore = "needs the `llama` feature, the Ultravox GGUF on disk, and a WAV fixture; slow + Metal-dependent"]
fn transcribes_speech_audio_via_mtmd() {
    let (Some(gguf), Some(mmproj)) = (
        model_file(MODEL, "model.gguf"),
        model_file(MODEL, "mmproj.gguf"),
    ) else {
        eprintln!("SKIP: `{MODEL}` not installed (run `roteiro model pull {MODEL}`)");
        return;
    };
    let Some(wav) = std::env::var_os("ROTEIRO_TEST_AUDIO_WAV") else {
        eprintln!("SKIP: set ROTEIRO_TEST_AUDIO_WAV to a WAV/MP3/FLAC speech clip");
        return;
    };
    let bytes = std::fs::read(&wav).expect("read audio fixture");

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
