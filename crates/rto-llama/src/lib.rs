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

pub mod engine;

#[cfg(feature = "llama")]
pub mod llama;

pub use engine::{
    ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
