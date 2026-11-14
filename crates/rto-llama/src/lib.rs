//! Roteiro's inference interface: the [`Engine`] trait and its request/result
//! types. Every model call — Ask, `spec draft`, `review --llm`, embeddings,
//! image description, transcription — goes through one implementation of it,
//! the HTTP provider engine in the `roteiro` crate, local (Ollama,
//! `llama-server`) or hosted (ADR-0028).
//!
//! The in-process llama.cpp engine that used to live here was retired with the
//! GGUF model store (ADR-0033, #969): no C/C++ toolchain, and no vendored
//! llama.cpp advisories outside `cargo audit`'s reach. The crate keeps its name
//! because `rto-serve`, `rto-graph` and `roteiro` depend on it by version.

pub mod engine;
// Reading a reasoning model's `<think>` block: which part of a completion is the
// answer, and when there isn't one (#582/#583). Both consumers of the rule
// (`rto-serve`'s `/v1` endpoint and the `roteiro` CLI) need to see it.
pub mod thinking;
// Typed questions (#897): arithmetic over a closed set of labels. Kept for the
// types recorded runs carry; the in-process engine that supplied the logits is
// gone (ADR-0033).
pub mod typed;

pub use engine::{
    ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
pub use thinking::Unterminated;
pub use typed::{Answer, Choice, Noul, TypedError};
