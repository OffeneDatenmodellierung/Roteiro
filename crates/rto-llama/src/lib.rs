//! Roteiro's llama.cpp inference core (ADR-0003/0006): the [`Engine`] trait and
//! its request/result types, plus a llama.cpp-backed [`llama::LlamaEngine`] that
//! does generation, embeddings, and multimodal (vision) inference over local
//! GGUF models.
//!
//! This crate is the single place llama.cpp lives, so both **serving**
//! (`rto-serve`'s `/v1` endpoint) and **internal uses** (`infer` embeddings,
//! `sync` image understanding, `spec draft` generation) share one engine — it is
//! Roteiro's sole local-inference engine (candle has been retired). It
//! deliberately has **no HTTP/async deps**: the pure-Rust trait + types build
//! without the C++ engine, which lands behind the `llama` feature.
//!
//! llama.cpp's backend is a process-global, so it is initialised **once** and
//! shared by every engine ([`backend`], issue #296) — a second engine redirects
//! to the first instead of failing to construct and going silently inert.

pub mod engine;

// The process's single llama.cpp backend (issue #296): every engine holds a
// handle to it, and it is freed only once no engine borrows it any more.
#[cfg(feature = "llama")]
pub mod backend;
#[cfg(feature = "llama")]
pub mod llama;
// MTP speculative decoding (issue #320): the draft head a Qwen3.5+ GGUF already
// carries, used to propose the next few tokens so the target model can confirm
// several per decode instead of one. Same sampler call sequence as plain
// decoding, but completions can still differ in practice due to llama.cpp
// cross-batch numerics — see the `speculative` module docs.
#[cfg(feature = "llama")]
pub mod speculative;
// The build-once / release-deterministically holder that both the shared backend
// and `rto-graph`'s per-modality media engines live in. Compiled unconditionally
// — it needs no C/C++ toolchain — so its unit tests run in the default CI build.
pub mod slot;
// Reading a reasoning model's `<think>` block: which part of a completion is the
// answer, and when there isn't one (#582/#583). Compiled unconditionally — it
// needs no C/C++ toolchain, and both consumers of the rule (`rto-serve`'s `/v1`
// endpoint and the `roteiro` CLI) have to be able to see it from crates that do
// not agree on which llama.cpp features are on.
pub mod thinking;

pub use engine::{
    ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
pub use slot::{EngineSlot, KeyedSlot};
pub use thinking::Unterminated;
