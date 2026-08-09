//! Roteiro local model serving (ADR-0006): an opt-in, loopback,
//! OpenAI-compatible `/v1` endpoint over the models a user has already pulled,
//! so other tools (an Omnigent agent, an editor) can call them offline with no
//! second download.
//!
//! The crate is split so the HTTP surface is testable without a C++ build: the
//! pure-Rust [`server`] is written against the [`engine::Engine`] trait, and the
//! real llama.cpp-backed [`llama::LlamaEngine`] lives behind the `llama` feature.
//! Graph-tool auto-registration and `/v1/embeddings` land in later PRs.

pub mod engine;
pub mod server;
pub mod tools;
pub mod types;

#[cfg(feature = "llama")]
pub mod llama;

pub use engine::{
    ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
pub use server::{app, app_with_tools, serve_blocking, serve_blocking_with_tools};
pub use tools::{ToolDef, ToolRegistry, chat_with_tools};
